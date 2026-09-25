//! Canonical read model for one Battle before live updates begin.
//!
//! This endpoint combines durable Battle identity, participant metadata, ranked
//! rating snapshots, commit projections, finalized evidence, and replaceable
//! score projections. It never invents a result when the database has no
//! authoritative value.

use std::fmt;

use serde::Serialize;
use sqlx::{PgPool, Row};

use crate::auth::parse_wallet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BattleSnapshot {
    pub battle_pubkey: String,
    pub market_round_id: i64,
    pub market_round_sequence: i64,
    pub mode: String,
    pub rated: bool,
    pub state: String,
    pub settlement_source_kind: Option<String>,
    pub player_a: BattlePlayer,
    pub player_b: BattlePlayer,
    pub projected_scores: ProjectedScores,
    pub result: Option<String>,
    pub as_of: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BattlePlayer {
    pub wallet: String,
    pub display_name: Option<String>,
    pub rating_snapshot: Option<i32>,
    pub commit_status: &'static str,
    pub reveal_status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectedScores {
    pub player_a_q9: Option<i64>,
    pub player_b_q9: Option<i64>,
    pub status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BattleSnapshotError {
    InvalidBattle,
    NotFound,
    Storage(String),
}

impl fmt::Display for BattleSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidBattle => "battle address is not a valid Solana public key",
            Self::NotFound => "Battle was not found",
            Self::Storage(_) => "Battle snapshot storage operation failed",
        })
    }
}

impl std::error::Error for BattleSnapshotError {}

/// Loads the stable initial state used before a browser subscribes to SSE.
pub async fn get_battle(
    pool: &PgPool,
    battle_pubkey: &str,
) -> Result<BattleSnapshot, BattleSnapshotError> {
    parse_wallet(battle_pubkey).map_err(|_| BattleSnapshotError::InvalidBattle)?;

    let row = sqlx::query(
        "SELECT b.chain_pubkey, b.market_round_id, b.mode, b.rated, b.state, b.result,
                b.settlement_source_kind, b.player_a, b.player_b,
                b.player_a_committed, b.player_b_committed,
                COALESCE(rp.rating_a_snapshot, b.rating_a_before) AS rating_a_snapshot,
                COALESCE(rp.rating_b_snapshot, b.rating_b_before) AS rating_b_snapshot,
                ua.display_name AS player_a_display_name,
                ub.display_name AS player_b_display_name,
                mr.round_sequence, mr.state AS round_state,
                l.player_a_score_q9, l.player_b_score_q9,
                facts.battle_pubkey AS finalized_facts_battle_pubkey,
                COALESCE(l.as_of, b.indexed_at) AS as_of
         FROM battles b
         JOIN market_rounds mr ON mr.id = b.market_round_id
         JOIN users ua ON ua.wallet = b.player_a
         JOIN users ub ON ub.wallet = b.player_b
         LEFT JOIN ranked_pairings rp ON rp.battle_pubkey = b.chain_pubkey
         LEFT JOIN battle_live_state l ON l.battle_pubkey = b.chain_pubkey
         LEFT JOIN battle_competitive_facts facts
                ON facts.battle_pubkey = b.chain_pubkey
         WHERE b.chain_pubkey = ",
    )
    .bind(battle_pubkey)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(BattleSnapshotError::NotFound)?;

    let state: String = row.try_get("state").map_err(storage_error)?;
    let result: Option<String> = row.try_get("result").map_err(storage_error)?;
    let terminal = is_terminal(&state, result.as_deref());
    let round_state: String = row.try_get("round_state").map_err(storage_error)?;
    let has_finalized_facts = row
        .try_get::<Option<String>, _>("finalized_facts_battle_pubkey")
        .map_err(storage_error)?
        .is_some();
    let player_a_committed: bool = row.try_get("player_a_committed").map_err(storage_error)?;
    let player_b_committed: bool = row.try_get("player_b_committed").map_err(storage_error)?;
    let player_a_score_q9: Option<i64> = row.try_get("player_a_score_q9").map_err(storage_error)?;
    let player_b_score_q9: Option<i64> = row.try_get("player_b_score_q9").map_err(storage_error)?;

    Ok(BattleSnapshot {
        battle_pubkey: row.try_get("chain_pubkey").map_err(storage_error)?,
        market_round_id: row.try_get("market_round_id").map_err(storage_error)?,
        market_round_sequence: row.try_get("round_sequence").map_err(storage_error)?,
        mode: row.try_get("mode").map_err(storage_error)?,
        rated: row.try_get("rated").map_err(storage_error)?,
        state,
        settlement_source_kind: row
            .try_get("settlement_source_kind")
            .map_err(storage_error)?,
        player_a: BattlePlayer {
            wallet: row.try_get("player_a").map_err(storage_error)?,
            display_name: row
                .try_get("player_a_display_name")
                .map_err(storage_error)?,
            rating_snapshot: row.try_get("rating_a_snapshot").map_err(storage_error)?,
            commit_status: commit_status(player_a_committed),
            reveal_status: reveal_status(
                &round_state,
                player_a_committed,
                has_finalized_facts,
                terminal,
            ),
        },
        player_b: BattlePlayer {
            wallet: row.try_get("player_b").map_err(storage_error)?,
            display_name: row
                .try_get("player_b_display_name")
                .map_err(storage_error)?,
            rating_snapshot: row.try_get("rating_b_snapshot").map_err(storage_error)?,
            commit_status: commit_status(player_b_committed),
            reveal_status: reveal_status(
                &round_state,
                player_b_committed,
                has_finalized_facts,
                terminal,
            ),
        },
        projected_scores: ProjectedScores {
            player_a_q9: player_a_score_q9,
            player_b_q9: player_b_score_q9,
            status: score_status(terminal, player_a_score_q9, player_b_score_q9),
        },
        result,
        as_of: row.try_get("as_of").map_err(storage_error)?,
    })
}

fn commit_status(committed: bool) -> &'static str {
    if committed {
        "COMMITTED"
    } else {
        "PENDING"
    }
}

fn reveal_status(
    round_state: &str,
    committed: bool,
    has_finalized_facts: bool,
    terminal: bool,
) -> &'static str {
    if has_finalized_facts {
        "REVEALED"
    } else if terminal {
        "NOT_AVAILABLE"
    } else if !committed {
        "NOT_COMMITTED"
    } else if round_state == "REVEAL_OPEN" {
        "PENDING"
    } else {
        "NOT_OPEN"
    }
}

fn score_status(
    terminal: bool,
    player_a_score_q9: Option<i64>,
    player_b_score_q9: Option<i64>,
) -> &'static str {
    if terminal {
        "FINAL"
    } else if player_a_score_q9.is_some() || player_b_score_q9.is_some() {
        "PROJECTED"
    } else {
        "NOT_AVAILABLE"
    }
}

fn is_terminal(state: &str, result: Option<&str>) -> bool {
    matches!(state, "FINALIZED" | "SETTLED" | "VOIDED") || result == Some("VOIDED")
}

fn storage_error(error: sqlx::Error) -> BattleSnapshotError {
    BattleSnapshotError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_does_not_claim_a_reveal_before_finalized_evidence_exists() {
        assert_eq!(reveal_status("REVEAL_OPEN", true, false, false), "PENDING");
        assert_eq!(reveal_status("SETTLED", true, true, true), "REVEALED");
        assert_eq!(reveal_status("SETTLED", true, false, true), "NOT_AVAILABLE");
    }
}
