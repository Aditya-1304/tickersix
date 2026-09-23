//! Read-only finalized Battle replay materialization.
//!
//! Replay data is built only from the retained projection timeline and the
//! indexed Battle source metadata. It is deliberately marked non-competitive
//! so a consumer cannot accidentally feed historical events into rating or
//! achievement flows.

use std::fmt;

use serde::Serialize;
use sqlx::{PgPool, Row};

use crate::{
    auth::parse_wallet,
    proof::{JUPITER_SETTLEMENT_TRUST_LABEL, PYTH_SETTLEMENT_TRUST_LABEL},
    ranked::{JUPITER_SOURCE_KIND, PYTH_LEGACY_SOURCE_KIND, PYTH_SOURCE_KIND},
};

pub const REPLAY_LABEL: &str = "REPLAY - FINALIZED HISTORICAL DEVNET ROUND";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplayTimeline {
    pub battle_pubkey: String,
    pub market_round_id: i64,
    pub competition_domain: String,
    pub originally_rated: bool,
    pub replay: bool,
    pub replay_label: &'static str,
    pub competitive_effects: bool,
    pub rating_updates: bool,
    pub achievement_updates: bool,
    pub settlement_source_kind: Option<String>,
    pub settlement_label: Option<&'static str>,
    pub proof_path: String,
    pub events: Vec<ReplayEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplayEvent {
    pub event_id: i64,
    pub state: String,
    pub result: Option<String>,
    pub settlement_source_kind: Option<String>,
    pub projection_status: &'static str,
    pub source_label: Option<&'static str>,
    pub player_a_score_q9: Option<i64>,
    pub player_b_score_q9: Option<i64>,
    pub as_of: i64,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    InvalidBattle,
    NotFound,
    Unavailable,
    Storage(String),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidBattle => "battle address is not a valid Solana public key",
            Self::NotFound => "Battle replay was not found",
            Self::Unavailable => "Battle replay timeline is not available",
            Self::Storage(_) => "Battle replay storage operation failed",
        })
    }
}

impl std::error::Error for ReplayError {}

/// Builds the immutable consumer contract used by both the HTTP API and the
/// static/mobile client. Replay is never an alternate rating input.
pub fn build_timeline(
    battle_pubkey: String,
    market_round_id: i64,
    competition_domain: String,
    originally_rated: bool,
    settlement_source_kind: Option<String>,
    events: Vec<ReplayEvent>,
) -> ReplayTimeline {
    let settlement_label = settlement_source_kind
        .as_deref()
        .and_then(final_settlement_label);
    ReplayTimeline {
        proof_path: format!("/v1/battles/{battle_pubkey}/proof"),
        battle_pubkey,
        market_round_id,
        competition_domain,
        originally_rated,
        replay: true,
        replay_label: REPLAY_LABEL,
        competitive_effects: false,
        rating_updates: false,
        achievement_updates: false,
        settlement_source_kind,
        settlement_label,
        events,
    }
}

pub fn source_label(projection_status: &str, source_kind: Option<&str>) -> Option<&'static str> {
    if projection_status == "FINAL" {
        return source_kind.and_then(final_settlement_label);
    }
    source_kind.and_then(projected_source_label)
}

fn projected_source_label(source_kind: &str) -> Option<&'static str> {
    match source_kind {
        JUPITER_SOURCE_KIND => Some("PROJECTED · LIVE TOKEN MARKET · JUPITER"),
        PYTH_SOURCE_KIND | PYTH_LEGACY_SOURCE_KIND => {
            Some("PROJECTED · LIVE MARKET DATA · PYTH PRO")
        }
        _ => None,
    }
}

fn final_settlement_label(source_kind: &str) -> Option<&'static str> {
    match source_kind {
        JUPITER_SOURCE_KIND => Some(JUPITER_SETTLEMENT_TRUST_LABEL),
        PYTH_SOURCE_KIND | PYTH_LEGACY_SOURCE_KIND => Some(PYTH_SETTLEMENT_TRUST_LABEL),
        _ => None,
    }
}

pub async fn get_replay(pool: &PgPool, battle_pubkey: &str) -> Result<ReplayTimeline, ReplayError> {
    parse_wallet(battle_pubkey).map_err(|_| ReplayError::InvalidBattle)?;
    let battle = sqlx::query(
        "SELECT b.chain_pubkey, b.market_round_id, b.rated,
                b.settlement_source_kind, mr.competition_domain
         FROM battles b
         JOIN market_rounds mr ON mr.id = b.market_round_id
         WHERE b.chain_pubkey = $1",
    )
    .bind(battle_pubkey)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(ReplayError::NotFound)?;

    let events = sqlx::query(
        "SELECT event_id, state, result, settlement_source_kind,
                projection_status, player_a_score_q9, player_b_score_q9,
                as_of, recorded_at
         FROM battle_projection_timeline
         WHERE battle_pubkey = $1
         ORDER BY event_id ASC, recorded_at ASC",
    )
    .bind(battle_pubkey)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?
    .into_iter()
    .map(|row| {
        let projection_status: String = row.try_get("projection_status").map_err(storage_error)?;
        let source_kind: Option<String> = row
            .try_get("settlement_source_kind")
            .map_err(storage_error)?;
        Ok(ReplayEvent {
            event_id: row.try_get("event_id").map_err(storage_error)?,
            state: row.try_get("state").map_err(storage_error)?,
            result: row.try_get("result").map_err(storage_error)?,
            source_label: source_label(projection_status.as_str(), source_kind.as_deref()),
            settlement_source_kind: source_kind,
            projection_status: if projection_status == "FINAL" {
                "FINAL"
            } else {
                "PROJECTED"
            },
            player_a_score_q9: row.try_get("player_a_score_q9").map_err(storage_error)?,
            player_b_score_q9: row.try_get("player_b_score_q9").map_err(storage_error)?,
            as_of: row.try_get("as_of").map_err(storage_error)?,
            recorded_at: row.try_get("recorded_at").map_err(storage_error)?,
        })
    })
    .collect::<Result<Vec<_>, ReplayError>>()?;

    if events.is_empty() {
        return Err(ReplayError::Unavailable);
    }

    let settlement_source_kind: Option<String> = battle
        .try_get("settlement_source_kind")
        .map_err(storage_error)?;
    Ok(build_timeline(
        battle.try_get("chain_pubkey").map_err(storage_error)?,
        battle.try_get("market_round_id").map_err(storage_error)?,
        battle
            .try_get("competition_domain")
            .map_err(storage_error)?,
        battle.try_get("rated").map_err(storage_error)?,
        settlement_source_kind,
        events,
    ))
}

/// Retains one projected event using the already-indexed Battle source. It
/// never fetches external market data and therefore cannot rewrite history.
pub async fn record_projection_event(
    pool: &PgPool,
    battle_pubkey: &str,
    event_id: i64,
    as_of: i64,
    recorded_at: i64,
    player_a_score_q9: Option<i64>,
    player_b_score_q9: Option<i64>,
) -> Result<(), ReplayError> {
    record_event(
        pool,
        battle_pubkey,
        event_id,
        "PROJECTED",
        as_of,
        recorded_at,
        player_a_score_q9,
        player_b_score_q9,
    )
    .await
}

pub async fn record_final_event(
    pool: &PgPool,
    battle_pubkey: &str,
    event_id: i64,
    recorded_at: i64,
    player_a_score_q9: Option<i64>,
    player_b_score_q9: Option<i64>,
) -> Result<(), ReplayError> {
    record_event(
        pool,
        battle_pubkey,
        event_id,
        "FINAL",
        event_id,
        recorded_at,
        player_a_score_q9,
        player_b_score_q9,
    )
    .await
}

async fn record_event(
    pool: &PgPool,
    battle_pubkey: &str,
    event_id: i64,
    projection_status: &str,
    as_of: i64,
    recorded_at: i64,
    player_a_score_q9: Option<i64>,
    player_b_score_q9: Option<i64>,
) -> Result<(), ReplayError> {
    let result = sqlx::query(
        "INSERT INTO battle_projection_timeline
            (battle_pubkey, event_id, state, result, settlement_source_kind,
             projection_status, player_a_score_q9, player_b_score_q9,
             as_of, recorded_at)
         SELECT b.chain_pubkey, $2, b.state, b.result, b.settlement_source_kind,
                $3, $4, $5, $6, $7
         FROM battles b
         WHERE b.chain_pubkey = $1
         ON CONFLICT (battle_pubkey, event_id) DO UPDATE SET
             state = EXCLUDED.state,
             result = EXCLUDED.result,
             settlement_source_kind = EXCLUDED.settlement_source_kind,
             projection_status = EXCLUDED.projection_status,
             player_a_score_q9 = EXCLUDED.player_a_score_q9,
             player_b_score_q9 = EXCLUDED.player_b_score_q9,
             as_of = EXCLUDED.as_of,
             recorded_at = EXCLUDED.recorded_at
         WHERE battle_projection_timeline.projection_status <> 'FINAL'
            OR EXCLUDED.projection_status = 'FINAL'",
    )
    .bind(battle_pubkey)
    .bind(event_id)
    .bind(projection_status)
    .bind(player_a_score_q9)
    .bind(player_b_score_q9)
    .bind(as_of)
    .bind(recorded_at)
    .execute(pool)
    .await
    .map_err(storage_error)?;
    if result.rows_affected() == 0 {
        return Err(ReplayError::NotFound);
    }
    Ok(())
}

fn storage_error(error: sqlx::Error) -> ReplayError {
    ReplayError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ranked::{JUPITER_QUEUE_TRUST_LABEL, PYTH_QUEUE_TRUST_LABEL};

    #[test]
    fn replay_contract_disables_all_competitive_side_effects() {
        let timeline = build_timeline(
            "11111111111111111111111111111111".to_owned(),
            7,
            "PUBLIC_EQUITY".to_owned(),
            true,
            Some(JUPITER_SOURCE_KIND.to_owned()),
            Vec::new(),
        );

        assert!(timeline.replay);
        assert!(!timeline.competitive_effects);
        assert!(!timeline.rating_updates);
        assert!(!timeline.achievement_updates);
        assert_eq!(timeline.replay_label, REPLAY_LABEL);
    }

    #[test]
    fn replay_keeps_projected_and_final_source_labels_distinct() {
        assert_eq!(
            source_label("PROJECTED", Some(JUPITER_SOURCE_KIND)),
            Some("PROJECTED · LIVE TOKEN MARKET · JUPITER")
        );
        assert_eq!(
            source_label("FINAL", Some(JUPITER_SOURCE_KIND)),
            Some(JUPITER_SETTLEMENT_TRUST_LABEL)
        );
        assert_eq!(
            source_label("FINAL", Some(PYTH_SOURCE_KIND)),
            Some(PYTH_SETTLEMENT_TRUST_LABEL)
        );
        assert_eq!(source_label("PROJECTED", Some("UNKNOWN")), None);
    }

    #[test]
    fn replay_source_label_constants_match_ranked_queue_labels() {
        assert_eq!(JUPITER_QUEUE_TRUST_LABEL, "JUPITER ATTESTED");
        assert_eq!(PYTH_QUEUE_TRUST_LABEL, "PYTH VERIFIED");
    }
}
