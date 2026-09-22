//! Permanent Public Equity baseline gates for Phase 0.
//!
//! This module owns the operational checks that must pass before provider
//! abstraction or sponsor-specific integrations can influence rated rounds.
//! It deliberately validates only the permanent Jupiter path; Pyth, PreStocks,
//! and Tessera belong to the separate optional sponsor slice.

use serde::{Deserialize, Serialize};

use crate::{sampling_plan_rate_milli_rps, validate_batched_sampling_plan, SamplingPlanError};

/// The minimum eligible universe required for a meaningful six-asset Public
/// Ranked lineup. The protocol absolute minimum remains six, but the baseline
/// gate does not lower the product target merely to admit a provider.
pub const PUBLIC_RANKED_MIN_ELIGIBLE_ASSETS: usize = 10;

/// Jupiter's documented free-plan general request budget, represented in
/// milli-RPS so the gate can remain exact without floating-point rounding.
pub const JUPITER_FREE_PLAN_LIMIT_MILLI_RPS: u64 = 1_000;

/// The maximum number of price IDs accepted by one Jupiter Price V3 request.
pub const JUPITER_PRICE_REQUEST_MINT_LIMIT: usize = 50;

/// Phase 0's permanent baseline uses three independently operated attestors.
pub const JUPITER_BASELINE_ATTESTOR_COUNT: usize = 3;

/// Initial bounded observation cadence from the V2.1 source specification.
pub const JUPITER_BASELINE_SAMPLE_INTERVAL_SECS: u64 = 5;
pub const JUPITER_BASELINE_OBSERVATION_WINDOW_SECS: u64 = 60;

/// Inputs to the permanent Jupiter operational gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JupiterBaselineConfig {
    pub eligible_asset_count: usize,
    pub attestor_count: usize,
    pub sample_interval_secs: u64,
    pub observation_window_secs: u64,
    pub provider_limit_milli_rps: u64,
}

/// Evidence calculated by the baseline gate for review and logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct JupiterBaselineAssessment {
    pub eligible_asset_count: usize,
    pub requests_per_sample: usize,
    pub attestor_count: usize,
    pub sample_interval_secs: u64,
    pub observation_window_secs: u64,
    pub aggregate_milli_rps: u64,
    pub provider_limit_milli_rps: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaselineError {
    InsufficientEligibleAssets {
        actual: usize,
    },
    InvalidAttestorCount {
        actual: usize,
    },
    InvalidObservationWindow,
    InvalidSampleInterval,
    Sampling(SamplingPlanError),
    ProviderRateLimitExceeded {
        aggregate_milli_rps: u64,
        limit_milli_rps: u64,
    },
}

impl std::fmt::Display for BaselineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientEligibleAssets { actual } => write!(
                formatter,
                "Public Ranked baseline requires at least {PUBLIC_RANKED_MIN_ELIGIBLE_ASSETS} eligible assets; got {actual}"
            ),
            Self::InvalidAttestorCount { actual } => write!(
                formatter,
                "Jupiter baseline requires exactly {JUPITER_BASELINE_ATTESTOR_COUNT} attestors; got {actual}"
            ),
            Self::InvalidObservationWindow => formatter.write_str(
                "Jupiter observation window must be non-zero and contain the sample interval",
            ),
            Self::InvalidSampleInterval => {
                formatter.write_str("Jupiter sample interval must be non-zero")
            }
            Self::Sampling(error) => error.fmt(formatter),
            Self::ProviderRateLimitExceeded {
                aggregate_milli_rps,
                limit_milli_rps,
            } => write!(
                formatter,
                "Jupiter baseline requires {aggregate_milli_rps} milli-RPS but provider allows {limit_milli_rps}"
            ),
        }
    }
}

impl std::error::Error for BaselineError {}

/// Validates the permanent Jupiter baseline before any V2.1 source expansion.
///
/// The request count is derived from the eligible universe and Jupiter's
/// per-request mint limit. This prevents a future 51+ asset universe from
/// silently changing one five-second sampling tick into two provider requests
/// while still claiming the old one-request-per-tick rate.
pub fn validate_jupiter_baseline(
    config: JupiterBaselineConfig,
) -> Result<JupiterBaselineAssessment, BaselineError> {
    if config.eligible_asset_count < PUBLIC_RANKED_MIN_ELIGIBLE_ASSETS {
        return Err(BaselineError::InsufficientEligibleAssets {
            actual: config.eligible_asset_count,
        });
    }
    if config.attestor_count != JUPITER_BASELINE_ATTESTOR_COUNT {
        return Err(BaselineError::InvalidAttestorCount {
            actual: config.attestor_count,
        });
    }
    if config.sample_interval_secs == 0 {
        return Err(BaselineError::InvalidSampleInterval);
    }
    if config.observation_window_secs == 0
        || config.sample_interval_secs > config.observation_window_secs
    {
        return Err(BaselineError::InvalidObservationWindow);
    }

    let requests_per_sample = config
        .eligible_asset_count
        .div_ceil(JUPITER_PRICE_REQUEST_MINT_LIMIT);
    let aggregate_milli_rps = sampling_plan_rate_milli_rps(
        config.attestor_count,
        requests_per_sample,
        config.sample_interval_secs,
    )
    .map_err(BaselineError::Sampling)?;
    if aggregate_milli_rps > config.provider_limit_milli_rps {
        return Err(BaselineError::ProviderRateLimitExceeded {
            aggregate_milli_rps,
            limit_milli_rps: config.provider_limit_milli_rps,
        });
    }
    validate_batched_sampling_plan(
        config.attestor_count,
        requests_per_sample,
        config.sample_interval_secs,
        config.provider_limit_milli_rps,
    )
    .map_err(BaselineError::Sampling)?;

    Ok(JupiterBaselineAssessment {
        eligible_asset_count: config.eligible_asset_count,
        requests_per_sample,
        attestor_count: config.attestor_count,
        sample_interval_secs: config.sample_interval_secs,
        observation_window_secs: config.observation_window_secs,
        aggregate_milli_rps,
        provider_limit_milli_rps: config.provider_limit_milli_rps,
    })
}
