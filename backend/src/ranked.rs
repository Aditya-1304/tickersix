//! Ranked queue, cutoff snapshots, and coordinator admission.
//!
//! This module owns off-chain scheduling and pairing only. The Anchor program
//! remains the final authority for rated exposure: the coordinator transaction
//! creates the Battle and both RatedSlot PDAs atomically, and this module only
//! materializes that transaction after confirmation.

use std::fmt;

use protocol::{pair_ranked, RankedPlayer};
use relay::{
    build_create_rated_battle_instruction, create_rated_battle_pdas, CreateRatedBattleAccounts,
};
use serde::Serialize;
use sqlx::{postgres::PgRow, PgPool, Postgres, Row, Transaction};

use crate::auth::parse_wallet;

pub const PAIRING_POLICY_VERSION: i32 = 1;
pub use crate::rating::RATING_FORMULA_VERSION;
pub const RECENT_REMATCH_WINDOW: i64 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RankedError {
    InvalidWallet,
    InvalidRound,
    RoundNotFound,
    RoundNotEligible,
    QueueClosed,
    QueueNotFound,
    AlreadyPaired,
    AlreadyExposed,
    LeagueReserved,
    UnresolvedPreviousBattle,
    CoordinatorPlanUnavailable,
    CoordinatorConflict,
    Storage(String),
}

impl fmt::Display for RankedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidWallet => "wallet is not a valid Solana public key",
            Self::InvalidRound => "Market Round timing or state is invalid for Ranked",
            Self::RoundNotFound => "Market Round was not found",
            Self::RoundNotEligible => "Market Round is not eligible for Ranked queueing",
            Self::QueueClosed => "Ranked queue is closed",
            Self::QueueNotFound => "wallet is not in the Ranked queue",
            Self::AlreadyPaired => "wallet already has a Ranked pairing for this round",
            Self::AlreadyExposed => "wallet already has a rated exposure in this round",
            Self::LeagueReserved => "wallet has an overlapping League reservation",
            Self::UnresolvedPreviousBattle => "an earlier rated Battle is unresolved",
            Self::CoordinatorPlanUnavailable => "coordinator Battle plan is unavailable",
            Self::CoordinatorConflict => {
                "coordinator Battle confirmation conflicts with existing state"
            }
            Self::Storage(_) => "Ranked backend storage operation failed",
        })
    }
}

impl std::error::Error for RankedError {}

#[derive(Debug, Clone, Serialize)]
pub struct NextMarketRound {
    pub id: i64,
    pub chain_pubkey: Option<String>,
    pub round_sequence: i64,
    pub state: String,
    pub is_replay: bool,
    pub queue_close_at: i64,
    pub start_target_at: i64,
    pub end_target_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueEntry {
    pub market_round_id: i64,
    pub wallet: String,
    pub status: String,
    pub current_rating: i32,
    pub tier: String,
    pub matchmaking_rating_status: &'static str,
    pub rating_snapshot: Option<i32>,
    pub rated_games_snapshot: Option<i32>,
    pub rating_snapshot_at: Option<i64>,
    pub joined_at: i64,
    pub queue_closes_at: i64,
    pub start_target_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RankedPairingView {
    pub pairing_id: i64,
    pub player_a: String,
    pub player_b: String,
    pub rating_a_snapshot: i32,
    pub rating_b_snapshot: i32,
    pub battle_pubkey: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchRunResult {
    pub match_run_id: i64,
    pub market_round_id: i64,
    pub cutoff_at: i64,
    pub pairing_policy_version: i32,
    pub pairings: Vec<RankedPairingView>,
    pub unmatched_wallets: Vec<String>,
    pub blocked_wallets: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct CoordinatorAccounts {
    pub config: [u8; 32],
    pub coordinator: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct CoordinatorBattlePlan {
    pub pairing_id: i64,
    pub battle_id: u64,
    pub player_a: String,
    pub player_b: String,
    pub instruction: solana_instruction::Instruction,
}

pub async fn next_market_round(
    pool: &PgPool,
    now: i64,
) -> Result<Option<NextMarketRound>, RankedError> {
    let row = sqlx::query(
        "SELECT id, chain_pubkey, round_sequence, state, is_replay,
                queue_close_at, start_target_at, end_target_at
         FROM market_rounds
         WHERE state IN ('SCHEDULED', 'COMMIT_OPEN')
           AND is_replay = FALSE
           AND queue_close_at > $1
         ORDER BY start_target_at ASC, round_sequence ASC
         LIMIT 1",
    )
    .bind(now)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?;

    row.map(|row| market_round_from_row(&row)).transpose()
}

pub async fn join_queue(
    pool: &PgPool,
    wallet: &str,
    market_round_id: i64,
    now: i64,
) -> Result<QueueEntry, RankedError> {
    parse_wallet(wallet).map_err(|_| RankedError::InvalidWallet)?;
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    lock_matchmaker_round(&mut transaction, market_round_id).await?;
    lock_ranked_wallet(&mut transaction, wallet).await?;
    let round = load_round(&mut transaction, market_round_id).await?;
    validate_queue_round(&round, now)?;

    if has_rated_exposure(&mut transaction, market_round_id, wallet).await? {
        return Err(RankedError::AlreadyExposed);
    }
    if has_league_reservation(&mut transaction, market_round_id, wallet).await? {
        return Err(RankedError::LeagueReserved);
    }
    if has_unresolved_previous_battle(&mut transaction, wallet, round.round_sequence).await? {
        return Err(RankedError::UnresolvedPreviousBattle);
    }

    let existing = sqlx::query(
        "SELECT status FROM ranked_queue
         WHERE market_round_id = $1 AND wallet = $2
         FOR UPDATE",
    )
    .bind(market_round_id)
    .bind(wallet)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(storage_error)?;
    if let Some(existing) = existing {
        let status: String = existing.try_get("status").map_err(storage_error)?;
        match status.as_str() {
            "MATCHED_PENDING_COORDINATOR" | "BATTLE_CREATED" => {
                return Err(RankedError::AlreadyPaired)
            }
            "QUEUED" | "SNAPSHOTTED" => {}
            _ => {
                sqlx::query(
                    "UPDATE ranked_queue
                     SET status = 'QUEUED', rating_snapshot = NULL,
                         rated_games_snapshot = NULL, rating_snapshot_at = NULL
                     WHERE market_round_id = $1 AND wallet = $2",
                )
                .bind(market_round_id)
                .bind(wallet)
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
            }
        }
    } else {
        sqlx::query(
            "INSERT INTO ranked_queue
                (market_round_id, wallet, joined_at, status)
             VALUES ($1, $2, $3, 'QUEUED')",
        )
        .bind(market_round_id)
        .bind(wallet)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
    }

    let entry = load_queue_entry(&mut transaction, market_round_id, wallet).await?;
    transaction.commit().await.map_err(storage_error)?;
    Ok(entry)
}

pub async fn leave_queue(
    pool: &PgPool,
    wallet: &str,
    market_round_id: i64,
    now: i64,
) -> Result<(), RankedError> {
    parse_wallet(wallet).map_err(|_| RankedError::InvalidWallet)?;
    // Queue cancellation participates in the same per-round critical section
    // as admission and matching. Without this lock, the matchmaker could
    // snapshot a wallet between the cutoff read and the cancellation update,
    // producing a Battle for a player whose leave request appeared to win.
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    lock_matchmaker_round(&mut transaction, market_round_id).await?;
    lock_ranked_wallet(&mut transaction, wallet).await?;
    let round = load_round(&mut transaction, market_round_id).await?;
    if now >= round.queue_close_at {
        return Err(RankedError::QueueClosed);
    }
    let result = sqlx::query(
        "UPDATE ranked_queue
         SET status = 'CANCELLED'
         WHERE market_round_id = $1 AND wallet = $2
           AND status IN ('QUEUED', 'UNMATCHED')
           AND EXISTS (
               SELECT 1 FROM market_rounds
               WHERE id = ranked_queue.market_round_id AND queue_close_at > $3
           )",
    )
    .bind(market_round_id)
    .bind(wallet)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    if result.rows_affected() == 1 {
        transaction.commit().await.map_err(storage_error)?;
        return Ok(());
    }

    let status = sqlx::query(
        "SELECT status FROM ranked_queue
         WHERE market_round_id = $1 AND wallet = $2
         FOR UPDATE",
    )
    .bind(market_round_id)
    .bind(wallet)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(storage_error)?;
    match status {
        None => Err(RankedError::QueueNotFound),
        Some(row) => {
            let status: String = row.try_get("status").map_err(storage_error)?;
            if status == "MATCHED_PENDING_COORDINATOR" || status == "BATTLE_CREATED" {
                Err(RankedError::AlreadyPaired)
            } else {
                Err(RankedError::QueueNotFound)
            }
        }
    }
}

pub async fn queue_status(
    pool: &PgPool,
    wallet: &str,
    market_round_id: i64,
) -> Result<QueueEntry, RankedError> {
    parse_wallet(wallet).map_err(|_| RankedError::InvalidWallet)?;
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    let entry = load_queue_entry(&mut transaction, market_round_id, wallet).await?;
    transaction.rollback().await.map_err(storage_error)?;
    Ok(entry)
}

/// Runs once after the round cutoff. PostgreSQL advisory locking and the
/// unique match-run key make a retry or a second worker converge on one plan.
pub async fn run_matchmaker(
    pool: &PgPool,
    market_round_id: i64,
    now: i64,
) -> Result<MatchRunResult, RankedError> {
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    lock_matchmaker_round(&mut transaction, market_round_id).await?;
    let round = load_round(&mut transaction, market_round_id).await?;
    if now < round.queue_close_at {
        return Err(RankedError::QueueClosed);
    }
    if round.is_replay || !matches!(round.state.as_str(), "SCHEDULED" | "COMMIT_OPEN") {
        return Err(RankedError::RoundNotEligible);
    }

    let cutoff_at = round.queue_close_at;
    if let Some(run_id) = find_match_run(&mut transaction, market_round_id, cutoff_at).await? {
        let result =
            load_match_run_result(&mut transaction, run_id, market_round_id, cutoff_at).await?;
        transaction.commit().await.map_err(storage_error)?;
        return Ok(result);
    }

    let queue_rows = sqlx::query(
        "SELECT wallet FROM ranked_queue
         WHERE market_round_id = $1 AND status = 'QUEUED'
         ORDER BY wallet ASC
         FOR UPDATE",
    )
    .bind(market_round_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(storage_error)?;

    let mut eligible = Vec::new();
    let mut blocked_wallets = Vec::new();
    for row in queue_rows {
        let wallet: String = row.try_get("wallet").map_err(storage_error)?;
        // League admission uses the same wallet advisory key. Holding it
        // while checking reservations closes the cross-mode race where a
        // League join and Ranked pairing could both pass their preflight.
        lock_ranked_wallet(&mut transaction, &wallet).await?;
        let (rating, rated_games) =
            rating_at_cutoff(&mut transaction, &wallet, round.round_sequence, cutoff_at).await?;
        sqlx::query(
            "UPDATE ranked_queue
             SET rating_snapshot = $1, rated_games_snapshot = $2,
                 rating_snapshot_at = $3, status = 'SNAPSHOTTED'
             WHERE market_round_id = $4 AND wallet = $5",
        )
        .bind(rating)
        .bind(rated_games)
        .bind(cutoff_at)
        .bind(market_round_id)
        .bind(&wallet)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;

        if has_rated_exposure(&mut transaction, market_round_id, &wallet).await?
            || has_league_reservation(&mut transaction, market_round_id, &wallet).await?
            || has_unresolved_previous_battle(&mut transaction, &wallet, round.round_sequence)
                .await?
        {
            sqlx::query(
                "UPDATE ranked_queue SET status = 'BLOCKED'
                 WHERE market_round_id = $1 AND wallet = $2",
            )
            .bind(market_round_id)
            .bind(&wallet)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
            blocked_wallets.push(wallet);
            continue;
        }

        let recent_opponents =
            recent_opponents(&mut transaction, &wallet, round.round_sequence).await?;
        eligible.push(EligiblePlayer {
            wallet: wallet.clone(),
            rating,
            ranked: RankedPlayer {
                wallet: parse_wallet(&wallet).map_err(|_| RankedError::InvalidWallet)?,
                rating,
                recent_opponents,
            },
        });
    }

    let run_id = get_or_create_match_run(&mut transaction, market_round_id, cutoff_at).await?;
    let pairing = pair_ranked(
        &eligible
            .iter()
            .map(|player| player.ranked.clone())
            .collect::<Vec<_>>(),
    );
    let mut pairings = Vec::with_capacity(pairing.pairs.len());
    for (left, right) in pairing.pairs {
        let player_a = &eligible[left];
        let player_b = &eligible[right];
        let pairing_id = insert_pairing(
            &mut transaction,
            run_id,
            market_round_id,
            player_a,
            player_b,
            cutoff_at,
        )
        .await?;
        mark_queue_matched(
            &mut transaction,
            market_round_id,
            &player_a.wallet,
            &player_b.wallet,
        )
        .await?;
        pairings.push(RankedPairingView {
            pairing_id,
            player_a: player_a.wallet.clone(),
            player_b: player_b.wallet.clone(),
            rating_a_snapshot: player_a.rating,
            rating_b_snapshot: player_b.rating,
            battle_pubkey: None,
            status: "PENDING_COORDINATOR".to_owned(),
        });
    }

    let unmatched_wallets = pairing
        .unmatched
        .map(|index| eligible[index].wallet.clone())
        .into_iter()
        .collect::<Vec<_>>();
    for wallet in &unmatched_wallets {
        sqlx::query(
            "UPDATE ranked_queue SET status = 'UNMATCHED'
             WHERE market_round_id = $1 AND wallet = $2",
        )
        .bind(market_round_id)
        .bind(wallet)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
    }

    let run_status = if pairings.is_empty() {
        "EMPTY"
    } else {
        "PLANNED"
    };
    sqlx::query("UPDATE ranked_match_runs SET status = $1 WHERE id = $2")
        .bind(run_status)
        .bind(run_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
    transaction.commit().await.map_err(storage_error)?;

    Ok(MatchRunResult {
        match_run_id: run_id,
        market_round_id,
        cutoff_at,
        pairing_policy_version: PAIRING_POLICY_VERSION,
        pairings,
        unmatched_wallets,
        blocked_wallets,
    })
}

/// Produces the coordinator instruction after matching. A separate signer/RPC
/// boundary submits it, keeping coordinator private keys outside this backend.
pub async fn coordinator_battle_plan(
    pool: &PgPool,
    pairing_id: i64,
    accounts: CoordinatorAccounts,
) -> Result<CoordinatorBattlePlan, RankedError> {
    let row = sqlx::query(
        "SELECT p.id, p.market_round_id, p.player_a, p.player_b,
                p.rating_a_snapshot, p.rating_b_snapshot, p.status,
                r.chain_pubkey
         FROM ranked_pairings p
         JOIN market_rounds r ON r.id = p.market_round_id
         WHERE p.id = $1",
    )
    .bind(pairing_id)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(RankedError::CoordinatorPlanUnavailable)?;

    let status: String = row.try_get("status").map_err(storage_error)?;
    if status != "PENDING_COORDINATOR" {
        return Err(RankedError::CoordinatorPlanUnavailable);
    }
    let market_round: String = row
        .try_get::<Option<String>, _>("chain_pubkey")
        .map_err(storage_error)?
        .ok_or(RankedError::CoordinatorPlanUnavailable)?;
    let market_round = parse_wallet(&market_round).map_err(|_| RankedError::InvalidRound)?;
    let player_a: String = row.try_get("player_a").map_err(storage_error)?;
    let player_b: String = row.try_get("player_b").map_err(storage_error)?;
    let player_a_bytes = parse_wallet(&player_a).map_err(|_| RankedError::InvalidWallet)?;
    let player_b_bytes = parse_wallet(&player_b).map_err(|_| RankedError::InvalidWallet)?;
    let battle_id: u64 = row
        .try_get::<i64, _>("id")
        .map_err(storage_error)?
        .try_into()
        .map_err(|_| RankedError::CoordinatorPlanUnavailable)?;
    let (battle, rated_slot_a, rated_slot_b) =
        create_rated_battle_pdas(market_round, battle_id, player_a_bytes, player_b_bytes);
    let instruction = build_create_rated_battle_instruction(
        battle_id,
        row.try_get("rating_a_snapshot").map_err(storage_error)?,
        row.try_get("rating_b_snapshot").map_err(storage_error)?,
        RATING_FORMULA_VERSION,
        CreateRatedBattleAccounts {
            config: accounts.config,
            coordinator: accounts.coordinator,
            market_round,
            player_a: player_a_bytes,
            player_b: player_b_bytes,
            battle,
            rated_slot_a,
            rated_slot_b,
        },
    );

    Ok(CoordinatorBattlePlan {
        pairing_id,
        battle_id,
        player_a,
        player_b,
        instruction,
    })
}

/// Materializes a confirmed coordinator transaction exactly once. The caller
/// must invoke this only after the Solana transaction is confirmed.
pub async fn confirm_coordinator_battle(
    pool: &PgPool,
    pairing_id: i64,
    battle_pubkey: &str,
    now: i64,
) -> Result<(), RankedError> {
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    let row = sqlx::query(
        "SELECT market_round_id, player_a, player_b, rating_a_snapshot,
                rating_b_snapshot, status, battle_pubkey,
                r.chain_pubkey AS market_round_pubkey
         FROM ranked_pairings p
         JOIN market_rounds r ON r.id = p.market_round_id
         WHERE p.id = $1
         FOR UPDATE",
    )
    .bind(pairing_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(storage_error)?
    .ok_or(RankedError::CoordinatorPlanUnavailable)?;
    let status: String = row.try_get("status").map_err(storage_error)?;
    let stored_battle: Option<String> = row.try_get("battle_pubkey").map_err(storage_error)?;
    if status == "CREATED" {
        return if stored_battle.as_deref() == Some(battle_pubkey) {
            Ok(())
        } else {
            Err(RankedError::CoordinatorConflict)
        };
    }
    if status != "PENDING_COORDINATOR" || stored_battle.is_some() {
        return Err(RankedError::CoordinatorConflict);
    }

    let market_round_id: i64 = row.try_get("market_round_id").map_err(storage_error)?;
    let player_a: String = row.try_get("player_a").map_err(storage_error)?;
    let player_b: String = row.try_get("player_b").map_err(storage_error)?;
    let market_round: String = row
        .try_get::<Option<String>, _>("market_round_pubkey")
        .map_err(storage_error)?
        .ok_or(RankedError::CoordinatorPlanUnavailable)?;
    let market_round = parse_wallet(&market_round).map_err(|_| RankedError::InvalidRound)?;
    let player_a_bytes = parse_wallet(&player_a).map_err(|_| RankedError::InvalidWallet)?;
    let player_b_bytes = parse_wallet(&player_b).map_err(|_| RankedError::InvalidWallet)?;
    let battle_id = pairing_id
        .try_into()
        .map_err(|_| RankedError::CoordinatorConflict)?;
    if !battle_pubkey_matches_pairing(
        market_round,
        battle_id,
        player_a_bytes,
        player_b_bytes,
        battle_pubkey,
    ) {
        return Err(RankedError::CoordinatorConflict);
    }
    let battle_insert = sqlx::query(
        "INSERT INTO battles
            (chain_pubkey, market_round_id, mode, rated, player_a, player_b,
             state, indexed_at, rating_a_before, rating_b_before,
             rating_formula_version)
         VALUES ($1, $2, 'RANKED', TRUE, $3, $4, 'CREATED', $5, $6, $7, $8)
         ON CONFLICT (chain_pubkey) DO UPDATE SET
             rating_a_before = COALESCE(battles.rating_a_before, EXCLUDED.rating_a_before),
             rating_b_before = COALESCE(battles.rating_b_before, EXCLUDED.rating_b_before),
             rating_formula_version = COALESCE(
                 battles.rating_formula_version,
                 EXCLUDED.rating_formula_version
             )
         WHERE battles.market_round_id = EXCLUDED.market_round_id
           AND battles.player_a = EXCLUDED.player_a
           AND battles.player_b = EXCLUDED.player_b
           AND battles.rated
           AND battles.mode = 'RANKED'",
    )
    .bind(battle_pubkey)
    .bind(market_round_id)
    .bind(&player_a)
    .bind(&player_b)
    .bind(now)
    .bind(
        row.try_get::<i32, _>("rating_a_snapshot")
            .map_err(storage_error)?,
    )
    .bind(
        row.try_get::<i32, _>("rating_b_snapshot")
            .map_err(storage_error)?,
    )
    .bind(i32::from(RATING_FORMULA_VERSION))
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    if battle_insert.rows_affected() != 1 {
        return Err(RankedError::CoordinatorConflict);
    }

    for wallet in [&player_a, &player_b] {
        let result = sqlx::query(
            "INSERT INTO rated_exposures (market_round_id, wallet, battle_pubkey)
             VALUES ($1, $2, $3)
             ON CONFLICT (market_round_id, wallet) DO NOTHING",
        )
        .bind(market_round_id)
        .bind(wallet)
        .bind(battle_pubkey)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            return Err(RankedError::CoordinatorConflict);
        }
    }

    sqlx::query(
        "UPDATE ranked_pairings
         SET battle_pubkey = $1, status = 'CREATED'
         WHERE id = $2",
    )
    .bind(battle_pubkey)
    .bind(pairing_id)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    sqlx::query(
        "UPDATE ranked_queue
         SET status = 'BATTLE_CREATED'
         WHERE market_round_id = $1 AND wallet IN ($2, $3)",
    )
    .bind(market_round_id)
    .bind(&player_a)
    .bind(&player_b)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    transaction.commit().await.map_err(storage_error)?;
    Ok(())
}

struct EligiblePlayer {
    wallet: String,
    rating: i32,
    ranked: RankedPlayer,
}

fn battle_pubkey_matches_pairing(
    market_round: [u8; 32],
    battle_id: u64,
    player_a: [u8; 32],
    player_b: [u8; 32],
    battle_pubkey: &str,
) -> bool {
    let (expected_battle, _, _) =
        create_rated_battle_pdas(market_round, battle_id, player_a, player_b);
    parse_wallet(battle_pubkey)
        .map(|candidate| candidate == expected_battle)
        .unwrap_or(false)
}

async fn lock_matchmaker_round(
    transaction: &mut Transaction<'_, Postgres>,
    market_round_id: i64,
) -> Result<(), RankedError> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(market_round_id)
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
    Ok(())
}

/// Serializes Ranked admission, cancellation, and matching with League's
/// wallet-scoped reservation checks. The advisory key is intentionally shared
/// with `league::lock_wallet` so the two competition modes cannot both admit
/// the same wallet into overlapping rated obligations.
async fn lock_ranked_wallet(
    transaction: &mut Transaction<'_, Postgres>,
    wallet: &str,
) -> Result<(), RankedError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(wallet)
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
    Ok(())
}

async fn load_round(
    transaction: &mut Transaction<'_, Postgres>,
    market_round_id: i64,
) -> Result<NextMarketRound, RankedError> {
    sqlx::query(
        "SELECT id, chain_pubkey, round_sequence, state, is_replay,
                queue_close_at, start_target_at, end_target_at
         FROM market_rounds WHERE id = $1 FOR UPDATE",
    )
    .bind(market_round_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .ok_or(RankedError::RoundNotFound)
    .and_then(|row| market_round_from_row(&row))
}

fn market_round_from_row(row: &PgRow) -> Result<NextMarketRound, RankedError> {
    let round = NextMarketRound {
        id: row.try_get("id").map_err(storage_error)?,
        chain_pubkey: row.try_get("chain_pubkey").map_err(storage_error)?,
        round_sequence: row.try_get("round_sequence").map_err(storage_error)?,
        state: row.try_get("state").map_err(storage_error)?,
        is_replay: row.try_get("is_replay").map_err(storage_error)?,
        queue_close_at: row.try_get("queue_close_at").map_err(storage_error)?,
        start_target_at: row.try_get("start_target_at").map_err(storage_error)?,
        end_target_at: row.try_get("end_target_at").map_err(storage_error)?,
    };
    if round.queue_close_at >= round.start_target_at || round.start_target_at >= round.end_target_at
    {
        return Err(RankedError::InvalidRound);
    }
    Ok(round)
}

fn validate_queue_round(round: &NextMarketRound, now: i64) -> Result<(), RankedError> {
    if round.queue_close_at >= round.start_target_at || round.start_target_at >= round.end_target_at
    {
        return Err(RankedError::InvalidRound);
    }
    if round.is_replay || !matches!(round.state.as_str(), "SCHEDULED" | "COMMIT_OPEN") {
        return Err(RankedError::RoundNotEligible);
    }
    if now >= round.queue_close_at {
        return Err(RankedError::QueueClosed);
    }
    Ok(())
}

async fn load_queue_entry(
    transaction: &mut Transaction<'_, Postgres>,
    market_round_id: i64,
    wallet: &str,
) -> Result<QueueEntry, RankedError> {
    let row = sqlx::query(
        "SELECT q.market_round_id, q.wallet, q.status, q.rating_snapshot,
                q.rated_games_snapshot, q.rating_snapshot_at, q.joined_at,
                r.queue_close_at,
                r.start_target_at,
                COALESCE((SELECT rating FROM ratings rt
                          JOIN seasons s ON s.id = rt.season_id
                          WHERE s.status = 'ACTIVE' AND rt.wallet = q.wallet
                          LIMIT 1), 1500) AS current_rating
                ,COALESCE((SELECT rated_games FROM ratings rt
                           JOIN seasons s ON s.id = rt.season_id
                           WHERE s.status = 'ACTIVE' AND rt.wallet = q.wallet
                           LIMIT 1), 0) AS current_rated_games
         FROM ranked_queue q
         JOIN market_rounds r ON r.id = q.market_round_id
         WHERE q.market_round_id = $1 AND q.wallet = $2",
    )
    .bind(market_round_id)
    .bind(wallet)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .ok_or(RankedError::QueueNotFound)?;
    let current_rating: i32 = row.try_get("current_rating").map_err(storage_error)?;
    let current_rated_games: i32 = row.try_get("current_rated_games").map_err(storage_error)?;
    Ok(QueueEntry {
        market_round_id: row.try_get("market_round_id").map_err(storage_error)?,
        wallet: row.try_get("wallet").map_err(storage_error)?,
        status: row.try_get("status").map_err(storage_error)?,
        current_rating,
        tier: crate::rating::rating_tier(current_rating, current_rated_games).to_owned(),
        matchmaking_rating_status: "SNAPSHOTS_AT_QUEUE_CLOSE",
        rating_snapshot: row.try_get("rating_snapshot").map_err(storage_error)?,
        rated_games_snapshot: row.try_get("rated_games_snapshot").map_err(storage_error)?,
        rating_snapshot_at: row.try_get("rating_snapshot_at").map_err(storage_error)?,
        joined_at: row.try_get("joined_at").map_err(storage_error)?,
        queue_closes_at: row.try_get("queue_close_at").map_err(storage_error)?,
        start_target_at: row.try_get("start_target_at").map_err(storage_error)?,
    })
}

/// Reconstructs the canonical active-season rating at the immutable pairing
/// cutoff. A delayed worker must not let a later rating event change a frozen
/// match decision, so both the event sequence and durable event timestamp are
/// bounded by the target round and queue-close instant.
async fn rating_at_cutoff(
    transaction: &mut Transaction<'_, Postgres>,
    wallet: &str,
    round_sequence: i64,
    cutoff_at: i64,
) -> Result<(i32, i32), RankedError> {
    let row = sqlx::query(
        "SELECT COALESCE((
                    SELECT re.rating_after
                    FROM rating_events re
                    JOIN seasons s ON s.id = re.season_id
                    WHERE s.status = 'ACTIVE'
                      AND re.wallet = $1
                      AND re.round_sequence < $2
                      AND re.created_at <= $3
                    ORDER BY re.round_sequence DESC, re.created_at DESC, re.id DESC
                    LIMIT 1
                ), COALESCE((
                    SELECT r.rating
                    FROM ratings r
                    JOIN seasons s ON s.id = r.season_id
                    WHERE s.status = 'ACTIVE'
                      AND r.wallet = $1
                      AND r.updated_at <= $3
                    LIMIT 1
                ), 1500)) AS rating,
                COALESCE((
                    SELECT COUNT(*)::INTEGER
                    FROM rating_events re
                    JOIN seasons s ON s.id = re.season_id
                    WHERE s.status = 'ACTIVE'
                      AND re.wallet = $1
                      AND re.event_kind = 'PLAYED_RATED_BATTLE'
                      AND re.round_sequence < $2
                      AND re.created_at <= $3
                ), 0) AS rated_games",
    )
    .bind(wallet)
    .bind(round_sequence)
    .bind(cutoff_at)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok((
        row.try_get("rating").map_err(storage_error)?,
        row.try_get("rated_games").map_err(storage_error)?,
    ))
}

async fn has_rated_exposure(
    transaction: &mut Transaction<'_, Postgres>,
    market_round_id: i64,
    wallet: &str,
) -> Result<bool, RankedError> {
    let row = sqlx::query(
        "SELECT EXISTS(
             SELECT 1 FROM rated_exposures
             WHERE market_round_id = $1 AND wallet = $2
         ) AS exists",
    )
    .bind(market_round_id)
    .bind(wallet)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.try_get("exists").map_err(storage_error)
}

async fn has_league_reservation(
    transaction: &mut Transaction<'_, Postgres>,
    market_round_id: i64,
    wallet: &str,
) -> Result<bool, RankedError> {
    let row = sqlx::query(
        "SELECT EXISTS(
             SELECT 1
             FROM league_reservations reservation
             JOIN market_rounds reserved_round
               ON reserved_round.id = reservation.market_round_id
             JOIN market_rounds requested_round
               ON requested_round.id = $1
             WHERE reservation.wallet = $2
               AND reservation.status NOT IN ('CANCELLED', 'EXPIRED', 'RESOLVED')
               AND reserved_round.start_target_at < requested_round.end_target_at
               AND requested_round.start_target_at < reserved_round.end_target_at
         ) AS exists",
    )
    .bind(market_round_id)
    .bind(wallet)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.try_get("exists").map_err(storage_error)
}

async fn has_unresolved_previous_battle(
    transaction: &mut Transaction<'_, Postgres>,
    wallet: &str,
    round_sequence: i64,
) -> Result<bool, RankedError> {
    let row = sqlx::query(
        "SELECT EXISTS(
             SELECT 1 FROM battles b
             JOIN market_rounds r ON r.id = b.market_round_id
             WHERE b.rated AND (b.player_a = $1 OR b.player_b = $1)
               AND r.round_sequence < $2
               AND (
                   b.state NOT IN ('FINALIZED', 'VOIDED')
                   OR (
                       b.state = 'FINALIZED'
                       AND NOT EXISTS (
                           SELECT 1 FROM rating_events re
                           WHERE re.battle_pubkey = b.chain_pubkey
                             AND re.wallet = $1
                       )
                   )
               )
         ) AS exists",
    )
    .bind(wallet)
    .bind(round_sequence)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.try_get("exists").map_err(storage_error)
}

async fn recent_opponents(
    transaction: &mut Transaction<'_, Postgres>,
    wallet: &str,
    round_sequence: i64,
) -> Result<Vec<[u8; 32]>, RankedError> {
    let rows = sqlx::query(
        "SELECT CASE WHEN b.player_a = $1 THEN b.player_b ELSE b.player_a END AS opponent
         FROM battles b
         JOIN market_rounds r ON r.id = b.market_round_id
         WHERE b.rated AND (b.player_a = $1 OR b.player_b = $1)
           AND r.round_sequence < $2
           AND b.state <> 'VOIDED'
         ORDER BY r.round_sequence DESC
         LIMIT $3",
    )
    .bind(wallet)
    .bind(round_sequence)
    .bind(RECENT_REMATCH_WINDOW)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    rows.into_iter()
        .map(|row| {
            let opponent: String = row.try_get("opponent").map_err(storage_error)?;
            parse_wallet(&opponent).map_err(|_| RankedError::InvalidWallet)
        })
        .collect()
}

async fn find_match_run(
    transaction: &mut Transaction<'_, Postgres>,
    market_round_id: i64,
    cutoff_at: i64,
) -> Result<Option<i64>, RankedError> {
    let row = sqlx::query(
        "SELECT id FROM ranked_match_runs
         WHERE market_round_id = $1
           AND cutoff_at = $2
           AND pairing_policy_version = $3",
    )
    .bind(market_round_id)
    .bind(cutoff_at)
    .bind(PAIRING_POLICY_VERSION)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.map(|row| row.try_get("id").map_err(storage_error))
        .transpose()
}

async fn load_match_run_result(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: i64,
    market_round_id: i64,
    cutoff_at: i64,
) -> Result<MatchRunResult, RankedError> {
    let pairing_rows = sqlx::query(
        "SELECT id, player_a, player_b, rating_a_snapshot, rating_b_snapshot,
                battle_pubkey, status
         FROM ranked_pairings
         WHERE match_run_id = $1
         ORDER BY id ASC",
    )
    .bind(run_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    let pairings = pairing_rows
        .into_iter()
        .map(|row| {
            Ok(RankedPairingView {
                pairing_id: row.try_get("id").map_err(storage_error)?,
                player_a: row.try_get("player_a").map_err(storage_error)?,
                player_b: row.try_get("player_b").map_err(storage_error)?,
                rating_a_snapshot: row.try_get("rating_a_snapshot").map_err(storage_error)?,
                rating_b_snapshot: row.try_get("rating_b_snapshot").map_err(storage_error)?,
                battle_pubkey: row.try_get("battle_pubkey").map_err(storage_error)?,
                status: row.try_get("status").map_err(storage_error)?,
            })
        })
        .collect::<Result<Vec<_>, RankedError>>()?;
    let unmatched_wallets =
        queue_wallets_by_status(transaction, market_round_id, "UNMATCHED").await?;
    let blocked_wallets = queue_wallets_by_status(transaction, market_round_id, "BLOCKED").await?;
    Ok(MatchRunResult {
        match_run_id: run_id,
        market_round_id,
        cutoff_at,
        pairing_policy_version: PAIRING_POLICY_VERSION,
        pairings,
        unmatched_wallets,
        blocked_wallets,
    })
}

async fn queue_wallets_by_status(
    transaction: &mut Transaction<'_, Postgres>,
    market_round_id: i64,
    status: &str,
) -> Result<Vec<String>, RankedError> {
    let rows = sqlx::query(
        "SELECT wallet FROM ranked_queue
         WHERE market_round_id = $1 AND status = $2
         ORDER BY wallet ASC",
    )
    .bind(market_round_id)
    .bind(status)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    rows.into_iter()
        .map(|row| row.try_get("wallet").map_err(storage_error))
        .collect()
}

async fn get_or_create_match_run(
    transaction: &mut Transaction<'_, Postgres>,
    market_round_id: i64,
    cutoff_at: i64,
) -> Result<i64, RankedError> {
    let row = sqlx::query(
        "INSERT INTO ranked_match_runs
            (market_round_id, cutoff_at, pairing_policy_version, status, created_at)
         VALUES ($1, $2, $3, 'RUNNING', $2)
         ON CONFLICT (market_round_id, cutoff_at, pairing_policy_version)
         DO UPDATE SET status = ranked_match_runs.status
         RETURNING id",
    )
    .bind(market_round_id)
    .bind(cutoff_at)
    .bind(PAIRING_POLICY_VERSION)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.try_get("id").map_err(storage_error)
}

async fn insert_pairing(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: i64,
    market_round_id: i64,
    player_a: &EligiblePlayer,
    player_b: &EligiblePlayer,
    now: i64,
) -> Result<i64, RankedError> {
    let row = sqlx::query(
        "INSERT INTO ranked_pairings
            (match_run_id, market_round_id, player_a, player_b,
             rating_a_snapshot, rating_b_snapshot, status, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, 'PENDING_COORDINATOR', $7)
         ON CONFLICT (match_run_id, player_a)
         DO UPDATE SET status = ranked_pairings.status
         RETURNING id",
    )
    .bind(run_id)
    .bind(market_round_id)
    .bind(&player_a.wallet)
    .bind(&player_b.wallet)
    .bind(player_a.rating)
    .bind(player_b.rating)
    .bind(now)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    row.try_get("id").map_err(storage_error)
}

async fn mark_queue_matched(
    transaction: &mut Transaction<'_, Postgres>,
    market_round_id: i64,
    player_a: &str,
    player_b: &str,
) -> Result<(), RankedError> {
    sqlx::query(
        "UPDATE ranked_queue SET status = 'MATCHED_PENDING_COORDINATOR'
         WHERE market_round_id = $1 AND wallet IN ($2, $3)",
    )
    .bind(market_round_id)
    .bind(player_a)
    .bind(player_b)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn storage_error(error: sqlx::Error) -> RankedError {
    RankedError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranked_constants_pin_the_cross_service_policy_versions() {
        assert_eq!(PAIRING_POLICY_VERSION, 1);
        assert_eq!(RATING_FORMULA_VERSION, 1);
        assert_eq!(RECENT_REMATCH_WINDOW, 5);
    }

    #[test]
    fn invalid_round_timing_is_rejected_before_queue_admission() {
        let round = NextMarketRound {
            id: 1,
            chain_pubkey: None,
            round_sequence: 1,
            state: "SCHEDULED".to_owned(),
            is_replay: false,
            queue_close_at: 100,
            start_target_at: 100,
            end_target_at: 200,
        };

        assert_eq!(
            validate_queue_round(&round, 50),
            Err(RankedError::InvalidRound)
        );
    }

    #[test]
    fn coordinator_confirmation_requires_the_pairing_derived_battle_pda() {
        let market_round = [1; 32];
        let player_a = [2; 32];
        let player_b = [3; 32];
        let battle_id = 9;
        let (expected_battle, _, _) =
            create_rated_battle_pdas(market_round, battle_id, player_a, player_b);
        let expected_battle = bs58::encode(expected_battle).into_string();
        let (different_battle, _, _) =
            create_rated_battle_pdas(market_round, battle_id + 1, player_a, player_b);
        let different_battle = bs58::encode(different_battle).into_string();

        assert!(battle_pubkey_matches_pairing(
            market_round,
            battle_id,
            player_a,
            player_b,
            &expected_battle,
        ));
        assert!(!battle_pubkey_matches_pairing(
            market_round,
            battle_id,
            player_a,
            player_b,
            &different_battle,
        ));
    }
}
