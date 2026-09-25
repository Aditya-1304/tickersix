//! Wallet-facing lineup preparation for official Battles.
//!
//! This module validates a browser-selected lineup against the frozen indexed
//! RoundAsset universe, computes the commitment through the shared protocol
//! crate, and returns an unsigned transaction for the connected wallet. It
//! never signs, sends, or stores private key material.

use std::{collections::HashSet, fmt};

use base64::{engine::general_purpose, Engine as _};
use protocol::{commitment, validate_lineup, MathError, LINEUP_SIZE};
use relay::{
    build_commit_lineup_instruction, build_reveal_lineup_instruction, config_pda, market_round_pda,
    serialize_unsigned_legacy_transaction, tickersix_program_id, CommitLineupAccounts,
    RevealLineupAccounts,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::auth::parse_wallet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineupError {
    InvalidBattle,
    BattleNotFound,
    NotBattleParticipant,
    InvalidLineupSize,
    DuplicateAsset,
    AssetNotInFrozenUniverse,
    CaptainNotSelected,
    InvalidSalt,
    RoundMetadataUnavailable,
    RoundAssetsUnavailable,
    CommitWindowClosed,
    AlreadyCommitted,
    RevealWindowClosed,
    NotCommitted,
    RpcUnavailable,
    TransactionBuildFailed,
    Storage(String),
}

impl fmt::Display for LineupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidBattle => "Battle public key is invalid",
            Self::BattleNotFound => "Battle was not found",
            Self::NotBattleParticipant => "wallet is not a Battle participant",
            Self::InvalidLineupSize => "lineup must contain exactly six assets",
            Self::DuplicateAsset => "lineup assets must be unique",
            Self::AssetNotInFrozenUniverse => {
                "lineup contains an asset outside the frozen RoundAsset universe"
            }
            Self::CaptainNotSelected => "captain must be one of the selected assets",
            Self::InvalidSalt => "salt must decode to exactly 32 bytes",
            Self::RoundMetadataUnavailable => "frozen MarketRound metadata is unavailable",
            Self::RoundAssetsUnavailable => "frozen RoundAsset universe is unavailable",
            Self::CommitWindowClosed => "Battle is not in the commit window",
            Self::AlreadyCommitted => "wallet already has a lineup commitment for this Battle",
            Self::RevealWindowClosed => "Battle is not in the reveal window",
            Self::NotCommitted => "wallet must commit a lineup before it can reveal",
            Self::RpcUnavailable => "Solana devnet RPC did not provide a recent blockhash",
            Self::TransactionBuildFailed => "wallet transaction could not be constructed",
            Self::Storage(_) => "lineup preparation storage operation failed",
        })
    }
}

impl std::error::Error for LineupError {}

#[derive(Debug, Deserialize)]
pub struct PrepareLineupRequest {
    pub asset_ids: Vec<u16>,
    pub captain_asset_id: u16,
    pub salt: String,
}

#[derive(Debug, Serialize)]
pub struct PreparedLineup {
    pub battle_pubkey: String,
    pub canonical_asset_ids: Vec<u16>,
    pub captain_asset_id: u16,
    pub commitment: String,
    pub commit_deadline: i64,
    pub transaction: PreparedTransaction,
}

#[derive(Debug, Serialize)]
pub struct PreparedTransaction {
    pub serialized_base64: String,
}

#[derive(Debug, Deserialize)]
pub struct RevealLineupRequest {
    pub asset_ids: Vec<u16>,
    pub captain_asset_id: u16,
    pub salt: String,
}

#[derive(Debug, Serialize)]
pub struct PreparedRevealLineup {
    pub battle_pubkey: String,
    pub canonical_asset_ids: Vec<u16>,
    pub captain_asset_id: u16,
    pub reveal_deadline: i64,
    pub transaction: PreparedTransaction,
}

#[derive(Debug)]
struct ValidatedLineup {
    asset_ids: [u16; LINEUP_SIZE],
    canonical_asset_ids: Vec<u16>,
    captain_asset_id: u16,
    salt: [u8; 32],
}

/// Validates request-local constraints before any database or RPC work.
///
/// This is intentionally separate from database authorization so malformed
/// browser input cannot cause a network request or produce a protocol hash.
fn validate_request(request: PrepareLineupRequest) -> Result<ValidatedLineup, LineupError> {
    if request.asset_ids.len() != LINEUP_SIZE {
        return Err(LineupError::InvalidLineupSize);
    }

    let mut asset_ids = [0u16; LINEUP_SIZE];
    asset_ids.copy_from_slice(&request.asset_ids);
    let mut unique = HashSet::with_capacity(LINEUP_SIZE);
    if asset_ids.iter().any(|asset_id| !unique.insert(*asset_id)) {
        return Err(LineupError::DuplicateAsset);
    }
    match validate_lineup(asset_ids, request.captain_asset_id) {
        Ok(()) => {}
        Err(MathError::CaptainNotInLineup) => return Err(LineupError::CaptainNotSelected),
        Err(MathError::InvalidLineup) => return Err(LineupError::DuplicateAsset),
        Err(_) => return Err(LineupError::InvalidLineupSize),
    }

    let salt = parse_salt(&request.salt)?;
    let mut canonical_asset_ids = asset_ids;
    canonical_asset_ids.sort_unstable();
    Ok(ValidatedLineup {
        asset_ids,
        canonical_asset_ids: canonical_asset_ids.to_vec(),
        captain_asset_id: request.captain_asset_id,
        salt,
    })
}

/// Validates the indexed reveal window at both boundary timestamps.
/// The chain remains authoritative, but rejecting stale preparation requests here
/// prevents the browser from presenting a transaction that cannot be accepted.
fn validate_reveal_window(
    round_state: &str,
    commit_deadline: i64,
    reveal_deadline: i64,
    now: i64,
) -> Result<(), LineupError> {
    if round_state != "REVEAL_OPEN"
        || commit_deadline <= 0
        || reveal_deadline < commit_deadline
        || now < commit_deadline
        || now > reveal_deadline
    {
        return Err(LineupError::RevealWindowClosed);
    }
    Ok(())
}

fn validate_reveal_request(request: RevealLineupRequest) -> Result<ValidatedLineup, LineupError> {
    validate_request(PrepareLineupRequest {
        asset_ids: request.asset_ids,
        captain_asset_id: request.captain_asset_id,
        salt: request.salt,
    })
}

/// Accepts either a 64-character hex salt or padded/unpadded base64.
fn parse_salt(value: &str) -> Result<[u8; 32], LineupError> {
    let trimmed = value.trim();
    let decoded = hex::decode(trimmed)
        .or_else(|_| {
            general_purpose::STANDARD
                .decode(trimmed)
                .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(trimmed))
                .or_else(|_| general_purpose::URL_SAFE.decode(trimmed))
                .or_else(|_| general_purpose::URL_SAFE_NO_PAD.decode(trimmed))
        })
        .map_err(|_| LineupError::InvalidSalt)?;
    decoded.try_into().map_err(|_| LineupError::InvalidSalt)
}

/// Prepares a wallet-owned CommitLineup transaction after checking the
/// authenticated participant and the indexed frozen round state.
pub async fn prepare_lineup(
    pool: &PgPool,
    solana_rpc_url: &str,
    battle_pubkey: &str,
    wallet: &str,
    request: PrepareLineupRequest,
    now: i64,
) -> Result<PreparedLineup, LineupError> {
    let battle = parse_wallet(battle_pubkey).map_err(|_| LineupError::InvalidBattle)?;
    let wallet_bytes = parse_wallet(wallet).map_err(|_| LineupError::NotBattleParticipant)?;
    let validated = validate_request(request)?;

    let row = sqlx::query(
        "SELECT b.market_round_id, b.player_a, b.player_b, b.state, b.result,
                b.player_a_committed, b.player_b_committed,
                r.chain_pubkey AS market_round_pubkey, r.round_sequence,
                r.state AS round_state, r.registry_version,
                r.commit_deadline
         FROM battles b
         JOIN market_rounds r ON r.id = b.market_round_id
         WHERE b.chain_pubkey = $1",
    )
    .bind(battle_pubkey)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(LineupError::BattleNotFound)?;

    let player_a: String = row.try_get("player_a").map_err(storage_error)?;
    let player_b: String = row.try_get("player_b").map_err(storage_error)?;
    let player_a_bytes = parse_wallet(&player_a).map_err(|_| LineupError::InvalidBattle)?;
    let player_b_bytes = parse_wallet(&player_b).map_err(|_| LineupError::InvalidBattle)?;
    let committed = if player_a_bytes == wallet_bytes {
        row.try_get::<bool, _>("player_a_committed")
            .map_err(storage_error)?
    } else if player_b_bytes == wallet_bytes {
        row.try_get::<bool, _>("player_b_committed")
            .map_err(storage_error)?
    } else {
        return Err(LineupError::NotBattleParticipant);
    };
    if committed {
        return Err(LineupError::AlreadyCommitted);
    }

    let round_state: String = row.try_get("round_state").map_err(storage_error)?;
    let commit_deadline: i64 = row.try_get("commit_deadline").map_err(storage_error)?;
    let registry_version: i64 = row.try_get("registry_version").map_err(storage_error)?;
    let round_sequence: i64 = row.try_get("round_sequence").map_err(storage_error)?;
    let result: Option<String> = row.try_get("result").map_err(storage_error)?;
    let battle_state: String = row.try_get("state").map_err(storage_error)?;
    if round_state != "COMMIT_OPEN"
        || commit_deadline <= 0
        || now > commit_deadline
        || result.is_some()
        || matches!(battle_state.as_str(), "FINALIZED" | "SETTLED" | "VOIDED")
    {
        return Err(LineupError::CommitWindowClosed);
    }
    if registry_version <= 0 || registry_version > i64::from(u32::MAX) || round_sequence <= 0 {
        return Err(LineupError::RoundMetadataUnavailable);
    }

    let market_round_pubkey: String = row.try_get("market_round_pubkey").map_err(storage_error)?;
    let market_round =
        parse_wallet(&market_round_pubkey).map_err(|_| LineupError::RoundMetadataUnavailable)?;
    let expected_market_round = market_round_pda(
        u64::try_from(round_sequence).map_err(|_| LineupError::RoundMetadataUnavailable)?,
    );
    if market_round != expected_market_round {
        return Err(LineupError::RoundMetadataUnavailable);
    }

    let market_round_id: i64 = row.try_get("market_round_id").map_err(storage_error)?;
    let asset_rows = sqlx::query(
        "SELECT asset_id FROM round_assets
         WHERE market_round_id = $1
         ORDER BY asset_id ASC",
    )
    .bind(market_round_id)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?;
    if asset_rows.is_empty() {
        return Err(LineupError::RoundAssetsUnavailable);
    }
    let frozen_assets = asset_rows
        .into_iter()
        .map(|asset| asset.try_get::<i64, _>("asset_id").map_err(storage_error))
        .collect::<Result<Vec<_>, _>>()?;
    if validated
        .asset_ids
        .iter()
        .any(|asset_id| !frozen_assets.contains(&i64::from(*asset_id)))
    {
        return Err(LineupError::AssetNotInFrozenUniverse);
    }

    let commitment = commitment(
        tickersix_program_id(),
        battle,
        wallet_bytes,
        u32::try_from(registry_version).map_err(|_| LineupError::RoundMetadataUnavailable)?,
        validated.asset_ids,
        validated.captain_asset_id,
        validated.salt,
    );
    let recent_blockhash = fetch_recent_blockhash(solana_rpc_url).await?;
    let instruction = build_commit_lineup_instruction(
        CommitLineupAccounts {
            config: config_pda(),
            battle,
            market_round,
            player: wallet_bytes,
        },
        commitment,
    );
    let serialized =
        serialize_unsigned_legacy_transaction(instruction, wallet_bytes, recent_blockhash)
            .map_err(|_| LineupError::TransactionBuildFailed)?;

    Ok(PreparedLineup {
        battle_pubkey: battle_pubkey.to_owned(),
        canonical_asset_ids: validated.canonical_asset_ids,
        captain_asset_id: validated.captain_asset_id,
        commitment: hex::encode(commitment),
        commit_deadline,
        transaction: PreparedTransaction {
            serialized_base64: general_purpose::STANDARD.encode(serialized),
        },
    })
}

/// Prepares the wallet-owned RevealLineup transaction after checking the
/// authenticated participant, prior commitment, frozen asset universe, and
/// indexed reveal window. The browser still signs and submits the transaction.
pub async fn prepare_reveal_lineup(
    pool: &PgPool,
    solana_rpc_url: &str,
    battle_pubkey: &str,
    wallet: &str,
    request: RevealLineupRequest,
    now: i64,
) -> Result<PreparedRevealLineup, LineupError> {
    let battle = parse_wallet(battle_pubkey).map_err(|_| LineupError::InvalidBattle)?;
    let wallet_bytes = parse_wallet(wallet).map_err(|_| LineupError::NotBattleParticipant)?;
    let validated = validate_reveal_request(request)?;

    let row = sqlx::query(
        "SELECT b.market_round_id, b.player_a, b.player_b, b.state, b.result,
                b.player_a_committed, b.player_b_committed,
                r.chain_pubkey AS market_round_pubkey, r.round_sequence,
                r.state AS round_state, r.registry_version,
                r.commit_deadline, r.reveal_deadline
         FROM battles b
         JOIN market_rounds r ON r.id = b.market_round_id
         WHERE b.chain_pubkey = $1",
    )
    .bind(battle_pubkey)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(LineupError::BattleNotFound)?;

    let player_a: String = row.try_get("player_a").map_err(storage_error)?;
    let player_b: String = row.try_get("player_b").map_err(storage_error)?;
    let player_a_bytes = parse_wallet(&player_a).map_err(|_| LineupError::InvalidBattle)?;
    let player_b_bytes = parse_wallet(&player_b).map_err(|_| LineupError::InvalidBattle)?;
    let committed = if player_a_bytes == wallet_bytes {
        row.try_get::<bool, _>("player_a_committed")
            .map_err(storage_error)?
    } else if player_b_bytes == wallet_bytes {
        row.try_get::<bool, _>("player_b_committed")
            .map_err(storage_error)?
    } else {
        return Err(LineupError::NotBattleParticipant);
    };
    if !committed {
        return Err(LineupError::NotCommitted);
    }

    let round_state: String = row.try_get("round_state").map_err(storage_error)?;
    let commit_deadline: i64 = row.try_get("commit_deadline").map_err(storage_error)?;
    let reveal_deadline: i64 = row.try_get("reveal_deadline").map_err(storage_error)?;
    validate_reveal_window(&round_state, commit_deadline, reveal_deadline, now)?;

    let result: Option<String> = row.try_get("result").map_err(storage_error)?;
    let battle_state: String = row.try_get("state").map_err(storage_error)?;
    if result.is_some() || matches!(battle_state.as_str(), "FINALIZED" | "SETTLED" | "VOIDED") {
        return Err(LineupError::RevealWindowClosed);
    }

    let registry_version: i64 = row.try_get("registry_version").map_err(storage_error)?;
    let round_sequence: i64 = row.try_get("round_sequence").map_err(storage_error)?;
    if registry_version <= 0 || registry_version > i64::from(u32::MAX) || round_sequence <= 0 {
        return Err(LineupError::RoundMetadataUnavailable);
    }

    let market_round_pubkey: String = row.try_get("market_round_pubkey").map_err(storage_error)?;
    let market_round =
        parse_wallet(&market_round_pubkey).map_err(|_| LineupError::RoundMetadataUnavailable)?;
    let expected_market_round = market_round_pda(
        u64::try_from(round_sequence).map_err(|_| LineupError::RoundMetadataUnavailable)?,
    );
    if market_round != expected_market_round {
        return Err(LineupError::RoundMetadataUnavailable);
    }

    let market_round_id: i64 = row.try_get("market_round_id").map_err(storage_error)?;
    let asset_rows = sqlx::query(
        "SELECT asset_id FROM round_assets
         WHERE market_round_id = $1
         ORDER BY asset_id ASC",
    )
    .bind(market_round_id)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?;
    if asset_rows.is_empty() {
        return Err(LineupError::RoundAssetsUnavailable);
    }
    let frozen_assets = asset_rows
        .into_iter()
        .map(|asset| asset.try_get::<i64, _>("asset_id").map_err(storage_error))
        .collect::<Result<Vec<_>, _>>()?;
    if validated
        .asset_ids
        .iter()
        .any(|asset_id| !frozen_assets.contains(&i64::from(*asset_id)))
    {
        return Err(LineupError::AssetNotInFrozenUniverse);
    }

    let recent_blockhash = fetch_recent_blockhash(solana_rpc_url).await?;
    let instruction = build_reveal_lineup_instruction(
        RevealLineupAccounts {
            battle,
            market_round,
            player: wallet_bytes,
        },
        validated.asset_ids,
        validated.captain_asset_id,
        validated.salt,
    );
    let serialized =
        serialize_unsigned_legacy_transaction(instruction, wallet_bytes, recent_blockhash)
            .map_err(|_| LineupError::TransactionBuildFailed)?;

    Ok(PreparedRevealLineup {
        battle_pubkey: battle_pubkey.to_owned(),
        canonical_asset_ids: validated.canonical_asset_ids,
        captain_asset_id: validated.captain_asset_id,
        reveal_deadline,
        transaction: PreparedTransaction {
            serialized_base64: general_purpose::STANDARD.encode(serialized),
        },
    })
}

async fn fetch_recent_blockhash(solana_rpc_url: &str) -> Result<[u8; 32], LineupError> {
    #[derive(Serialize)]
    struct RpcRequest {
        jsonrpc: &'static str,
        id: u8,
        method: &'static str,
        params: [RpcConfig; 1],
    }
    #[derive(Serialize)]
    struct RpcConfig {
        commitment: &'static str,
    }
    #[derive(Deserialize)]
    struct RpcResponse {
        result: Option<RpcResult>,
        error: Option<RpcError>,
    }
    #[derive(Deserialize)]
    struct RpcResult {
        value: RpcValue,
    }
    #[derive(Deserialize)]
    struct RpcValue {
        blockhash: String,
    }
    #[derive(Deserialize)]
    struct RpcError {}

    let response = reqwest::Client::new()
        .post(solana_rpc_url)
        .json(&RpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "getLatestBlockhash",
            params: [RpcConfig {
                commitment: "finalized",
            }],
        })
        .send()
        .await
        .map_err(|_| LineupError::RpcUnavailable)?;
    if !response.status().is_success() {
        return Err(LineupError::RpcUnavailable);
    }
    let payload = response
        .json::<RpcResponse>()
        .await
        .map_err(|_| LineupError::RpcUnavailable)?;
    if payload.error.is_some() {
        return Err(LineupError::RpcUnavailable);
    }
    let blockhash = payload
        .result
        .ok_or(LineupError::RpcUnavailable)?
        .value
        .blockhash;
    parse_wallet(&blockhash).map_err(|_| LineupError::RpcUnavailable)
}

fn storage_error(error: sqlx::Error) -> LineupError {
    LineupError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_validation_rejects_duplicate_assets_before_storage_access() {
        let error = validate_request(PrepareLineupRequest {
            asset_ids: vec![2, 7, 11, 18, 23, 23],
            captain_asset_id: 7,
            salt: "00".repeat(32),
        })
        .unwrap_err();
        assert_eq!(error, LineupError::DuplicateAsset);
    }

    #[test]
    fn request_validation_canonicalizes_ids_but_preserves_the_captain() {
        let validated = validate_request(PrepareLineupRequest {
            asset_ids: vec![31, 7, 23, 2, 18, 11],
            captain_asset_id: 7,
            salt: general_purpose::STANDARD.encode([5u8; 32]),
        })
        .unwrap();
        assert_eq!(validated.canonical_asset_ids, vec![2, 7, 11, 18, 23, 31]);
        assert_eq!(validated.captain_asset_id, 7);
        assert_eq!(validated.salt, [5u8; 32]);
    }

    #[test]
    fn request_validation_rejects_a_captain_outside_the_lineup() {
        let error = validate_request(PrepareLineupRequest {
            asset_ids: vec![2, 7, 11, 18, 23, 31],
            captain_asset_id: 99,
            salt: "00".repeat(32),
        })
        .unwrap_err();
        assert_eq!(error, LineupError::CaptainNotSelected);
    }

    #[test]
    fn reveal_window_rejects_transactions_outside_the_authoritative_window() {
        assert_eq!(
            validate_reveal_window("REVEAL_OPEN", 100, 200, 99),
            Err(LineupError::RevealWindowClosed)
        );
        assert_eq!(
            validate_reveal_window("REVEAL_OPEN", 100, 200, 201),
            Err(LineupError::RevealWindowClosed)
        );
        assert_eq!(validate_reveal_window("REVEAL_OPEN", 100, 200, 100), Ok(()));
        assert_eq!(validate_reveal_window("REVEAL_OPEN", 100, 200, 200), Ok(()));
    }
}
