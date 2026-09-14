//! Jupiter market-data adapter used by Phase 0 and the future attestor workers.
//!
//! The adapter stops at normalized observations. It does not decide eligibility,
//! sign reports, or settle a Battle. Keeping those responsibilities separate
//! prevents an HTTP response from becoming implicit competitive authority.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::{create_dir_all, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ed25519_dalek::Signer as DalekSigner;
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use protocol::{parse_decimal_q9, MathError};
use reqwest::StatusCode;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::value::RawValue;

/// Jupiter documents a maximum of 50 comma-separated price IDs per request.
pub const MAX_PRICE_REQUEST_MINTS: usize = 50;
pub const MAX_TOKEN_QUERY_MINTS: usize = 100;
pub const MAX_ELIGIBLE_ASSETS: usize = 256;
pub const DEFAULT_JUPITER_BASE_URL: &str = "https://api.jup.ag";

/// One normalized observation retained for evidence and later attestation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceObservation {
    pub mint: String,
    pub price_q9: i64,
    pub source_block_id: u64,
    pub observed_at_unix_ms: i64,
    pub raw_price: String,
}

/// The normalized result of one batched Price V3 request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceBatch {
    pub observations: Vec<PriceObservation>,
    pub missing_mints: Vec<String>,
    pub observed_at_unix_ms: i64,
    /// Request timing and status are retained with the normalized response so
    /// an evidence record can explain exactly when and how the source replied.
    #[serde(default)]
    pub request_started_at_unix_ms: i64,
    #[serde(default)]
    pub request_completed_at_unix_ms: i64,
    #[serde(default)]
    pub http_status: u16,
    /// The exact response body retained for the Phase 0 evidence record.
    #[serde(skip_serializing, skip_deserializing, default)]
    pub raw_response: String,
}

/// Exact-mint metadata returned by Jupiter Tokens V2.
///
/// These fields are evidence for registry and market-quality review only. They
/// never override the issuer-approved mint registry and never enter settlement
/// math automatically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JupiterTokenMetadata {
    #[serde(rename = "id")]
    pub mint: String,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    #[serde(default)]
    pub is_verified: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub liquidity: Option<f64>,
    #[serde(default)]
    pub holder_count: Option<u64>,
    #[serde(default)]
    pub organic_score: Option<f64>,
    #[serde(default)]
    pub price_block_id: Option<u64>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JupiterTokenBatch {
    pub tokens: Vec<JupiterTokenMetadata>,
    pub missing_mints: Vec<String>,
    pub raw_response: String,
}

/// One normalized observation loaded from a Phase 0 recorder file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase0Observation {
    pub attestor_id: String,
    pub mint: String,
    pub price_q9: i64,
    pub source_block_id: u64,
    pub observed_at_unix_ms: i64,
}

/// One observation accepted by an attestor for one asset/phase report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceptedPriceSample {
    pub source_block_id: u64,
    pub observed_at_unix_ms: i64,
    pub price_q9: i64,
}

impl AcceptedPriceSample {
    pub fn new(source_block_id: u64, observed_at_unix_ms: i64, price_q9: i64) -> Self {
        Self {
            source_block_id,
            observed_at_unix_ms,
            price_q9,
        }
    }
}

/// The frozen context that gives one attestor report its protocol meaning.
///
/// Every field is included in the canonical signed message. Keeping this
/// context explicit prevents a report from being copied between rounds,
/// phases, mints, policy versions, or Round Asset accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceReportContext {
    pub program_id: [u8; 32],
    pub market_round: [u8; 32],
    pub round_asset: [u8; 32],
    pub asset_id: u16,
    pub phase: u8,
    pub scoring_mint: [u8; 32],
    pub price_policy_version: u16,
    pub market_quality_policy_version: u16,
    pub attestor_set_version: u16,
    /// Solana Unix seconds, matching `MarketRound` and the on-chain message.
    pub observation_window_start: i64,
    /// Solana Unix seconds, matching `MarketRound` and the on-chain message.
    pub observation_window_end: i64,
    /// Solana Unix seconds, matching the on-chain `report_created_at` field.
    pub report_created_at: i64,
}

/// Calibrated evidence requirements copied into a report-building worker.
///
/// These values are deliberately supplied by the frozen PricePolicy rather
/// than invented by the attestor. Phase 0 is responsible for choosing and
/// versioning them before a rated round is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestorEvidencePolicy {
    pub min_accepted_observations: u16,
    pub min_unique_source_blocks: u16,
    pub max_source_block_lag: u64,
}

/// The exact numeric/evidence payload signed by one attestor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalPriceReport {
    pub context: PriceReportContext,
    pub median_price_q9: i64,
    pub accepted_observation_count: u16,
    pub unique_source_block_count: u16,
    pub first_source_block_id: u64,
    pub last_source_block_id: u64,
    pub evidence_root: [u8; 32],
}

impl CanonicalPriceReport {
    /// Returns the byte sequence that the Solana program reconstructs before
    /// inspecting the native Ed25519 verification instruction.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let context = self.context;
        protocol::attestation_message(
            context.program_id,
            context.market_round,
            context.round_asset,
            context.asset_id,
            context.phase,
            context.scoring_mint,
            context.price_policy_version,
            context.market_quality_policy_version,
            context.attestor_set_version,
            self.median_price_q9,
            self.accepted_observation_count,
            self.unique_source_block_count,
            self.first_source_block_id,
            self.last_source_block_id,
            self.evidence_root,
            context.observation_window_start,
            context.observation_window_end,
            context.report_created_at,
        )
    }
}

/// A canonical report plus the registered attestor signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedAttestorReport {
    pub report: CanonicalPriceReport,
    pub attestor: [u8; 32],
    /// Detached Ed25519 signature bytes. A vector keeps the persisted JSON
    /// representation portable; verification requires exactly 64 bytes.
    pub signature: Vec<u8>,
}

impl SignedAttestorReport {
    /// Verifies the detached signature against the exact canonical report
    /// bytes. This is an off-chain preflight; the on-chain relay still has to
    /// include and inspect the native Ed25519 instruction.
    pub fn verify(&self) -> Result<(), AttestorReportError> {
        let public_key = VerifyingKey::from_bytes(&self.attestor)
            .map_err(|_| AttestorReportError::InvalidSignature)?;
        let signature = Signature::from_slice(&self.signature)
            .map_err(|_| AttestorReportError::InvalidSignature)?;
        public_key
            .verify(&self.report.canonical_bytes(), &signature)
            .map_err(|_| AttestorReportError::InvalidSignature)
    }
}

/// A non-serializable signing boundary for one attestor worker.
///
/// Secret key material is accepted only at construction time and is never
/// included in report structures, debug output, or evidence logs.
pub struct AttestorSigner {
    signing_key: SigningKey,
}

impl AttestorSigner {
    pub fn from_secret_key(secret_key: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&secret_key),
        }
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    pub fn sign(&self, report: CanonicalPriceReport) -> SignedAttestorReport {
        let signature = self.signing_key.sign(&report.canonical_bytes());
        SignedAttestorReport {
            report,
            attestor: self.public_key_bytes(),
            signature: signature.to_bytes().to_vec(),
        }
    }
}

impl fmt::Debug for AttestorSigner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttestorSigner")
            .field("public_key", &self.public_key_bytes())
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

/// Errors raised while converting persisted observations into a signed report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttestorReportError {
    InvalidPhase,
    InvalidObservationWindow,
    InvalidReportTimestamp,
    InvalidPolicy,
    InvalidPrice(MathError),
    FutureSourceBlock,
    TooManyObservations,
    InsufficientAcceptedObservations { actual: usize, required: usize },
    InsufficientUniqueSourceBlocks { actual: usize, required: usize },
    InvalidSignature,
}

impl fmt::Display for AttestorReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPhase => formatter.write_str("price report phase must be START or END"),
            Self::InvalidObservationWindow => {
                formatter.write_str("price report observation window is invalid")
            }
            Self::InvalidReportTimestamp => {
                formatter.write_str("report timestamp is outside the observation window")
            }
            Self::InvalidPolicy => {
                formatter.write_str("attestor evidence policy thresholds must be positive")
            }
            Self::InvalidPrice(error) => write!(formatter, "invalid report price: {error}"),
            Self::FutureSourceBlock => {
                formatter.write_str("observation references a future source block")
            }
            Self::TooManyObservations => {
                formatter.write_str("report contains more observations than u16 can encode")
            }
            Self::InsufficientAcceptedObservations { actual, required } => write!(
                formatter,
                "report has {actual} accepted observations but requires {required}"
            ),
            Self::InsufficientUniqueSourceBlocks { actual, required } => write!(
                formatter,
                "report has {actual} unique source blocks but requires {required}"
            ),
            Self::InvalidSignature => formatter.write_str("attestor signature is invalid"),
        }
    }
}

impl std::error::Error for AttestorReportError {}

/// Builds the full report that the attestor signs for one Round Asset phase.
///
/// Samples outside the frozen window and samples older than the frozen source
/// lag are excluded. A source block newer than the worker's observed current
/// block is rejected because accepting it would make freshness accounting
/// ambiguous. Repeated polls remain accepted observations, while the unique
/// block count records independent source updates separately.
pub fn build_canonical_price_report(
    context: PriceReportContext,
    samples: &[AcceptedPriceSample],
    policy: AttestorEvidencePolicy,
    current_source_block_id: u64,
) -> Result<CanonicalPriceReport, AttestorReportError> {
    if context.phase > 1 {
        return Err(AttestorReportError::InvalidPhase);
    }
    if context.observation_window_start >= context.observation_window_end {
        return Err(AttestorReportError::InvalidObservationWindow);
    }
    if context.report_created_at < context.observation_window_start
        || context.report_created_at > context.observation_window_end
    {
        return Err(AttestorReportError::InvalidReportTimestamp);
    }
    if policy.min_accepted_observations == 0 || policy.min_unique_source_blocks == 0 {
        return Err(AttestorReportError::InvalidPolicy);
    }

    let window_start_ms = context
        .observation_window_start
        .checked_mul(1_000)
        .ok_or(AttestorReportError::InvalidObservationWindow)?;
    let window_end_ms = context
        .observation_window_end
        .checked_mul(1_000)
        .ok_or(AttestorReportError::InvalidObservationWindow)?;

    let mut accepted = Vec::new();
    let mut unique_source_blocks = BTreeSet::new();
    for sample in samples {
        if sample.source_block_id > current_source_block_id {
            return Err(AttestorReportError::FutureSourceBlock);
        }
        if sample.observed_at_unix_ms < window_start_ms
            || sample.observed_at_unix_ms >= window_end_ms
            || current_source_block_id - sample.source_block_id > policy.max_source_block_lag
        {
            continue;
        }
        if sample.price_q9 <= 0 {
            return Err(AttestorReportError::InvalidPrice(
                MathError::NonPositivePrice,
            ));
        }
        accepted.push(*sample);
        unique_source_blocks.insert(sample.source_block_id);
    }

    if accepted.len() > usize::from(u16::MAX) {
        return Err(AttestorReportError::TooManyObservations);
    }
    if accepted.len() < usize::from(policy.min_accepted_observations) {
        return Err(AttestorReportError::InsufficientAcceptedObservations {
            actual: accepted.len(),
            required: usize::from(policy.min_accepted_observations),
        });
    }
    if unique_source_blocks.len() < usize::from(policy.min_unique_source_blocks) {
        return Err(AttestorReportError::InsufficientUniqueSourceBlocks {
            actual: unique_source_blocks.len(),
            required: usize::from(policy.min_unique_source_blocks),
        });
    }

    let prices: Vec<i64> = accepted.iter().map(|sample| sample.price_q9).collect();
    let protocol_samples: Vec<protocol::PriceSample> = accepted
        .iter()
        .map(|sample| protocol::PriceSample {
            market_round: context.market_round,
            asset_id: context.asset_id,
            phase: context.phase,
            scoring_mint: context.scoring_mint,
            source_block_id: sample.source_block_id,
            observed_at_unix_ms: sample.observed_at_unix_ms,
            price_q9: sample.price_q9,
        })
        .collect();
    let mut source_blocks: Vec<u64> = unique_source_blocks.into_iter().collect();
    source_blocks.sort_unstable();

    Ok(CanonicalPriceReport {
        context,
        median_price_q9: protocol::median_q9(&prices).map_err(AttestorReportError::InvalidPrice)?,
        accepted_observation_count: u16::try_from(accepted.len())
            .map_err(|_| AttestorReportError::TooManyObservations)?,
        unique_source_block_count: u16::try_from(source_blocks.len())
            .map_err(|_| AttestorReportError::TooManyObservations)?,
        first_source_block_id: *source_blocks
            .first()
            .expect("minimum unique source blocks guarantees a first block"),
        last_source_block_id: *source_blocks
            .last()
            .expect("minimum unique source blocks guarantees a last block"),
        evidence_root: protocol::evidence_root(&protocol_samples),
    })
}

/// One append-only source response record retained for evidence and replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceBatchRecord {
    pub schema_version: u16,
    pub market_round: [u8; 32],
    pub phase: u8,
    pub attestor: [u8; 32],
    pub requested_mints: Vec<String>,
    pub request_started_at_unix_ms: i64,
    pub request_completed_at_unix_ms: i64,
    pub http_status: Option<u16>,
    pub observations: Vec<PriceObservation>,
    pub missing_mints: Vec<String>,
    pub raw_response: Option<String>,
    pub error: Option<String>,
}

impl EvidenceBatchRecord {
    /// Creates a durable success record from one normalized Jupiter batch.
    pub fn from_success(
        market_round: [u8; 32],
        phase: u8,
        attestor: [u8; 32],
        requested_mints: Vec<String>,
        batch: PriceBatch,
    ) -> Self {
        Self {
            schema_version: 1,
            market_round,
            phase,
            attestor,
            requested_mints,
            request_started_at_unix_ms: batch.request_started_at_unix_ms,
            request_completed_at_unix_ms: batch.request_completed_at_unix_ms,
            http_status: Some(batch.http_status),
            observations: batch.observations,
            missing_mints: batch.missing_mints,
            raw_response: Some(batch.raw_response),
            error: None,
        }
    }

    /// Creates a durable failure record. The raw response is retained when
    /// Jupiter returned one; transport failures have no response body.
    pub fn from_failure(
        market_round: [u8; 32],
        phase: u8,
        attestor: [u8; 32],
        requested_mints: Vec<String>,
        request_started_at_unix_ms: i64,
        request_completed_at_unix_ms: i64,
        error: &JupiterClientError,
    ) -> Self {
        Self {
            schema_version: 1,
            market_round,
            phase,
            attestor,
            requested_mints,
            request_started_at_unix_ms,
            request_completed_at_unix_ms,
            http_status: error.http_status(),
            observations: Vec::new(),
            missing_mints: Vec::new(),
            raw_response: error.raw_response().map(str::to_owned),
            error: Some(error.to_string()),
        }
    }

    /// Extracts observations for one exact requested mint. Only successful
    /// records matching the report's round and phase are considered.
    pub fn samples_for_mint(
        records: &[Self],
        market_round: [u8; 32],
        phase: u8,
        attestor: [u8; 32],
        mint: &str,
    ) -> Vec<AcceptedPriceSample> {
        records
            .iter()
            .filter(|record| {
                record.market_round == market_round
                    && record.phase == phase
                    && record.attestor == attestor
                    && record.error.is_none()
            })
            .flat_map(|record| {
                record
                    .observations
                    .iter()
                    .filter(move |observation| observation.mint == mint)
                    .map(|observation| {
                        AcceptedPriceSample::new(
                            observation.source_block_id,
                            observation.observed_at_unix_ms,
                            observation.price_q9,
                        )
                    })
            })
            .collect()
    }

    /// Returns the greatest source block observed in this record, if any.
    pub fn max_source_block_id(&self) -> Option<u64> {
        self.observations
            .iter()
            .map(|observation| observation.source_block_id)
            .max()
    }
}

/// Errors returned by the append-only JSONL evidence store.
#[derive(Debug)]
pub enum JsonlStoreError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for JsonlStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "evidence store I/O error: {error}"),
            Self::Json(error) => write!(formatter, "evidence store JSON error: {error}"),
        }
    }
}

impl std::error::Error for JsonlStoreError {}

/// A durable append-only JSONL writer used by each attestor process.
pub struct JsonlStore {
    file: File,
}

impl JsonlStore {
    pub fn open_append(path: impl AsRef<Path>) -> Result<Self, JsonlStoreError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            create_dir_all(parent).map_err(JsonlStoreError::Io)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(JsonlStoreError::Io)?;
        Ok(Self { file })
    }

    /// Writes one complete record, flushes it, and syncs the file data before
    /// returning so a successful call represents durable local evidence.
    pub fn append<T: Serialize>(&mut self, record: &T) -> Result<(), JsonlStoreError> {
        serde_json::to_writer(&mut self.file, record).map_err(JsonlStoreError::Json)?;
        self.file.write_all(b"\n").map_err(JsonlStoreError::Io)?;
        self.file.flush().map_err(JsonlStoreError::Io)?;
        self.file.sync_data().map_err(JsonlStoreError::Io)
    }
}

/// Loads an append-only JSONL file without silently skipping malformed rows.
/// Missing files are treated as an empty log so a first worker run can create
/// its output naturally.
pub fn read_jsonl<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<Vec<T>, JsonlStoreError> {
    let file = match File::open(path.as_ref()) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(JsonlStoreError::Io(error)),
    };
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(JsonlStoreError::Io)?;
        if line.trim().is_empty() {
            continue;
        }
        records.push(serde_json::from_str(&line).map_err(JsonlStoreError::Json)?);
    }
    Ok(records)
}

/// The exact numeric/evidence payload an attestor signs for one asset phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttestorPriceReport {
    pub market_round: [u8; 32],
    pub asset_id: u16,
    pub phase: u8,
    pub scoring_mint: [u8; 32],
    pub median_price_q9: i64,
    pub accepted_observation_count: u16,
    pub unique_source_block_count: u16,
    pub first_source_block_id: u64,
    pub last_source_block_id: u64,
    pub evidence_root: [u8; 32],
}

/// Computes an attestor median and evidence metadata using the shared rules.
pub fn build_attestor_price_report(
    market_round: [u8; 32],
    asset_id: u16,
    phase: u8,
    scoring_mint: [u8; 32],
    samples: &[AcceptedPriceSample],
) -> Result<AttestorPriceReport, JupiterParseError> {
    if phase > 1 || samples.is_empty() || samples.len() > usize::from(u16::MAX) {
        return Err(JupiterParseError::InvalidRequest(
            "attestor report phase or sample set is invalid".to_owned(),
        ));
    }
    let mut prices = Vec::with_capacity(samples.len());
    let mut source_blocks = BTreeSet::new();
    let mut source_block_bounds = Vec::with_capacity(samples.len());
    let mut protocol_samples = Vec::with_capacity(samples.len());
    for sample in samples {
        if sample.price_q9 <= 0 {
            return Err(JupiterParseError::Price(MathError::NonPositivePrice));
        }
        prices.push(sample.price_q9);
        source_blocks.insert(sample.source_block_id);
        source_block_bounds.push(sample.source_block_id);
        protocol_samples.push(protocol::PriceSample {
            market_round,
            asset_id,
            phase,
            scoring_mint,
            source_block_id: sample.source_block_id,
            observed_at_unix_ms: sample.observed_at_unix_ms,
            price_q9: sample.price_q9,
        });
    }
    source_block_bounds.sort_unstable();
    let median_price_q9 = protocol::median_q9(&prices).map_err(JupiterParseError::Price)?;
    Ok(AttestorPriceReport {
        market_round,
        asset_id,
        phase,
        scoring_mint,
        median_price_q9,
        accepted_observation_count: u16::try_from(samples.len())
            .map_err(|_| JupiterParseError::Price(MathError::Overflow))?,
        unique_source_block_count: u16::try_from(source_blocks.len())
            .map_err(|_| JupiterParseError::Price(MathError::Overflow))?,
        first_source_block_id: *source_block_bounds.first().expect("samples is non-empty"),
        last_source_block_id: *source_block_bounds.last().expect("samples is non-empty"),
        evidence_root: protocol::evidence_root(&protocol_samples),
    })
}

impl Phase0Observation {
    pub fn new(
        attestor_id: impl Into<String>,
        mint: impl Into<String>,
        price_q9: i64,
        source_block_id: u64,
        observed_at_unix_ms: i64,
    ) -> Self {
        Self {
            attestor_id: attestor_id.into(),
            mint: mint.into(),
            price_q9,
            source_block_id,
            observed_at_unix_ms,
        }
    }
}

/// Per-mint facts reported by the Phase 0 analyzer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Phase0AssetSummary {
    pub mint: String,
    pub missing: bool,
    pub observation_count: usize,
    pub unique_source_block_count: usize,
    pub stale_observation_count: usize,
    pub attestor_count: usize,
    pub min_price_q9: Option<i64>,
    pub max_price_q9: Option<i64>,
    pub price_range_bps: Option<u64>,
}

/// A measurement-only summary for one candidate observation duration.
///
/// This output intentionally contains facts and not pass/fail decisions. A
/// human or separately versioned policy review must choose calibrated minimums
/// before a rated Market Round can use them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Phase0WindowSummary {
    pub window_start_unix_ms: i64,
    pub window_end_unix_ms: i64,
    pub window_secs: u64,
    pub total_observations: usize,
    pub assets: Vec<Phase0AssetSummary>,
}

/// Summarizes coverage, repeated source blocks, attestor participation, and
/// observed price range without fabricating eligibility thresholds.
pub fn summarize_phase0_window(
    requested_mints: &[String],
    observations: &[Phase0Observation],
    window_start_unix_ms: i64,
    window_secs: u64,
) -> Result<Phase0WindowSummary, JupiterParseError> {
    if requested_mints.is_empty() || window_secs == 0 {
        return Err(JupiterParseError::InvalidRequest(
            "Phase 0 summary requires mints and a non-zero window".to_owned(),
        ));
    }
    let window_millis = window_secs.checked_mul(1_000).ok_or_else(|| {
        JupiterParseError::InvalidRequest("Phase 0 window is too large".to_owned())
    })?;
    let window_end_unix_ms = window_start_unix_ms
        .checked_add(i64::try_from(window_millis).map_err(|_| {
            JupiterParseError::InvalidRequest("Phase 0 window is too large".to_owned())
        })?)
        .ok_or_else(|| {
            JupiterParseError::InvalidRequest("Phase 0 window overflows time".to_owned())
        })?;
    validate_requested_mints(requested_mints)?;

    let requested: BTreeSet<&str> = requested_mints.iter().map(String::as_str).collect();
    let mut grouped: BTreeMap<&str, Vec<&Phase0Observation>> = requested_mints
        .iter()
        .map(|mint| (mint.as_str(), Vec::new()))
        .collect();
    for observation in observations.iter().filter(|observation| {
        observation.observed_at_unix_ms >= window_start_unix_ms
            && observation.observed_at_unix_ms < window_end_unix_ms
    }) {
        if !requested.contains(observation.mint.as_str()) {
            continue;
        }
        if observation.price_q9 <= 0 {
            return Err(JupiterParseError::Price(MathError::NonPositivePrice));
        }
        grouped
            .get_mut(observation.mint.as_str())
            .expect("requested mint was inserted into the grouping map")
            .push(observation);
    }

    let mut assets = Vec::with_capacity(requested_mints.len());
    let mut total_observations = 0;
    for mint in requested_mints {
        let samples = grouped
            .get(mint.as_str())
            .expect("requested mint was inserted into the grouping map");
        let mut independent_blocks = BTreeSet::new();
        let mut attestors = BTreeSet::new();
        let mut min_price = None;
        let mut max_price = None;
        for sample in samples {
            independent_blocks.insert((sample.attestor_id.as_str(), sample.source_block_id));
            attestors.insert(sample.attestor_id.as_str());
            min_price =
                Some(min_price.map_or(sample.price_q9, |value: i64| value.min(sample.price_q9)));
            max_price =
                Some(max_price.map_or(sample.price_q9, |value: i64| value.max(sample.price_q9)));
        }
        let observation_count = samples.len();
        total_observations += observation_count;
        let stale_observation_count = observation_count.saturating_sub(independent_blocks.len());
        let price_range_bps = match (min_price, max_price) {
            (Some(minimum), Some(maximum)) => {
                let range = i128::from(maximum - minimum)
                    .checked_mul(10_000)
                    .and_then(|value| value.checked_div(i128::from(minimum)))
                    .ok_or(JupiterParseError::Price(MathError::Overflow))?;
                Some(
                    u64::try_from(range)
                        .map_err(|_| JupiterParseError::Price(MathError::Overflow))?,
                )
            }
            _ => None,
        };
        assets.push(Phase0AssetSummary {
            mint: mint.clone(),
            missing: observation_count == 0,
            observation_count,
            unique_source_block_count: independent_blocks.len(),
            stale_observation_count,
            attestor_count: attestors.len(),
            min_price_q9: min_price,
            max_price_q9: max_price,
            price_range_bps,
        });
    }

    Ok(Phase0WindowSummary {
        window_start_unix_ms,
        window_end_unix_ms,
        window_secs,
        total_observations,
        assets,
    })
}

#[derive(Debug, Deserialize)]
struct JupiterPriceRecord<'a> {
    #[serde(rename = "usdPrice", borrow)]
    usd_price: &'a RawValue,
    #[serde(rename = "blockId")]
    block_id: u64,
}

#[derive(Debug)]
pub enum JupiterParseError {
    InvalidRequest(String),
    InvalidJson(serde_json::Error),
    Price(MathError),
    MissingField(String),
}

impl fmt::Display for JupiterParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => formatter.write_str(message),
            Self::InvalidJson(error) => write!(formatter, "invalid Jupiter JSON: {error}"),
            Self::Price(error) => write!(formatter, "invalid Jupiter price: {error}"),
            Self::MissingField(mint) => {
                write!(formatter, "Jupiter response is missing price for {mint}")
            }
        }
    }
}

impl std::error::Error for JupiterParseError {}

/// Parses a Price V3 response while preserving the source decimal lexeme.
///
/// `RawValue` is important here: deserializing `usdPrice` into `f64` would
/// violate the protocol's exact decimal-to-Q9 rule before settlement code sees
/// the value. Missing mints are returned explicitly because Jupiter omits them
/// when no reliable price is available.
pub fn parse_jupiter_price_response(
    requested_mints: &[String],
    body: &str,
    observed_at_unix_ms: i64,
) -> Result<PriceBatch, JupiterParseError> {
    validate_requested_mints(requested_mints)?;
    let response: std::collections::BTreeMap<String, JupiterPriceRecord<'_>> =
        serde_json::from_str(body).map_err(JupiterParseError::InvalidJson)?;

    let mut observations = Vec::with_capacity(response.len());
    let mut missing_mints = Vec::new();
    for mint in requested_mints {
        let Some(record) = response.get(mint) else {
            missing_mints.push(mint.clone());
            continue;
        };

        let raw_price = raw_decimal_text(record.usd_price)?;
        let price_q9 = parse_decimal_q9(&raw_price).map_err(JupiterParseError::Price)?;
        observations.push(PriceObservation {
            mint: mint.clone(),
            price_q9,
            source_block_id: record.block_id,
            observed_at_unix_ms,
            raw_price,
        });
    }

    Ok(PriceBatch {
        observations,
        missing_mints,
        observed_at_unix_ms,
        request_started_at_unix_ms: 0,
        request_completed_at_unix_ms: 0,
        http_status: 0,
        raw_response: body.to_owned(),
    })
}

/// Parses Tokens V2 metadata while retaining only records for the requested
/// exact mints. Unexpected response records are ignored rather than becoming
/// eligible registry entries by accident.
pub fn parse_jupiter_token_response(
    requested_mints: &[String],
    body: &str,
) -> Result<JupiterTokenBatch, JupiterParseError> {
    validate_token_mints(requested_mints)?;
    let response: Vec<JupiterTokenMetadata> =
        serde_json::from_str(body).map_err(JupiterParseError::InvalidJson)?;
    let requested: BTreeSet<&str> = requested_mints.iter().map(String::as_str).collect();
    let mut seen = BTreeSet::new();
    let mut tokens = Vec::new();
    for token in response {
        if requested.contains(token.mint.as_str()) {
            if !seen.insert(token.mint.clone()) {
                return Err(JupiterParseError::InvalidRequest(
                    "Jupiter returned duplicate metadata for a requested mint".to_owned(),
                ));
            }
            tokens.push(token);
        }
    }
    let missing_mints = requested_mints
        .iter()
        .filter(|mint| !seen.contains(*mint))
        .cloned()
        .collect();
    Ok(JupiterTokenBatch {
        tokens,
        missing_mints,
        raw_response: body.to_owned(),
    })
}

fn validate_requested_mints(requested_mints: &[String]) -> Result<(), JupiterParseError> {
    if requested_mints.is_empty() {
        return Err(JupiterParseError::InvalidRequest(
            "at least one mint is required".to_owned(),
        ));
    }
    if requested_mints.len() > MAX_PRICE_REQUEST_MINTS {
        return Err(JupiterParseError::InvalidRequest(format!(
            "Jupiter accepts at most {MAX_PRICE_REQUEST_MINTS} mints per price request"
        )));
    }

    let mut unique = BTreeSet::new();
    for mint in requested_mints {
        if mint.is_empty() || !unique.insert(mint) {
            return Err(JupiterParseError::InvalidRequest(
                "requested mints must be non-empty and unique".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_token_mints(requested_mints: &[String]) -> Result<(), JupiterParseError> {
    if requested_mints.is_empty() {
        return Err(JupiterParseError::InvalidRequest(
            "at least one mint is required".to_owned(),
        ));
    }
    if requested_mints.len() > MAX_TOKEN_QUERY_MINTS {
        return Err(JupiterParseError::InvalidRequest(format!(
            "Jupiter accepts at most {MAX_TOKEN_QUERY_MINTS} mints per metadata request"
        )));
    }
    let mut unique = BTreeSet::new();
    for mint in requested_mints {
        if mint.is_empty() || !unique.insert(mint) {
            return Err(JupiterParseError::InvalidRequest(
                "requested mints must be non-empty and unique".to_owned(),
            ));
        }
    }
    Ok(())
}

fn raw_decimal_text(raw: &RawValue) -> Result<String, JupiterParseError> {
    let text = raw.get().trim();
    if text.starts_with('"') {
        serde_json::from_str::<String>(text).map_err(JupiterParseError::InvalidJson)
    } else {
        Ok(text.to_owned())
    }
}

/// Tracks total accepted polls separately from unique source blocks.
///
/// Re-polling a stale Jupiter `blockId` may be useful evidence for availability,
/// but it must never inflate the independent-update count used by quality gates.
#[derive(Debug, Default, Clone)]
pub struct EvidenceLedger {
    accepted: Vec<(String, u64, i64)>,
    unique_source_blocks: BTreeSet<(String, u64)>,
}

impl EvidenceLedger {
    pub fn record(&mut self, mint: impl Into<String>, source_block_id: u64, price_q9: i64) {
        let mint = mint.into();
        self.unique_source_blocks
            .insert((mint.clone(), source_block_id));
        self.accepted.push((mint, source_block_id, price_q9));
    }

    pub fn accepted_observation_count(&self) -> usize {
        self.accepted.len()
    }

    pub fn unique_source_block_count(&self, mint: &str) -> usize {
        self.unique_source_blocks
            .iter()
            .filter(|(stored_mint, _)| stored_mint == mint)
            .count()
    }

    pub fn has_enough_unique_source_blocks(&self, mint: &str, minimum: usize) -> bool {
        self.unique_source_block_count(mint) >= minimum
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplingPlanError {
    ZeroAttestors,
    ZeroRequests,
    ZeroInterval,
    ZeroProviderLimit,
    ProviderRateLimitExceeded {
        aggregate_milli_rps: u64,
        limit_milli_rps: u64,
    },
    Overflow,
}

impl fmt::Display for SamplingPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroAttestors => formatter.write_str("attestor count must be non-zero"),
            Self::ZeroRequests => formatter.write_str("request batch count must be non-zero"),
            Self::ZeroInterval => formatter.write_str("sample interval must be non-zero"),
            Self::ZeroProviderLimit => formatter.write_str("provider rate limit must be non-zero"),
            Self::ProviderRateLimitExceeded {
                aggregate_milli_rps,
                limit_milli_rps,
            } => write!(
                formatter,
                "sampling plan requires {aggregate_milli_rps} milli-RPS but provider allows {limit_milli_rps}"
            ),
            Self::Overflow => formatter.write_str("sampling plan arithmetic overflow"),
        }
    }
}

impl std::error::Error for SamplingPlanError {}

/// Validates the aggregate request rate for one shared sampling plan.
///
/// Rates are expressed in milli-RPS to keep the operational gate exact. For
/// example, three attestors polling every five seconds require 600 milli-RPS,
/// which fits the documented one-request-per-second free API plan.
pub fn validate_sampling_plan(
    attestor_count: usize,
    sample_interval_secs: u64,
    provider_limit_milli_rps: u64,
) -> Result<(), SamplingPlanError> {
    validate_batched_sampling_plan(
        attestor_count,
        1,
        sample_interval_secs,
        provider_limit_milli_rps,
    )
}

/// Validates an aggregate plan when one sampling tick requires multiple
/// Jupiter requests because the eligible universe exceeds the provider's
/// per-request mint limit.
pub fn validate_batched_sampling_plan(
    attestor_count: usize,
    requests_per_sample: usize,
    sample_interval_secs: u64,
    provider_limit_milli_rps: u64,
) -> Result<(), SamplingPlanError> {
    if attestor_count == 0 {
        return Err(SamplingPlanError::ZeroAttestors);
    }
    if requests_per_sample == 0 {
        return Err(SamplingPlanError::ZeroRequests);
    }
    if sample_interval_secs == 0 {
        return Err(SamplingPlanError::ZeroInterval);
    }
    if provider_limit_milli_rps == 0 {
        return Err(SamplingPlanError::ZeroProviderLimit);
    }

    let aggregate_milli_rps = (attestor_count as u128)
        .checked_mul(requests_per_sample as u128)
        .and_then(|value| value.checked_mul(1_000))
        .and_then(|value| value.checked_add(u128::from(sample_interval_secs) - 1))
        .and_then(|value| value.checked_div(u128::from(sample_interval_secs)))
        .ok_or(SamplingPlanError::Overflow)?;
    let aggregate_milli_rps =
        u64::try_from(aggregate_milli_rps).map_err(|_| SamplingPlanError::Overflow)?;
    if aggregate_milli_rps > provider_limit_milli_rps {
        return Err(SamplingPlanError::ProviderRateLimitExceeded {
            aggregate_milli_rps,
            limit_milli_rps: provider_limit_milli_rps,
        });
    }

    Ok(())
}

/// Returns deterministic millisecond offsets inside each sampling interval.
pub fn stagger_offsets_millis(
    attestor_count: usize,
    sample_interval_secs: u64,
) -> Result<Vec<u64>, SamplingPlanError> {
    if attestor_count == 0 {
        return Err(SamplingPlanError::ZeroAttestors);
    }
    if sample_interval_secs == 0 {
        return Err(SamplingPlanError::ZeroInterval);
    }

    let interval_millis = sample_interval_secs
        .checked_mul(1_000)
        .ok_or(SamplingPlanError::Overflow)?;
    (0..attestor_count)
        .map(|index| {
            interval_millis
                .checked_mul(index as u64)
                .and_then(|value| value.checked_div(attestor_count as u64))
                .ok_or(SamplingPlanError::Overflow)
        })
        .collect()
}

#[derive(Debug)]
pub enum JupiterClientError {
    InvalidRequest(String),
    Http(reqwest::Error),
    HttpStatus {
        status: StatusCode,
        body: String,
    },
    Parse(JupiterParseError),
    ParseResponse {
        error: JupiterParseError,
        body: String,
    },
}

impl fmt::Display for JupiterClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => formatter.write_str(message),
            Self::Http(error) => write!(formatter, "Jupiter request failed: {error}"),
            Self::HttpStatus { status, body } => {
                write!(
                    formatter,
                    "Jupiter returned {status}: {}",
                    truncate_body(body)
                )
            }
            Self::Parse(error) => error.fmt(formatter),
            Self::ParseResponse { error, .. } => error.fmt(formatter),
        }
    }
}

impl std::error::Error for JupiterClientError {}

impl JupiterClientError {
    /// Returns an HTTP status when Jupiter returned a response. Transport and
    /// parse failures intentionally return `None` because no trustworthy
    /// status is available to the caller.
    pub fn http_status(&self) -> Option<u16> {
        match self {
            Self::HttpStatus { status, .. } => Some(status.as_u16()),
            Self::ParseResponse { .. } => Some(StatusCode::OK.as_u16()),
            _ => None,
        }
    }

    /// Exposes the raw response body for durable evidence persistence without
    /// exposing it through the human-readable error formatter.
    pub fn raw_response(&self) -> Option<&str> {
        match self {
            Self::HttpStatus { body, .. } | Self::ParseResponse { body, .. } => Some(body),
            _ => None,
        }
    }
}

/// Thin asynchronous client for the official Jupiter Price V3 endpoint.
pub struct JupiterClient {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl JupiterClient {
    pub fn new(api_key: Option<String>) -> Result<Self, JupiterClientError> {
        Self::with_base_url(DEFAULT_JUPITER_BASE_URL, api_key)
    }

    pub fn with_base_url(
        base_url: impl Into<String>,
        api_key: Option<String>,
    ) -> Result<Self, JupiterClientError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if base_url.is_empty() {
            return Err(JupiterClientError::InvalidRequest(
                "Jupiter base URL cannot be empty".to_owned(),
            ));
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(JupiterClientError::Http)?;
        Ok(Self {
            client,
            base_url,
            api_key,
        })
    }

    /// Fetches a single batched price response and normalizes it into evidence.
    pub async fn fetch_prices(
        &self,
        requested_mints: &[String],
        observed_at_unix_ms: i64,
    ) -> Result<PriceBatch, JupiterClientError> {
        validate_requested_mints(requested_mints).map_err(JupiterClientError::Parse)?;

        let request_started_at_unix_ms =
            unix_timestamp_millis().map_err(JupiterClientError::InvalidRequest)?;

        let mut request = self
            .client
            .get(format!("{}/price/v3", self.base_url))
            .query(&[("ids", requested_mints.join(","))]);
        if let Some(api_key) = &self.api_key {
            request = request.header("x-api-key", api_key);
        }

        let response = request.send().await.map_err(JupiterClientError::Http)?;
        let status = response.status();
        let body = response.text().await.map_err(JupiterClientError::Http)?;
        let request_completed_at_unix_ms =
            unix_timestamp_millis().map_err(JupiterClientError::InvalidRequest)?;
        if !status.is_success() {
            return Err(JupiterClientError::HttpStatus { status, body });
        }

        let mut batch = parse_jupiter_price_response(requested_mints, &body, observed_at_unix_ms)
            .map_err(|error| JupiterClientError::ParseResponse {
            error,
            body: body.clone(),
        })?;
        batch.request_started_at_unix_ms = request_started_at_unix_ms;
        batch.request_completed_at_unix_ms = request_completed_at_unix_ms;
        batch.http_status = status.as_u16();
        Ok(batch)
    }

    /// Fetches metadata for the exact registry mints in one Tokens V2 query.
    pub async fn fetch_token_information(
        &self,
        requested_mints: &[String],
    ) -> Result<JupiterTokenBatch, JupiterClientError> {
        validate_token_mints(requested_mints).map_err(JupiterClientError::Parse)?;

        let mut request = self
            .client
            .get(format!("{}/tokens/v2/search", self.base_url))
            .query(&[("query", requested_mints.join(","))]);
        if let Some(api_key) = &self.api_key {
            request = request.header("x-api-key", api_key);
        }

        let response = request.send().await.map_err(JupiterClientError::Http)?;
        let status = response.status();
        let body = response.text().await.map_err(JupiterClientError::Http)?;
        if !status.is_success() {
            return Err(JupiterClientError::HttpStatus {
                status,
                body: truncate_body(&body),
            });
        }

        parse_jupiter_token_response(requested_mints, &body).map_err(JupiterClientError::Parse)
    }
}

fn unix_timestamp_millis() -> Result<i64, String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))?;
    i64::try_from(duration.as_millis())
        .map_err(|_| "system clock timestamp overflowed i64".to_owned())
}

fn truncate_body(body: &str) -> String {
    const MAX_ERROR_BODY_CHARS: usize = 512;
    body.chars().take(MAX_ERROR_BODY_CHARS).collect()
}
