//! Optional sponsor-track readiness checks for Phase 0 Slice 0.2.
//!
//! Pyth is represented as a source-specific, cryptographically-gated evidence
//! path. This module validates the application-level payload contract and
//! exact Q9 conversion, but it does not claim that a payload is verified on
//! Solana merely because its `solana` field contains hex bytes. Devnet
//! verifier evidence remains an explicit activation input.
//!
//! PreStocks and Tessera are parsed as private-market representations. Their
//! provider disclosures are preserved in the descriptor classification so
//! later UI/proof work cannot accidentally call economic exposure ordinary
//! equity or enable it for Public Ranked settlement.

use std::{fmt, time::Duration};

use protocol::parse_decimal_q9;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

pub const DEFAULT_PYTH_PRO_BASE_URL: &str = "https://pyth-lazer.dourolabs.app";
pub const DEFAULT_PRESTOCKS_URL: &str = "https://prestocks.com/api/prestocks";
pub const DEFAULT_TESSERA_URL: &str = "https://rest-api.tessera.pe/v1/public/token-details";
pub const PYTH_PRO_DEVNET_VERIFIER_PROGRAM: &str = "pytd2yyk641x7ak7mkaasSJVXh6YYZnC7wTmtgAyxPt";
pub const PYTH_MIN_PUBLIC_EQUITY_FEEDS: usize = 10;
pub const EXCLUDED_PHASE0_SPONSOR_PROVIDERS: [&str; 2] = ["ClawPump", "Meteora"];

/// Keeps explicitly excluded sponsor ecosystems out of Phase 0 public-market
/// discovery. The comparison is case-insensitive so provider labels from
/// external catalogs cannot bypass the policy through casing differences.
pub fn is_excluded_phase0_sponsor_provider(provider: &str) -> bool {
    EXCLUDED_PHASE0_SPONSOR_PROVIDERS
        .iter()
        .any(|excluded| excluded.eq_ignore_ascii_case(provider.trim()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythValidationPolicy {
    pub expected_feed_id: u32,
    pub target_timestamp_us: u64,
    pub max_feed_age_us: u64,
    pub max_confidence_bps: u32,
    pub require_fresh_update: bool,
    pub reject_closed_session: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PythPayloadAssessment {
    pub feed_id: u32,
    pub payload_timestamp_us: u64,
    pub feed_update_timestamp_us: u64,
    pub price_mantissa: i64,
    pub exponent: i16,
    pub confidence: i64,
    pub confidence_bps: u32,
    pub price_q9: i64,
    pub carried_forward: bool,
    pub market_session: PythMarketSession,
    pub solana_payload_bytes: usize,
    /// Hash of the exact signed Solana payload bytes sent to the verifier.
    pub solana_payload_hash: [u8; 32],
}

/// Source-specific evidence values passed to the feature-gated on-chain Pyth
/// instruction. The verifier instruction-data hash is intentionally supplied
/// separately because its wire envelope is owned by the pinned Pyth verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PythEvidenceEnvelope {
    pub feed_id: u32,
    pub payload_timestamp_us: u64,
    pub feed_update_timestamp_us: u64,
    pub price_mantissa: i64,
    pub confidence_mantissa: u64,
    pub exponent: i16,
    pub normalized_price_q9: i64,
    pub payload_hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PythMarketSession {
    Regular,
    PreMarket,
    PostMarket,
    Overnight,
    Closed,
}

impl PythMarketSession {
    fn parse(value: &str) -> Result<Self, PythPayloadError> {
        match value {
            "regular" => Ok(Self::Regular),
            "preMarket" => Ok(Self::PreMarket),
            "postMarket" => Ok(Self::PostMarket),
            "overNight" | "overnight" => Ok(Self::Overnight),
            "closed" => Ok(Self::Closed),
            _ => Err(PythPayloadError::InvalidMarketSession),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythPayloadError {
    InvalidJson,
    InvalidTimestamp,
    TargetTimestampMismatch { expected: u64, actual: u64 },
    FeedNotFound,
    FeedIdMismatch { expected: u32, actual: u32 },
    MissingPrice,
    InvalidPrice,
    InvalidConfidence,
    FeedTimestampAfterPayload,
    FeedTooOld { age_us: u64, max_age_us: u64 },
    CarriedForwardPrice,
    InvalidMarketSession,
    ClosedMarketSession,
    InvalidQ9Conversion,
    InvalidSolanaEncoding,
    InvalidSolanaPayload,
}

impl fmt::Display for PythPayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson => formatter.write_str("Pyth payload is not valid JSON"),
            Self::InvalidTimestamp => formatter.write_str("Pyth timestamp is invalid"),
            Self::TargetTimestampMismatch { expected, actual } => write!(
                formatter,
                "Pyth payload timestamp {actual} does not match target {expected}"
            ),
            Self::FeedNotFound => formatter.write_str("Pyth payload contains no price feed"),
            Self::FeedIdMismatch { expected, actual } => {
                write!(
                    formatter,
                    "Pyth feed {actual} does not match expected feed {expected}"
                )
            }
            Self::MissingPrice => formatter.write_str("Pyth feed price is missing"),
            Self::InvalidPrice => formatter.write_str("Pyth feed price is invalid or non-positive"),
            Self::InvalidConfidence => {
                formatter.write_str("Pyth confidence is invalid or too wide")
            }
            Self::FeedTimestampAfterPayload => {
                formatter.write_str("Pyth feed update timestamp is after payload timestamp")
            }
            Self::FeedTooOld { age_us, max_age_us } => {
                write!(
                    formatter,
                    "Pyth feed age {age_us} exceeds {max_age_us} microseconds"
                )
            }
            Self::CarriedForwardPrice => {
                formatter.write_str("Pyth price was carried forward instead of freshly generated")
            }
            Self::InvalidMarketSession => formatter.write_str("Pyth market session is invalid"),
            Self::ClosedMarketSession => formatter.write_str("Pyth feed is in a closed session"),
            Self::InvalidQ9Conversion => {
                formatter.write_str("Pyth price cannot be represented in Q9")
            }
            Self::InvalidSolanaEncoding => {
                formatter.write_str("Pyth Solana payload encoding is not hex")
            }
            Self::InvalidSolanaPayload => {
                formatter.write_str("Pyth Solana payload bytes are invalid")
            }
        }
    }
}

impl std::error::Error for PythPayloadError {}

#[derive(Debug, Deserialize)]
struct PythPayloadDocument {
    parsed: PythParsedDocument,
    solana: PythSolanaDocument,
}

#[derive(Debug, Deserialize)]
struct PythParsedDocument {
    #[serde(rename = "timestampUs")]
    timestamp_us: Box<RawValue>,
    #[serde(rename = "priceFeeds")]
    price_feeds: Vec<PythFeedDocument>,
}

#[derive(Debug, Deserialize)]
struct PythFeedDocument {
    #[serde(rename = "priceFeedId")]
    price_feed_id: u32,
    price: Option<Box<RawValue>>,
    exponent: i16,
    confidence: Option<Box<RawValue>>,
    #[serde(rename = "marketSession")]
    market_session: String,
    #[serde(rename = "feedUpdateTimestamp")]
    feed_update_timestamp: Box<RawValue>,
}

#[derive(Debug, Deserialize)]
struct PythSolanaDocument {
    encoding: String,
    data: String,
}

/// Validates the signed-payload shape and application policy without claiming
/// cryptographic verification. The Devnet verifier result is an independent
/// activation input returned by `decide_pyth_activation`.
pub fn validate_pyth_payload(
    body: &str,
    policy: PythValidationPolicy,
) -> Result<PythPayloadAssessment, PythPayloadError> {
    let payload: PythPayloadDocument =
        serde_json::from_str(body).map_err(|_| PythPayloadError::InvalidJson)?;
    if payload.solana.encoding != "hex" {
        return Err(PythPayloadError::InvalidSolanaEncoding);
    }
    let solana_payload =
        hex::decode(&payload.solana.data).map_err(|_| PythPayloadError::InvalidSolanaPayload)?;
    if solana_payload.len() < 64 {
        return Err(PythPayloadError::InvalidSolanaPayload);
    }

    let payload_timestamp_us =
        parse_raw_u64(&payload.parsed.timestamp_us).ok_or(PythPayloadError::InvalidTimestamp)?;
    if payload_timestamp_us != policy.target_timestamp_us {
        return Err(PythPayloadError::TargetTimestampMismatch {
            expected: policy.target_timestamp_us,
            actual: payload_timestamp_us,
        });
    }
    let feed = payload
        .parsed
        .price_feeds
        .iter()
        .find(|feed| feed.price_feed_id == policy.expected_feed_id)
        .ok_or(PythPayloadError::FeedNotFound)?;
    if feed.price_feed_id != policy.expected_feed_id {
        return Err(PythPayloadError::FeedIdMismatch {
            expected: policy.expected_feed_id,
            actual: feed.price_feed_id,
        });
    }
    let price_raw = feed
        .price
        .as_deref()
        .ok_or(PythPayloadError::MissingPrice)?;
    let price_mantissa = parse_raw_i64(price_raw).ok_or(PythPayloadError::InvalidPrice)?;
    if price_mantissa <= 0 {
        return Err(PythPayloadError::InvalidPrice);
    }
    let confidence_raw = feed
        .confidence
        .as_deref()
        .ok_or(PythPayloadError::InvalidConfidence)?;
    let confidence = parse_raw_i64(confidence_raw).ok_or(PythPayloadError::InvalidConfidence)?;
    if confidence < 0 {
        return Err(PythPayloadError::InvalidConfidence);
    }
    let feed_update_timestamp_us =
        parse_raw_u64(&feed.feed_update_timestamp).ok_or(PythPayloadError::InvalidTimestamp)?;
    if feed_update_timestamp_us > payload_timestamp_us {
        return Err(PythPayloadError::FeedTimestampAfterPayload);
    }
    let age_us = payload_timestamp_us - feed_update_timestamp_us;
    if age_us > policy.max_feed_age_us {
        return Err(PythPayloadError::FeedTooOld {
            age_us,
            max_age_us: policy.max_feed_age_us,
        });
    }
    let carried_forward = age_us > 0;
    if policy.require_fresh_update && carried_forward {
        return Err(PythPayloadError::CarriedForwardPrice);
    }
    let market_session = PythMarketSession::parse(&feed.market_session)?;
    if policy.reject_closed_session && market_session == PythMarketSession::Closed {
        return Err(PythPayloadError::ClosedMarketSession);
    }
    let confidence_bps =
        confidence_bps(price_mantissa, confidence).ok_or(PythPayloadError::InvalidConfidence)?;
    if confidence_bps > policy.max_confidence_bps {
        return Err(PythPayloadError::InvalidConfidence);
    }
    let price_q9 = pyth_mantissa_to_q9(price_mantissa, feed.exponent)?;

    Ok(PythPayloadAssessment {
        feed_id: feed.price_feed_id,
        payload_timestamp_us,
        feed_update_timestamp_us,
        price_mantissa,
        exponent: feed.exponent,
        confidence,
        confidence_bps,
        price_q9,
        carried_forward,
        market_session,
        solana_payload_bytes: solana_payload.len(),
        solana_payload_hash: Sha256::digest(&solana_payload).into(),
    })
}

/// Converts a validated payload assessment into the exact semantic evidence
/// envelope expected by the Pyth relay. This function cannot be called with an
/// unchecked price because the assessment is produced only by
/// `validate_pyth_payload`.
pub fn pyth_evidence_envelope(assessment: PythPayloadAssessment) -> PythEvidenceEnvelope {
    PythEvidenceEnvelope {
        feed_id: assessment.feed_id,
        payload_timestamp_us: assessment.payload_timestamp_us,
        feed_update_timestamp_us: assessment.feed_update_timestamp_us,
        price_mantissa: assessment.price_mantissa,
        confidence_mantissa: u64::try_from(assessment.confidence).unwrap_or_default(),
        exponent: assessment.exponent,
        normalized_price_q9: assessment.price_q9,
        payload_hash: assessment.solana_payload_hash,
    }
}

fn parse_raw_i64(value: &RawValue) -> Option<i64> {
    let raw = value.get().trim();
    let text = if raw.starts_with('"') {
        serde_json::from_str::<String>(raw).ok()?
    } else {
        raw.to_owned()
    };
    text.parse::<i64>().ok()
}

fn parse_raw_u64(value: &RawValue) -> Option<u64> {
    let raw = value.get().trim();
    let text = if raw.starts_with('"') {
        serde_json::from_str::<String>(raw).ok()?
    } else {
        raw.to_owned()
    };
    text.parse::<u64>().ok()
}

fn confidence_bps(price: i64, confidence: i64) -> Option<u32> {
    let numerator = u128::try_from(confidence).ok()?.checked_mul(10_000)?;
    let denominator = u128::try_from(price).ok()?;
    u32::try_from(numerator.checked_add(denominator - 1)? / denominator).ok()
}

fn pyth_mantissa_to_q9(mantissa: i64, exponent: i16) -> Result<i64, PythPayloadError> {
    let scale = i32::from(exponent) + 9;
    let value = i128::from(mantissa);
    let scaled = if scale >= 0 {
        value
            .checked_mul(power_of_ten(scale as u32).ok_or(PythPayloadError::InvalidQ9Conversion)?)
            .ok_or(PythPayloadError::InvalidQ9Conversion)?
    } else {
        value / power_of_ten(scale.unsigned_abs()).ok_or(PythPayloadError::InvalidQ9Conversion)?
    };
    if scaled <= 0 {
        return Err(PythPayloadError::InvalidQ9Conversion);
    }
    i64::try_from(scaled).map_err(|_| PythPayloadError::InvalidQ9Conversion)
}

fn power_of_ten(exponent: u32) -> Option<i128> {
    (0..exponent).try_fold(1_i128, |value, _| value.checked_mul(10))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythActivationInputs {
    pub trial_token_active: bool,
    pub required_feed_count: usize,
    pub available_feed_count: usize,
    pub payload_policy_passed: bool,
    pub devnet_verification_passed: bool,
    pub q9_vectors_passed: bool,
    pub cost_evidence_recorded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PythActivationDecision {
    Enabled,
    AnalyticsOnly,
    KeepJupiter,
}

pub fn decide_pyth_activation(inputs: PythActivationInputs) -> PythActivationDecision {
    let feed_coverage_passed = inputs.required_feed_count >= PYTH_MIN_PUBLIC_EQUITY_FEEDS
        && inputs.available_feed_count >= inputs.required_feed_count;
    if inputs.trial_token_active
        && feed_coverage_passed
        && inputs.payload_policy_passed
        && inputs.devnet_verification_passed
        && inputs.q9_vectors_passed
        && inputs.cost_evidence_recorded
    {
        return PythActivationDecision::Enabled;
    }
    if inputs.trial_token_active
        && inputs.available_feed_count > 0
        && inputs.payload_policy_passed
        && inputs.q9_vectors_passed
    {
        return PythActivationDecision::AnalyticsOnly;
    }
    PythActivationDecision::KeepJupiter
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrivateRepresentationDescriptor {
    pub provider: String,
    pub reference_symbol: String,
    pub representation_symbol: String,
    pub display_name: String,
    /// Provider-reported lifecycle when available; otherwise the contract
    /// deliberately exposes UNSPECIFIED instead of inferring ACTIVE.
    pub lifecycle_status: String,
    /// Provider-described economic structure retained for transparent UI.
    pub provider_disclosure: String,
    pub source_url: Option<String>,
    pub structure_kind: String,
    pub mint_or_contract: String,
    pub mark_price_q9: i64,
    /// Exact Q9 valuation text is retained as a string because provider
    /// valuations routinely exceed the protocol settlement integer range.
    /// These values are descriptive metadata only and are never passed into
    /// ranked-round arithmetic.
    pub mark_valuation_q9: Option<String>,
    pub holder_count: Option<u64>,
    pub comparability: String,
    pub rated_settlement_eligible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateRepresentationError {
    InvalidJson,
    MissingEconomicDisclosure,
    MissingIdentity,
    InvalidMarkPrice,
    InvalidMarkValuation,
    InvalidMintOrContract,
}

impl fmt::Display for PrivateRepresentationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidJson => "private-market provider response is not valid JSON",
            Self::MissingEconomicDisclosure => {
                "private-market provider disclosure does not describe economic exposure"
            }
            Self::MissingIdentity => "private-market representation identity is incomplete",
            Self::InvalidMarkPrice => "private-market mark price is missing or invalid",
            Self::InvalidMarkValuation => "private-market mark valuation is invalid",
            Self::InvalidMintOrContract => "private-market mint or contract is missing",
        })
    }
}

impl std::error::Error for PrivateRepresentationError {}

#[derive(Debug, Deserialize)]
struct PreStocksDocument {
    name: String,
    symbol: String,
    description: String,
    #[serde(default)]
    external_url: Option<String>,
    contract_address: String,
    #[serde(rename = "markPrice")]
    mark_price: Option<Box<RawValue>>,
    #[serde(rename = "markValuation")]
    mark_valuation: Option<Box<RawValue>>,
    #[serde(default)]
    lifecycle: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TesseraDocument {
    name: String,
    symbol: String,
    mint: String,
    #[serde(rename = "markPrice")]
    mark_price: Option<Box<RawValue>>,
    #[serde(rename = "markValuation")]
    mark_valuation: Option<Box<RawValue>>,
    holders: Option<u64>,
    #[serde(default)]
    lifecycle: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

pub fn parse_prestocks_catalog(
    body: &str,
) -> Result<Vec<PrivateRepresentationDescriptor>, PrivateRepresentationError> {
    let documents: Vec<PreStocksDocument> =
        serde_json::from_str(body).map_err(|_| PrivateRepresentationError::InvalidJson)?;
    documents
        .into_iter()
        .map(|document| {
            let description = document.description.to_ascii_lowercase();
            if !description.contains("spv exposure")
                || !description.contains("backed")
                || !description.contains("tracks")
            {
                return Err(PrivateRepresentationError::MissingEconomicDisclosure);
            }
            if document.symbol.trim().is_empty() || document.name.trim().is_empty() {
                return Err(PrivateRepresentationError::MissingIdentity);
            }
            if document.contract_address.trim().is_empty() {
                return Err(PrivateRepresentationError::InvalidMintOrContract);
            }
            Ok(PrivateRepresentationDescriptor {
                provider: "PreStocks".to_owned(),
                reference_symbol: document.symbol.clone(),
                representation_symbol: document.symbol,
                display_name: document.name,
                lifecycle_status: normalize_lifecycle_status(
                    document.lifecycle.or(document.status),
                ),
                provider_disclosure:
                    "Provider-described SPV economic exposure; not ordinary shareholder rights"
                        .to_owned(),
                source_url: document.external_url,
                structure_kind: "SpvEconomicExposure".to_owned(),
                mint_or_contract: document.contract_address,
                mark_price_q9: parse_required_q9(document.mark_price.as_deref())?,
                mark_valuation_q9: parse_optional_valuation_q9(document.mark_valuation.as_deref())?,
                holder_count: None,
                comparability: "Unsupported".to_owned(),
                rated_settlement_eligible: false,
            })
        })
        .collect()
}

pub fn parse_tessera_catalog(
    body: &str,
) -> Result<Vec<PrivateRepresentationDescriptor>, PrivateRepresentationError> {
    let documents: Vec<TesseraDocument> =
        serde_json::from_str(body).map_err(|_| PrivateRepresentationError::InvalidJson)?;
    documents
        .into_iter()
        .map(|document| {
            if !document.symbol.starts_with("T-")
                || document.name.trim().is_empty()
                || document.mint.trim().is_empty()
            {
                return Err(PrivateRepresentationError::MissingIdentity);
            }
            Ok(PrivateRepresentationDescriptor {
                provider: "Tessera".to_owned(),
                reference_symbol: document.symbol.trim_start_matches("T-").to_owned(),
                representation_symbol: document.symbol,
                display_name: document.name,
                lifecycle_status: normalize_lifecycle_status(
                    document.lifecycle.or(document.status),
                ),
                provider_disclosure:
                    "Provider-described loan participation right; not ordinary equity".to_owned(),
                source_url: None,
                structure_kind: "LoanParticipationRight".to_owned(),
                mint_or_contract: document.mint,
                mark_price_q9: parse_required_q9(document.mark_price.as_deref())?,
                mark_valuation_q9: parse_optional_valuation_q9(document.mark_valuation.as_deref())?,
                holder_count: document.holders,
                comparability: "Unsupported".to_owned(),
                rated_settlement_eligible: false,
            })
        })
        .collect()
}

fn normalize_lifecycle_status(value: Option<String>) -> String {
    value
        .map(|value| value.trim().to_ascii_uppercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "UNSPECIFIED".to_owned())
}

fn parse_required_q9(value: Option<&RawValue>) -> Result<i64, PrivateRepresentationError> {
    let raw = value.ok_or(PrivateRepresentationError::InvalidMarkPrice)?;
    let text = raw_decimal_text(raw).ok_or(PrivateRepresentationError::InvalidMarkPrice)?;
    parse_decimal_q9(&text).map_err(|_| PrivateRepresentationError::InvalidMarkPrice)
}

fn parse_optional_valuation_q9(
    value: Option<&RawValue>,
) -> Result<Option<String>, PrivateRepresentationError> {
    value
        .map(|raw| {
            let text =
                raw_decimal_text(raw).ok_or(PrivateRepresentationError::InvalidMarkValuation)?;
            parse_decimal_q9_wide(&text)
                .map_err(|_| PrivateRepresentationError::InvalidMarkValuation)
        })
        .transpose()
}

/// Converts a positive decimal into an exact Q9 string without narrowing the
/// result to the `i64` range used by settlement prices. Provider valuations
/// are display metadata, so preserving the exact integer is safer than
/// rounding, saturating, or rejecting a valid large valuation.
fn parse_decimal_q9_wide(input: &str) -> Result<String, ()> {
    if input.is_empty() || input.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(());
    }

    let (mantissa, exponent_text) = match input.find(['e', 'E']) {
        Some(index) => {
            if input[index + 1..].contains(['e', 'E']) {
                return Err(());
            }
            (&input[..index], Some(&input[index + 1..]))
        }
        None => (input, None),
    };
    let exponent = exponent_text
        .map(parse_wide_exponent)
        .transpose()?
        .unwrap_or(0);
    let (negative, mantissa) = match mantissa.as_bytes().first() {
        Some(b'-') => (true, &mantissa[1..]),
        Some(b'+') => (false, &mantissa[1..]),
        _ => (false, mantissa),
    };
    let mut parts = mantissa.split('.');
    let integer_part = parts.next().ok_or(())?;
    let fractional_part = parts.next().unwrap_or("");
    if parts.next().is_some()
        || integer_part.is_empty() && fractional_part.is_empty()
        || !integer_part.bytes().all(|byte| byte.is_ascii_digit())
        || !fractional_part.bytes().all(|byte| byte.is_ascii_digit())
        || negative
    {
        return Err(());
    }
    let digits = format!("{integer_part}{fractional_part}");
    let coefficient = digits.bytes().try_fold(0i128, |value, byte| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i128::from(byte - b'0')))
            .ok_or(())
    })?;
    if coefficient == 0 {
        return Err(());
    }
    let fractional_digits = i64::try_from(fractional_part.len()).map_err(|_| ())?;
    let scale = exponent
        .checked_sub(fractional_digits)
        .and_then(|value| value.checked_add(9))
        .ok_or(())?;
    let scaled = if scale >= 0 {
        coefficient
            .checked_mul(power_of_ten(u32::try_from(scale).map_err(|_| ())?).ok_or(())?)
            .ok_or(())?
    } else {
        let divisor_exponent = u32::try_from(scale.checked_neg().ok_or(())?).map_err(|_| ())?;
        coefficient / power_of_ten(divisor_exponent).ok_or(())?
    };
    if scaled <= 0 {
        return Err(());
    }
    Ok(scaled.to_string())
}

fn parse_wide_exponent(input: &str) -> Result<i64, ()> {
    if input.is_empty() {
        return Err(());
    }
    let (negative, digits) = match input.as_bytes().first() {
        Some(b'-') => (true, &input[1..]),
        Some(b'+') => (false, &input[1..]),
        _ => (false, input),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    let magnitude = digits.bytes().try_fold(0i64, |value, byte| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i64::from(byte - b'0')))
            .ok_or(())
    })?;
    if negative {
        magnitude.checked_neg().ok_or(())
    } else {
        Ok(magnitude)
    }
}

fn raw_decimal_text(value: &RawValue) -> Option<String> {
    let raw = value.get().trim();
    if raw.starts_with('"') {
        serde_json::from_str::<String>(raw).ok()
    } else {
        Some(raw.to_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrivateMarketActivation {
    MetadataOnly,
    ExhibitionEligible,
    SeparateRatedEligible,
}

pub fn decide_private_market_activation(
    prestocks_catalog_valid: bool,
    tessera_catalog_valid: bool,
    quality_measured: bool,
) -> PrivateMarketActivation {
    if !prestocks_catalog_valid || !tessera_catalog_valid {
        return PrivateMarketActivation::MetadataOnly;
    }
    if !quality_measured {
        return PrivateMarketActivation::MetadataOnly;
    }
    PrivateMarketActivation::ExhibitionEligible
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrivateMarketCatalogAssessment {
    pub prestocks: Vec<PrivateRepresentationDescriptor>,
    pub tessera: Vec<PrivateRepresentationDescriptor>,
    pub activation: PrivateMarketActivation,
}

pub fn assess_private_market_catalogs(
    prestocks_body: &str,
    tessera_body: &str,
    quality_measured: bool,
) -> Result<PrivateMarketCatalogAssessment, PrivateRepresentationError> {
    let prestocks = parse_prestocks_catalog(prestocks_body)?;
    let tessera = parse_tessera_catalog(tessera_body)?;
    Ok(PrivateMarketCatalogAssessment {
        prestocks,
        tessera,
        activation: decide_private_market_activation(true, true, quality_measured),
    })
}

/// Enables private exhibition readiness only after both provider catalogs are
/// valid, quality has been measured, and at least six distinct usable reference
/// assets are available. This gate never makes a representation Public Ranked
/// eligible or creates a cross-domain rating event.
pub fn private_market_exhibition_ready(
    assessment: &PrivateMarketCatalogAssessment,
    quality_measured: bool,
) -> bool {
    if !quality_measured || assessment.activation == PrivateMarketActivation::MetadataOnly {
        return false;
    }

    let mut references = std::collections::BTreeSet::new();
    assessment
        .prestocks
        .iter()
        .chain(assessment.tessera.iter())
        .filter(|descriptor| {
            descriptor.mark_price_q9 > 0 && !descriptor.mint_or_contract.trim().is_empty()
        })
        .for_each(|descriptor| {
            references.insert(descriptor.reference_symbol.trim().to_ascii_uppercase());
        });
    references.len() >= 6
}

#[derive(Debug)]
pub enum SponsorClientError {
    InvalidRequest(String),
    Http(reqwest::Error),
    HttpStatus { status: StatusCode, body: String },
    ParsePrivate(PrivateRepresentationError),
    ParsePyth(PythPayloadError),
}

impl fmt::Display for SponsorClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => formatter.write_str(message),
            Self::Http(error) => write!(formatter, "sponsor request failed: {error}"),
            Self::HttpStatus { status, body } => {
                write!(
                    formatter,
                    "sponsor returned {status}: {}",
                    truncate_body(body)
                )
            }
            Self::ParsePrivate(error) => error.fmt(formatter),
            Self::ParsePyth(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SponsorClientError {}

pub struct SponsorClient {
    client: reqwest::Client,
    pyth_base_url: String,
    prestocks_url: String,
    tessera_url: String,
    pyth_api_key: Option<String>,
}

impl SponsorClient {
    pub fn new(pyth_api_key: Option<String>) -> Result<Self, SponsorClientError> {
        Self::with_urls(
            DEFAULT_PYTH_PRO_BASE_URL,
            DEFAULT_PRESTOCKS_URL,
            DEFAULT_TESSERA_URL,
            pyth_api_key,
        )
    }

    pub fn with_urls(
        pyth_base_url: impl Into<String>,
        prestocks_url: impl Into<String>,
        tessera_url: impl Into<String>,
        pyth_api_key: Option<String>,
    ) -> Result<Self, SponsorClientError> {
        let pyth_base_url = non_empty_url(pyth_base_url.into(), "Pyth base URL")?;
        let prestocks_url = non_empty_url(prestocks_url.into(), "PreStocks URL")?;
        let tessera_url = non_empty_url(tessera_url.into(), "Tessera URL")?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(SponsorClientError::Http)?;
        Ok(Self {
            client,
            pyth_base_url,
            prestocks_url,
            tessera_url,
            pyth_api_key,
        })
    }

    pub async fn fetch_private_catalogs(
        &self,
        quality_measured: bool,
    ) -> Result<PrivateMarketCatalogAssessment, SponsorClientError> {
        let prestocks = self.fetch_body(&self.prestocks_url).await?;
        let tessera = self.fetch_body(&self.tessera_url).await?;
        assess_private_market_catalogs(&prestocks, &tessera, quality_measured)
            .map_err(SponsorClientError::ParsePrivate)
    }

    pub async fn fetch_pyth_price(
        &self,
        feed_ids: &[u32],
        target_timestamp_us: u64,
        channel: &str,
        policy: PythValidationPolicy,
    ) -> Result<PythPayloadAssessment, SponsorClientError> {
        if self.pyth_api_key.is_none() {
            return Err(SponsorClientError::InvalidRequest(
                "Pyth Pro API key is required for live price queries".to_owned(),
            ));
        }
        if feed_ids.is_empty() || channel.is_empty() {
            return Err(SponsorClientError::InvalidRequest(
                "Pyth Pro query requires feed IDs and a channel".to_owned(),
            ));
        }
        let body = serde_json::json!({
            "priceFeedIds": feed_ids,
            "properties": ["price", "exponent", "confidence", "feedUpdateTimestamp", "marketSession"],
            "formats": ["solana"],
            "channel": channel,
            "timestamp": target_timestamp_us,
        });
        let url = format!("{}/v1/price", self.pyth_base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(url)
            .bearer_auth(self.pyth_api_key.as_deref().unwrap_or_default())
            .json(&body)
            .send()
            .await
            .map_err(SponsorClientError::Http)?;
        let status = response.status();
        let response_body = response.text().await.map_err(SponsorClientError::Http)?;
        if !status.is_success() {
            return Err(SponsorClientError::HttpStatus {
                status,
                body: response_body,
            });
        }
        validate_pyth_payload(&response_body, policy).map_err(SponsorClientError::ParsePyth)
    }

    async fn fetch_body(&self, url: &str) -> Result<String, SponsorClientError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(SponsorClientError::Http)?;
        let status = response.status();
        let body = response.text().await.map_err(SponsorClientError::Http)?;
        if !status.is_success() {
            return Err(SponsorClientError::HttpStatus { status, body });
        }
        Ok(body)
    }
}

fn non_empty_url(value: String, label: &str) -> Result<String, SponsorClientError> {
    let value = value.trim_end_matches('/').to_owned();
    if value.is_empty() {
        return Err(SponsorClientError::InvalidRequest(format!(
            "{label} cannot be empty"
        )));
    }
    Ok(value)
}

fn truncate_body(body: &str) -> String {
    body.chars().take(512).collect()
}
