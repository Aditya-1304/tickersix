//! Ephemeral projected Battle state for the client SSE stream.
//!
//! This data is deliberately replaceable. The stream can show projected
//! scores and lifecycle changes quickly, but it never changes finalized
//! Battle facts or supplies input to on-chain settlement.

use std::{convert::Infallible, fmt, time::Duration};

use axum::response::sse::Event;
use futures_util::stream::{self, Stream};
use serde::Serialize;
use sqlx::{PgPool, Row};

use crate::{
    auth::{self, parse_wallet},
    metrics,
};

const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BattleLiveState {
    pub battle_pubkey: String,
    pub market_round_id: i64,
    pub state: String,
    pub result: Option<String>,
    pub settlement_source_kind: Option<String>,
    pub player_a_score_q9: Option<i64>,
    pub player_b_score_q9: Option<i64>,
    pub as_of: i64,
    pub projection_status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveError {
    InvalidBattle,
    NotFound,
    Storage(String),
}

impl fmt::Display for LiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidBattle => "battle address is not a valid Solana public key",
            Self::NotFound => "Battle was not found",
            Self::Storage(_) => "live Battle state storage operation failed",
        })
    }
}

impl std::error::Error for LiveError {}

pub async fn ensure_battle(pool: &PgPool, battle_pubkey: &str) -> Result<(), LiveError> {
    parse_wallet(battle_pubkey).map_err(|_| LiveError::InvalidBattle)?;
    let exists = sqlx::query(
        "SELECT EXISTS(
             SELECT 1 FROM battles WHERE chain_pubkey = $1
         ) AS exists",
    )
    .bind(battle_pubkey)
    .fetch_one(pool)
    .await
    .map_err(storage_error)?
    .try_get("exists")
    .map_err(storage_error)?;
    if exists {
        Ok(())
    } else {
        Err(LiveError::NotFound)
    }
}

/// Updates only the replaceable projected score row used by SSE consumers.
/// A scoring worker can call this repeatedly while the market is live.
pub async fn upsert_projection(
    pool: &PgPool,
    battle_pubkey: &str,
    player_a_score_q9: Option<i64>,
    player_b_score_q9: Option<i64>,
    as_of: i64,
    updated_at: i64,
) -> Result<(), LiveError> {
    ensure_battle(pool, battle_pubkey).await?;
    sqlx::query(
        "INSERT INTO battle_live_state
            (battle_pubkey, player_a_score_q9, player_b_score_q9, as_of, updated_at)
         SELECT $1, $2, $3, $4, $5
         FROM battles b
         WHERE b.chain_pubkey = $1
           AND b.state NOT IN ('FINALIZED', 'SETTLED', 'VOIDED')
         ON CONFLICT (battle_pubkey) DO UPDATE SET
             player_a_score_q9 = EXCLUDED.player_a_score_q9,
             player_b_score_q9 = EXCLUDED.player_b_score_q9,
             as_of = EXCLUDED.as_of,
             updated_at = EXCLUDED.updated_at
         WHERE EXCLUDED.as_of >= battle_live_state.as_of",
    )
    .bind(battle_pubkey)
    .bind(player_a_score_q9)
    .bind(player_b_score_q9)
    .bind(as_of)
    .bind(updated_at)
    .execute(pool)
    .await
    .map_err(storage_error)?;
    crate::replay::record_projection_event(
        pool,
        battle_pubkey,
        as_of,
        as_of,
        updated_at,
        player_a_score_q9,
        player_b_score_q9,
    )
    .await
    .map_err(|error| LiveError::Storage(error.to_string()))?;
    Ok(())
}

pub fn battle_stream(
    pool: PgPool,
    battle_pubkey: String,
) -> impl Stream<Item = Result<Event, Infallible>> {
    metrics::increment("sse_connected_clients", 1);
    stream::unfold(
        StreamState {
            pool,
            battle_pubkey,
            interval: tokio::time::interval(POLL_INTERVAL),
            last: None,
            finished: false,
        },
        |mut state| async move {
            if state.finished {
                return None;
            }
            loop {
                state.interval.tick().await;
                match load_state(&state.pool, &state.battle_pubkey).await {
                    Ok(Some(snapshot)) if state.last.as_ref() != Some(&snapshot) => {
                        let lag_seconds = auth::unix_now().saturating_sub(snapshot.as_of).max(0);
                        metrics::set("sse_publish_lag_ms", lag_seconds.saturating_mul(1_000));
                        state.finished = is_final(&snapshot);
                        state.last = Some(snapshot.clone());
                        return Some((Ok(snapshot_event(&snapshot)), state));
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => {
                        metrics::increment("sse_unavailable_total", 1);
                        state.finished = true;
                        return Some((Ok(Event::default().event("battle_unavailable")), state));
                    }
                    Err(_) => {
                        metrics::increment("sse_stream_errors_total", 1);
                        state.finished = true;
                        return Some((Ok(Event::default().event("stream_error")), state));
                    }
                }
            }
        },
    )
}

struct StreamState {
    pool: PgPool,
    battle_pubkey: String,
    interval: tokio::time::Interval,
    last: Option<BattleLiveState>,
    finished: bool,
}

impl Drop for StreamState {
    fn drop(&mut self) {
        metrics::decrement("sse_connected_clients", 1);
    }
}

async fn load_state(
    pool: &PgPool,
    battle_pubkey: &str,
) -> Result<Option<BattleLiveState>, LiveError> {
    let row = sqlx::query(
        "SELECT b.chain_pubkey, b.market_round_id, b.state, b.result,
                b.settlement_source_kind,
                l.player_a_score_q9, l.player_b_score_q9,
                COALESCE(l.as_of, b.indexed_at) AS as_of
         FROM battles b
         LEFT JOIN battle_live_state l ON l.battle_pubkey = b.chain_pubkey
         WHERE b.chain_pubkey = $1",
    )
    .bind(battle_pubkey)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?;
    row.map(|row| {
        let mut snapshot = BattleLiveState {
            battle_pubkey: row.try_get("chain_pubkey").map_err(storage_error)?,
            market_round_id: row.try_get("market_round_id").map_err(storage_error)?,
            state: row.try_get("state").map_err(storage_error)?,
            result: row.try_get("result").map_err(storage_error)?,
            settlement_source_kind: row
                .try_get("settlement_source_kind")
                .map_err(storage_error)?,
            player_a_score_q9: row.try_get("player_a_score_q9").map_err(storage_error)?,
            player_b_score_q9: row.try_get("player_b_score_q9").map_err(storage_error)?,
            as_of: row.try_get("as_of").map_err(storage_error)?,
            projection_status: "PROJECTED",
        };
        snapshot.projection_status = projection_status(&snapshot);
        Ok(snapshot)
    })
    .transpose()
}

fn snapshot_event(snapshot: &BattleLiveState) -> Event {
    Event::default()
        .id(snapshot.as_of.to_string())
        .event(event_name(snapshot))
        .data(serde_json::to_string(snapshot).unwrap_or_else(|_| "{}".to_owned()))
}

fn event_name(snapshot: &BattleLiveState) -> &'static str {
    if is_final(snapshot) {
        "battle_finalized"
    } else if snapshot.player_a_score_q9.is_some() || snapshot.player_b_score_q9.is_some() {
        "projected_score"
    } else {
        "battle_state"
    }
}

fn is_final(snapshot: &BattleLiveState) -> bool {
    matches!(snapshot.state.as_str(), "FINALIZED" | "SETTLED" | "VOIDED")
        || matches!(snapshot.result.as_deref(), Some("VOIDED"))
}

fn projection_status(snapshot: &BattleLiveState) -> &'static str {
    if is_final(snapshot) {
        "FINAL"
    } else {
        "PROJECTED"
    }
}

fn storage_error(error: sqlx::Error) -> LiveError {
    LiveError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalized_battle_stream_uses_a_terminal_event() {
        let snapshot = BattleLiveState {
            battle_pubkey: "11111111111111111111111111111111".to_owned(),
            market_round_id: 42,
            state: "FINALIZED".to_owned(),
            result: Some("PLAYER_A".to_owned()),
            settlement_source_kind: Some("JUPITER_TOKEN_SPOT_V1".to_owned()),
            player_a_score_q9: Some(12),
            player_b_score_q9: Some(4),
            as_of: 100,
            projection_status: "FINAL",
        };
        assert!(is_final(&snapshot));
        assert_eq!(event_name(&snapshot), "battle_finalized");
    }

    #[test]
    fn voided_battle_state_is_terminal_even_without_a_result_value() {
        let snapshot = BattleLiveState {
            battle_pubkey: "11111111111111111111111111111111".to_owned(),
            market_round_id: 42,
            state: "VOIDED".to_owned(),
            result: None,
            settlement_source_kind: None,
            player_a_score_q9: None,
            player_b_score_q9: None,
            as_of: 100,
            projection_status: "FINAL",
        };

        assert!(is_final(&snapshot));
        assert_eq!(event_name(&snapshot), "battle_finalized");
    }

    #[test]
    fn projected_event_exposes_source_and_projected_status() {
        let snapshot = BattleLiveState {
            battle_pubkey: "11111111111111111111111111111111".to_owned(),
            market_round_id: 42,
            state: "COMMIT_OPEN".to_owned(),
            result: None,
            settlement_source_kind: Some("PYTH_PRO_VERIFIED_V1".to_owned()),
            player_a_score_q9: Some(12),
            player_b_score_q9: Some(4),
            as_of: 100,
            projection_status: "PROJECTED",
        };

        assert_eq!(projection_status(&snapshot), "PROJECTED");
        assert_eq!(event_name(&snapshot), "projected_score");
        assert_eq!(
            snapshot.settlement_source_kind.as_deref(),
            Some("PYTH_PRO_VERIFIED_V1")
        );
    }
}
