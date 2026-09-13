//! Jupiter market-data adapter used by Phase 0 and the future attestor workers.
//!
//! The adapter stops at normalized observations. It does not decide eligibility,
//! sign reports, or settle a Battle. Keeping those responsibilities separate
//! prevents an HTTP response from becoming implicit competitive authority.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    time::Duration,
};

use protocol::{parse_decimal_q9, MathError};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// Jupiter documents a maximum of 50 comma-separated price IDs per request.
pub const MAX_PRICE_REQUEST_MINTS: usize = 50;
pub const MAX_TOKEN_QUERY_MINTS: usize = 100;
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
    if attestor_count == 0 {
        return Err(SamplingPlanError::ZeroAttestors);
    }
    if sample_interval_secs == 0 {
        return Err(SamplingPlanError::ZeroInterval);
    }
    if provider_limit_milli_rps == 0 {
        return Err(SamplingPlanError::ZeroProviderLimit);
    }

    let aggregate_milli_rps = (attestor_count as u128)
        .checked_mul(1_000)
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
    HttpStatus { status: StatusCode, body: String },
    Parse(JupiterParseError),
}

impl fmt::Display for JupiterClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => formatter.write_str(message),
            Self::Http(error) => write!(formatter, "Jupiter request failed: {error}"),
            Self::HttpStatus { status, body } => {
                write!(formatter, "Jupiter returned {status}: {body}")
            }
            Self::Parse(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for JupiterClientError {}

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
        if !status.is_success() {
            return Err(JupiterClientError::HttpStatus {
                status,
                body: truncate_body(&body),
            });
        }

        parse_jupiter_price_response(requested_mints, &body, observed_at_unix_ms)
            .map_err(JupiterClientError::Parse)
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

fn truncate_body(body: &str) -> String {
    const MAX_ERROR_BODY_CHARS: usize = 512;
    body.chars().take(MAX_ERROR_BODY_CHARS).collect()
}
