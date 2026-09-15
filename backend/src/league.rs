//! League catalog, membership intents, and Market Round reservations.
//!
//! The Anchor League and LeagueMember accounts are the competitive source of
//! truth. This module therefore records wallet actions as pending intents,
//! returns the exact wallet-signed instruction, and promotes those intents to
//! active only after a chain indexer confirms the corresponding account.
//! PostgreSQL transactions reserve every scheduled Market Round before a join
//! instruction is returned, so Ranked admission and another League cannot
//! claim the same wallet during an unresolved membership transition.

use std::{collections::HashSet, fmt};

use relay::{
    build_join_league_instruction, build_leave_league_instruction, config_pda, league_member_pda,
    LeagueMembershipAccounts,
};
use serde::Serialize;
use solana_instruction::Instruction;
use sqlx::{postgres::PgRow, PgPool, Postgres, Row, Transaction};

use crate::auth::parse_wallet;

pub const MAX_LEAGUE_PLAYERS: i32 = 100;
pub const PENDING_JOIN_TTL_SECS: i64 = 15 * 60;

const LEAGUE_REGISTRATION: &str = "REGISTRATION";
const LEAGUE_ACTIVE: &str = "ACTIVE";
const LEAGUE_COMPLETED: &str = "COMPLETED";
const LEAGUE_CANCELLED: &str = "CANCELLED";
const MEMBERSHIP_PENDING_JOIN: &str = "PENDING_JOIN";
const MEMBERSHIP_ACTIVE: &str = "ACTIVE";
const MEMBERSHIP_PENDING_LEAVE: &str = "PENDING_LEAVE";
const MEMBERSHIP_LEFT: &str = "LEFT";

/// One immutable time window assigned to one League round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleEntry {
    pub league_round_no: i32,
    pub market_round_id: i64,
    pub start_target_at: i64,
    pub end_target_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeagueError {
    InvalidLeagueId,
    InvalidWallet,
    InvalidName,
    InvalidLeague,
    NotFound,
    RegistrationClosed,
    LeagueFull,
    AlreadyMember,
    NotMember,
    ScheduleUnavailable,
    InvalidSchedule,
    EmptySchedule,
    DuplicateMarketRound,
    DuplicateLeagueRound,
    ScheduleOverlap,
    ScheduleConflict,
    ChainIdentityUnavailable,
    InvalidMembershipState,
    Storage(String),
}

impl fmt::Display for LeagueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLeagueId => "League id must be a positive integer",
            Self::InvalidWallet => "wallet is not a valid Solana public key",
            Self::InvalidName => "League name must contain 1 to 80 non-whitespace characters",
            Self::InvalidLeague => "League configuration is invalid",
            Self::NotFound => "League was not found",
            Self::RegistrationClosed => "League registration is closed",
            Self::LeagueFull => "League has reached its player limit",
            Self::AlreadyMember => "wallet already has a membership in this League",
            Self::NotMember => "wallet is not an active member of this League",
            Self::ScheduleUnavailable => "requested Market Round schedule is unavailable",
            Self::InvalidSchedule => "League schedule contains an invalid round or time window",
            Self::EmptySchedule => "League schedule must contain at least one round",
            Self::DuplicateMarketRound => "a Market Round cannot be assigned twice",
            Self::DuplicateLeagueRound => "a League round number cannot be assigned twice",
            Self::ScheduleOverlap => "League Market Round windows must not overlap",
            Self::ScheduleConflict => "wallet has an overlapping rated obligation",
            Self::ChainIdentityUnavailable => "League chain identity is unavailable or invalid",
            Self::InvalidMembershipState => "membership state is not valid for this operation",
            Self::Storage(_) => "League storage operation failed",
        })
    }
}

impl std::error::Error for LeagueError {}

#[derive(Debug, Clone, Serialize)]
pub struct LeagueSummary {
    pub id: i64,
    pub chain_pubkey: String,
    pub name: String,
    pub max_players: i32,
    pub joined_players: i32,
    pub total_rounds: i32,
    pub current_round: i32,
    pub pairing_policy_version: i32,
    pub rated: bool,
    pub registration_close_at: i64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LeagueRound {
    pub league_round_no: i32,
    pub market_round_id: i64,
    pub market_round_chain_pubkey: Option<String>,
    pub market_round_state: String,
    pub start_target_at: i64,
    pub end_target_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LeagueDetails {
    pub league: LeagueSummary,
    pub rounds: Vec<LeagueRound>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MembershipView {
    pub league_id: i64,
    pub wallet: String,
    pub membership_status: String,
    pub active: bool,
    pub joined_at: i64,
    pub bye_count: i32,
    pub pending_until: Option<i64>,
    pub member_pubkey: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstructionAccountView {
    pub address: String,
    pub signer: bool,
    pub writable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstructionView {
    pub program_id: String,
    pub accounts: Vec<InstructionAccountView>,
    pub data_base58: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JoinLeagueResponse {
    pub league: LeagueSummary,
    pub membership: MembershipView,
    pub instruction: InstructionView,
}

#[derive(Debug, Clone, Serialize)]
pub struct LeaveLeagueResponse {
    pub membership: MembershipView,
    pub instruction: InstructionView,
}

/// Inputs for an official League projection created from the coordinator's
/// on-chain League transaction. The chain public key is the stable identity;
/// the database id is only an API and foreign-key convenience.
#[derive(Debug, Clone, Copy)]
pub struct OfficialLeagueInput<'a> {
    pub chain_pubkey: &'a str,
    pub name: &'a str,
    pub max_players: i32,
    pub total_rounds: i32,
    pub pairing_policy_version: i32,
    pub registration_close_at: i64,
    pub now: i64,
}

#[derive(Debug, Clone)]
struct LeagueRow {
    chain_pubkey: String,
    max_players: i32,
    total_rounds: i32,
    registration_close_at: i64,
    status: String,
}

/// Validates the local representation of a League schedule.
///
/// Windows are half-open: `[start_target_at, end_target_at)`. Therefore an
/// end at exactly the next round's start is valid, while even a one-second
/// overlap is rejected. This is the same boundary used by reservation checks.
pub fn validate_schedule(schedule: &[ScheduleEntry]) -> Result<(), LeagueError> {
    if schedule.is_empty() {
        return Err(LeagueError::EmptySchedule);
    }

    let mut market_rounds = HashSet::with_capacity(schedule.len());
    let mut league_rounds = HashSet::with_capacity(schedule.len());
    for entry in schedule {
        if entry.league_round_no <= 0
            || entry.market_round_id <= 0
            || entry.start_target_at >= entry.end_target_at
        {
            return Err(LeagueError::InvalidSchedule);
        }
        if !market_rounds.insert(entry.market_round_id) {
            return Err(LeagueError::DuplicateMarketRound);
        }
        if !league_rounds.insert(entry.league_round_no) {
            return Err(LeagueError::DuplicateLeagueRound);
        }
    }

    for (index, left) in schedule.iter().enumerate() {
        for right in schedule.iter().skip(index + 1) {
            if left.start_target_at < right.end_target_at
                && right.start_target_at < left.end_target_at
            {
                return Err(LeagueError::ScheduleOverlap);
            }
        }
    }
    Ok(())
}

/// Returns the canonical on-chain LeagueMember PDA for a wallet.
pub fn expected_member_pubkey(league_pubkey: &str, wallet: &str) -> Result<String, LeagueError> {
    let league = parse_wallet(league_pubkey).map_err(|_| LeagueError::ChainIdentityUnavailable)?;
    let player = parse_wallet(wallet).map_err(|_| LeagueError::InvalidWallet)?;
    Ok(bs58::encode(league_member_pda(league, player)).into_string())
}

pub async fn list_leagues(
    pool: &PgPool,
    status: Option<&str>,
) -> Result<Vec<LeagueSummary>, LeagueError> {
    if let Some(status) = status {
        validate_status_filter(status)?;
    }
    let rows = sqlx::query(
        "SELECT l.id, l.chain_pubkey, l.name, l.max_players,
                COUNT(m.wallet) FILTER (
                    WHERE m.membership_state IN (
                        'PENDING_JOIN', 'ACTIVE', 'PENDING_LEAVE'
                    )
                )::INTEGER AS joined_players,
                l.total_rounds, l.current_round, l.pairing_policy_version,
                l.rated, l.registration_close_at, l.status
         FROM leagues l
         LEFT JOIN league_memberships m ON m.league_id = l.id
         WHERE ($1::TEXT IS NULL OR l.status = $1)
         GROUP BY l.id
         ORDER BY l.registration_close_at ASC, l.id ASC",
    )
    .bind(status)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?;

    rows.iter().map(summary_from_row).collect()
}

pub async fn get_league(pool: &PgPool, league_id: i64) -> Result<LeagueDetails, LeagueError> {
    validate_league_id(league_id)?;
    let league = load_league_summary(pool, league_id).await?;
    let rounds = list_rounds(pool, league_id).await?;
    Ok(LeagueDetails { league, rounds })
}

pub async fn list_rounds(pool: &PgPool, league_id: i64) -> Result<Vec<LeagueRound>, LeagueError> {
    validate_league_id(league_id)?;
    let rows = sqlx::query(
        "SELECT s.league_round_no, s.market_round_id, mr.chain_pubkey,
                mr.state, mr.start_target_at, mr.end_target_at
         FROM league_round_schedule s
         JOIN market_rounds mr ON mr.id = s.market_round_id
         WHERE s.league_id = $1
         ORDER BY s.league_round_no ASC",
    )
    .bind(league_id)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?;

    if rows.is_empty() && !league_exists(pool, league_id).await? {
        return Err(LeagueError::NotFound);
    }
    rows.iter().map(round_from_row).collect()
}

pub async fn upsert_official_league(
    pool: &PgPool,
    input: OfficialLeagueInput<'_>,
) -> Result<i64, LeagueError> {
    validate_official_league(&input)?;

    let inserted = sqlx::query(
        "INSERT INTO leagues
            (chain_pubkey, name, max_players, total_rounds,
             pairing_policy_version, rated, registration_close_at,
             current_round, status, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, TRUE, $6, 0, 'REGISTRATION', $7, $7)
         ON CONFLICT (chain_pubkey) DO NOTHING
         RETURNING id",
    )
    .bind(input.chain_pubkey)
    .bind(input.name.trim())
    .bind(input.max_players)
    .bind(input.total_rounds)
    .bind(input.pairing_policy_version)
    .bind(input.registration_close_at)
    .bind(input.now)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?;

    if let Some(row) = inserted {
        return row.try_get("id").map_err(storage_error);
    }

    let existing = sqlx::query(
        "SELECT id, name, max_players, total_rounds, pairing_policy_version,
                rated, registration_close_at
         FROM leagues
         WHERE chain_pubkey = $1",
    )
    .bind(input.chain_pubkey)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(LeagueError::NotFound)?;

    let matches = existing
        .try_get::<String, _>("name")
        .map_err(storage_error)?
        == input.name.trim()
        && existing
            .try_get::<i32, _>("max_players")
            .map_err(storage_error)?
            == input.max_players
        && existing
            .try_get::<i32, _>("total_rounds")
            .map_err(storage_error)?
            == input.total_rounds
        && existing
            .try_get::<i32, _>("pairing_policy_version")
            .map_err(storage_error)?
            == input.pairing_policy_version
        && existing
            .try_get::<bool, _>("rated")
            .map_err(storage_error)?
        && existing
            .try_get::<i64, _>("registration_close_at")
            .map_err(storage_error)?
            == input.registration_close_at;
    if !matches {
        return Err(LeagueError::InvalidLeague);
    }
    existing.try_get("id").map_err(storage_error)
}

/// Atomically assigns every future Market Round for an official League.
///
/// A transaction-wide advisory lock serializes schedule writers across
/// different League rows. The row locks and half-open overlap query then make
/// the no-overlapping-rated-schedule invariant hold under concurrent requests.
pub async fn assign_schedule(
    pool: &PgPool,
    league_id: i64,
    schedule: &[ScheduleEntry],
    now: i64,
) -> Result<Vec<LeagueRound>, LeagueError> {
    validate_league_id(league_id)?;
    validate_schedule(schedule)?;

    let mut transaction = pool.begin().await.map_err(storage_error)?;
    lock_scheduler(&mut transaction).await?;
    let league = load_league_for_update(&mut transaction, league_id).await?;
    if league.status != LEAGUE_REGISTRATION || now > league.registration_close_at {
        return Err(LeagueError::RegistrationClosed);
    }
    if schedule.len() != league.total_rounds as usize
        || schedule
            .iter()
            .any(|entry| entry.league_round_no > league.total_rounds)
    {
        return Err(LeagueError::InvalidSchedule);
    }
    if has_membership_started(&mut transaction, league_id).await? {
        return Err(LeagueError::ScheduleUnavailable);
    }

    for entry in schedule {
        let market_round = load_market_round_for_update(&mut transaction, entry.market_round_id)
            .await?
            .ok_or(LeagueError::ScheduleUnavailable)?;
        if market_round.chain_pubkey.is_none()
            || market_round.state != "SCHEDULED"
            || market_round.is_replay
            || market_round.queue_close_at >= market_round.start_target_at
            || market_round.start_target_at != entry.start_target_at
            || market_round.end_target_at != entry.end_target_at
            || market_round.start_target_at <= now
            || market_round.start_target_at <= league.registration_close_at
        {
            return Err(LeagueError::ScheduleUnavailable);
        }
        if schedule_conflicts_with_other_league(
            &mut transaction,
            league_id,
            entry.start_target_at,
            entry.end_target_at,
        )
        .await?
        {
            return Err(LeagueError::ScheduleConflict);
        }
    }

    sqlx::query("DELETE FROM league_round_schedule WHERE league_id = $1")
        .bind(league_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
    for entry in schedule {
        sqlx::query(
            "INSERT INTO league_round_schedule
                (league_id, league_round_no, market_round_id, assigned_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(league_id)
        .bind(entry.league_round_no)
        .bind(entry.market_round_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
    }
    sqlx::query("UPDATE leagues SET updated_at = $2 WHERE id = $1")
        .bind(league_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
    transaction.commit().await.map_err(storage_error)?;
    list_rounds(pool, league_id).await
}

/// Creates a pending join intent and returns a wallet-signed Anchor
/// instruction. It reserves all scheduled rounds until confirmation or expiry.
pub async fn request_join(
    pool: &PgPool,
    league_id: i64,
    wallet: &str,
    now: i64,
) -> Result<JoinLeagueResponse, LeagueError> {
    validate_league_id(league_id)?;
    let player = parse_wallet(wallet).map_err(|_| LeagueError::InvalidWallet)?;
    let pending_until = now
        .checked_add(PENDING_JOIN_TTL_SECS)
        .ok_or_else(|| LeagueError::Storage("membership expiry overflow".to_owned()))?;

    let mut transaction = pool.begin().await.map_err(storage_error)?;
    lock_scheduler(&mut transaction).await?;
    lock_wallet(&mut transaction, wallet).await?;
    expire_pending_memberships_in_tx(&mut transaction, now).await?;
    let league = load_league_for_update(&mut transaction, league_id).await?;
    if league.status != LEAGUE_REGISTRATION || now > league.registration_close_at {
        return Err(LeagueError::RegistrationClosed);
    }

    let schedule = load_schedule_in_tx(&mut transaction, league_id).await?;
    if schedule.len() != league.total_rounds as usize {
        return Err(LeagueError::ScheduleUnavailable);
    }
    let existing = sqlx::query(
        "SELECT membership_state
         FROM league_memberships
         WHERE league_id = $1 AND wallet = $2
         FOR UPDATE",
    )
    .bind(league_id)
    .bind(wallet)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(storage_error)?;
    if let Some(row) = existing {
        let state: String = row.try_get("membership_state").map_err(storage_error)?;
        if matches!(
            state.as_str(),
            MEMBERSHIP_PENDING_JOIN | MEMBERSHIP_ACTIVE | MEMBERSHIP_PENDING_LEAVE
        ) {
            return Err(LeagueError::AlreadyMember);
        }
    }

    let joined_players: i64 = sqlx::query(
        "SELECT COUNT(*)::BIGINT AS joined_players
         FROM league_memberships
         WHERE league_id = $1
           AND membership_state IN ('PENDING_JOIN', 'ACTIVE', 'PENDING_LEAVE')",
    )
    .bind(league_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(storage_error)?
    .try_get("joined_players")
    .map_err(storage_error)?;
    if joined_players >= i64::from(league.max_players) {
        return Err(LeagueError::LeagueFull);
    }

    for entry in &schedule {
        if wallet_has_overlapping_league(
            &mut transaction,
            wallet,
            league_id,
            entry.start_target_at,
            entry.end_target_at,
        )
        .await?
            || wallet_has_overlapping_reservation(
                &mut transaction,
                wallet,
                entry.start_target_at,
                entry.end_target_at,
            )
            .await?
            || wallet_has_overlapping_ranked_queue(
                &mut transaction,
                wallet,
                entry.start_target_at,
                entry.end_target_at,
            )
            .await?
        {
            return Err(LeagueError::ScheduleConflict);
        }
    }

    let member_pubkey = bs58::encode(league_member_pda(
        parse_chain_pubkey(&league.chain_pubkey)?,
        player,
    ))
    .into_string();
    sqlx::query(
        "INSERT INTO league_memberships
            (league_id, wallet, joined_at, active, bye_count,
             membership_state, onchain_member_pubkey, pending_until, updated_at)
         VALUES ($1, $2, $3, FALSE, 0, 'PENDING_JOIN', $4, $5, $3)
         ON CONFLICT (league_id, wallet) DO UPDATE SET
             joined_at = EXCLUDED.joined_at,
             active = FALSE,
             membership_state = 'PENDING_JOIN',
             onchain_member_pubkey = EXCLUDED.onchain_member_pubkey,
             pending_until = EXCLUDED.pending_until,
             updated_at = EXCLUDED.updated_at",
    )
    .bind(league_id)
    .bind(wallet)
    .bind(now)
    .bind(&member_pubkey)
    .bind(pending_until)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;

    for entry in &schedule {
        let result = sqlx::query(
            "INSERT INTO league_reservations
                (market_round_id, wallet, league_id, status)
             VALUES ($1, $2, $3, 'PENDING')
             ON CONFLICT (market_round_id, wallet) DO UPDATE SET
                 league_id = EXCLUDED.league_id,
                 status = 'PENDING'
             WHERE league_reservations.status IN ('CANCELLED', 'EXPIRED', 'RESOLVED')",
        )
        .bind(entry.market_round_id)
        .bind(wallet)
        .bind(league_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            return Err(LeagueError::ScheduleConflict);
        }
    }
    transaction.commit().await.map_err(storage_error)?;

    let league_summary = load_league_summary(pool, league_id).await?;
    let membership = load_membership(pool, league_id, wallet).await?;
    let instruction = build_join_league_instruction(LeagueMembershipAccounts {
        config: config_pda(),
        player,
        league: parse_chain_pubkey(&league_summary.chain_pubkey)?,
        member: parse_chain_pubkey(&member_pubkey)?,
    });
    Ok(JoinLeagueResponse {
        league: league_summary,
        membership,
        instruction: instruction_view(instruction),
    })
}

/// Marks an active membership as pending leave and returns the corresponding
/// wallet-signed close instruction. Reservations remain blocking until the
/// indexer confirms that the on-chain LeagueMember account was closed.
pub async fn request_leave(
    pool: &PgPool,
    league_id: i64,
    wallet: &str,
    now: i64,
) -> Result<LeaveLeagueResponse, LeagueError> {
    validate_league_id(league_id)?;
    let player = parse_wallet(wallet).map_err(|_| LeagueError::InvalidWallet)?;
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    lock_wallet(&mut transaction, wallet).await?;
    let league = load_league_for_update(&mut transaction, league_id).await?;
    if league.status != LEAGUE_REGISTRATION || now > league.registration_close_at {
        return Err(LeagueError::RegistrationClosed);
    }

    let row = sqlx::query(
        "SELECT membership_state, active, onchain_member_pubkey
         FROM league_memberships
         WHERE league_id = $1 AND wallet = $2
         FOR UPDATE",
    )
    .bind(league_id)
    .bind(wallet)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(storage_error)?
    .ok_or(LeagueError::NotMember)?;
    let state: String = row.try_get("membership_state").map_err(storage_error)?;
    let active: bool = row.try_get("active").map_err(storage_error)?;
    if state != MEMBERSHIP_ACTIVE || !active {
        return Err(LeagueError::InvalidMembershipState);
    }
    let expected_member = bs58::encode(league_member_pda(
        parse_chain_pubkey(&league.chain_pubkey)?,
        player,
    ))
    .into_string();
    if row
        .try_get::<Option<String>, _>("onchain_member_pubkey")
        .map_err(storage_error)?
        .as_deref()
        != Some(expected_member.as_str())
    {
        return Err(LeagueError::InvalidMembershipState);
    }

    sqlx::query(
        "UPDATE league_memberships
         SET active = FALSE, membership_state = 'PENDING_LEAVE',
             pending_until = NULL, updated_at = $3
         WHERE league_id = $1 AND wallet = $2",
    )
    .bind(league_id)
    .bind(wallet)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    sqlx::query(
        "UPDATE league_reservations
         SET status = 'PENDING_LEAVE'
         WHERE league_id = $1 AND wallet = $2
           AND status IN ('PENDING', 'ACTIVE')",
    )
    .bind(league_id)
    .bind(wallet)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    transaction.commit().await.map_err(storage_error)?;

    let membership = load_membership(pool, league_id, wallet).await?;
    let instruction = build_leave_league_instruction(LeagueMembershipAccounts {
        config: config_pda(),
        player,
        league: parse_chain_pubkey(&league.chain_pubkey)?,
        member: parse_chain_pubkey(&expected_member)?,
    });
    Ok(LeaveLeagueResponse {
        membership,
        instruction: instruction_view(instruction),
    })
}

/// Promotes a pending join after an indexed LeagueMember account matches the
/// canonical PDA derived from the indexed League and wallet.
pub async fn confirm_join(
    pool: &PgPool,
    league_id: i64,
    wallet: &str,
    member_pubkey: &str,
    now: i64,
) -> Result<MembershipView, LeagueError> {
    validate_league_id(league_id)?;
    parse_wallet(wallet).map_err(|_| LeagueError::InvalidWallet)?;
    let expected = expected_member_for_league(pool, league_id, wallet).await?;
    if expected != member_pubkey {
        return Err(LeagueError::InvalidMembershipState);
    }
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    lock_wallet(&mut transaction, wallet).await?;
    let row = sqlx::query(
        "SELECT membership_state, onchain_member_pubkey
         FROM league_memberships
         WHERE league_id = $1 AND wallet = $2
         FOR UPDATE",
    )
    .bind(league_id)
    .bind(wallet)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(storage_error)?
    .ok_or(LeagueError::NotMember)?;
    let state: String = row.try_get("membership_state").map_err(storage_error)?;
    let stored_member_pubkey: Option<String> = row
        .try_get("onchain_member_pubkey")
        .map_err(storage_error)?;
    if stored_member_pubkey.as_deref() != Some(member_pubkey) {
        return Err(LeagueError::InvalidMembershipState);
    }
    if !matches!(state.as_str(), MEMBERSHIP_PENDING_JOIN | MEMBERSHIP_ACTIVE) {
        return Err(LeagueError::InvalidMembershipState);
    }
    sqlx::query(
        "UPDATE league_memberships
         SET active = TRUE, membership_state = 'ACTIVE',
             pending_until = NULL, updated_at = $3
         WHERE league_id = $1 AND wallet = $2",
    )
    .bind(league_id)
    .bind(wallet)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    sqlx::query(
        "UPDATE league_reservations
         SET status = 'ACTIVE'
         WHERE league_id = $1 AND wallet = $2 AND status = 'PENDING'",
    )
    .bind(league_id)
    .bind(wallet)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    transaction.commit().await.map_err(storage_error)?;
    load_membership(pool, league_id, wallet).await
}

/// Completes a previously requested leave after the indexed member account is
/// gone. Until this function runs, the wallet remains reserved for its rounds.
pub async fn confirm_leave(
    pool: &PgPool,
    league_id: i64,
    wallet: &str,
    now: i64,
) -> Result<MembershipView, LeagueError> {
    validate_league_id(league_id)?;
    parse_wallet(wallet).map_err(|_| LeagueError::InvalidWallet)?;
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    lock_wallet(&mut transaction, wallet).await?;
    let row = sqlx::query(
        "SELECT membership_state
         FROM league_memberships
         WHERE league_id = $1 AND wallet = $2
         FOR UPDATE",
    )
    .bind(league_id)
    .bind(wallet)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(storage_error)?
    .ok_or(LeagueError::NotMember)?;
    let state: String = row.try_get("membership_state").map_err(storage_error)?;
    if state != MEMBERSHIP_PENDING_LEAVE && state != MEMBERSHIP_LEFT {
        return Err(LeagueError::InvalidMembershipState);
    }
    if state == MEMBERSHIP_PENDING_LEAVE {
        sqlx::query(
            "UPDATE league_memberships
             SET active = FALSE, membership_state = 'LEFT', updated_at = $3
             WHERE league_id = $1 AND wallet = $2",
        )
        .bind(league_id)
        .bind(wallet)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "UPDATE league_reservations
             SET status = 'CANCELLED'
             WHERE league_id = $1 AND wallet = $2 AND status = 'PENDING_LEAVE'",
        )
        .bind(league_id)
        .bind(wallet)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
    }
    transaction.commit().await.map_err(storage_error)?;
    load_membership(pool, league_id, wallet).await
}

/// Expires abandoned join intents and releases their reservations atomically.
pub async fn expire_pending_memberships(pool: &PgPool, now: i64) -> Result<u64, LeagueError> {
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    lock_scheduler(&mut transaction).await?;
    let count = expire_pending_memberships_in_tx(&mut transaction, now).await?;
    transaction.commit().await.map_err(storage_error)?;
    Ok(count)
}

async fn expire_pending_memberships_in_tx(
    transaction: &mut Transaction<'_, Postgres>,
    now: i64,
) -> Result<u64, LeagueError> {
    let result = sqlx::query(
        "UPDATE league_memberships
         SET active = FALSE, membership_state = 'EXPIRED', pending_until = NULL,
             updated_at = $1
         WHERE membership_state = 'PENDING_JOIN'
           AND pending_until IS NOT NULL AND pending_until <= $1",
    )
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    sqlx::query(
        "UPDATE league_reservations r
         SET status = 'EXPIRED'
         WHERE r.status = 'PENDING'
           AND EXISTS (
               SELECT 1 FROM league_memberships m
               WHERE m.league_id = r.league_id AND m.wallet = r.wallet
                 AND m.membership_state = 'EXPIRED' AND m.updated_at = $1
           )",
    )
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(result.rows_affected())
}

async fn load_league_summary(pool: &PgPool, league_id: i64) -> Result<LeagueSummary, LeagueError> {
    sqlx::query(
        "SELECT l.id, l.chain_pubkey, l.name, l.max_players,
                COUNT(m.wallet) FILTER (
                    WHERE m.membership_state IN (
                        'PENDING_JOIN', 'ACTIVE', 'PENDING_LEAVE'
                    )
                )::INTEGER AS joined_players,
                l.total_rounds, l.current_round, l.pairing_policy_version,
                l.rated, l.registration_close_at, l.status
         FROM leagues l
         LEFT JOIN league_memberships m ON m.league_id = l.id
         WHERE l.id = $1
         GROUP BY l.id",
    )
    .bind(league_id)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(LeagueError::NotFound)
    .and_then(|row| summary_from_row(&row))
}

async fn load_league_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    league_id: i64,
) -> Result<LeagueRow, LeagueError> {
    sqlx::query(
        "SELECT chain_pubkey, max_players, total_rounds,
                registration_close_at, status
         FROM leagues
         WHERE id = $1
         FOR UPDATE",
    )
    .bind(league_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?
    .ok_or(LeagueError::NotFound)
    .and_then(|row| {
        Ok(LeagueRow {
            chain_pubkey: row.try_get("chain_pubkey").map_err(storage_error)?,
            max_players: row.try_get("max_players").map_err(storage_error)?,
            total_rounds: row.try_get("total_rounds").map_err(storage_error)?,
            registration_close_at: row
                .try_get("registration_close_at")
                .map_err(storage_error)?,
            status: row.try_get("status").map_err(storage_error)?,
        })
    })
}

async fn load_schedule_in_tx(
    transaction: &mut Transaction<'_, Postgres>,
    league_id: i64,
) -> Result<Vec<ScheduleEntry>, LeagueError> {
    let rows = sqlx::query(
        "SELECT s.league_round_no, s.market_round_id,
                mr.start_target_at, mr.end_target_at
         FROM league_round_schedule s
         JOIN market_rounds mr ON mr.id = s.market_round_id
         WHERE s.league_id = $1
         ORDER BY s.league_round_no ASC",
    )
    .bind(league_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    rows.iter()
        .map(|row| {
            Ok(ScheduleEntry {
                league_round_no: row.try_get("league_round_no").map_err(storage_error)?,
                market_round_id: row.try_get("market_round_id").map_err(storage_error)?,
                start_target_at: row.try_get("start_target_at").map_err(storage_error)?,
                end_target_at: row.try_get("end_target_at").map_err(storage_error)?,
            })
        })
        .collect()
}

async fn load_market_round_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    market_round_id: i64,
) -> Result<Option<MarketRoundRow>, LeagueError> {
    sqlx::query(
        "SELECT chain_pubkey, state, is_replay, queue_close_at,
                start_target_at, end_target_at
         FROM market_rounds
         WHERE id = $1
         FOR UPDATE",
    )
    .bind(market_round_id)
    .fetch_optional(&mut **transaction)
    .await
    .map(|row| {
        row.map(|row| {
            Ok(MarketRoundRow {
                chain_pubkey: row.try_get("chain_pubkey").map_err(storage_error)?,
                state: row.try_get("state").map_err(storage_error)?,
                is_replay: row.try_get("is_replay").map_err(storage_error)?,
                queue_close_at: row.try_get("queue_close_at").map_err(storage_error)?,
                start_target_at: row.try_get("start_target_at").map_err(storage_error)?,
                end_target_at: row.try_get("end_target_at").map_err(storage_error)?,
            })
        })
    })
    .map_err(storage_error)?
    .transpose()
}

#[derive(Debug, Clone)]
struct MarketRoundRow {
    chain_pubkey: Option<String>,
    state: String,
    is_replay: bool,
    queue_close_at: i64,
    start_target_at: i64,
    end_target_at: i64,
}

async fn schedule_conflicts_with_other_league(
    transaction: &mut Transaction<'_, Postgres>,
    league_id: i64,
    start_target_at: i64,
    end_target_at: i64,
) -> Result<bool, LeagueError> {
    let row = sqlx::query(
        "SELECT 1
         FROM league_round_schedule s
         JOIN leagues l ON l.id = s.league_id
         JOIN market_rounds mr ON mr.id = s.market_round_id
         WHERE s.league_id <> $1
           AND l.rated = TRUE
           AND l.status IN ('REGISTRATION', 'ACTIVE')
           AND mr.is_replay = FALSE
           AND mr.start_target_at < $3
           AND $2 < mr.end_target_at
         LIMIT 1",
    )
    .bind(league_id)
    .bind(start_target_at)
    .bind(end_target_at)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(row.is_some())
}

async fn wallet_has_overlapping_league(
    transaction: &mut Transaction<'_, Postgres>,
    wallet: &str,
    league_id: i64,
    start_target_at: i64,
    end_target_at: i64,
) -> Result<bool, LeagueError> {
    let row = sqlx::query(
        "SELECT 1
         FROM league_memberships m
         JOIN league_round_schedule s ON s.league_id = m.league_id
         JOIN market_rounds mr ON mr.id = s.market_round_id
         JOIN leagues l ON l.id = m.league_id
         WHERE m.wallet = $1 AND m.league_id <> $2
           AND m.membership_state IN ('PENDING_JOIN', 'ACTIVE', 'PENDING_LEAVE')
           AND l.rated = TRUE AND l.status IN ('REGISTRATION', 'ACTIVE')
           AND mr.start_target_at < $4 AND $3 < mr.end_target_at
         LIMIT 1",
    )
    .bind(wallet)
    .bind(league_id)
    .bind(start_target_at)
    .bind(end_target_at)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(row.is_some())
}

async fn wallet_has_overlapping_reservation(
    transaction: &mut Transaction<'_, Postgres>,
    wallet: &str,
    start_target_at: i64,
    end_target_at: i64,
) -> Result<bool, LeagueError> {
    let row = sqlx::query(
        "SELECT 1
         FROM league_reservations r
         JOIN market_rounds mr ON mr.id = r.market_round_id
         WHERE r.wallet = $1
           AND r.status NOT IN ('CANCELLED', 'EXPIRED', 'RESOLVED')
           AND mr.start_target_at < $3 AND $2 < mr.end_target_at
         LIMIT 1",
    )
    .bind(wallet)
    .bind(start_target_at)
    .bind(end_target_at)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(row.is_some())
}

async fn wallet_has_overlapping_ranked_queue(
    transaction: &mut Transaction<'_, Postgres>,
    wallet: &str,
    start_target_at: i64,
    end_target_at: i64,
) -> Result<bool, LeagueError> {
    let row = sqlx::query(
        "SELECT 1
         FROM ranked_queue q
         JOIN market_rounds mr ON mr.id = q.market_round_id
         WHERE q.wallet = $1
           AND q.status NOT IN ('CANCELLED', 'UNMATCHED', 'BLOCKED')
           AND mr.start_target_at < $3 AND $2 < mr.end_target_at
         LIMIT 1",
    )
    .bind(wallet)
    .bind(start_target_at)
    .bind(end_target_at)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(row.is_some())
}

async fn has_membership_started(
    transaction: &mut Transaction<'_, Postgres>,
    league_id: i64,
) -> Result<bool, LeagueError> {
    let row = sqlx::query(
        "SELECT 1 FROM league_memberships
         WHERE league_id = $1
           AND membership_state IN ('PENDING_JOIN', 'ACTIVE', 'PENDING_LEAVE')
         LIMIT 1",
    )
    .bind(league_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(row.is_some())
}

async fn load_membership(
    pool: &PgPool,
    league_id: i64,
    wallet: &str,
) -> Result<MembershipView, LeagueError> {
    sqlx::query(
        "SELECT league_id, wallet, membership_state, active, joined_at,
                bye_count, pending_until, onchain_member_pubkey
         FROM league_memberships
         WHERE league_id = $1 AND wallet = $2",
    )
    .bind(league_id)
    .bind(wallet)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(LeagueError::NotMember)
    .and_then(|row| {
        Ok(MembershipView {
            league_id: row.try_get("league_id").map_err(storage_error)?,
            wallet: row.try_get("wallet").map_err(storage_error)?,
            membership_status: row.try_get("membership_state").map_err(storage_error)?,
            active: row.try_get("active").map_err(storage_error)?,
            joined_at: row.try_get("joined_at").map_err(storage_error)?,
            bye_count: row.try_get("bye_count").map_err(storage_error)?,
            pending_until: row.try_get("pending_until").map_err(storage_error)?,
            member_pubkey: row
                .try_get("onchain_member_pubkey")
                .map_err(storage_error)?,
        })
    })
}

async fn expected_member_for_league(
    pool: &PgPool,
    league_id: i64,
    wallet: &str,
) -> Result<String, LeagueError> {
    let row = sqlx::query("SELECT chain_pubkey FROM leagues WHERE id = $1")
        .bind(league_id)
        .fetch_optional(pool)
        .await
        .map_err(storage_error)?
        .ok_or(LeagueError::NotFound)?;
    expected_member_pubkey(
        &row.try_get::<String, _>("chain_pubkey")
            .map_err(storage_error)?,
        wallet,
    )
}

async fn league_exists(pool: &PgPool, league_id: i64) -> Result<bool, LeagueError> {
    sqlx::query("SELECT 1 FROM leagues WHERE id = $1")
        .bind(league_id)
        .fetch_optional(pool)
        .await
        .map(|row| row.is_some())
        .map_err(storage_error)
}

async fn lock_scheduler(transaction: &mut Transaction<'_, Postgres>) -> Result<(), LeagueError> {
    sqlx::query("SELECT pg_advisory_xact_lock(6782414)")
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
    Ok(())
}

async fn lock_wallet(
    transaction: &mut Transaction<'_, Postgres>,
    wallet: &str,
) -> Result<(), LeagueError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(wallet)
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
    Ok(())
}

fn summary_from_row(row: &PgRow) -> Result<LeagueSummary, LeagueError> {
    Ok(LeagueSummary {
        id: row.try_get("id").map_err(storage_error)?,
        chain_pubkey: row.try_get("chain_pubkey").map_err(storage_error)?,
        name: row.try_get("name").map_err(storage_error)?,
        max_players: row.try_get("max_players").map_err(storage_error)?,
        joined_players: row.try_get("joined_players").map_err(storage_error)?,
        total_rounds: row.try_get("total_rounds").map_err(storage_error)?,
        current_round: row.try_get("current_round").map_err(storage_error)?,
        pairing_policy_version: row
            .try_get("pairing_policy_version")
            .map_err(storage_error)?,
        rated: row.try_get("rated").map_err(storage_error)?,
        registration_close_at: row
            .try_get("registration_close_at")
            .map_err(storage_error)?,
        status: row.try_get("status").map_err(storage_error)?,
    })
}

fn round_from_row(row: &PgRow) -> Result<LeagueRound, LeagueError> {
    Ok(LeagueRound {
        league_round_no: row.try_get("league_round_no").map_err(storage_error)?,
        market_round_id: row.try_get("market_round_id").map_err(storage_error)?,
        market_round_chain_pubkey: row.try_get("chain_pubkey").map_err(storage_error)?,
        market_round_state: row.try_get("state").map_err(storage_error)?,
        start_target_at: row.try_get("start_target_at").map_err(storage_error)?,
        end_target_at: row.try_get("end_target_at").map_err(storage_error)?,
    })
}

fn instruction_view(instruction: Instruction) -> InstructionView {
    InstructionView {
        program_id: bs58::encode(instruction.program_id.to_bytes()).into_string(),
        accounts: instruction
            .accounts
            .into_iter()
            .map(|account| InstructionAccountView {
                address: bs58::encode(account.pubkey.to_bytes()).into_string(),
                signer: account.is_signer,
                writable: account.is_writable,
            })
            .collect(),
        data_base58: bs58::encode(instruction.data).into_string(),
    }
}

fn validate_official_league(input: &OfficialLeagueInput<'_>) -> Result<(), LeagueError> {
    parse_chain_pubkey(input.chain_pubkey)?;
    if input.name.trim().is_empty() || input.name.trim().len() > 80 {
        return Err(LeagueError::InvalidName);
    }
    if !(2..=MAX_LEAGUE_PLAYERS).contains(&input.max_players)
        || input.total_rounds <= 0
        || input.pairing_policy_version <= 0
        || input.registration_close_at <= input.now
    {
        return Err(LeagueError::InvalidLeague);
    }
    Ok(())
}

fn parse_chain_pubkey(value: &str) -> Result<[u8; 32], LeagueError> {
    parse_wallet(value).map_err(|_| LeagueError::ChainIdentityUnavailable)
}

fn validate_league_id(league_id: i64) -> Result<(), LeagueError> {
    (league_id > 0)
        .then_some(())
        .ok_or(LeagueError::InvalidLeagueId)
}

fn validate_status_filter(status: &str) -> Result<(), LeagueError> {
    matches!(
        status,
        LEAGUE_REGISTRATION | LEAGUE_ACTIVE | LEAGUE_COMPLETED | LEAGUE_CANCELLED
    )
    .then_some(())
    .ok_or(LeagueError::InvalidLeague)
}

fn storage_error(error: sqlx::Error) -> LeagueError {
    LeagueError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(round_no: i32, market_round_id: i64, start: i64, end: i64) -> ScheduleEntry {
        ScheduleEntry {
            league_round_no: round_no,
            market_round_id,
            start_target_at: start,
            end_target_at: end,
        }
    }

    #[test]
    fn schedule_rejects_overlapping_market_rounds() {
        let schedule = vec![round(1, 10, 100, 200), round(2, 11, 199, 300)];

        assert_eq!(
            validate_schedule(&schedule),
            Err(LeagueError::ScheduleOverlap)
        );
    }

    #[test]
    fn schedule_accepts_adjacent_half_open_windows() {
        let schedule = vec![round(1, 10, 100, 200), round(2, 11, 200, 300)];

        assert_eq!(validate_schedule(&schedule), Ok(()));
    }

    #[test]
    fn schedule_rejects_duplicate_market_round_assignments() {
        let schedule = vec![round(1, 10, 100, 200), round(2, 10, 300, 400)];

        assert_eq!(
            validate_schedule(&schedule),
            Err(LeagueError::DuplicateMarketRound)
        );
    }

    #[test]
    fn schedule_rejects_duplicate_league_round_numbers() {
        let schedule = vec![round(1, 10, 100, 200), round(1, 11, 200, 300)];

        assert_eq!(
            validate_schedule(&schedule),
            Err(LeagueError::DuplicateLeagueRound)
        );
    }

    #[test]
    fn member_pda_derivation_is_stable_for_same_chain_identity_and_wallet() {
        let league = bs58::encode([12; 32]).into_string();
        let wallet = bs58::encode([11; 32]).into_string();

        let first = expected_member_pubkey(&league, &wallet).unwrap();
        let second = expected_member_pubkey(&league, &wallet).unwrap();

        assert_eq!(first, second);
        assert_ne!(first, league);
    }
}
