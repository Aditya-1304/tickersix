//! Ordered, idempotent Elo application for finalized rated Battles.
//!
//! Solana remains authoritative for the Battle result and rated exposure. This
//! module consumes the indexed result as an off-chain projection, applies the
//! documented V2 formula in Market Round order, and records the rating event
//! in the same PostgreSQL transaction as the mutable rating row.

use std::{collections::BTreeMap, fmt};

use crate::metrics;
use protocol::{apply_elo_update, EloOutcome, MathError, RATING_FLOOR};
use serde::Serialize;
use sqlx::{postgres::PgRow, PgPool, Postgres, Row, Transaction};

pub const RATING_FORMULA_VERSION: u16 = 1;
pub const PLACEMENT_BATTLES: i32 = 5;
pub const FORFEIT_ELO_PENALTY: i32 = 8;
const RATING_WORKER_LOCK_KEY: i64 = 6_782_413;

const PLAYED_EVENT_KIND: &str = "PLAYED_RATED_BATTLE";
const FORFEIT_EVENT_KIND: &str = "FORFEIT_PENALTY";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RatingError {
    NoPendingBattle,
    InvalidBattle,
    InvalidResult,
    RatingSequenceBlocked,
    SnapshotMismatch,
    ConflictingEvents,
    InvalidState,
    Math(String),
    Storage(String),
}

impl fmt::Display for RatingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NoPendingBattle => "no finalized rated Battle is ready for rating application",
            Self::InvalidBattle => "rated Battle projection is invalid",
            Self::InvalidResult => "rated Battle has an unsupported result",
            Self::RatingSequenceBlocked => "an earlier rated Battle is unresolved",
            Self::SnapshotMismatch => "coordinator rating snapshot does not match canonical state",
            Self::ConflictingEvents => "rating events conflict with the Battle result",
            Self::InvalidState => "rating state is outside the supported range",
            Self::Math(_) => "rating formula failed",
            Self::Storage(_) => "rating storage operation failed",
        })
    }
}

impl std::error::Error for RatingError {}

#[derive(Debug, Clone, Serialize)]
pub struct RatingApplyResult {
    pub battle_pubkey: String,
    pub market_round_id: i64,
    pub round_sequence: i64,
    pub event_kind: String,
    pub events_created: u8,
    pub rating_a_after: i32,
    pub rating_b_after: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RatingResolution {
    Played,
    ForfeitA,
    ForfeitB,
    BothForfeit,
    Void,
    Unsupported,
}

/// Applies the earliest ready rating-affecting Battle and returns `None` when
/// the worker has nothing to do. The global advisory lock makes concurrent
/// worker invocations serialize before they lock and mutate player ratings.
pub async fn apply_next(pool: &PgPool, now: i64) -> Result<Option<RatingApplyResult>, RatingError> {
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(RATING_WORKER_LOCK_KEY)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;

    let candidates = load_finalized_battles(&mut transaction).await?;
    for battle in candidates {
        let resolution =
            classify_result(battle.result.as_deref().ok_or(RatingError::InvalidResult)?);
        if resolution == RatingResolution::Unsupported {
            return Err(RatingError::InvalidResult);
        }

        let event_status = event_status(&mut transaction, &battle.chain_pubkey).await?;
        if resolution == RatingResolution::Void {
            if event_status.count != 0 {
                return Err(RatingError::ConflictingEvents);
            }
            continue;
        }
        if effects_complete(&battle, resolution, &event_status)? {
            continue;
        }
        if event_status.count != 0 {
            return Err(RatingError::ConflictingEvents);
        }
        if !prior_rating_chain_resolved(
            &mut transaction,
            &battle.player_a,
            &battle.player_b,
            battle.round_sequence,
        )
        .await?
        {
            continue;
        }

        let season_id = active_season(&mut transaction).await?;
        let mut states = lock_or_create_ratings(
            &mut transaction,
            season_id,
            &battle.player_a,
            &battle.player_b,
            now,
        )
        .await?;
        validate_coordinator_snapshots(&battle, &states)?;
        let result = match resolution {
            RatingResolution::Played => {
                apply_played(&mut transaction, season_id, &battle, &mut states, now).await
            }
            RatingResolution::ForfeitA => {
                apply_forfeit(&mut transaction, season_id, &battle, &mut states, true, now).await
            }
            RatingResolution::ForfeitB => {
                apply_forfeit(
                    &mut transaction,
                    season_id,
                    &battle,
                    &mut states,
                    false,
                    now,
                )
                .await
            }
            RatingResolution::BothForfeit => {
                apply_both_forfeit(&mut transaction, season_id, &battle, &mut states, now).await
            }
            RatingResolution::Void | RatingResolution::Unsupported => {
                return Err(RatingError::InvalidResult)
            }
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                metrics::increment("rating_event_failures_total", 1);
                return Err(error);
            }
        };
        transaction.commit().await.map_err(storage_error)?;
        if result.event_kind == FORFEIT_EVENT_KIND {
            metrics::increment("forfeit_total", i64::from(result.events_created));
        }
        return Ok(Some(result));
    }

    transaction.commit().await.map_err(storage_error)?;
    Ok(None)
}

/// Maps the chain/indexer result into the mutually exclusive rating classes.
fn classify_result(result: &str) -> RatingResolution {
    match result {
        "PLAYER_A" | "PLAYER_B" | "DRAW" => RatingResolution::Played,
        "FORFEIT_A" => RatingResolution::ForfeitA,
        "FORFEIT_B" => RatingResolution::ForfeitB,
        "BOTH_FORFEIT" => RatingResolution::BothForfeit,
        "VOIDED" => RatingResolution::Void,
        _ => RatingResolution::Unsupported,
    }
}

fn played_outcomes(result: &str) -> Option<(EloOutcome, EloOutcome)> {
    match result {
        "PLAYER_A" => Some((EloOutcome::Win, EloOutcome::Loss)),
        "PLAYER_B" => Some((EloOutcome::Loss, EloOutcome::Win)),
        "DRAW" => Some((EloOutcome::Draw, EloOutcome::Draw)),
        _ => None,
    }
}

async fn load_finalized_battles(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<Vec<PendingBattle>, RatingError> {
    let rows = sqlx::query(
        "SELECT b.chain_pubkey, b.market_round_id, r.round_sequence,
                b.player_a, b.player_b, b.state, b.result,
                b.rating_a_before, b.rating_b_before, b.rating_formula_version,
                p.rating_a_snapshot, p.rating_b_snapshot
         FROM battles b
         JOIN market_rounds r ON r.id = b.market_round_id
         LEFT JOIN ranked_pairings p ON p.battle_pubkey = b.chain_pubkey
         WHERE b.rated
           AND NOT r.is_replay
           AND b.state IN ('FINALIZED', 'SETTLED')
           AND b.result IS NOT NULL
           AND b.result <> 'PENDING'
         ORDER BY r.round_sequence ASC, b.chain_pubkey ASC
         FOR UPDATE OF b",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    rows.into_iter().map(pending_battle_from_row).collect()
}

#[derive(Debug, Clone)]
struct PendingBattle {
    chain_pubkey: String,
    market_round_id: i64,
    round_sequence: i64,
    player_a: String,
    player_b: String,
    result: Option<String>,
    rating_a_before: Option<i32>,
    rating_b_before: Option<i32>,
    rating_formula_version: Option<i32>,
    pairing_rating_a: Option<i32>,
    pairing_rating_b: Option<i32>,
}

fn pending_battle_from_row(row: PgRow) -> Result<PendingBattle, RatingError> {
    Ok(PendingBattle {
        chain_pubkey: row.try_get("chain_pubkey").map_err(storage_error)?,
        market_round_id: row.try_get("market_round_id").map_err(storage_error)?,
        round_sequence: row.try_get("round_sequence").map_err(storage_error)?,
        player_a: row.try_get("player_a").map_err(storage_error)?,
        player_b: row.try_get("player_b").map_err(storage_error)?,
        result: row.try_get("result").map_err(storage_error)?,
        rating_a_before: row.try_get("rating_a_before").map_err(storage_error)?,
        rating_b_before: row.try_get("rating_b_before").map_err(storage_error)?,
        rating_formula_version: row
            .try_get("rating_formula_version")
            .map_err(storage_error)?,
        pairing_rating_a: row.try_get("rating_a_snapshot").map_err(storage_error)?,
        pairing_rating_b: row.try_get("rating_b_snapshot").map_err(storage_error)?,
    })
}

#[derive(Debug, Default)]
struct EventStatus {
    count: usize,
    by_wallet: BTreeMap<String, String>,
}

async fn event_status(
    transaction: &mut Transaction<'_, Postgres>,
    battle_pubkey: &str,
) -> Result<EventStatus, RatingError> {
    let rows = sqlx::query(
        "SELECT wallet, event_kind FROM rating_events
         WHERE battle_pubkey = $1",
    )
    .bind(battle_pubkey)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    let mut status = EventStatus {
        count: rows.len(),
        ..EventStatus::default()
    };
    for row in rows {
        status.by_wallet.insert(
            row.try_get("wallet").map_err(storage_error)?,
            row.try_get("event_kind").map_err(storage_error)?,
        );
    }
    Ok(status)
}

fn effects_complete(
    battle: &PendingBattle,
    resolution: RatingResolution,
    events: &EventStatus,
) -> Result<bool, RatingError> {
    let expected = match resolution {
        RatingResolution::Played => vec![
            (&battle.player_a, PLAYED_EVENT_KIND),
            (&battle.player_b, PLAYED_EVENT_KIND),
        ],
        RatingResolution::ForfeitA => vec![(&battle.player_a, FORFEIT_EVENT_KIND)],
        RatingResolution::ForfeitB => vec![(&battle.player_b, FORFEIT_EVENT_KIND)],
        RatingResolution::BothForfeit => vec![
            (&battle.player_a, FORFEIT_EVENT_KIND),
            (&battle.player_b, FORFEIT_EVENT_KIND),
        ],
        RatingResolution::Void | RatingResolution::Unsupported => return Ok(false),
    };
    if events.count != expected.len() {
        return Ok(false);
    }
    Ok(expected.iter().all(|(wallet, kind)| {
        events
            .by_wallet
            .get(*wallet)
            .is_some_and(|value| value == kind)
    }))
}

async fn prior_rating_chain_resolved(
    transaction: &mut Transaction<'_, Postgres>,
    player_a: &str,
    player_b: &str,
    round_sequence: i64,
) -> Result<bool, RatingError> {
    let rows = sqlx::query(
        "SELECT b.chain_pubkey, b.state, b.result, b.player_a, b.player_b
         FROM battles b
         JOIN market_rounds r ON r.id = b.market_round_id
         WHERE b.rated
           AND NOT r.is_replay
           AND (b.player_a IN ($1, $2) OR b.player_b IN ($1, $2))
           AND r.round_sequence < $3
         ORDER BY r.round_sequence ASC, b.chain_pubkey ASC",
    )
    .bind(player_a)
    .bind(player_b)
    .bind(round_sequence)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;

    for row in rows {
        let state: String = row.try_get("state").map_err(storage_error)?;
        let result: Option<String> = row.try_get("result").map_err(storage_error)?;
        if !matches!(state.as_str(), "FINALIZED" | "SETTLED") {
            return Ok(false);
        }
        let Some(result) = result else {
            return Ok(false);
        };
        let resolution = classify_result(&result);
        if resolution == RatingResolution::Unsupported {
            return Err(RatingError::InvalidResult);
        }
        if resolution == RatingResolution::Void {
            continue;
        }
        let battle = PendingBattle {
            chain_pubkey: row.try_get("chain_pubkey").map_err(storage_error)?,
            market_round_id: 0,
            round_sequence: 0,
            player_a: row.try_get("player_a").map_err(storage_error)?,
            player_b: row.try_get("player_b").map_err(storage_error)?,
            result: Some(result),
            rating_a_before: None,
            rating_b_before: None,
            rating_formula_version: None,
            pairing_rating_a: None,
            pairing_rating_b: None,
        };
        let events = event_status(&mut *transaction, &battle.chain_pubkey).await?;
        if !effects_complete(&battle, resolution, &events)? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Debug, Clone, Copy)]
struct RatingState {
    rating: i32,
    rated_games: i32,
}

async fn active_season(transaction: &mut Transaction<'_, Postgres>) -> Result<i64, RatingError> {
    let row = sqlx::query("SELECT id FROM seasons WHERE status = 'ACTIVE'")
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage_error)?
        .ok_or(RatingError::InvalidState)?;
    row.try_get("id").map_err(storage_error)
}

async fn lock_or_create_ratings(
    transaction: &mut Transaction<'_, Postgres>,
    season_id: i64,
    player_a: &str,
    player_b: &str,
    now: i64,
) -> Result<BTreeMap<String, RatingState>, RatingError> {
    for wallet in [player_a, player_b] {
        sqlx::query(
            "INSERT INTO ratings (season_id, wallet, updated_at)
             VALUES ($1, $2, $3)
             ON CONFLICT (season_id, wallet) DO NOTHING",
        )
        .bind(season_id)
        .bind(wallet)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
    }
    let rows = sqlx::query(
        "SELECT wallet, rating, rated_games FROM ratings
         WHERE season_id = $1 AND wallet IN ($2, $3)
         ORDER BY wallet ASC
         FOR UPDATE",
    )
    .bind(season_id)
    .bind(player_a)
    .bind(player_b)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    let mut states = BTreeMap::new();
    for row in rows {
        states.insert(
            row.try_get("wallet").map_err(storage_error)?,
            RatingState {
                rating: row.try_get("rating").map_err(storage_error)?,
                rated_games: row.try_get("rated_games").map_err(storage_error)?,
            },
        );
    }
    if states.len() != 2 {
        return Err(RatingError::InvalidState);
    }
    Ok(states)
}

fn validate_coordinator_snapshots(
    battle: &PendingBattle,
    states: &BTreeMap<String, RatingState>,
) -> Result<(), RatingError> {
    if battle.player_a == battle.player_b {
        return Err(RatingError::InvalidBattle);
    }
    let state_a = states
        .get(&battle.player_a)
        .ok_or(RatingError::InvalidState)?;
    let state_b = states
        .get(&battle.player_b)
        .ok_or(RatingError::InvalidState)?;
    for snapshot in [battle.rating_a_before, battle.pairing_rating_a] {
        if snapshot.is_some_and(|value| value != state_a.rating) {
            return Err(RatingError::SnapshotMismatch);
        }
    }
    for snapshot in [battle.rating_b_before, battle.pairing_rating_b] {
        if snapshot.is_some_and(|value| value != state_b.rating) {
            return Err(RatingError::SnapshotMismatch);
        }
    }
    if battle
        .rating_formula_version
        .is_some_and(|value| value != i32::from(RATING_FORMULA_VERSION))
    {
        return Err(RatingError::SnapshotMismatch);
    }
    Ok(())
}

async fn apply_played(
    transaction: &mut Transaction<'_, Postgres>,
    season_id: i64,
    battle: &PendingBattle,
    states: &mut BTreeMap<String, RatingState>,
    now: i64,
) -> Result<RatingApplyResult, RatingError> {
    let result = battle.result.as_deref().ok_or(RatingError::InvalidResult)?;
    let (outcome_a, outcome_b) = played_outcomes(result).ok_or(RatingError::InvalidResult)?;
    let state_a = *states
        .get(&battle.player_a)
        .ok_or(RatingError::InvalidState)?;
    let state_b = *states
        .get(&battle.player_b)
        .ok_or(RatingError::InvalidState)?;
    let update_a = elo_update(state_a, state_b.rating, outcome_a)?;
    let update_b = elo_update(state_b, state_a.rating, outcome_b)?;

    insert_rating_event(
        transaction,
        RatingEventInput {
            season_id,
            battle,
            wallet: &battle.player_a,
            opponent: &battle.player_b,
            event_kind: PLAYED_EVENT_KIND,
            update: &update_a,
            now,
        },
    )
    .await?;
    insert_rating_event(
        transaction,
        RatingEventInput {
            season_id,
            battle,
            wallet: &battle.player_b,
            opponent: &battle.player_a,
            event_kind: PLAYED_EVENT_KIND,
            update: &update_b,
            now,
        },
    )
    .await?;
    update_played_rating(
        transaction,
        season_id,
        &battle.player_a,
        &update_a,
        outcome_a,
        now,
    )
    .await?;
    update_played_rating(
        transaction,
        season_id,
        &battle.player_b,
        &update_b,
        outcome_b,
        now,
    )
    .await?;

    Ok(RatingApplyResult {
        battle_pubkey: battle.chain_pubkey.clone(),
        market_round_id: battle.market_round_id,
        round_sequence: battle.round_sequence,
        event_kind: PLAYED_EVENT_KIND.to_owned(),
        events_created: 2,
        rating_a_after: update_a.rating_after,
        rating_b_after: Some(update_b.rating_after),
    })
}

async fn apply_forfeit(
    transaction: &mut Transaction<'_, Postgres>,
    season_id: i64,
    battle: &PendingBattle,
    states: &mut BTreeMap<String, RatingState>,
    player_a_forfeited: bool,
    now: i64,
) -> Result<RatingApplyResult, RatingError> {
    let forfeiter = if player_a_forfeited {
        &battle.player_a
    } else {
        &battle.player_b
    };
    let opponent = if player_a_forfeited {
        &battle.player_b
    } else {
        &battle.player_a
    };
    let state_a = *states
        .get(&battle.player_a)
        .ok_or(RatingError::InvalidState)?;
    let state_b = *states
        .get(&battle.player_b)
        .ok_or(RatingError::InvalidState)?;
    let state = *states.get(forfeiter).ok_or(RatingError::InvalidState)?;
    let opponent_state = *states.get(opponent).ok_or(RatingError::InvalidState)?;
    let rating_after = forfeit_rating_after(state.rating)?;
    let (rating_a_after, rating_b_after) = forfeit_result_ratings(
        player_a_forfeited,
        rating_after,
        state_a.rating,
        state_b.rating,
    );
    let update = ForfeitUpdate {
        rating_before: state.rating,
        opponent_rating: opponent_state.rating,
        rating_after,
        delta: rating_after - state.rating,
    };
    insert_forfeit_event(
        transaction,
        season_id,
        battle,
        forfeiter,
        opponent,
        &update,
        now,
    )
    .await?;
    update_forfeit_rating(transaction, season_id, forfeiter, rating_after, now).await?;

    Ok(RatingApplyResult {
        battle_pubkey: battle.chain_pubkey.clone(),
        market_round_id: battle.market_round_id,
        round_sequence: battle.round_sequence,
        event_kind: FORFEIT_EVENT_KIND.to_owned(),
        events_created: 1,
        rating_a_after,
        rating_b_after: Some(rating_b_after),
    })
}

async fn apply_both_forfeit(
    transaction: &mut Transaction<'_, Postgres>,
    season_id: i64,
    battle: &PendingBattle,
    states: &mut BTreeMap<String, RatingState>,
    now: i64,
) -> Result<RatingApplyResult, RatingError> {
    let state_a = *states
        .get(&battle.player_a)
        .ok_or(RatingError::InvalidState)?;
    let state_b = *states
        .get(&battle.player_b)
        .ok_or(RatingError::InvalidState)?;
    let after_a = forfeit_rating_after(state_a.rating)?;
    let after_b = forfeit_rating_after(state_b.rating)?;
    insert_forfeit_event(
        transaction,
        season_id,
        battle,
        &battle.player_a,
        &battle.player_b,
        &ForfeitUpdate {
            rating_before: state_a.rating,
            opponent_rating: state_b.rating,
            rating_after: after_a,
            delta: after_a - state_a.rating,
        },
        now,
    )
    .await?;
    insert_forfeit_event(
        transaction,
        season_id,
        battle,
        &battle.player_b,
        &battle.player_a,
        &ForfeitUpdate {
            rating_before: state_b.rating,
            opponent_rating: state_a.rating,
            rating_after: after_b,
            delta: after_b - state_b.rating,
        },
        now,
    )
    .await?;
    update_forfeit_rating(transaction, season_id, &battle.player_a, after_a, now).await?;
    update_forfeit_rating(transaction, season_id, &battle.player_b, after_b, now).await?;

    Ok(RatingApplyResult {
        battle_pubkey: battle.chain_pubkey.clone(),
        market_round_id: battle.market_round_id,
        round_sequence: battle.round_sequence,
        event_kind: FORFEIT_EVENT_KIND.to_owned(),
        events_created: 2,
        rating_a_after: after_a,
        rating_b_after: Some(after_b),
    })
}

fn forfeit_rating_after(rating_before: i32) -> Result<i32, RatingError> {
    let after = i64::from(rating_before)
        .checked_sub(i64::from(FORFEIT_ELO_PENALTY))
        .ok_or(RatingError::InvalidState)?
        .max(i64::from(RATING_FLOOR));
    i32::try_from(after).map_err(|_| RatingError::InvalidState)
}

#[derive(Debug, Clone, Copy)]
struct ForfeitUpdate {
    rating_before: i32,
    opponent_rating: i32,
    rating_after: i32,
    delta: i32,
}

fn elo_update(
    state: RatingState,
    opponent_rating: i32,
    outcome: EloOutcome,
) -> Result<protocol::EloUpdate, RatingError> {
    let completed_games =
        u32::try_from(state.rated_games).map_err(|_| RatingError::InvalidState)?;
    apply_elo_update(state.rating, opponent_rating, outcome, completed_games).map_err(math_error)
}

fn forfeit_result_ratings(
    player_a_forfeited: bool,
    forfeiter_rating_after: i32,
    rating_a_before: i32,
    rating_b_before: i32,
) -> (i32, i32) {
    if player_a_forfeited {
        (forfeiter_rating_after, rating_b_before)
    } else {
        (rating_a_before, forfeiter_rating_after)
    }
}

struct RatingEventInput<'a> {
    season_id: i64,
    battle: &'a PendingBattle,
    wallet: &'a str,
    opponent: &'a str,
    event_kind: &'a str,
    update: &'a protocol::EloUpdate,
    now: i64,
}

async fn insert_rating_event(
    transaction: &mut Transaction<'_, Postgres>,
    input: RatingEventInput<'_>,
) -> Result<(), RatingError> {
    sqlx::query(
        "INSERT INTO rating_events
            (season_id, market_round_id, round_sequence, battle_pubkey, wallet,
             opponent, event_kind, rating_before, opponent_rating_snapshot,
             expected_score, actual_score, k_factor, delta, rating_after,
             formula_version, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
    )
    .bind(input.season_id)
    .bind(input.battle.market_round_id)
    .bind(input.battle.round_sequence)
    .bind(&input.battle.chain_pubkey)
    .bind(input.wallet)
    .bind(input.opponent)
    .bind(input.event_kind)
    .bind(input.update.rating_before)
    .bind(input.update.opponent_rating)
    .bind(input.update.expected_score)
    .bind(input.update.actual_score)
    .bind(input.update.k_factor)
    .bind(input.update.delta)
    .bind(input.update.rating_after)
    .bind(i32::from(RATING_FORMULA_VERSION))
    .bind(input.now)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

async fn insert_forfeit_event(
    transaction: &mut Transaction<'_, Postgres>,
    season_id: i64,
    battle: &PendingBattle,
    wallet: &str,
    opponent: &str,
    update: &ForfeitUpdate,
    now: i64,
) -> Result<(), RatingError> {
    sqlx::query(
        "INSERT INTO rating_events
            (season_id, market_round_id, round_sequence, battle_pubkey, wallet,
             opponent, event_kind, rating_before, opponent_rating_snapshot,
             delta, rating_after, formula_version, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(season_id)
    .bind(battle.market_round_id)
    .bind(battle.round_sequence)
    .bind(&battle.chain_pubkey)
    .bind(wallet)
    .bind(opponent)
    .bind(FORFEIT_EVENT_KIND)
    .bind(update.rating_before)
    .bind(update.opponent_rating)
    .bind(update.delta)
    .bind(update.rating_after)
    .bind(i32::from(RATING_FORMULA_VERSION))
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

async fn update_played_rating(
    transaction: &mut Transaction<'_, Postgres>,
    season_id: i64,
    wallet: &str,
    update: &protocol::EloUpdate,
    outcome: EloOutcome,
    now: i64,
) -> Result<(), RatingError> {
    let (wins, draws, losses) = match outcome {
        EloOutcome::Win => (1, 0, 0),
        EloOutcome::Draw => (0, 1, 0),
        EloOutcome::Loss => (0, 0, 1),
    };
    sqlx::query(
        "UPDATE ratings
         SET rating = $1,
             rated_games = rated_games + 1,
             peak_rating = GREATEST(peak_rating, $1),
             wins = wins + $2,
             draws = draws + $3,
             losses = losses + $4,
             updated_at = $5
         WHERE season_id = $6 AND wallet = $7",
    )
    .bind(update.rating_after)
    .bind(wins)
    .bind(draws)
    .bind(losses)
    .bind(now)
    .bind(season_id)
    .bind(wallet)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

async fn update_forfeit_rating(
    transaction: &mut Transaction<'_, Postgres>,
    season_id: i64,
    wallet: &str,
    rating_after: i32,
    now: i64,
) -> Result<(), RatingError> {
    sqlx::query(
        "UPDATE ratings
         SET rating = $1,
             peak_rating = GREATEST(peak_rating, $1),
             updated_at = $2
         WHERE season_id = $3 AND wallet = $4",
    )
    .bind(rating_after)
    .bind(now)
    .bind(season_id)
    .bind(wallet)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

/// Checks the mutable rating projection against the append-only rating event
/// ledger. The ledger is the recovery source; this read-only pass only records
/// drift and never repairs a row by guessing over chain history.
pub async fn reconcile_projection(pool: &PgPool) -> Result<u64, RatingError> {
    let rows = sqlx::query(
        "SELECT r.rating, r.rated_games,
                COALESCE(last_event.rating_after, 1500) AS expected_rating,
                COALESCE(played_events.played_games, 0) AS expected_games
         FROM ratings r
         LEFT JOIN LATERAL (
             SELECT rating_after
             FROM rating_events e
             WHERE e.season_id = r.season_id AND e.wallet = r.wallet
             ORDER BY e.round_sequence DESC, e.id DESC
             LIMIT 1
         ) last_event ON TRUE
         LEFT JOIN LATERAL (
             SELECT COUNT(*) AS played_games
             FROM rating_events e
             WHERE e.season_id = r.season_id
               AND e.wallet = r.wallet
               AND e.event_kind = $1
         ) played_events ON TRUE
         WHERE r.season_id = (SELECT id FROM seasons WHERE status = 'ACTIVE' LIMIT 1)",
    )
    .bind(PLAYED_EVENT_KIND)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?;
    let mut mismatches = 0;
    for row in rows {
        let actual_rating: i32 = row.try_get("rating").map_err(storage_error)?;
        let actual_games: i32 = row.try_get("rated_games").map_err(storage_error)?;
        let expected_rating: i32 = row.try_get("expected_rating").map_err(storage_error)?;
        let expected_games: i64 = row.try_get("expected_games").map_err(storage_error)?;
        if !rating_projection_matches(actual_rating, actual_games, expected_rating, expected_games)
        {
            mismatches += 1;
            metrics::increment("rating_reconciliation_mismatch_total", 1);
        }
    }
    Ok(mismatches)
}

fn rating_projection_matches(
    actual_rating: i32,
    actual_games: i32,
    expected_rating: i32,
    expected_games: i64,
) -> bool {
    actual_rating == expected_rating && i64::from(actual_games) == expected_games
}

pub fn rating_tier(rating: i32, rated_games: i32) -> &'static str {
    if rated_games < PLACEMENT_BATTLES {
        "UNRANKED"
    } else if rating < 1400 {
        "BRONZE"
    } else if rating < 1550 {
        "SILVER"
    } else if rating < 1700 {
        "GOLD"
    } else if rating < 1850 {
        "PLATINUM"
    } else if rating < 2000 {
        "DIAMOND"
    } else if rating < 2200 {
        "MASTER"
    } else {
        "GRANDMASTER"
    }
}

fn math_error(error: MathError) -> RatingError {
    RatingError::Math(error.to_string())
}

fn storage_error(error: sqlx::Error) -> RatingError {
    RatingError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battle_result_classification_is_mutually_exclusive() {
        assert_eq!(classify_result("PLAYER_A"), RatingResolution::Played);
        assert_eq!(classify_result("FORFEIT_A"), RatingResolution::ForfeitA);
        assert_eq!(classify_result("VOIDED"), RatingResolution::Void);
        assert_eq!(classify_result("PENDING"), RatingResolution::Unsupported);
    }

    #[test]
    fn forfeit_result_summary_preserves_the_non_forfeiter_rating() {
        assert_eq!(forfeit_result_ratings(true, 1492, 1500, 1600), (1492, 1600));
        assert_eq!(
            forfeit_result_ratings(false, 1492, 1500, 1600),
            (1500, 1492)
        );
    }

    #[test]
    fn forfeits_do_not_enter_played_rating_tiers_or_apply_an_opponent_win() {
        assert_eq!(rating_tier(protocol::INITIAL_RATING, 4), "UNRANKED");
        assert_eq!(rating_tier(protocol::INITIAL_RATING, 5), "SILVER");
        assert_eq!(forfeit_rating_after(1500).unwrap(), 1492);
        assert_eq!(forfeit_rating_after(100).unwrap(), 100);
    }

    #[test]
    fn rating_reconciliation_detects_projection_drift_without_repairing_it() {
        assert!(rating_projection_matches(1517, 1, 1517, 1));
        assert!(!rating_projection_matches(1500, 1, 1517, 1));
        assert!(!rating_projection_matches(1517, 0, 1517, 1));
    }
}
