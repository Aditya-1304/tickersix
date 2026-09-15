//! Attestor worker.
//!
//! Each worker owns one registered Ed25519 attestor key, samples the complete
//! frozen Round Asset mint batch through Jupiter, appends every response to a
//! durable evidence log, and emits one signed report per asset and phase. The
//! worker never selects an issuer, changes a mint, or performs on-chain
//! settlement; those responsibilities remain in the frozen round and the
//! relay/finalization path.

use std::{
    collections::BTreeSet,
    env,
    error::Error,
    fs,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use market_data::{
    build_canonical_price_report, read_jsonl, stagger_offsets_millis,
    validate_batched_sampling_plan, AttestorEvidencePolicy, AttestorSigner, EvidenceBatchRecord,
    JsonlStore, JupiterClient, PriceReportContext, SignedAttestorReport, MAX_ELIGIBLE_ASSETS,
    MAX_PRICE_REQUEST_MINTS,
};
use serde::Deserialize;

const REQUIRED_ATTESTOR_COUNT: usize = 3;
const API_KEY_PROVIDER_LIMIT_MILLI_RPS: u64 = 1_000;
const KEYLESS_PROVIDER_LIMIT_MILLI_RPS: u64 = 500;

#[derive(Debug, Deserialize)]
struct RawAttestorConfig {
    program_id_hex: String,
    market_round_hex: String,
    phase: u8,
    observation_window_start_unix_secs: i64,
    observation_window_end_unix_secs: i64,
    report_created_at_unix_secs: i64,
    price_policy_version: u16,
    market_quality_policy_version: u16,
    attestor_set_version: u16,
    min_accepted_observations: u16,
    min_unique_source_blocks: u16,
    max_source_block_lag: u64,
    attestor_index: usize,
    attestor_count: usize,
    sample_interval_secs: u64,
    secret_key_hex: String,
    evidence_output_path: String,
    report_output_path: String,
    assets: Vec<RawAssetConfig>,
}

#[derive(Debug, Deserialize)]
struct RawAssetConfig {
    asset_id: u16,
    round_asset_hex: String,
    /// Exact base58 Solana mint used as the Jupiter request identifier.
    scoring_mint: String,
}

#[derive(Debug)]
struct AttestorConfig {
    program_id: [u8; 32],
    market_round: [u8; 32],
    phase: u8,
    observation_window_start_unix_secs: i64,
    observation_window_end_unix_secs: i64,
    report_created_at_unix_secs: i64,
    price_policy_version: u16,
    market_quality_policy_version: u16,
    attestor_set_version: u16,
    evidence_policy: AttestorEvidencePolicy,
    attestor_index: usize,
    attestor_count: usize,
    sample_interval_secs: u64,
    secret_key: [u8; 32],
    evidence_output_path: String,
    report_output_path: String,
    assets: Vec<AttestorAsset>,
}

#[derive(Debug)]
struct AttestorAsset {
    asset_id: u16,
    round_asset: [u8; 32],
    scoring_mint: String,
    scoring_mint_bytes: [u8; 32],
}

impl AttestorConfig {
    fn load(path: &str) -> Result<Self, Box<dyn Error>> {
        let raw: RawAttestorConfig = serde_json::from_str(&fs::read_to_string(path)?)?;
        if raw.phase > 1 {
            return Err("phase must be 0 (START) or 1 (END)".into());
        }
        if raw.attestor_count != REQUIRED_ATTESTOR_COUNT {
            return Err(format!(
                "attestor quorum requires exactly {REQUIRED_ATTESTOR_COUNT} attestors"
            )
            .into());
        }
        if raw.attestor_index >= raw.attestor_count {
            return Err("attestor_index must be less than attestor_count".into());
        }
        if raw.observation_window_start_unix_secs >= raw.observation_window_end_unix_secs {
            return Err("observation window must have positive duration".into());
        }
        if raw.report_created_at_unix_secs < raw.observation_window_start_unix_secs
            || raw.report_created_at_unix_secs > raw.observation_window_end_unix_secs
        {
            return Err("report timestamp must be inside the observation window".into());
        }
        if raw.min_accepted_observations == 0 || raw.min_unique_source_blocks == 0 {
            return Err("evidence thresholds must be positive".into());
        }
        if raw.assets.is_empty() || raw.assets.len() > MAX_ELIGIBLE_ASSETS {
            return Err(
                format!("assets must contain between one and {MAX_ELIGIBLE_ASSETS} mints").into(),
            );
        }
        let requests_per_sample = raw.assets.len().div_ceil(MAX_PRICE_REQUEST_MINTS);
        validate_batched_sampling_plan(
            raw.attestor_count,
            requests_per_sample,
            raw.sample_interval_secs,
            provider_limit_milli_rps()?,
        )?;

        let program_id = parse_fixed_hex::<32>("program_id_hex", &raw.program_id_hex)?;
        let market_round = parse_fixed_hex::<32>("market_round_hex", &raw.market_round_hex)?;
        let secret_key = parse_fixed_hex::<32>("secret_key_hex", &raw.secret_key_hex)?;
        let mut asset_ids = BTreeSet::new();
        let mut mints = BTreeSet::new();
        let mut assets = Vec::with_capacity(raw.assets.len());
        for raw_asset in raw.assets {
            if !asset_ids.insert(raw_asset.asset_id) {
                return Err(format!("duplicate asset id {}", raw_asset.asset_id).into());
            }
            if !mints.insert(raw_asset.scoring_mint.clone()) {
                return Err(format!("duplicate scoring mint {}", raw_asset.scoring_mint).into());
            }
            let scoring_mint_bytes =
                bs58::decode(&raw_asset.scoring_mint)
                    .into_vec()
                    .map_err(|error| {
                        format!(
                            "scoring mint {} is not valid base58: {error}",
                            raw_asset.scoring_mint
                        )
                    })?;
            let scoring_mint_bytes: [u8; 32] =
                scoring_mint_bytes.try_into().map_err(|bytes: Vec<u8>| {
                    format!(
                        "scoring mint {} decoded to {} bytes instead of 32",
                        raw_asset.scoring_mint,
                        bytes.len()
                    )
                })?;
            assets.push(AttestorAsset {
                asset_id: raw_asset.asset_id,
                round_asset: parse_fixed_hex::<32>("round_asset_hex", &raw_asset.round_asset_hex)?,
                scoring_mint: raw_asset.scoring_mint,
                scoring_mint_bytes,
            });
        }

        Ok(Self {
            program_id,
            market_round,
            phase: raw.phase,
            observation_window_start_unix_secs: raw.observation_window_start_unix_secs,
            observation_window_end_unix_secs: raw.observation_window_end_unix_secs,
            report_created_at_unix_secs: raw.report_created_at_unix_secs,
            price_policy_version: raw.price_policy_version,
            market_quality_policy_version: raw.market_quality_policy_version,
            attestor_set_version: raw.attestor_set_version,
            evidence_policy: AttestorEvidencePolicy {
                min_accepted_observations: raw.min_accepted_observations,
                min_unique_source_blocks: raw.min_unique_source_blocks,
                max_source_block_lag: raw.max_source_block_lag,
            },
            attestor_index: raw.attestor_index,
            attestor_count: raw.attestor_count,
            sample_interval_secs: raw.sample_interval_secs,
            secret_key,
            evidence_output_path: raw.evidence_output_path,
            report_output_path: raw.report_output_path,
            assets,
        })
    }

    fn report_context(&self, asset: &AttestorAsset) -> PriceReportContext {
        PriceReportContext {
            program_id: self.program_id,
            market_round: self.market_round,
            round_asset: asset.round_asset,
            asset_id: asset.asset_id,
            phase: self.phase,
            scoring_mint: asset.scoring_mint_bytes,
            price_policy_version: self.price_policy_version,
            market_quality_policy_version: self.market_quality_policy_version,
            attestor_set_version: self.attestor_set_version,
            observation_window_start: self.observation_window_start_unix_secs,
            observation_window_end: self.observation_window_end_unix_secs,
            report_created_at: self.report_created_at_unix_secs,
        }
    }
}

/// Runs one of the three independent workers. Start three processes with the
/// same frozen round/asset config and distinct `attestor_index`/key/output
/// values; the deterministic offsets prevent synchronized provider bursts.
pub async fn run() -> Result<(), Box<dyn Error>> {
    let config_path = env::args()
        .nth(2)
        .or_else(|| env::var("TICKERSIX_ATTESTOR_CONFIG").ok())
        .ok_or("usage: cargo run -p backend -- attestor-run <config.json>")?;
    let config = AttestorConfig::load(&config_path)?;
    let signer = AttestorSigner::from_secret_key(config.secret_key);
    let attestor = signer.public_key_bytes();
    let client = JupiterClient::with_base_url(
        env::var("TICKERSIX_JUPITER_BASE_URL")
            .unwrap_or_else(|_| market_data::DEFAULT_JUPITER_BASE_URL.to_owned()),
        env::var("TICKERSIX_JUPITER_API_KEY").ok(),
    )?;
    let mut evidence_store = JsonlStore::open_append(&config.evidence_output_path)?;
    collect_phase_evidence(&config, &client, attestor, &mut evidence_store).await?;

    let records: Vec<EvidenceBatchRecord> = read_jsonl(&config.evidence_output_path)?;
    let current_source_block_id = records
        .iter()
        .filter(|record| {
            record.market_round == config.market_round
                && record.phase == config.phase
                && record.attestor == attestor
        })
        .filter_map(EvidenceBatchRecord::max_source_block_id)
        .max()
        .ok_or("attestor produced no successful source observations")?;

    let existing_reports: Vec<SignedAttestorReport> = read_jsonl(&config.report_output_path)?;
    for existing in &existing_reports {
        existing.verify()?;
    }
    let mut report_store = JsonlStore::open_append(&config.report_output_path)?;
    let mut emitted = 0usize;
    for asset in &config.assets {
        let samples = EvidenceBatchRecord::samples_for_mint(
            &records,
            config.market_round,
            config.phase,
            attestor,
            &asset.scoring_mint,
        );
        let report = build_canonical_price_report(
            config.report_context(asset),
            &samples,
            config.evidence_policy,
            current_source_block_id,
        )?;
        let signed = signer.sign(report);
        signed.verify()?;

        if let Some(existing) = existing_reports
            .iter()
            .find(|existing| same_logical_report(existing, &signed))
        {
            if existing.report != signed.report || existing.signature != signed.signature {
                return Err(format!(
                    "immutable report conflict for asset {} phase {}",
                    asset.asset_id, config.phase
                )
                .into());
            }
            continue;
        }

        report_store.append(&signed)?;
        emitted += 1;
    }

    println!(
        "attestor={} phase={} evidence={} reports_emitted={}",
        bs58::encode(attestor).into_string(),
        config.phase,
        config.evidence_output_path,
        emitted
    );
    Ok(())
}

async fn collect_phase_evidence(
    config: &AttestorConfig,
    client: &JupiterClient,
    attestor: [u8; 32],
    evidence_store: &mut JsonlStore,
) -> Result<(), Box<dyn Error>> {
    let offsets = stagger_offsets_millis(config.attestor_count, config.sample_interval_secs)?;
    let window_start_ms = config
        .observation_window_start_unix_secs
        .checked_mul(1_000)
        .ok_or("observation window start overflows milliseconds")?;
    let window_end_ms = config
        .observation_window_end_unix_secs
        .checked_mul(1_000)
        .ok_or("observation window end overflows milliseconds")?;
    let interval_ms = config
        .sample_interval_secs
        .checked_mul(1_000)
        .ok_or("sample interval overflows milliseconds")?;
    let offset_ms = offsets[config.attestor_index];
    let requested_mints: Vec<String> = config
        .assets
        .iter()
        .map(|asset| asset.scoring_mint.clone())
        .collect();
    let mut iteration = 0u64;

    loop {
        let scheduled_offset_ms = offset_ms
            .checked_add(
                interval_ms
                    .checked_mul(iteration)
                    .ok_or("sampling schedule overflows milliseconds")?,
            )
            .ok_or("sampling schedule overflows milliseconds")?;
        let scheduled_at_ms = window_start_ms
            .checked_add(i64::try_from(scheduled_offset_ms).map_err(|_| "sampling time overflow")?)
            .ok_or("sampling time overflows milliseconds")?;
        if scheduled_at_ms >= window_end_ms {
            break;
        }
        sleep_until_millis(scheduled_at_ms).await?;

        let observed_at_unix_ms = now_unix_ms()?;
        let request_started_at_unix_ms = observed_at_unix_ms;
        for mint_chunk in requested_mints.chunks(MAX_PRICE_REQUEST_MINTS) {
            match client.fetch_prices(mint_chunk, observed_at_unix_ms).await {
                Ok(batch) => {
                    evidence_store.append(&EvidenceBatchRecord::from_success(
                        config.market_round,
                        config.phase,
                        attestor,
                        mint_chunk.to_vec(),
                        batch,
                    ))?;
                }
                Err(error) => {
                    evidence_store.append(&EvidenceBatchRecord::from_failure(
                        config.market_round,
                        config.phase,
                        attestor,
                        mint_chunk.to_vec(),
                        request_started_at_unix_ms,
                        now_unix_ms()?,
                        &error,
                    ))?;
                }
            }
        }

        iteration = iteration
            .checked_add(1)
            .ok_or("sampling iteration overflow")?;
    }

    Ok(())
}

async fn sleep_until_millis(target_unix_ms: i64) -> Result<(), Box<dyn Error>> {
    let now = now_unix_ms()?;
    if target_unix_ms > now {
        let delay = u64::try_from(target_unix_ms - now)?;
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    Ok(())
}

fn same_logical_report(left: &SignedAttestorReport, right: &SignedAttestorReport) -> bool {
    left.attestor == right.attestor
        && left.report.context.market_round == right.report.context.market_round
        && left.report.context.round_asset == right.report.context.round_asset
        && left.report.context.phase == right.report.context.phase
}

fn provider_limit_milli_rps() -> Result<u64, Box<dyn Error>> {
    match env::var("TICKERSIX_ATTESTOR_PROVIDER_LIMIT_MILLI_RPS") {
        Ok(value) => Ok(value.parse()?),
        Err(_) if env::var("TICKERSIX_JUPITER_API_KEY").is_ok() => {
            Ok(API_KEY_PROVIDER_LIMIT_MILLI_RPS)
        }
        Err(_) => Ok(KEYLESS_PROVIDER_LIMIT_MILLI_RPS),
    }
}

fn parse_fixed_hex<const N: usize>(field: &str, input: &str) -> Result<[u8; N], Box<dyn Error>> {
    let input = input.strip_prefix("0x").unwrap_or(input);
    let bytes = hex::decode(input)?;
    if bytes.len() != N {
        return Err(format!("{field} must decode to exactly {N} bytes").into());
    }
    bytes
        .try_into()
        .map_err(|_| format!("{field} has invalid length").into())
}

fn now_unix_ms() -> Result<i64, Box<dyn Error>> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| "system clock exceeds i64 milliseconds".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_hex_parser_requires_exact_byte_width() {
        assert!(parse_fixed_hex::<32>("key", &"00".repeat(32)).is_ok());
        assert!(parse_fixed_hex::<32>("key", "00").is_err());
    }

    #[test]
    fn logical_report_identity_is_round_asset_and_phase_scoped() {
        let report = market_data::CanonicalPriceReport {
            context: PriceReportContext {
                program_id: [1; 32],
                market_round: [2; 32],
                round_asset: [3; 32],
                asset_id: 1,
                phase: 0,
                scoring_mint: [4; 32],
                price_policy_version: 1,
                market_quality_policy_version: 1,
                attestor_set_version: 1,
                observation_window_start: 1,
                observation_window_end: 2,
                report_created_at: 1,
            },
            median_price_q9: 100,
            accepted_observation_count: 2,
            unique_source_block_count: 2,
            first_source_block_id: 1,
            last_source_block_id: 2,
            evidence_root: [5; 32],
        };
        let signer = AttestorSigner::from_secret_key([9; 32]);
        let left = signer.sign(report.clone());
        let mut right_report = report;
        right_report.context.phase = 1;
        let right = signer.sign(right_report);

        assert!(!same_logical_report(&left, &right));
    }
}
