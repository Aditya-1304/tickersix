//! Backend entry points that are safe to run before the production API exists.
//!
//! Phase 0 deliberately ships as a small recorder command. It gathers the
//! evidence needed to calibrate Market Quality Policy values without silently
//! turning unmeasured assumptions into rated-round configuration.

use std::{
    collections::BTreeSet,
    env,
    error::Error,
    fs::{create_dir_all, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use market_data::{
    stagger_offsets_millis, summarize_phase0_window, validate_sampling_plan, JupiterClient,
    Phase0Observation, PriceBatch,
};
use serde::{Deserialize, Serialize};

pub mod attestor;
pub mod proof;
pub mod recovery;
pub mod settlement;

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
        Some("phase0-record") => record_phase0().await?,
        Some("phase0-metadata") => record_phase0_metadata().await?,
        Some("phase0-analyze") => analyze_phase0()?,
        Some("attestor-run") => attestor::run().await?,
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

/// Captures exact-mint Tokens V2 metadata as a separate immutable Phase 0
/// evidence artifact. Metadata is review input; it never discovers or replaces
/// the issuer-approved registry automatically.
async fn record_phase0_metadata() -> Result<(), Box<dyn Error>> {
    let mints = required_csv("TICKERSIX_PHASE0_MINTS")?;
    let api_key = env::var("TICKERSIX_JUPITER_API_KEY").ok();
    let base_url = env::var("TICKERSIX_JUPITER_BASE_URL")
        .unwrap_or_else(|_| market_data::DEFAULT_JUPITER_BASE_URL.to_owned());
    let client = JupiterClient::with_base_url(base_url, api_key)?;
    let batch = client.fetch_token_information(&mints).await?;
    println!("{}", serde_json::to_string_pretty(&batch)?);
    Ok(())
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

/// Reads recorder NDJSON and emits measurement-only summaries for the three
/// candidate observation durations in the source specification.
fn analyze_phase0() -> Result<(), Box<dyn Error>> {
    let input_path = env::args()
        .nth(2)
        .or_else(|| env::var("TICKERSIX_PHASE0_INPUT").ok())
        .ok_or("usage: cargo run -p backend -- phase0-analyze <ndjson>")?;
    let file = File::open(&input_path)?;
    let reader = BufReader::new(file);
    let mut requested_mints = BTreeSet::new();
    let mut observations = Vec::new();

    for (line_number, line) in reader.lines().enumerate() {
        let line = line?;
        let record: RecorderLineOwned = serde_json::from_str(&line).map_err(|error| {
            format!(
                "invalid Phase 0 NDJSON at line {}: {error}",
                line_number + 1
            )
        })?;
        requested_mints.extend(record.requested_mints);
        if record.status != "ok" {
            continue;
        }
        let Some(batch) = record.batch else {
            return Err(format!("successful Phase 0 row {} has no batch", line_number + 1).into());
        };
        for observation in batch.observations {
            observations.push(Phase0Observation::new(
                record.attestor_id.clone(),
                observation.mint,
                observation.price_q9,
                observation.source_block_id,
                observation.observed_at_unix_ms,
            ));
        }
    }

    if observations.is_empty() {
        return Err("Phase 0 input contains no successful observations".into());
    }
    if requested_mints.is_empty() {
        requested_mints.extend(
            observations
                .iter()
                .map(|observation| observation.mint.clone()),
        );
    }
    let requested_mints: Vec<String> = requested_mints.into_iter().collect();
    let window_start = match env::var("TICKERSIX_PHASE0_WINDOW_START_UNIX_MS") {
        Ok(value) => value.parse::<i64>()?,
        Err(_) => observations
            .iter()
            .map(|observation| observation.observed_at_unix_ms)
            .min()
            .ok_or("Phase 0 input contains no timestamps")?,
    };
    let windows = env::var("TICKERSIX_PHASE0_WINDOWS_SECS")
        .unwrap_or_else(|_| "3600,7200,14400".to_owned())
        .split(',')
        .map(|value| value.trim().parse::<u64>())
        .collect::<Result<Vec<_>, _>>()?;
    if windows.is_empty() || windows.contains(&0) {
        return Err("TICKERSIX_PHASE0_WINDOWS_SECS must contain positive durations".into());
    }

    let summaries = windows
        .into_iter()
        .map(|window_secs| {
            summarize_phase0_window(&requested_mints, &observations, window_start, window_secs)
        })
        .collect::<Result<Vec<_>, _>>()?;
    println!("{}", serde_json::to_string_pretty(&summaries)?);
    Ok(())
}

async fn record_phase0() -> Result<(), Box<dyn Error>> {
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
        let mints = required_csv("TICKERSIX_PHASE0_MINTS")?;
        let attestor_count =
            parse_env_or("TICKERSIX_PHASE0_ATTESTOR_COUNT", DEFAULT_ATTESTOR_COUNT)?;
        let attestor_index = parse_env_or("TICKERSIX_PHASE0_ATTESTOR_INDEX", 0usize)?;
        if attestor_index >= attestor_count {
            return Err(
                "TICKERSIX_PHASE0_ATTESTOR_INDEX must be less than attestor count"
                    .to_owned()
                    .into(),
            );
        }

        let sample_interval_secs = parse_env_or(
            "TICKERSIX_PHASE0_SAMPLE_INTERVAL_SECS",
            DEFAULT_SAMPLE_INTERVAL_SECS,
        )?;
        let api_key = env::var("TICKERSIX_JUPITER_API_KEY").ok();
        let default_provider_limit = if api_key.is_some() {
            API_KEY_PROVIDER_LIMIT_MILLI_RPS
        } else {
            KEYLESS_PROVIDER_LIMIT_MILLI_RPS
        };
        let provider_limit_milli_rps = parse_env_or(
            "TICKERSIX_PHASE0_PROVIDER_LIMIT_MILLI_RPS",
            default_provider_limit,
        )?;
        validate_sampling_plan(
            attestor_count,
            sample_interval_secs,
            provider_limit_milli_rps,
        )?;

        let attestor_id = env::var("TICKERSIX_PHASE0_ATTESTOR_ID")
            .unwrap_or_else(|_| format!("attestor-{attestor_index}"));
        let output_path = env::var("TICKERSIX_PHASE0_OUTPUT")
            .unwrap_or_else(|_| format!("phase0/{attestor_id}.ndjson"));

        Ok(Self {
            api_key,
            base_url: env::var("TICKERSIX_JUPITER_BASE_URL")
                .unwrap_or_else(|_| market_data::DEFAULT_JUPITER_BASE_URL.to_owned()),
            attestor_id,
            attestor_index,
            attestor_count,
            sample_interval_secs,
            iterations: parse_env_or("TICKERSIX_PHASE0_ITERATIONS", DEFAULT_ITERATIONS)?,
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
        r#"Usage: cargo run -p backend -- phase0-record
Metadata: cargo run -p backend -- phase0-metadata

Required: TICKERSIX_PHASE0_MINTS=mint_a,mint_b,...
Optional: TICKERSIX_JUPITER_API_KEY, TICKERSIX_PHASE0_OUTPUT,
TICKERSIX_PHASE0_ATTESTOR_ID, TICKERSIX_PHASE0_ATTESTOR_INDEX,
TICKERSIX_PHASE0_ITERATIONS, TICKERSIX_PHASE0_SAMPLE_INTERVAL_SECS

Analysis: cargo run -p backend -- phase0-analyze phase0/attestor-0.ndjson
Phase 2 attestor: TICKERSIX_ATTESTOR_CONFIG=attestor.json cargo run -p backend -- attestor-run
Settlement planner: cargo run -p backend -- settlement-plan settlement.json
Proof endpoint: cargo run -p backend -- proof-serve proof.json [bind]"#
    );
}
