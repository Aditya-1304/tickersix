//! Backend entry points that are safe to run before the production API exists.
//!
//! The market-data recorder gathers the evidence needed to calibrate Market
//! Quality Policy values without silently turning unmeasured assumptions into
//! rated-round configuration.

use std::{
    collections::BTreeSet,
    env,
    error::Error,
    fs::{create_dir_all, read_to_string, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use market_data::{
    assess_private_market_catalogs, decide_pyth_activation,
    evaluate_xstocks_baseline_with_verified_token_program, stagger_offsets_millis,
    summarize_observation_window, validate_jupiter_baseline, validate_pyth_payload,
    validate_sampling_plan, JupiterBaselineConfig, JupiterClient, MarketDataObservation,
    PriceBatch, PythActivationDecision, PythActivationInputs, PythPayloadAssessment,
    PythValidationPolicy, SponsorClient, XStocksBaselineSnapshot, XStocksClient,
};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;

pub mod achievements;
pub mod api;
pub mod attestor;
pub mod auth;
pub mod battle_facts;
pub mod db;
pub mod hardening;
pub mod indexer;
pub mod jobs;
pub mod leaderboard;
pub mod league;
pub mod live;
pub mod metrics;
pub mod private_markets;
pub mod profile;
pub mod proof;
pub mod ranked;
pub mod rating;
pub mod recovery;
pub mod release_evidence;
pub mod replay;
pub mod settlement;
pub mod standings;

const DEFAULT_ITERATIONS: usize = 1;
const DEFAULT_SAMPLE_INTERVAL_SECS: u64 = 5;
const DEFAULT_ATTESTOR_COUNT: usize = 3;
const API_KEY_PROVIDER_LIMIT_MILLI_RPS: u64 = 1_000;
const KEYLESS_PROVIDER_LIMIT_MILLI_RPS: u64 = 500;

#[derive(Debug, Serialize)]
struct RecorderLine<'a> {
    schema_version: u16,
    attestor_id: &'a str,
    observed_at_unix_ms: i64,
    requested_mints: &'a [String],
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    batch: Option<&'a PriceBatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_response: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct MarketDataBaselineReport {
    scope: &'static str,
    jupiter: market_data::JupiterBaselineAssessment,
    xstocks: XStocksBaselineSnapshot,
}

#[derive(Debug, Serialize)]
struct JupiterSmokeReport {
    api_key_configured: bool,
    batch: PriceBatch,
}

#[derive(Debug, Deserialize)]
struct PythCoverageFixture {
    available_stable_equity_feed_count: usize,
    required_public_equity_feed_count: usize,
    target_timestamp_us: u64,
    target_feed_id: u32,
    feeds: Vec<PythCoverageFeed>,
}

#[derive(Debug, Deserialize)]
struct PythCoverageFeed {
    pyth_lazer_id: u32,
    symbol: String,
    asset_type: String,
    instrument_type: String,
    state: String,
    exponent: i16,
    min_channel: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct PythDevnetEvidence {
    cluster: String,
    verifier_program: String,
    payload_format: String,
    trial_token_active: bool,
    verified: bool,
    simulation_performed: bool,
    transaction_signature: Option<String>,
    compute_units: Option<u64>,
    transaction_bytes: Option<u64>,
    lamports: Option<u64>,
    latency_ms: Option<u64>,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct ExcludedProvidersFixture {
    providers: Vec<String>,
    reason: String,
}

#[derive(Debug, Serialize)]
struct SponsorReadinessReport {
    scope: &'static str,
    public_ranked_provider: &'static str,
    excluded_providers: Vec<&'static str>,
    pyth: SponsorReadinessPythReport,
    private_market: market_data::PrivateMarketCatalogAssessment,
}

#[derive(Debug, Serialize)]
struct SponsorReadinessPythReport {
    feed_coverage_available: usize,
    feed_coverage_required: usize,
    target_feed_id: u32,
    target_timestamp_us: u64,
    coverage_feed_count: usize,
    coverage_valid: bool,
    validation: PythPayloadAssessment,
    validation_policy_passed: bool,
    q9_vectors_passed: bool,
    devnet: PythDevnetEvidence,
    cost_evidence_recorded: bool,
    activation_decision: PythActivationDecision,
}

#[derive(Debug, Serialize)]
struct PrivateMarketSmokeReport {
    quality_measured: bool,
    catalog: market_data::PrivateMarketCatalogAssessment,
}

#[derive(Debug, Serialize)]
struct ReleaseFreezeReport {
    scope: &'static str,
    beta: release_evidence::BetaEvidenceReport,
    freeze: release_evidence::FeatureFreezeReport,
}

#[derive(Debug, Serialize)]
struct PythSmokeReport {
    api_key_configured: bool,
    expected_feed_id: u32,
    target_timestamp_us: u64,
    assessment: PythPayloadAssessment,
}

#[derive(Debug, Deserialize)]
struct VerifiedTokenProgramFixture {
    owner: String,
    token_program: String,
}

#[derive(Debug)]
struct RecorderConfig {
    api_key: Option<String>,
    base_url: String,
    attestor_id: String,
    attestor_index: usize,
    attestor_count: usize,
    sample_interval_secs: u64,
    iterations: usize,
    output_path: String,
    mints: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    match env::args().nth(1).as_deref() {
        Some("record-market-data") => record_market_data().await?,
        Some("record-token-metadata") => record_token_metadata().await?,
        Some("analyze-market-data") => analyze_market_data()?,
        Some("market-data-baseline-gate") => run_market_data_baseline_gate()?,
        Some("sponsor-readiness-gate") => run_sponsor_readiness_gate()?,
        Some("beta-evidence-gate") => run_beta_evidence_gate()?,
        Some("release-freeze-gate") => run_release_freeze_gate()?,
        Some("jupiter-smoke") => run_jupiter_smoke().await?,
        Some("xstocks-smoke") => run_xstocks_smoke().await?,
        Some("private-market-smoke") => run_private_market_smoke().await?,
        Some("pyth-pro-smoke") => run_pyth_pro_smoke().await?,
        Some("attestor-run") => attestor::run().await?,
        Some("api-serve") => api::serve_from_env().await?,
        Some("ranked-match") => run_ranked_match().await?,
        Some("rating-apply") => run_rating_apply().await?,
        Some("scheduler-once") => run_scheduler_once().await?,
        Some("scheduler") => run_scheduler().await?,
        Some("proof-serve") => {
            let path = env::args()
                .nth(2)
                .ok_or("usage: cargo run -p backend -- proof-serve <snapshot.json> [bind]")?;
            let bind = env::args()
                .nth(3)
                .unwrap_or_else(|| "127.0.0.1:8787".to_owned());
            proof::serve_from_file(path, &bind)?;
        }
        Some("settlement-plan") => {
            let path = env::args()
                .nth(2)
                .ok_or("usage: cargo run -p backend -- settlement-plan <snapshot.json>")?;
            settlement::plan_from_file(path)?;
        }
        _ => print_usage(),
    }

    Ok(())
}

/// Runs the offline Market-data baseline gate against committed schema fixtures.
///
/// Fixture validation proves that the baseline policy and parser contracts are
/// executable and reviewable without pretending that fixture data is fresh
/// provider evidence. The live xStocks smoke command below is intentionally a
/// separate read-only operation for that operational claim.
fn run_market_data_baseline_gate() -> Result<(), Box<dyn Error>> {
    let fixture_dir = env::args()
        .nth(2)
        .unwrap_or_else(|| "fixtures/market-data".to_owned());
    let expected_symbol = env::args().nth(3).unwrap_or_else(|| "SPYx".to_owned());
    let fixture_root = Path::new(&fixture_dir);
    let xstocks_root = fixture_root.join("xstocks");

    let jupiter_config: JupiterBaselineConfig =
        serde_json::from_str(&read_to_string(fixture_root.join("jupiter-baseline.json"))?)?;
    let jupiter = validate_jupiter_baseline(jupiter_config)?;
    let verified_token_program: VerifiedTokenProgramFixture = serde_json::from_str(
        &read_to_string(xstocks_root.join("solana-token-program.json"))?,
    )?;
    if verified_token_program.owner != market_data::SPL_TOKEN_2022_PROGRAM_ID
        || verified_token_program.token_program != "Token2022Program"
    {
        return Err("xStocks fixture token-program owner is not Token-2022".into());
    }
    let xstocks = evaluate_xstocks_baseline_with_verified_token_program(
        &read_to_string(xstocks_root.join("asset.json"))?,
        &read_to_string(xstocks_root.join("price-data.json"))?,
        &read_to_string(xstocks_root.join("multiplier.json"))?,
        &read_to_string(xstocks_root.join("corporate-actions.json"))?,
        &expected_symbol,
        &verified_token_program.token_program,
    )?;

    let report = MarketDataBaselineReport {
        scope: "market_data_baseline",
        jupiter,
        xstocks,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Runs deterministic sponsor-readiness validation against committed
/// fixtures. The gate validates provider-shaped data, exact timestamp/Q9
/// policy, and the evidence required before any Pyth path could be promoted.
/// It intentionally keeps Public Ranked on Jupiter because the checked-in
/// Devnet evidence records no authorized verifier run.
fn run_sponsor_readiness_gate() -> Result<(), Box<dyn Error>> {
    let fixture_dir = env::args()
        .nth(2)
        .unwrap_or_else(|| "fixtures/market-data/sponsors".to_owned());
    let fixture_root = Path::new(&fixture_dir);
    let coverage: PythCoverageFixture = serde_json::from_str(&read_to_string(
        fixture_root.join("pyth-feed-coverage.json"),
    )?)?;
    validate_pyth_coverage(&coverage)?;

    let policy = PythValidationPolicy {
        expected_feed_id: coverage.target_feed_id,
        target_timestamp_us: coverage.target_timestamp_us,
        max_feed_age_us: 1_000_000,
        max_confidence_bps: 100,
        require_fresh_update: true,
        reject_closed_session: true,
    };
    let validation = validate_pyth_payload(
        &read_to_string(fixture_root.join("pyth-payload.json"))?,
        policy,
    )?;
    let devnet: PythDevnetEvidence = serde_json::from_str(&read_to_string(
        fixture_root.join("pyth-devnet-evidence.json"),
    )?)?;
    let excluded: ExcludedProvidersFixture = serde_json::from_str(&read_to_string(
        fixture_root.join("excluded-providers.json"),
    )?)?;
    validate_excluded_providers(&excluded)?;
    if devnet.cluster != "devnet"
        || devnet.verifier_program != market_data::PYTH_PRO_DEVNET_VERIFIER_PROGRAM
        || devnet.payload_format != "solana"
    {
        return Err("Pyth Devnet evidence fixture does not identify the pinned verifier".into());
    }
    let cost_evidence_recorded = devnet.compute_units.is_some()
        && devnet.transaction_bytes.is_some()
        && devnet.lamports.is_some()
        && devnet.latency_ms.is_some();
    let q9_vectors_passed = validation.price_q9 == 774_100_500_000;
    let activation_decision = decide_pyth_activation(PythActivationInputs {
        trial_token_active: devnet.trial_token_active,
        required_feed_count: coverage.required_public_equity_feed_count,
        available_feed_count: coverage.available_stable_equity_feed_count,
        payload_policy_passed: true,
        devnet_verification_passed: devnet.verified && devnet.simulation_performed,
        q9_vectors_passed,
        cost_evidence_recorded,
    });
    let private_market = assess_private_market_catalogs(
        &read_to_string(fixture_root.join("prestocks.json"))?,
        &read_to_string(fixture_root.join("tessera.json"))?,
        false,
    )?;

    println!(
        "{}",
        serde_json::to_string_pretty(&SponsorReadinessReport {
            scope: "sponsor_readiness",
            public_ranked_provider: "Jupiter",
            excluded_providers: market_data::EXCLUDED_SPONSOR_PROVIDERS.to_vec(),
            pyth: SponsorReadinessPythReport {
                feed_coverage_available: coverage.available_stable_equity_feed_count,
                feed_coverage_required: coverage.required_public_equity_feed_count,
                target_feed_id: coverage.target_feed_id,
                target_timestamp_us: coverage.target_timestamp_us,
                coverage_feed_count: coverage.feeds.len(),
                coverage_valid: true,
                validation,
                validation_policy_passed: true,
                q9_vectors_passed,
                devnet,
                cost_evidence_recorded,
                activation_decision,
            },
            private_market,
        })?
    );
    Ok(())
}

/// Runs the beta evidence contract gate.
///
/// Contract mode is intentionally accepted by this offline command so CI can
/// validate the record shape without fabricating user sessions or Devnet
/// transactions. An operator-supplied evidence-mode record must pass every
/// external-evidence check before this command exits successfully.
fn run_beta_evidence_gate() -> Result<(), Box<dyn Error>> {
    let manifest_path = env::args()
        .nth(2)
        .unwrap_or_else(|| "fixtures/release-evidence/beta-evidence.contract.json".to_owned());
    let manifest: release_evidence::BetaEvidenceManifest =
        serde_json::from_str(&read_to_string(manifest_path)?)?;
    let report = release_evidence::validate_beta_evidence(&manifest);
    let release_ready = report.release_ready;
    let contract_valid = report.contract_valid;

    println!("{}", serde_json::to_string_pretty(&report)?);

    if !contract_valid {
        return Err("beta-evidence contract is invalid".into());
    }
    if manifest.mode == release_evidence::EVIDENCE_MODE && !release_ready {
        return Err("beta evidence is incomplete".into());
    }
    Ok(())
}

/// Runs the release freeze gate against a freeze record and
/// the beta evidence record it consumes. Contract mode remains available for
/// offline CI; freeze mode exits successfully only after all P0/P1 and beta
/// evidence checks are green.
fn run_release_freeze_gate() -> Result<(), Box<dyn Error>> {
    let freeze_path = env::args()
        .nth(2)
        .unwrap_or_else(|| "fixtures/release-evidence/feature-freeze.contract.json".to_owned());
    let freeze: release_evidence::FeatureFreezeManifest =
        serde_json::from_str(&read_to_string(&freeze_path)?)?;
    let beta_path = env::args()
        .nth(3)
        .unwrap_or_else(|| freeze.beta_manifest_path.clone());
    let beta: release_evidence::BetaEvidenceManifest =
        serde_json::from_str(&read_to_string(beta_path)?)?;
    let beta_report = release_evidence::validate_beta_evidence(&beta);
    let freeze_report = release_evidence::validate_feature_freeze(&freeze, &beta_report);
    let contract_valid = freeze_report.contract_valid;
    let release_ready = freeze_report.release_ready;

    println!(
        "{}",
        serde_json::to_string_pretty(&ReleaseFreezeReport {
            scope: "release_freeze",
            beta: beta_report,
            freeze: freeze_report,
        })?
    );

    if !contract_valid {
        return Err("feature-freeze contract is invalid".into());
    }
    if freeze.mode == release_evidence::FREEZE_MODE && !release_ready {
        return Err("feature freeze is incomplete".into());
    }
    Ok(())
}

fn validate_excluded_providers(fixture: &ExcludedProvidersFixture) -> Result<(), Box<dyn Error>> {
    let actual = fixture
        .providers
        .iter()
        .map(|provider| provider.trim().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let expected = market_data::EXCLUDED_SPONSOR_PROVIDERS
        .iter()
        .map(|provider| provider.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    if fixture.reason.trim().is_empty() || actual != expected {
        return Err("baseline sponsor exclusion fixture is incomplete".into());
    }
    Ok(())
}

fn validate_pyth_coverage(coverage: &PythCoverageFixture) -> Result<(), Box<dyn Error>> {
    if coverage.required_public_equity_feed_count < market_data::PYTH_MIN_PUBLIC_EQUITY_FEEDS
        || coverage.available_stable_equity_feed_count < coverage.required_public_equity_feed_count
        || coverage.feeds.len() < coverage.required_public_equity_feed_count
    {
        return Err("Pyth fixture does not meet the minimum public-equity coverage gate".into());
    }
    let target = coverage
        .feeds
        .iter()
        .find(|feed| feed.pyth_lazer_id == coverage.target_feed_id)
        .ok_or("Pyth fixture omits its target feed")?;
    if target.asset_type != "equity"
        || target.instrument_type != "spot"
        || target.state != "stable"
        || target.exponent != -5
        || target.symbol.is_empty()
        || target.min_channel.is_empty()
    {
        return Err("Pyth target feed is not a stable spot-equity fixture".into());
    }
    Ok(())
}

/// Performs a live, unauthenticated, read-only xStocks contract smoke test.
///
/// No wallet, signing key, transaction, or settlement state is touched. The
/// optional base URL exists solely to make the command testable against a
/// controlled mirror while production defaults to the documented public API.
async fn run_xstocks_smoke() -> Result<(), Box<dyn Error>> {
    let symbol = env::args()
        .nth(2)
        .ok_or("usage: cargo run -p backend -- xstocks-smoke <symbol> [network]")?;
    let network = env::args().nth(3).unwrap_or_else(|| "Solana".to_owned());
    let base_url = env::var("TICKERSIX_XSTOCKS_BASE_URL")
        .unwrap_or_else(|_| market_data::DEFAULT_XSTOCKS_BASE_URL.to_owned());
    let solana_rpc_url = env::var("TICKERSIX_SOLANA_RPC_URL")
        .unwrap_or_else(|_| market_data::DEFAULT_SOLANA_RPC_URL.to_owned());
    let client = XStocksClient::with_base_url_and_rpc_url(base_url, solana_rpc_url)?;
    let snapshot = client.fetch_baseline(&symbol, &network).await?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}

/// Performs one read-only Jupiter Price V3 batch request for exact registry
/// mints. The command reports whether an API key was configured but never
/// prints the key itself or turns a provider response into settlement state.
async fn run_jupiter_smoke() -> Result<(), Box<dyn Error>> {
    let mints = env::args()
        .nth(2)
        .ok_or("usage: cargo run -p backend -- jupiter-smoke <mint[,mint...]>")?
        .split(',')
        .map(str::trim)
        .filter(|mint| !mint.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if mints.is_empty() {
        return Err("jupiter-smoke requires at least one mint".into());
    }
    let api_key = env::var("TICKERSIX_JUPITER_API_KEY").ok();
    let base_url = env::var("TICKERSIX_JUPITER_BASE_URL")
        .unwrap_or_else(|_| market_data::DEFAULT_JUPITER_BASE_URL.to_owned());
    let client = JupiterClient::with_base_url(base_url, api_key.clone())?;
    let batch = client.fetch_prices(&mints, now_unix_ms()?).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&JupiterSmokeReport {
            api_key_configured: api_key.is_some(),
            batch,
        })?
    );
    Ok(())
}

/// Performs read-only catalog fetches for PreStocks and Tessera. The command
/// records metadata readiness only; it never marks private representations as
/// comparable or eligible for rated settlement.
async fn run_private_market_smoke() -> Result<(), Box<dyn Error>> {
    let pyth_base_url = env::var("TICKERSIX_PYTH_PRO_BASE_URL")
        .unwrap_or_else(|_| market_data::DEFAULT_PYTH_PRO_BASE_URL.to_owned());
    let prestocks_url = env::var("TICKERSIX_PRESTOCKS_URL")
        .unwrap_or_else(|_| market_data::DEFAULT_PRESTOCKS_URL.to_owned());
    let tessera_url = env::var("TICKERSIX_TESSERA_URL")
        .unwrap_or_else(|_| market_data::DEFAULT_TESSERA_URL.to_owned());
    let client = SponsorClient::with_urls(pyth_base_url, prestocks_url, tessera_url, None)?;
    let catalog = client.fetch_private_catalogs(false).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&PrivateMarketSmokeReport {
            quality_measured: false,
            catalog,
        })?
    );
    Ok(())
}

/// Performs one authenticated, read-only Pyth Pro `/v1/price` request. This
/// command validates the returned application payload but does not submit the
/// embedded Solana payload or sign any transaction.
async fn run_pyth_pro_smoke() -> Result<(), Box<dyn Error>> {
    let feed_ids = env::args()
        .nth(2)
        .ok_or("usage: cargo run -p backend -- pyth-pro-smoke <feed[,feed...]> <timestamp_us> [channel]")?
        .split(',')
        .map(str::trim)
        .filter(|feed_id| !feed_id.is_empty())
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()?;
    if feed_ids.is_empty() {
        return Err("pyth-pro-smoke requires at least one feed ID".into());
    }
    let target_timestamp_us = env::args()
        .nth(3)
        .ok_or("pyth-pro-smoke requires target_timestamp_us")?
        .parse::<u64>()?;
    let channel = env::args()
        .nth(4)
        .unwrap_or_else(|| "fixed_rate@50ms".to_owned());
    let expected_feed_id = env::var("TICKERSIX_PYTH_EXPECTED_FEED_ID")
        .ok()
        .map(|value| value.parse::<u32>())
        .transpose()?
        .unwrap_or(feed_ids[0]);
    if !feed_ids.contains(&expected_feed_id) {
        return Err("expected Pyth feed ID must be included in the requested feed list".into());
    }
    let api_key = env::var("TICKERSIX_PYTH_PRO_API_KEY")
        .map_err(|_| "TICKERSIX_PYTH_PRO_API_KEY is required for pyth-pro-smoke")?;
    let pyth_base_url = env::var("TICKERSIX_PYTH_PRO_BASE_URL")
        .unwrap_or_else(|_| market_data::DEFAULT_PYTH_PRO_BASE_URL.to_owned());
    let client = SponsorClient::with_urls(
        pyth_base_url,
        market_data::DEFAULT_PRESTOCKS_URL,
        market_data::DEFAULT_TESSERA_URL,
        Some(api_key),
    )?;
    let assessment = client
        .fetch_pyth_price(
            &feed_ids,
            target_timestamp_us,
            &channel,
            PythValidationPolicy {
                expected_feed_id,
                target_timestamp_us,
                max_feed_age_us: env::var("TICKERSIX_PYTH_MAX_FEED_AGE_US")
                    .ok()
                    .map(|value| value.parse::<u64>())
                    .transpose()?
                    .unwrap_or(1_000_000),
                max_confidence_bps: env::var("TICKERSIX_PYTH_MAX_CONFIDENCE_BPS")
                    .ok()
                    .map(|value| value.parse::<u32>())
                    .transpose()?
                    .unwrap_or(100),
                require_fresh_update: true,
                reject_closed_session: true,
            },
        )
        .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&PythSmokeReport {
            api_key_configured: true,
            expected_feed_id,
            target_timestamp_us,
            assessment,
        })?
    );
    Ok(())
}

/// Captures exact-mint Tokens V2 metadata as a separate immutable evidence
/// artifact. Metadata is review input; it never discovers or replaces the
/// issuer-approved registry automatically.
async fn record_token_metadata() -> Result<(), Box<dyn Error>> {
    let mints = required_csv("TICKERSIX_MARKET_DATA_MINTS")?;
    let api_key = env::var("TICKERSIX_JUPITER_API_KEY").ok();
    let base_url = env::var("TICKERSIX_JUPITER_BASE_URL")
        .unwrap_or_else(|_| market_data::DEFAULT_JUPITER_BASE_URL.to_owned());
    let client = JupiterClient::with_base_url(base_url, api_key)?;
    let batch = client.fetch_token_information(&mints).await?;
    println!("{}", serde_json::to_string_pretty(&batch)?);
    Ok(())
}

async fn run_ranked_match() -> Result<(), Box<dyn Error>> {
    let market_round_id = env::args()
        .nth(2)
        .ok_or("usage: cargo run -p backend -- ranked-match <market_round_id>")?
        .parse::<i64>()?;
    let database_url = env::var("TICKERSIX_DATABASE_URL")
        .map_err(|_| "TICKERSIX_DATABASE_URL must point to PostgreSQL")?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await?;
    db::run_migrations(&pool).await?;
    let result = ranked::run_matchmaker(&pool, market_round_id, auth::unix_now()).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn run_rating_apply() -> Result<(), Box<dyn Error>> {
    let database_url = env::var("TICKERSIX_DATABASE_URL")
        .map_err(|_| "TICKERSIX_DATABASE_URL must point to PostgreSQL")?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await?;
    db::run_migrations(&pool).await?;
    let result = rating::apply_next(&pool, auth::unix_now()).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn open_worker_pool() -> Result<sqlx::PgPool, Box<dyn Error>> {
    let database_url = env::var("TICKERSIX_DATABASE_URL")
        .map_err(|_| "TICKERSIX_DATABASE_URL must point to PostgreSQL")?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await?;
    db::run_migrations(&pool).await?;
    Ok(pool)
}

async fn run_scheduler_once() -> Result<(), Box<dyn Error>> {
    let pool = open_worker_pool().await?;
    let result = jobs::run_once(&pool, auth::unix_now()).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn run_scheduler() -> Result<(), Box<dyn Error>> {
    let pool = open_worker_pool().await?;
    let interval_secs = env::var("TICKERSIX_SCHEDULER_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5)
        .max(1);
    loop {
        match jobs::run_once(&pool, auth::unix_now()).await {
            Ok(result) => println!("{}", serde_json::to_string(&result)?),
            Err(error) => eprintln!("scheduler tick failed: {error}"),
        }
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
    }
}

#[derive(Debug, Deserialize)]
struct RecorderLineOwned {
    #[serde(default)]
    requested_mints: Vec<String>,
    #[serde(default)]
    attestor_id: String,
    #[serde(default)]
    status: String,
    batch: Option<PriceBatch>,
}

/// Reads recorder NDJSON and emits measurement-only summaries for the
/// candidate observation durations in the source specification.
fn analyze_market_data() -> Result<(), Box<dyn Error>> {
    let input_path = env::args()
        .nth(2)
        .or_else(|| env::var("TICKERSIX_MARKET_DATA_INPUT").ok())
        .ok_or("usage: cargo run -p backend -- analyze-market-data <ndjson>")?;
    let file = File::open(&input_path)?;
    let reader = BufReader::new(file);
    let mut requested_mints = BTreeSet::new();
    let mut observations = Vec::new();

    for (line_number, line) in reader.lines().enumerate() {
        let line = line?;
        let record: RecorderLineOwned = serde_json::from_str(&line).map_err(|error| {
            format!(
                "invalid market-data NDJSON at line {}: {error}",
                line_number + 1
            )
        })?;
        requested_mints.extend(record.requested_mints);
        if record.status != "ok" {
            continue;
        }
        let Some(batch) = record.batch else {
            return Err(format!(
                "successful market-data row {} has no batch",
                line_number + 1
            )
            .into());
        };
        for observation in batch.observations {
            observations.push(MarketDataObservation::new(
                record.attestor_id.clone(),
                observation.mint,
                observation.price_q9,
                observation.source_block_id,
                observation.observed_at_unix_ms,
            ));
        }
    }

    if observations.is_empty() {
        return Err("market-data input contains no successful observations".into());
    }
    if requested_mints.is_empty() {
        requested_mints.extend(
            observations
                .iter()
                .map(|observation| observation.mint.clone()),
        );
    }
    let requested_mints: Vec<String> = requested_mints.into_iter().collect();
    let window_start = match env::var("TICKERSIX_OBSERVATION_WINDOW_START_UNIX_MS") {
        Ok(value) => value.parse::<i64>()?,
        Err(_) => observations
            .iter()
            .map(|observation| observation.observed_at_unix_ms)
            .min()
            .ok_or("market-data input contains no timestamps")?,
    };
    let windows = env::var("TICKERSIX_OBSERVATION_WINDOWS_SECS")
        .unwrap_or_else(|_| "3600,7200,14400".to_owned())
        .split(',')
        .map(|value| value.trim().parse::<u64>())
        .collect::<Result<Vec<_>, _>>()?;
    if windows.is_empty() || windows.contains(&0) {
        return Err("TICKERSIX_OBSERVATION_WINDOWS_SECS must contain positive durations".into());
    }

    let summaries = windows
        .into_iter()
        .map(|window_secs| {
            summarize_observation_window(&requested_mints, &observations, window_start, window_secs)
        })
        .collect::<Result<Vec<_>, _>>()?;
    println!("{}", serde_json::to_string_pretty(&summaries)?);
    Ok(())
}

async fn record_market_data() -> Result<(), Box<dyn Error>> {
    let config = RecorderConfig::from_environment()?;
    let client = JupiterClient::with_base_url(&config.base_url, config.api_key.clone())?;

    if let Some(parent) = Path::new(&config.output_path).parent() {
        if !parent.as_os_str().is_empty() {
            create_dir_all(parent)?;
        }
    }
    let mut output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config.output_path)?;

    let offsets = stagger_offsets_millis(config.attestor_count, config.sample_interval_secs)?;
    tokio::time::sleep(Duration::from_millis(offsets[config.attestor_index])).await;

    for iteration in 0..config.iterations {
        let observed_at_unix_ms = now_unix_ms()?;
        match client
            .fetch_prices(&config.mints, observed_at_unix_ms)
            .await
        {
            Ok(batch) => append_line(
                &mut output,
                RecorderLine {
                    schema_version: 1,
                    attestor_id: &config.attestor_id,
                    observed_at_unix_ms,
                    requested_mints: &config.mints,
                    status: "ok",
                    batch: Some(&batch),
                    raw_response: Some(&batch.raw_response),
                    error: None,
                },
            )?,
            Err(error) => append_line(
                &mut output,
                RecorderLine {
                    schema_version: 1,
                    attestor_id: &config.attestor_id,
                    observed_at_unix_ms,
                    requested_mints: &config.mints,
                    status: "error",
                    batch: None,
                    raw_response: None,
                    error: Some(error.to_string()),
                },
            )?,
        }

        if iteration + 1 < config.iterations {
            tokio::time::sleep(Duration::from_secs(config.sample_interval_secs)).await;
        }
    }

    Ok(())
}

impl RecorderConfig {
    fn from_environment() -> Result<Self, Box<dyn Error>> {
        let mints = required_csv("TICKERSIX_MARKET_DATA_MINTS")?;
        let attestor_count = parse_env_or(
            "TICKERSIX_MARKET_DATA_ATTESTOR_COUNT",
            DEFAULT_ATTESTOR_COUNT,
        )?;
        let attestor_index = parse_env_or("TICKERSIX_MARKET_DATA_ATTESTOR_INDEX", 0usize)?;
        if attestor_index >= attestor_count {
            return Err(
                "TICKERSIX_MARKET_DATA_ATTESTOR_INDEX must be less than attestor count"
                    .to_owned()
                    .into(),
            );
        }

        let sample_interval_secs = parse_env_or(
            "TICKERSIX_MARKET_DATA_SAMPLE_INTERVAL_SECS",
            DEFAULT_SAMPLE_INTERVAL_SECS,
        )?;
        let api_key = env::var("TICKERSIX_JUPITER_API_KEY").ok();
        let default_provider_limit = if api_key.is_some() {
            API_KEY_PROVIDER_LIMIT_MILLI_RPS
        } else {
            KEYLESS_PROVIDER_LIMIT_MILLI_RPS
        };
        let provider_limit_milli_rps = parse_env_or(
            "TICKERSIX_MARKET_DATA_PROVIDER_LIMIT_MILLI_RPS",
            default_provider_limit,
        )?;
        validate_sampling_plan(
            attestor_count,
            sample_interval_secs,
            provider_limit_milli_rps,
        )?;

        let attestor_id = env::var("TICKERSIX_MARKET_DATA_ATTESTOR_ID")
            .unwrap_or_else(|_| format!("attestor-{attestor_index}"));
        let output_path = env::var("TICKERSIX_MARKET_DATA_OUTPUT")
            .unwrap_or_else(|_| format!("market-data/{attestor_id}.ndjson"));

        Ok(Self {
            api_key,
            base_url: env::var("TICKERSIX_JUPITER_BASE_URL")
                .unwrap_or_else(|_| market_data::DEFAULT_JUPITER_BASE_URL.to_owned()),
            attestor_id,
            attestor_index,
            attestor_count,
            sample_interval_secs,
            iterations: parse_env_or("TICKERSIX_MARKET_DATA_ITERATIONS", DEFAULT_ITERATIONS)?,
            output_path,
            mints,
        })
    }
}

fn required_csv(name: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let value = env::var(name).map_err(|_| format!("{name} must be configured"))?;
    let values: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect();
    if values.is_empty() {
        return Err(format!("{name} must contain at least one mint").into());
    }
    Ok(values)
}

fn parse_env_or<T>(name: &str, default: T) -> Result<T, Box<dyn Error>>
where
    T: std::str::FromStr,
    T::Err: Error + Send + Sync + 'static,
{
    match env::var(name) {
        Ok(value) => Ok(value.parse::<T>()?),
        Err(_) => Ok(default),
    }
}

fn append_line<'a>(output: &mut impl Write, line: RecorderLine<'a>) -> Result<(), Box<dyn Error>> {
    serde_json::to_writer(&mut *output, &line)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn now_unix_ms() -> Result<i64, Box<dyn Error>> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| "system clock exceeds i64 milliseconds".into())
}

fn print_usage() {
    println!(
        r#"Usage: cargo run -p backend -- record-market-data
Metadata: cargo run -p backend -- record-token-metadata
Market-data baseline gate: cargo run -p backend -- market-data-baseline-gate fixtures/market-data SPYx
Sponsor readiness gate: cargo run -p backend -- sponsor-readiness-gate fixtures/market-data/sponsors
Beta evidence gate: cargo run -p backend -- beta-evidence-gate [manifest.json]
Release freeze gate: cargo run -p backend -- release-freeze-gate [freeze.json] [beta.json]
Live Jupiter smoke: cargo run -p backend -- jupiter-smoke <mint[,mint...]>
Live xStocks smoke: cargo run -p backend -- xstocks-smoke SPYx Solana
Live private-market smoke: cargo run -p backend -- private-market-smoke
Live Pyth Pro smoke: TICKERSIX_PYTH_PRO_API_KEY=... cargo run -p backend -- pyth-pro-smoke <feed[,feed...]> <timestamp_us> [channel]

Required: TICKERSIX_MARKET_DATA_MINTS=mint_a,mint_b,...
Optional: TICKERSIX_JUPITER_API_KEY, TICKERSIX_MARKET_DATA_OUTPUT,
TICKERSIX_XSTOCKS_BASE_URL, TICKERSIX_SOLANA_RPC_URL,
TICKERSIX_PYTH_PRO_API_KEY, TICKERSIX_PYTH_PRO_BASE_URL,
TICKERSIX_PYTH_EXPECTED_FEED_ID, TICKERSIX_PYTH_MAX_FEED_AGE_US,
TICKERSIX_PYTH_MAX_CONFIDENCE_BPS, TICKERSIX_PRESTOCKS_URL,
TICKERSIX_TESSERA_URL,
TICKERSIX_MARKET_DATA_ATTESTOR_ID, TICKERSIX_MARKET_DATA_ATTESTOR_INDEX,
TICKERSIX_MARKET_DATA_ITERATIONS, TICKERSIX_MARKET_DATA_SAMPLE_INTERVAL_SECS

Analysis: cargo run -p backend -- analyze-market-data market-data/attestor-0.ndjson
Attestor worker: TICKERSIX_ATTESTOR_CONFIG=attestor.json cargo run -p backend -- attestor-run
HTTP API: TICKERSIX_DATABASE_URL=postgres://... cargo run -p backend -- api-serve
Ranked matcher: TICKERSIX_DATABASE_URL=postgres://... cargo run -p backend -- ranked-match <market_round_id>
Rating worker: TICKERSIX_DATABASE_URL=postgres://... cargo run -p backend -- rating-apply
Scheduler once: TICKERSIX_DATABASE_URL=postgres://... cargo run -p backend -- scheduler-once
Scheduler loop: TICKERSIX_DATABASE_URL=postgres://... TICKERSIX_SCHEDULER_INTERVAL_SECS=5 cargo run -p backend -- scheduler
Settlement planner: cargo run -p backend -- settlement-plan settlement.json
Proof endpoint: cargo run -p backend -- proof-serve proof.json [bind]"#
    );
}
