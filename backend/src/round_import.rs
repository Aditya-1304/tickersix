//! Finalized Devnet round-manifest ingestion for the backend projection.
//!
//! The bootstrap tool owns transaction construction. This module accepts only
//! its public manifest after validating canonical PDAs and confirming that the
//! referenced accounts exist under the deployed TickerSix program at finalized
//! commitment, then writes the normal indexer projections.

use std::{fmt, fs, path::Path};

use relay::{market_round_pda, registry_entry_pda, round_asset_pda, tickersix_program_id};
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;

use crate::{
    auth::parse_wallet,
    indexer::{self, IndexedMarketRound, IndexedRoundAsset},
};

pub const MIN_PUBLIC_ROUND_ASSETS: usize = 10;

#[derive(Debug)]
pub enum RoundImportError {
    InvalidManifest(String),
    Rpc(String),
    Storage(String),
}

impl fmt::Display for RoundImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidManifest(message) => {
                write!(formatter, "invalid Devnet round manifest: {message}")
            }
            Self::Rpc(message) => {
                write!(formatter, "Devnet account verification failed: {message}")
            }
            Self::Storage(message) => {
                write!(formatter, "round manifest projection failed: {message}")
            }
        }
    }
}

impl std::error::Error for RoundImportError {}

#[derive(Debug, Clone, Deserialize)]
pub struct RoundManifest {
    pub schema_version: u16,
    pub cluster: String,
    pub program_id: String,
    pub round: ManifestRound,
    pub assets: Vec<ManifestAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestRound {
    pub chain_pubkey: String,
    pub round_sequence: i64,
    pub registry_version: u32,
    pub state: String,
    pub is_replay: bool,
    pub competition_domain: String,
    pub settlement_source_kind: String,
    #[serde(rename = "eligibilityFrozenAt")]
    pub eligibility_frozen_at: i64,
    #[serde(rename = "queueCloseAt")]
    pub queue_close_at: i64,
    #[serde(rename = "commitDeadline")]
    pub commit_deadline: i64,
    #[serde(rename = "revealDeadline")]
    pub reveal_deadline: i64,
    #[serde(rename = "startTargetAt")]
    pub start_target_at: i64,
    #[serde(rename = "endTargetAt")]
    pub end_target_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestAsset {
    pub asset_id: i64,
    pub symbol: String,
    pub name: String,
    pub representation: String,
    pub provider: String,
    pub scoring_mint: String,
    pub round_asset_pubkey: String,
    pub registry_entry_pubkey: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    pub market_round_id: i64,
    pub round_asset_count: usize,
}

fn manifest_error(message: impl Into<String>) -> RoundImportError {
    RoundImportError::InvalidManifest(message.into())
}

fn parse_identity(value: &str, field: &str) -> Result<[u8; 32], RoundImportError> {
    parse_wallet(value)
        .map_err(|_| manifest_error(format!("{field} is not a 32-byte base58 public key")))
}

/// Validates all public identities before touching PostgreSQL.
///
/// The database is a projection, not an authority. Matching the canonical PDA
/// derivations here prevents a hand-edited manifest from making a different
/// round or registry entry appear to be the one created on Devnet.
pub fn validate_manifest(manifest: &RoundManifest) -> Result<(), RoundImportError> {
    if manifest.schema_version != 1 {
        return Err(manifest_error("schema_version must be 1"));
    }
    if manifest.cluster != "devnet" {
        return Err(manifest_error("cluster must be devnet"));
    }
    let expected_program = bs58::encode(tickersix_program_id()).into_string();
    if manifest.program_id != expected_program {
        return Err(manifest_error(
            "program_id does not match the deployed TickerSix program",
        ));
    }
    if manifest.round.is_replay
        || manifest.round.state != "SCHEDULED"
        || manifest.round.competition_domain != "PUBLIC_EQUITY"
        || manifest.round.settlement_source_kind != "JUPITER_TOKEN_SPOT_V1"
    {
        return Err(manifest_error(
            "only a scheduled Public Ranked Jupiter round can be imported",
        ));
    }
    if manifest.round.round_sequence <= 0 || manifest.round.registry_version == 0 {
        return Err(manifest_error(
            "round sequence and registry version must be positive",
        ));
    }
    if manifest.round.eligibility_frozen_at >= manifest.round.queue_close_at
        || manifest.round.queue_close_at >= manifest.round.commit_deadline
        || manifest.round.commit_deadline >= manifest.round.reveal_deadline
        || manifest.round.reveal_deadline >= manifest.round.start_target_at
        || manifest.round.start_target_at >= manifest.round.end_target_at
    {
        return Err(manifest_error("round timestamps are not strictly ordered"));
    }

    let market_round = parse_identity(&manifest.round.chain_pubkey, "round.chain_pubkey")?;
    let expected_market_round = market_round_pda(
        u64::try_from(manifest.round.round_sequence)
            .map_err(|_| manifest_error("round sequence is outside u64"))?,
    );
    if market_round != expected_market_round {
        return Err(manifest_error(
            "round.chain_pubkey is not the canonical MarketRound PDA",
        ));
    }

    if manifest.assets.len() < MIN_PUBLIC_ROUND_ASSETS {
        return Err(manifest_error(format!(
            "public round requires at least {MIN_PUBLIC_ROUND_ASSETS} assets",
        )));
    }
    let mut asset_ids = std::collections::BTreeSet::new();
    let mut round_asset_keys = std::collections::BTreeSet::new();
    let mut registry_keys = std::collections::BTreeSet::new();
    let mut scoring_mints = std::collections::BTreeSet::new();
    for asset in &manifest.assets {
        let asset_id = u16::try_from(asset.asset_id)
            .map_err(|_| manifest_error("asset_id must fit in u16"))?;
        if !asset_ids.insert(asset_id) {
            return Err(manifest_error(format!("duplicate asset_id {asset_id}")));
        }
        if asset.status != "ELIGIBLE"
            || asset.symbol.trim().is_empty()
            || asset.name.trim().is_empty()
            || asset.representation.trim().is_empty()
            || asset.provider.trim().is_empty()
        {
            return Err(manifest_error(format!(
                "asset {asset_id} has incomplete eligible metadata"
            )));
        }
        let scoring_mint = parse_identity(&asset.scoring_mint, "asset.scoring_mint")?;
        if !scoring_mints.insert(scoring_mint) {
            return Err(manifest_error(format!(
                "duplicate scoring mint for asset {asset_id}"
            )));
        }
        let round_asset = parse_identity(&asset.round_asset_pubkey, "asset.round_asset_pubkey")?;
        if !round_asset_keys.insert(round_asset) {
            return Err(manifest_error(format!(
                "duplicate RoundAsset PDA for asset {asset_id}"
            )));
        }
        if round_asset != round_asset_pda(market_round, asset_id) {
            return Err(manifest_error(format!(
                "RoundAsset PDA does not match asset {asset_id} and the MarketRound",
            )));
        }
        let registry_entry =
            parse_identity(&asset.registry_entry_pubkey, "asset.registry_entry_pubkey")?;
        if !registry_keys.insert(registry_entry) {
            return Err(manifest_error(format!(
                "duplicate registry entry PDA for asset {asset_id}"
            )));
        }
        if registry_entry != registry_entry_pda(manifest.round.registry_version, asset_id) {
            return Err(manifest_error(format!(
                "registry entry PDA does not match asset {asset_id} and the registry version",
            )));
        }
    }
    Ok(())
}

async fn verify_finalized_accounts(
    manifest: &RoundManifest,
    rpc_url: &str,
) -> Result<(), RoundImportError> {
    let mut addresses = Vec::with_capacity(manifest.assets.len() + 1);
    addresses.push(manifest.round.chain_pubkey.clone());
    addresses.extend(
        manifest
            .assets
            .iter()
            .map(|asset| asset.round_asset_pubkey.clone()),
    );
    let response = reqwest::Client::new()
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": "tickersix-round-import",
            "method": "getMultipleAccounts",
            "params": [addresses, {"encoding": "base64", "commitment": "finalized"}]
        }))
        .send()
        .await
        .map_err(|error| RoundImportError::Rpc(error.to_string()))?
        .error_for_status()
        .map_err(|error| RoundImportError::Rpc(error.to_string()))?
        .json::<serde_json::Value>()
        .await
        .map_err(|error| RoundImportError::Rpc(error.to_string()))?;
    if let Some(error) = response.get("error") {
        return Err(RoundImportError::Rpc(error.to_string()));
    }
    let values = response
        .pointer("/result/value")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| RoundImportError::Rpc("RPC response omitted result.value".to_owned()))?;
    if values.len() != addresses.len() {
        return Err(RoundImportError::Rpc(
            "RPC returned an incomplete account list".to_owned(),
        ));
    }
    let expected_owner = manifest.program_id.as_str();
    for (address, account) in addresses.iter().zip(values) {
        let Some(account) = account.as_object() else {
            return Err(RoundImportError::Rpc(format!(
                "finalized account is missing on Devnet: {address}",
            )));
        };
        if account.get("owner").and_then(serde_json::Value::as_str) != Some(expected_owner) {
            return Err(RoundImportError::Rpc(format!(
                "account {address} is not owned by the TickerSix program",
            )));
        }
    }
    Ok(())
}

/// Imports a bootstrap manifest only after finalized account existence checks.
pub async fn import_manifest(
    pool: &PgPool,
    manifest_path: impl AsRef<Path>,
    rpc_url: &str,
) -> Result<ImportSummary, RoundImportError> {
    let manifest: RoundManifest = serde_json::from_str(
        &fs::read_to_string(manifest_path.as_ref())
            .map_err(|error| manifest_error(error.to_string()))?,
    )
    .map_err(|error| manifest_error(error.to_string()))?;
    validate_manifest(&manifest)?;
    verify_finalized_accounts(&manifest, rpc_url).await?;

    let round = IndexedMarketRound {
        chain_pubkey: manifest.round.chain_pubkey.clone(),
        round_sequence: manifest.round.round_sequence,
        registry_version: manifest.round.registry_version,
        state: manifest.round.state.clone(),
        is_replay: manifest.round.is_replay,
        competition_domain: manifest.round.competition_domain.clone(),
        settlement_source_kind: manifest.round.settlement_source_kind.clone(),
        queue_close_at: manifest.round.queue_close_at,
        commit_deadline: manifest.round.commit_deadline,
        reveal_deadline: manifest.round.reveal_deadline,
        start_target_at: manifest.round.start_target_at,
        end_target_at: manifest.round.end_target_at,
        indexed_at: manifest.round.eligibility_frozen_at,
    };
    let market_round_id = indexer::upsert_market_round(pool, &round)
        .await
        .map_err(|error| RoundImportError::Storage(error.to_string()))?;
    for asset in &manifest.assets {
        indexer::upsert_round_asset(
            pool,
            &IndexedRoundAsset {
                market_round_id,
                round_asset_pubkey: asset.round_asset_pubkey.clone(),
                asset_id: asset.asset_id,
                symbol: asset.symbol.clone(),
                name: asset.name.clone(),
                representation: asset.representation.clone(),
                provider: asset.provider.clone(),
                scoring_mint: asset.scoring_mint.clone(),
                status: asset.status.clone(),
                indexed_at: manifest.round.eligibility_frozen_at,
            },
        )
        .await
        .map_err(|error| RoundImportError::Storage(error.to_string()))?;
    }
    Ok(ImportSummary {
        market_round_id,
        round_asset_count: manifest.assets.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(bytes: [u8; 32]) -> String {
        bs58::encode(bytes).into_string()
    }

    fn manifest() -> RoundManifest {
        let round_sequence = 1i64;
        let market_round = market_round_pda(round_sequence as u64);
        let assets = (0..MIN_PUBLIC_ROUND_ASSETS)
            .map(|asset_id| {
                let asset_id = asset_id as u16;
                ManifestAsset {
                    asset_id: i64::from(asset_id),
                    symbol: format!("A{asset_id}"),
                    name: format!("Asset {asset_id}"),
                    representation: format!("Asset{asset_id}"),
                    provider: "xStocks".to_owned(),
                    scoring_mint: key([100 + asset_id as u8; 32]),
                    round_asset_pubkey: key(round_asset_pda(market_round, asset_id)),
                    registry_entry_pubkey: key(registry_entry_pda(1, asset_id)),
                    status: "ELIGIBLE".to_owned(),
                }
            })
            .collect();
        RoundManifest {
            schema_version: 1,
            cluster: "devnet".to_owned(),
            program_id: bs58::encode(tickersix_program_id()).into_string(),
            round: ManifestRound {
                chain_pubkey: key(market_round),
                round_sequence,
                registry_version: 1,
                state: "SCHEDULED".to_owned(),
                is_replay: false,
                competition_domain: "PUBLIC_EQUITY".to_owned(),
                settlement_source_kind: "JUPITER_TOKEN_SPOT_V1".to_owned(),
                eligibility_frozen_at: 100,
                queue_close_at: 200,
                commit_deadline: 300,
                reveal_deadline: 400,
                start_target_at: 500,
                end_target_at: 600,
            },
            assets,
        }
    }

    #[test]
    fn manifest_accepts_canonical_round_and_asset_identities() {
        validate_manifest(&manifest()).expect("canonical manifest must validate");
    }

    #[test]
    fn manifest_rejects_a_round_asset_bound_to_another_round() {
        let mut manifest = manifest();
        manifest.assets[0].round_asset_pubkey = key(round_asset_pda(market_round_pda(2), 0));
        let error = validate_manifest(&manifest).unwrap_err();
        assert!(error.to_string().contains("RoundAsset PDA"));
    }

    #[test]
    fn manifest_rejects_a_registry_entry_bound_to_another_version() {
        let mut manifest = manifest();
        manifest.assets[0].registry_entry_pubkey = key(registry_entry_pda(2, 0));
        let error = validate_manifest(&manifest).unwrap_err();
        assert!(error.to_string().contains("registry entry PDA"));
    }
}
