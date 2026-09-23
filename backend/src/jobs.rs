//! Repeatable local scheduler for the backend workers.
//!
//! Every operation in this module is one-shot and idempotent. The process loop
//! in `main` only provides timing; PostgreSQL advisory locks and the existing
//! unique constraints provide correctness when two local worker processes
//! overlap.

use std::{error::Error, fmt};

use serde::Serialize;
use sqlx::{PgPool, Row};

use crate::{league, metrics, ranked, rating};

const MAX_DUE_RANKED_ROUNDS: i64 = 32;
const MAX_RATING_APPLIES_PER_TICK: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchedulerTick {
    pub expired_memberships: u64,
    pub matched_rounds: Vec<i64>,
    pub rating_batches_applied: usize,
}

#[derive(Debug)]
pub enum JobError {
    League(league::LeagueError),
    Ranked(ranked::RankedError),
    Rating(rating::RatingError),
    Storage(String),
}

impl fmt::Display for JobError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::League(error) => write!(formatter, "League job failed: {error}"),
            Self::Ranked(error) => write!(formatter, "Ranked job failed: {error}"),
            Self::Rating(error) => write!(formatter, "rating job failed: {error}"),
            Self::Storage(error) => write!(formatter, "scheduler storage query failed: {error}"),
        }
    }
}

impl Error for JobError {}

impl From<league::LeagueError> for JobError {
    fn from(error: league::LeagueError) -> Self {
        Self::League(error)
    }
}

impl From<ranked::RankedError> for JobError {
    fn from(error: ranked::RankedError) -> Self {
        Self::Ranked(error)
    }
}

impl From<rating::RatingError> for JobError {
    fn from(error: rating::RatingError) -> Self {
        Self::Rating(error)
    }
}

/// Executes one bounded scheduler tick.
///
/// League pairing itself still requires finalized chain entropy and is invoked
/// by the chain-aware coordinator path. This tick handles the safe local
/// lifecycle work: expiring stale membership intents, running due Ranked
/// matchmakers, and applying ready rating events in round order.
pub async fn run_once(pool: &PgPool, now: i64) -> Result<SchedulerTick, JobError> {
    let expired_memberships = league::expire_pending_memberships(pool, now).await?;
    let due_rounds = due_ranked_rounds(pool, now).await?;
    let mut matched_rounds = Vec::with_capacity(due_rounds.len());
    for market_round_id in due_rounds {
        ranked::run_matchmaker(pool, market_round_id, now).await?;
        matched_rounds.push(market_round_id);
    }

    let mut rating_batches_applied = 0;
    while rating_batches_applied < MAX_RATING_APPLIES_PER_TICK {
        if rating::apply_next(pool, now).await?.is_none() {
            break;
        }
        rating_batches_applied += 1;
    }

    metrics::increment("scheduler_ticks_total", 1);
    Ok(SchedulerTick {
        expired_memberships,
        matched_rounds,
        rating_batches_applied,
    })
}

async fn due_ranked_rounds(pool: &PgPool, now: i64) -> Result<Vec<i64>, JobError> {
    let rows = sqlx::query(
        "SELECT id
         FROM market_rounds
         WHERE state IN ('SCHEDULED', 'COMMIT_OPEN')
           AND is_replay = FALSE
           AND queue_close_at <= $1
           AND start_target_at > $1
         ORDER BY queue_close_at ASC, round_sequence ASC
         LIMIT $2",
    )
    .bind(now)
    .bind(MAX_DUE_RANKED_ROUNDS)
    .fetch_all(pool)
    .await
    .map_err(|error| JobError::Storage(error.to_string()))?;

    rows.into_iter()
        .map(|row| {
            row.try_get("id")
                .map_err(|error| JobError::Storage(error.to_string()))
        })
        .collect()
}
