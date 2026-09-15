//! League catalog, membership intents, and Market Round reservations.
//!
//! The Anchor League and LeagueMember accounts are the competitive source of
//! truth. This module therefore records wallet actions as pending intents,
//! returns the exact wallet-signed instruction, and promotes those intents to
//! active only after a chain indexer confirms the corresponding account.
//! PostgreSQL transactions reserve every scheduled Market Round before a join
//! instruction is returned, so Ranked admission and another League cannot
//! claim the same wallet during an unresolved membership transition.

use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use protocol::{league_pairing_seed, league_standings_input_hash, pair_swiss, PairingPlayer};
use relay::{
    build_create_league_rated_battle_instruction, build_join_league_instruction,
    build_leave_league_instruction, config_pda, create_rated_battle_pdas, league_member_pda,
    LeagueMembershipAccounts, LeagueRatedBattleAccounts,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use solana_instruction::Instruction;
use sqlx::{postgres::PgRow, PgPool, Postgres, Row, Transaction};

use crate::auth::parse_wallet;
use crate::standings::{
    calculate_standings, LeagueBattleFact, LeagueBattleResult, LeagueByeFact, LeagueStanding,
};

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

/// A finalized chain block eligible to provide deterministic pairing entropy.
/// The chain reader supplies candidates; this module never chooses an
/// operator-selected block or treats an unfinalized block as randomness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizedEntropyCandidate {
    pub slot: i64,
    pub block_time: i64,
    pub blockhash: String,
    pub finalized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingEntropy {
    pub slot: i64,
    pub block_time: i64,
    pub blockhash: String,
    pub bytes: [u8; 32],
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
    EntropyUnavailable,
    InvalidEntropy,
    PairingNotReady,
    InvalidPairing,
    PairingConflict,
    CoordinatorPlanUnavailable,
    CoordinatorConflict,
    InvalidStandings,
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
            Self::EntropyUnavailable => "no finalized chain entropy is available for pairing",
            Self::InvalidEntropy => "pairing entropy is invalid",
            Self::PairingNotReady => "League pairing is not ready at the configured cutoff",
            Self::InvalidPairing => "League pairing state is invalid",
            Self::PairingConflict => "League pairing conflicts with existing state",
            Self::CoordinatorPlanUnavailable => "League coordinator Battle plan is unavailable",
            Self::CoordinatorConflict => "League coordinator Battle confirmation conflicts",
            Self::InvalidStandings => "finalized League Battle data cannot produce standings",
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

#[derive(Debug, Clone, Serialize)]
pub struct LeaguePairingView {
    pub pairing_id: i64,
    pub league_round_no: i32,
    pub market_round_id: i64,
    pub player_a: String,
    pub player_b: String,
    pub pairing_seed_hex: String,
    pub seed_source_slot: i64,
    pub seed_source_blockhash: String,
    pub repeat_relaxed: bool,
    pub battle_pubkey: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LeaguePairingRun {
    pub league_id: i64,
    pub league_round_no: i32,
    pub market_round_id: i64,
    pub pairing_policy_version: i32,
    pub pairing_seed_hex: String,
    pub standings_input_hash_hex: String,
    pub seed_source_slot: i64,
    pub seed_source_blockhash: String,
    pub bye_wallet: Option<String>,
    pub pairings: Vec<LeaguePairingView>,
}

/// Public projection of one deterministic League ranking. Wallets remain
/// strings at the HTTP boundary while the standings engine uses fixed-width
/// public-key bytes for canonical ordering and arithmetic.
#[derive(Debug, Clone, Serialize)]
pub struct LeagueStandingView {
    pub rank: u32,
    pub wallet: String,
    pub league_points: u32,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
    pub byes: u32,
    pub buchholz_sos: u32,
    pub head_to_head_points: u32,
    pub cumulative_margin_q9: i64,
}

/// Standings response with enough progress metadata for clients to avoid
/// presenting a partial table as the final League result.
#[derive(Debug, Clone, Serialize)]
pub struct LeagueStandings {
    pub league: LeagueSummary,
    pub standings: Vec<LeagueStandingView>,
    pub resolved_rounds: u32,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct LeagueBattleAccounts {
    pub config: [u8; 32],
    pub coordinator: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct LeagueBattlePlan {
    pub pairing_id: i64,
    pub battle_id: u64,
    pub market_round_id: i64,
    pub league_round_no: i32,
    pub player_a: String,
    pub player_b: String,
    pub instruction: Instruction,
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
    current_round: i32,
    pairing_policy_version: i32,
    rated: bool,
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

/// Chooses the lowest finalized slot whose block time reaches the pairing
/// cutoff. Sorting by slot makes the result independent of RPC response order,
/// while retaining the selected slot and blockhash for the audit record.
pub fn select_pairing_entropy(
    candidates: &[FinalizedEntropyCandidate],
    pairing_cutoff_at: i64,
) -> Result<PairingEntropy, LeagueError> {
    let mut eligible = candidates
        .iter()
        .filter(|candidate| candidate.finalized && candidate.block_time >= pairing_cutoff_at)
        .collect::<Vec<_>>();
    eligible.sort_by_key(|candidate| (candidate.slot, candidate.blockhash.as_str()));
    let candidate = eligible.first().ok_or(LeagueError::EntropyUnavailable)?;
    if candidate.slot < 0 {
        return Err(LeagueError::InvalidEntropy);
    }
    let bytes = parse_blockhash(&candidate.blockhash)?;
    Ok(PairingEntropy {
        slot: candidate.slot,
        block_time: candidate.block_time,
        blockhash: candidate.blockhash.clone(),
        bytes,
    })
}

/// Returns the canonical on-chain LeagueMember PDA for a wallet.
pub fn expected_member_pubkey(league_pubkey: &str, wallet: &str) -> Result<String, LeagueError> {
    let league = parse_wallet(league_pubkey).map_err(|_| LeagueError::ChainIdentityUnavailable)?;
    let player = parse_wallet(wallet).map_err(|_| LeagueError::InvalidWallet)?;
    Ok(bs58::encode(league_member_pda(league, player)).into_string())
}

pub fn validate_league_chain_pubkey(league_pubkey: &str) -> Result<(), LeagueError> {
    parse_chain_pubkey(league_pubkey).map(|_| ())
}

fn validate_pairing_entropy(entropy: &PairingEntropy) -> Result<(), LeagueError> {
    if entropy.slot < 0
        || entropy.block_time < 0
        || parse_blockhash(&entropy.blockhash)? != entropy.bytes
    {
        return Err(LeagueError::InvalidEntropy);
    }
    Ok(())
}

fn parse_blockhash(value: &str) -> Result<[u8; 32], LeagueError> {
    parse_wallet(value).map_err(|_| LeagueError::InvalidEntropy)
}

fn parse_player_pubkey(value: &str) -> Result<[u8; 32], LeagueError> {
    parse_wallet(value).map_err(|_| LeagueError::InvalidWallet)
}

fn db_seed(row: &PgRow) -> Result<[u8; 32], LeagueError> {
    db_bytes32(row, "pairing_seed")
}

fn db_bytes32(row: &PgRow, column: &str) -> Result<[u8; 32], LeagueError> {
    let bytes: Vec<u8> = row.try_get(column).map_err(storage_error)?;
    bytes.try_into().map_err(|_| LeagueError::InvalidPairing)
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

/// Rebuilds the public League table exclusively from active memberships and
/// terminal indexed Battle accounts. The query joins through the immutable
/// pairing record, so an unrelated Battle projection cannot affect League
/// points, SOS, head-to-head, or score-margin tie-breakers.
pub async fn get_league_standings(
    pool: &PgPool,
    league_id: i64,
) -> Result<LeagueStandings, LeagueError> {
    validate_league_id(league_id)?;
    let league = load_league_summary(pool, league_id).await?;

    let member_rows = sqlx::query(
        "SELECT wallet
         FROM league_memberships
         WHERE league_id = $1
           AND membership_state = 'ACTIVE'
           AND active = TRUE
         ORDER BY wallet ASC",
    )
    .bind(league_id)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?;
    let members = member_rows
        .iter()
        .map(|row| {
            let wallet: String = row.try_get("wallet").map_err(storage_error)?;
            parse_player_pubkey(&wallet).map_err(|_| LeagueError::InvalidStandings)
        })
        .collect::<Result<Vec<_>, LeagueError>>()?;

    let battle_rows = sqlx::query(
        "SELECT p.league_round_no, p.player_a, p.player_b,
                b.result, b.score_a_q9, b.score_b_q9
         FROM league_pairings p
         JOIN battles b ON b.chain_pubkey = p.battle_pubkey
         WHERE p.league_id = $1
           AND p.status = 'CREATED'
           AND b.state IN ('FINALIZED', 'SETTLED', 'VOIDED')
           AND b.result IS NOT NULL
         ORDER BY p.league_round_no ASC, p.id ASC",
    )
    .bind(league_id)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?;
    let battles = battle_rows
        .iter()
        .map(|row| {
            let result: String = row.try_get("result").map_err(storage_error)?;
            Ok(LeagueBattleFact {
                round_no: row.try_get("league_round_no").map_err(storage_error)?,
                player_a: parse_standings_wallet(row, "player_a")?,
                player_b: parse_standings_wallet(row, "player_b")?,
                result: parse_standings_result(&result).ok_or(LeagueError::InvalidStandings)?,
                score_a_q9: row.try_get("score_a_q9").map_err(storage_error)?,
                score_b_q9: row.try_get("score_b_q9").map_err(storage_error)?,
            })
        })
        .collect::<Result<Vec<_>, LeagueError>>()?;

    // A bye contributes only after the complete pairing run is resolvable.
    // This prevents a currently scheduled bye from appearing as earned points
    // while the same round's opponent Battles are still in progress.
    let bye_rows = sqlx::query(
        "SELECT r.league_round_no, r.bye_wallet
         FROM league_pairing_runs r
         WHERE r.league_id = $1
           AND r.bye_wallet IS NOT NULL
           AND r.status = 'COMPLETE'
           AND NOT EXISTS (
               SELECT 1
               FROM league_pairings p
               LEFT JOIN battles b ON b.chain_pubkey = p.battle_pubkey
               WHERE p.run_id = r.id
                 AND (
                     p.status <> 'CREATED'
                     OR b.chain_pubkey IS NULL
                     OR b.state NOT IN ('FINALIZED', 'SETTLED', 'VOIDED')
                     OR b.result IS NULL
                 )
           )
         ORDER BY r.league_round_no ASC",
    )
    .bind(league_id)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?;
    let byes = bye_rows
        .iter()
        .map(|row| {
            Ok(LeagueByeFact {
                round_no: row.try_get("league_round_no").map_err(storage_error)?,
                wallet: parse_standings_wallet(row, "bye_wallet")?,
            })
        })
        .collect::<Result<Vec<_>, LeagueError>>()?;

    let latest_seed = sqlx::query(
        "SELECT pairing_seed
         FROM league_pairing_runs
         WHERE league_id = $1
         ORDER BY league_round_no DESC
         LIMIT 1",
    )
    .bind(league_id)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .map(|row| db_seed(&row))
    .transpose()?
    .unwrap_or([0; 32]);
    let league_pubkey =
        parse_chain_pubkey(&league.chain_pubkey).map_err(|_| LeagueError::InvalidStandings)?;
    let final_seed = standings_final_seed(league_pubkey, latest_seed);

    let resolved_round_set = battles
        .iter()
        .map(|battle| battle.round_no)
        .chain(byes.iter().map(|bye| bye.round_no))
        .collect::<HashSet<_>>();
    let resolved_rounds =
        u32::try_from(resolved_round_set.len()).map_err(|_| LeagueError::InvalidStandings)?;
    let complete = (1..=league.total_rounds).all(|round| resolved_round_set.contains(&round));

    let standings = if members.is_empty() {
        Vec::new()
    } else {
        calculate_standings(&members, &battles, &byes, final_seed)
            .map_err(|_| LeagueError::InvalidStandings)?
            .into_iter()
            .map(standing_view)
            .collect()
    };

    Ok(LeagueStandings {
        league,
        standings,
        resolved_rounds,
        complete,
    })
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

/// Reconciles the lifecycle fields written by the on-chain League account.
/// Membership rows remain wallet-intent projections; the indexed account is
/// still authoritative for whether registration or a round is active.
pub async fn reconcile_league_state(
    pool: &PgPool,
    chain_pubkey: &str,
    state: &str,
    joined_players: i32,
    current_round: i32,
    indexed_at: i64,
) -> Result<(), LeagueError> {
    parse_chain_pubkey(chain_pubkey)?;
    if !matches!(
        state,
        LEAGUE_REGISTRATION | LEAGUE_ACTIVE | LEAGUE_COMPLETED | LEAGUE_CANCELLED
    ) || !(0..=MAX_LEAGUE_PLAYERS).contains(&joined_players)
        || current_round < 0
    {
        return Err(LeagueError::InvalidLeague);
    }
    let result = sqlx::query(
        "UPDATE leagues
         SET status = $2, onchain_joined_players = $3,
             current_round = $4, updated_at = $5
         WHERE chain_pubkey = $1",
    )
    .bind(chain_pubkey)
    .bind(state)
    .bind(joined_players)
    .bind(current_round)
    .bind(indexed_at)
    .execute(pool)
    .await
    .map_err(storage_error)?;
    if result.rows_affected() != 1 {
        return Err(LeagueError::NotFound);
    }
    Ok(())
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

/// Computes and durably records one deterministic Swiss pairing run.
///
/// The caller supplies entropy selected from finalized chain blocks. The
/// transaction locks the League pairing lane, snapshots current ratings and
/// prior formal pairings, and writes the complete pair/participant set before
/// any coordinator Battle exists. A retry returns the original immutable run.
pub async fn create_league_pairings(
    pool: &PgPool,
    league_id: i64,
    league_round_no: i32,
    entropy: &PairingEntropy,
    now: i64,
) -> Result<LeaguePairingRun, LeagueError> {
    validate_league_id(league_id)?;
    if league_round_no <= 0 {
        return Err(LeagueError::InvalidPairing);
    }
    validate_pairing_entropy(entropy)?;

    let mut transaction = pool.begin().await.map_err(storage_error)?;
    lock_scheduler(&mut transaction).await?;
    let league = load_league_for_update(&mut transaction, league_id).await?;

    let existing_run = sqlx::query(
        "SELECT id
         FROM league_pairing_runs
         WHERE league_id = $1 AND league_round_no = $2
         FOR UPDATE",
    )
    .bind(league_id)
    .bind(league_round_no)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(storage_error)?;
    if let Some(row) = existing_run {
        let run_id: i64 = row.try_get("id").map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        return load_pairing_run(pool, run_id).await;
    }

    if league.status != LEAGUE_ACTIVE
        || !league.rated
        || league.current_round != league_round_no
        || league_round_no > league.total_rounds
    {
        return Err(LeagueError::PairingNotReady);
    }
    ensure_previous_league_round_resolved(&mut transaction, league_id, league_round_no).await?;
    let market_round = load_pairing_market_round(&mut transaction, league_id, league_round_no)
        .await?
        .ok_or(LeagueError::PairingNotReady)?;
    if market_round.chain_pubkey.is_none()
        || market_round.is_replay
        || !matches!(market_round.state.as_str(), "SCHEDULED" | "COMMIT_OPEN")
        || now >= market_round.start_target_at
        || now < market_round.queue_close_at
        || entropy.block_time < market_round.queue_close_at
        || entropy.block_time > now
    {
        return Err(LeagueError::PairingNotReady);
    }
    parse_chain_pubkey(
        market_round
            .chain_pubkey
            .as_deref()
            .ok_or(LeagueError::PairingNotReady)?,
    )
    .map_err(|_| LeagueError::InvalidPairing)?;
    let league_pubkey = parse_chain_pubkey(&league.chain_pubkey)?;
    let round_number = u16::try_from(league_round_no).map_err(|_| LeagueError::InvalidPairing)?;
    let pairing_seed = league_pairing_seed(league_pubkey, round_number, entropy.bytes);

    let candidates = load_pairing_candidates(&mut transaction, league_id, league_round_no).await?;
    if candidates.len() < 2 {
        return Err(LeagueError::InvalidPairing);
    }
    let protocol_players = candidates
        .iter()
        .map(|candidate| PairingPlayer {
            wallet: candidate.wallet_bytes,
            league_points: candidate.league_points,
            rating: candidate.rating,
            bye_count: candidate.bye_count,
            prior_opponents: candidate.prior_opponents.clone(),
        })
        .collect::<Vec<_>>();
    let standings_input_hash = league_standings_input_hash(&protocol_players);
    let pairing =
        pair_swiss(&protocol_players, pairing_seed).map_err(|_| LeagueError::InvalidPairing)?;
    if pairing.pairs.is_empty() && pairing.bye.is_none() {
        return Err(LeagueError::InvalidPairing);
    }
    let bye_wallet = pairing.bye.map(|index| candidates[index].wallet.as_str());
    let run_id = sqlx::query(
        "INSERT INTO league_pairing_runs
            (league_id, league_round_no, market_round_id, pairing_seed,
             pairing_policy_version, standings_input_hash,
             seed_source_slot, seed_source_blockhash, bye_wallet, status, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'PLANNED', $10)
         RETURNING id",
    )
    .bind(league_id)
    .bind(league_round_no)
    .bind(market_round.market_round_id)
    .bind(pairing_seed.as_slice())
    .bind(league.pairing_policy_version)
    .bind(standings_input_hash.as_slice())
    .bind(entropy.slot)
    .bind(&entropy.blockhash)
    .bind(bye_wallet)
    .bind(now)
    .fetch_one(&mut *transaction)
    .await
    .map_err(storage_error)?
    .try_get::<i64, _>("id")
    .map_err(storage_error)?;

    if let Some(bye_wallet) = bye_wallet {
        let result = sqlx::query(
            "UPDATE league_memberships
             SET bye_count = bye_count + 1, updated_at = $3
             WHERE league_id = $1 AND wallet = $2
               AND membership_state = 'ACTIVE' AND active = TRUE",
        )
        .bind(league_id)
        .bind(bye_wallet)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            return Err(LeagueError::PairingConflict);
        }
    }

    for (left, right) in pairing.pairs {
        let player_a = &candidates[left];
        let player_b = &candidates[right];
        let repeat_relaxed = player_a.prior_opponents.contains(&player_b.wallet_bytes)
            || player_b.prior_opponents.contains(&player_a.wallet_bytes);
        let pairing_id = sqlx::query(
            "INSERT INTO league_pairings
                (run_id, league_id, league_round_no, market_round_id,
                 player_a, player_b, rating_a_snapshot, rating_b_snapshot,
                 pairing_seed, seed_source_slot, seed_source_blockhash,
                 repeat_relaxed, status, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                     'PENDING_COORDINATOR', $13)
             RETURNING id",
        )
        .bind(run_id)
        .bind(league_id)
        .bind(league_round_no)
        .bind(market_round.market_round_id)
        .bind(&player_a.wallet)
        .bind(&player_b.wallet)
        .bind(player_a.rating)
        .bind(player_b.rating)
        .bind(pairing_seed.as_slice())
        .bind(entropy.slot)
        .bind(&entropy.blockhash)
        .bind(repeat_relaxed)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if pairing_id.rows_affected() != 1 {
            return Err(LeagueError::PairingConflict);
        }
        sqlx::query(
            "INSERT INTO league_pairing_participants
                (run_id, league_id, league_round_no, wallet, side)
             VALUES ($1, $2, $3, $4, 'A'), ($1, $2, $3, $5, 'B')",
        )
        .bind(run_id)
        .bind(league_id)
        .bind(league_round_no)
        .bind(&player_a.wallet)
        .bind(&player_b.wallet)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
    }
    transaction.commit().await.map_err(storage_error)?;
    load_pairing_run(pool, run_id).await
}

/// Prevents the League scheduler from pairing a new round while the preceding
/// round still has an uncreated Battle, an unresolved Battle result, or no
/// persisted pairing run at all. A League round may be voided, but that void
/// must still be represented by a terminal indexed result before advancement.
async fn ensure_previous_league_round_resolved(
    transaction: &mut Transaction<'_, Postgres>,
    league_id: i64,
    league_round_no: i32,
) -> Result<(), LeagueError> {
    if league_round_no <= 1 {
        return Ok(());
    }
    let previous_round = league_round_no - 1;
    let row = sqlx::query(
        "SELECT EXISTS (
             SELECT 1
             FROM league_pairing_runs r
             WHERE r.league_id = $1
               AND r.league_round_no = $2
               AND r.status IN ('IN_PROGRESS', 'COMPLETE')
               AND NOT EXISTS (
                   SELECT 1
                   FROM league_pairings p
                   LEFT JOIN battles b ON b.chain_pubkey = p.battle_pubkey
                   WHERE p.run_id = r.id
                     AND (
                         p.status <> 'CREATED'
                         OR p.battle_pubkey IS NULL
                         OR b.chain_pubkey IS NULL
                         OR b.state NOT IN ('FINALIZED', 'SETTLED', 'VOIDED')
                         OR b.result IS NULL
                     )
               )
         ) AS resolved",
    )
    .bind(league_id)
    .bind(previous_round)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage_error)?;
    if !row.try_get("resolved").map_err(storage_error)? {
        return Err(LeagueError::PairingNotReady);
    }
    Ok(())
}

/// Returns the coordinator transaction plan for one persisted League pairing.
/// The LeagueMember accounts are read-only identity proofs; the coordinator
/// creates the Battle and both RatedSlots atomically on chain.
pub async fn league_coordinator_battle_plan(
    pool: &PgPool,
    pairing_id: i64,
    accounts: LeagueBattleAccounts,
) -> Result<LeagueBattlePlan, LeagueError> {
    if pairing_id <= 0 {
        return Err(LeagueError::InvalidPairing);
    }
    if accounts.config != config_pda() {
        return Err(LeagueError::CoordinatorPlanUnavailable);
    }
    let row = sqlx::query(
        "SELECT p.id, p.league_id, p.league_round_no, p.market_round_id,
                p.player_a, p.player_b, p.rating_a_snapshot,
                p.rating_b_snapshot, p.status, p.battle_pubkey,
                l.chain_pubkey AS league_pubkey, l.status AS league_status,
                l.rated, l.current_round, mr.chain_pubkey AS market_round_pubkey,
                mr.state AS market_round_state, mr.is_replay,
                ma.onchain_member_pubkey AS member_a,
                mb.onchain_member_pubkey AS member_b
         FROM league_pairings p
         JOIN leagues l ON l.id = p.league_id
         JOIN market_rounds mr ON mr.id = p.market_round_id
         JOIN league_memberships ma
           ON ma.league_id = p.league_id AND ma.wallet = p.player_a
          AND ma.membership_state = 'ACTIVE' AND ma.active = TRUE
         JOIN league_memberships mb
           ON mb.league_id = p.league_id AND mb.wallet = p.player_b
          AND mb.membership_state = 'ACTIVE' AND mb.active = TRUE
         WHERE p.id = $1",
    )
    .bind(pairing_id)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(LeagueError::CoordinatorPlanUnavailable)?;
    let status: String = row.try_get("status").map_err(storage_error)?;
    if status != "PENDING_COORDINATOR" {
        return Err(LeagueError::CoordinatorPlanUnavailable);
    }
    let league_status: String = row.try_get("league_status").map_err(storage_error)?;
    let rated: bool = row.try_get("rated").map_err(storage_error)?;
    let league_round_no: i32 = row.try_get("league_round_no").map_err(storage_error)?;
    let current_round: i32 = row.try_get("current_round").map_err(storage_error)?;
    let market_round_state: String = row.try_get("market_round_state").map_err(storage_error)?;
    let is_replay: bool = row.try_get("is_replay").map_err(storage_error)?;
    if league_status != LEAGUE_ACTIVE
        || !rated
        || league_round_no != current_round
        || is_replay
        || !matches!(market_round_state.as_str(), "SCHEDULED" | "COMMIT_OPEN")
    {
        return Err(LeagueError::CoordinatorPlanUnavailable);
    }
    let league_pubkey: [u8; 32] = parse_chain_pubkey(
        &row.try_get::<String, _>("league_pubkey")
            .map_err(storage_error)?,
    )?;
    let market_round: [u8; 32] = parse_chain_pubkey(
        &row.try_get::<Option<String>, _>("market_round_pubkey")
            .map_err(storage_error)?
            .ok_or(LeagueError::CoordinatorPlanUnavailable)?,
    )?;
    let player_a: String = row.try_get("player_a").map_err(storage_error)?;
    let player_b: String = row.try_get("player_b").map_err(storage_error)?;
    let player_a_bytes = parse_player_pubkey(&player_a)?;
    let player_b_bytes = parse_player_pubkey(&player_b)?;
    let expected_member_a_bytes = league_member_pda(league_pubkey, player_a_bytes);
    let expected_member_b_bytes = league_member_pda(league_pubkey, player_b_bytes);
    let expected_member_a = bs58::encode(expected_member_a_bytes).into_string();
    let expected_member_b = bs58::encode(expected_member_b_bytes).into_string();
    let stored_member_a: Option<String> = row.try_get("member_a").map_err(storage_error)?;
    let stored_member_b: Option<String> = row.try_get("member_b").map_err(storage_error)?;
    if stored_member_a.as_deref() != Some(expected_member_a.as_str())
        || stored_member_b.as_deref() != Some(expected_member_b.as_str())
    {
        return Err(LeagueError::CoordinatorPlanUnavailable);
    }
    let battle_id = u64::try_from(pairing_id).map_err(|_| LeagueError::InvalidPairing)?;
    let (battle, rated_slot_a, rated_slot_b) =
        create_rated_battle_pdas(market_round, battle_id, player_a_bytes, player_b_bytes);
    let instruction = build_create_league_rated_battle_instruction(
        battle_id,
        u16::try_from(league_round_no).map_err(|_| LeagueError::InvalidPairing)?,
        row.try_get("rating_a_snapshot").map_err(storage_error)?,
        row.try_get("rating_b_snapshot").map_err(storage_error)?,
        crate::rating::RATING_FORMULA_VERSION,
        LeagueRatedBattleAccounts {
            config: accounts.config,
            coordinator: accounts.coordinator,
            market_round,
            player_a: player_a_bytes,
            player_b: player_b_bytes,
            league: league_pubkey,
            league_member_a: expected_member_a_bytes,
            league_member_b: expected_member_b_bytes,
            battle,
            rated_slot_a,
            rated_slot_b,
        },
    );
    Ok(LeagueBattlePlan {
        pairing_id,
        battle_id,
        market_round_id: row.try_get("market_round_id").map_err(storage_error)?,
        league_round_no,
        player_a,
        player_b,
        instruction,
    })
}

/// Materializes one confirmed League Battle and both rated exposures in one
/// idempotent transaction after the coordinator transaction is finalized.
pub async fn confirm_league_coordinator_battle(
    pool: &PgPool,
    pairing_id: i64,
    battle_pubkey: &str,
    now: i64,
) -> Result<(), LeagueError> {
    if pairing_id <= 0 {
        return Err(LeagueError::InvalidPairing);
    }
    let battle_bytes =
        parse_player_pubkey(battle_pubkey).map_err(|_| LeagueError::CoordinatorConflict)?;
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    lock_scheduler(&mut transaction).await?;
    let row = sqlx::query(
        "SELECT p.id, p.league_id, p.league_round_no, p.market_round_id,
                p.player_a, p.player_b, p.rating_a_snapshot,
                p.rating_b_snapshot, p.status, p.battle_pubkey,
                l.chain_pubkey AS league_pubkey,
                mr.chain_pubkey AS market_round_pubkey
         FROM league_pairings p
         JOIN leagues l ON l.id = p.league_id
         JOIN market_rounds mr ON mr.id = p.market_round_id
         WHERE p.id = $1
         FOR UPDATE",
    )
    .bind(pairing_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(storage_error)?
    .ok_or(LeagueError::CoordinatorPlanUnavailable)?;
    let status: String = row.try_get("status").map_err(storage_error)?;
    let stored_battle: Option<String> = row.try_get("battle_pubkey").map_err(storage_error)?;
    if status == "CREATED" {
        if stored_battle.as_deref() == Some(battle_pubkey) {
            transaction.commit().await.map_err(storage_error)?;
            return Ok(());
        }
        return Err(LeagueError::CoordinatorConflict);
    }
    if status != "PENDING_COORDINATOR" || stored_battle.is_some() {
        return Err(LeagueError::CoordinatorConflict);
    }
    let market_round_id: i64 = row.try_get("market_round_id").map_err(storage_error)?;
    let league_id: i64 = row.try_get("league_id").map_err(storage_error)?;
    let league_round_no: i32 = row.try_get("league_round_no").map_err(storage_error)?;
    let player_a: String = row.try_get("player_a").map_err(storage_error)?;
    let player_b: String = row.try_get("player_b").map_err(storage_error)?;
    let market_round = parse_chain_pubkey(
        &row.try_get::<Option<String>, _>("market_round_pubkey")
            .map_err(storage_error)?
            .ok_or(LeagueError::CoordinatorPlanUnavailable)?,
    )?;
    let _league = parse_chain_pubkey(
        &row.try_get::<String, _>("league_pubkey")
            .map_err(storage_error)?,
    )?;
    let player_a_bytes = parse_player_pubkey(&player_a)?;
    let player_b_bytes = parse_player_pubkey(&player_b)?;
    let battle_id = u64::try_from(pairing_id).map_err(|_| LeagueError::InvalidPairing)?;
    let (expected_battle, _, _) =
        create_rated_battle_pdas(market_round, battle_id, player_a_bytes, player_b_bytes);
    if battle_bytes != expected_battle {
        return Err(LeagueError::CoordinatorConflict);
    }
    let battle_insert = sqlx::query(
        "INSERT INTO battles
            (chain_pubkey, market_round_id, mode, rated, league_id,
             league_round_no, player_a, player_b, state, indexed_at,
             rating_a_before, rating_b_before, rating_formula_version)
         VALUES ($1, $2, 'LEAGUE', TRUE, $3, $4, $5, $6, 'CREATED', $7,
                 $8, $9, $10)
         ON CONFLICT (chain_pubkey) DO UPDATE SET
             league_id = EXCLUDED.league_id,
             league_round_no = EXCLUDED.league_round_no,
             rating_a_before = COALESCE(battles.rating_a_before, EXCLUDED.rating_a_before),
             rating_b_before = COALESCE(battles.rating_b_before, EXCLUDED.rating_b_before),
             rating_formula_version = COALESCE(
                 battles.rating_formula_version, EXCLUDED.rating_formula_version
             )
         WHERE battles.market_round_id = EXCLUDED.market_round_id
           AND battles.player_a = EXCLUDED.player_a
           AND battles.player_b = EXCLUDED.player_b
           AND battles.rated
           AND battles.mode = 'LEAGUE'",
    )
    .bind(battle_pubkey)
    .bind(market_round_id)
    .bind(league_id)
    .bind(league_round_no)
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
    .bind(i32::from(crate::rating::RATING_FORMULA_VERSION))
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    if battle_insert.rows_affected() != 1 {
        return Err(LeagueError::CoordinatorConflict);
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
            return Err(LeagueError::CoordinatorConflict);
        }
    }
    let pairing_update = sqlx::query(
        "UPDATE league_pairings
         SET battle_pubkey = $1, status = 'CREATED'
         WHERE id = $2 AND status = 'PENDING_COORDINATOR'",
    )
    .bind(battle_pubkey)
    .bind(pairing_id)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    if pairing_update.rows_affected() != 1 {
        return Err(LeagueError::CoordinatorConflict);
    }

    let participant_update = sqlx::query(
        "UPDATE league_pairing_participants
         SET battle_pubkey = $1
         WHERE run_id = (SELECT run_id FROM league_pairings WHERE id = $2)
           AND league_id = $3 AND league_round_no = $4
           AND wallet IN ($5, $6)",
    )
    .bind(battle_pubkey)
    .bind(pairing_id)
    .bind(league_id)
    .bind(league_round_no)
    .bind(&player_a)
    .bind(&player_b)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    if participant_update.rows_affected() != 2 {
        return Err(LeagueError::CoordinatorConflict);
    }

    let run_update = sqlx::query(
        "UPDATE league_pairing_runs run
         SET status = CASE
             WHEN NOT EXISTS (
                 SELECT 1 FROM league_pairings p
                 WHERE p.run_id = run.id AND p.status = 'PENDING_COORDINATOR'
             ) THEN 'COMPLETE'
             ELSE 'IN_PROGRESS'
         END
         WHERE run.id = (SELECT run_id FROM league_pairings WHERE id = $1)",
    )
    .bind(pairing_id)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    if run_update.rows_affected() != 1 {
        return Err(LeagueError::CoordinatorConflict);
    }
    transaction.commit().await.map_err(storage_error)?;
    Ok(())
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
        "SELECT chain_pubkey, max_players, total_rounds, current_round,
                pairing_policy_version, rated, registration_close_at, status
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

async fn load_pairing_market_round(
    transaction: &mut Transaction<'_, Postgres>,
    league_id: i64,
    league_round_no: i32,
) -> Result<Option<PairingMarketRound>, LeagueError> {
    sqlx::query(
        "SELECT s.market_round_id, mr.chain_pubkey, mr.state, mr.is_replay,
                mr.queue_close_at, mr.start_target_at
         FROM league_round_schedule s
         JOIN market_rounds mr ON mr.id = s.market_round_id
         WHERE s.league_id = $1 AND s.league_round_no = $2
         FOR UPDATE OF mr",
    )
    .bind(league_id)
    .bind(league_round_no)
    .fetch_optional(&mut **transaction)
    .await
    .map(|row| {
        row.map(|row| {
            Ok(PairingMarketRound {
                market_round_id: row.try_get("market_round_id").map_err(storage_error)?,
                chain_pubkey: row.try_get("chain_pubkey").map_err(storage_error)?,
                state: row.try_get("state").map_err(storage_error)?,
                is_replay: row.try_get("is_replay").map_err(storage_error)?,
                queue_close_at: row.try_get("queue_close_at").map_err(storage_error)?,
                start_target_at: row.try_get("start_target_at").map_err(storage_error)?,
            })
        })
    })
    .map_err(storage_error)?
    .transpose()
}

#[derive(Debug, Clone)]
struct PairingMarketRound {
    market_round_id: i64,
    chain_pubkey: Option<String>,
    state: String,
    is_replay: bool,
    queue_close_at: i64,
    start_target_at: i64,
}

#[derive(Debug, Clone)]
struct PairingCandidate {
    wallet: String,
    wallet_bytes: [u8; 32],
    league_points: u32,
    rating: i32,
    bye_count: u16,
    prior_opponents: Vec<[u8; 32]>,
}

async fn load_pairing_candidates(
    transaction: &mut Transaction<'_, Postgres>,
    league_id: i64,
    league_round_no: i32,
) -> Result<Vec<PairingCandidate>, LeagueError> {
    let mut points = HashMap::<String, u32>::new();
    let score_rows = sqlx::query(
        "SELECT p.player_a, p.player_b, b.result
         FROM league_pairings p
         JOIN battles b ON b.chain_pubkey = p.battle_pubkey
         WHERE p.league_id = $1 AND p.league_round_no < $2
           AND b.state IN ('FINALIZED', 'SETTLED', 'VOIDED')
           AND b.result IS NOT NULL",
    )
    .bind(league_id)
    .bind(league_round_no)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    for row in score_rows {
        let player_a: String = row.try_get("player_a").map_err(storage_error)?;
        let player_b: String = row.try_get("player_b").map_err(storage_error)?;
        let result: String = row.try_get("result").map_err(storage_error)?;
        let (points_a, points_b) =
            league_result_points(&result).ok_or(LeagueError::InvalidPairing)?;
        let current_a = points.get(&player_a).copied().unwrap_or_default();
        let current_b = points.get(&player_b).copied().unwrap_or_default();
        let next_a = current_a
            .checked_add(points_a)
            .ok_or(LeagueError::InvalidPairing)?;
        let next_b = current_b
            .checked_add(points_b)
            .ok_or(LeagueError::InvalidPairing)?;
        points.insert(player_a, next_a);
        points.insert(player_b, next_b);
    }

    let bye_rows = sqlx::query(
        "SELECT bye_wallet
         FROM league_pairing_runs
         WHERE league_id = $1 AND league_round_no < $2
           AND bye_wallet IS NOT NULL",
    )
    .bind(league_id)
    .bind(league_round_no)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    for row in bye_rows {
        let wallet: String = row.try_get("bye_wallet").map_err(storage_error)?;
        let current = points.get(&wallet).copied().unwrap_or_default();
        points.insert(
            wallet,
            current.checked_add(3).ok_or(LeagueError::InvalidPairing)?,
        );
    }

    let mut prior_opponents = HashMap::<String, Vec<[u8; 32]>>::new();
    let prior_rows = sqlx::query(
        "SELECT player_a, player_b
         FROM league_pairings
         WHERE league_id = $1 AND league_round_no < $2",
    )
    .bind(league_id)
    .bind(league_round_no)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    for row in prior_rows {
        let player_a: String = row.try_get("player_a").map_err(storage_error)?;
        let player_b: String = row.try_get("player_b").map_err(storage_error)?;
        let player_a_bytes = parse_player_pubkey(&player_a)?;
        let player_b_bytes = parse_player_pubkey(&player_b)?;
        prior_opponents
            .entry(player_a)
            .or_default()
            .push(player_b_bytes);
        prior_opponents
            .entry(player_b)
            .or_default()
            .push(player_a_bytes);
    }

    let member_rows = sqlx::query(
        "SELECT m.wallet, m.bye_count,
                COALESCE(r.rating, 1500)::INTEGER AS rating
         FROM league_memberships m
         LEFT JOIN ratings r
           ON r.wallet = m.wallet
          AND r.season_id = (SELECT id FROM seasons WHERE status = 'ACTIVE')
         WHERE m.league_id = $1
           AND m.membership_state = 'ACTIVE'
           AND m.active = TRUE
         ORDER BY m.wallet ASC",
    )
    .bind(league_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    member_rows
        .into_iter()
        .map(|row| {
            let wallet: String = row.try_get("wallet").map_err(storage_error)?;
            let bye_count: i32 = row.try_get("bye_count").map_err(storage_error)?;
            let bye_count = u16::try_from(bye_count).map_err(|_| LeagueError::InvalidPairing)?;
            Ok(PairingCandidate {
                wallet_bytes: parse_player_pubkey(&wallet)?,
                league_points: points.get(&wallet).copied().unwrap_or_default(),
                rating: row.try_get("rating").map_err(storage_error)?,
                bye_count,
                prior_opponents: prior_opponents.remove(&wallet).unwrap_or_default(),
                wallet,
            })
        })
        .collect()
}

fn league_result_points(result: &str) -> Option<(u32, u32)> {
    match result {
        "PLAYER_A" => Some((3, 0)),
        "PLAYER_B" => Some((0, 3)),
        "DRAW" => Some((1, 1)),
        "FORFEIT_A" => Some((0, 3)),
        "FORFEIT_B" => Some((3, 0)),
        "BOTH_FORFEIT" | "VOIDED" => Some((0, 0)),
        _ => None,
    }
}

async fn load_pairing_run(pool: &PgPool, run_id: i64) -> Result<LeaguePairingRun, LeagueError> {
    let run = sqlx::query(
        "SELECT league_id, league_round_no, market_round_id,
                pairing_policy_version, pairing_seed, standings_input_hash,
                seed_source_slot, seed_source_blockhash, bye_wallet
         FROM league_pairing_runs
         WHERE id = $1",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(LeagueError::NotFound)?;
    let seed = db_seed(&run)?;
    let standings_input_hash = db_bytes32(&run, "standings_input_hash")?;
    let rows = sqlx::query(
        "SELECT id, league_round_no, market_round_id, player_a, player_b,
                pairing_seed, seed_source_slot, seed_source_blockhash,
                repeat_relaxed, battle_pubkey, status
         FROM league_pairings
         WHERE run_id = $1
         ORDER BY id ASC",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?;
    let pairings = rows
        .iter()
        .map(|row| {
            let row_seed = db_seed(row)?;
            if row_seed != seed {
                return Err(LeagueError::InvalidPairing);
            }
            Ok(LeaguePairingView {
                pairing_id: row.try_get("id").map_err(storage_error)?,
                league_round_no: row.try_get("league_round_no").map_err(storage_error)?,
                market_round_id: row.try_get("market_round_id").map_err(storage_error)?,
                player_a: row.try_get("player_a").map_err(storage_error)?,
                player_b: row.try_get("player_b").map_err(storage_error)?,
                pairing_seed_hex: hex::encode(row_seed),
                seed_source_slot: row.try_get("seed_source_slot").map_err(storage_error)?,
                seed_source_blockhash: row
                    .try_get("seed_source_blockhash")
                    .map_err(storage_error)?,
                repeat_relaxed: row.try_get("repeat_relaxed").map_err(storage_error)?,
                battle_pubkey: row.try_get("battle_pubkey").map_err(storage_error)?,
                status: row.try_get("status").map_err(storage_error)?,
            })
        })
        .collect::<Result<Vec<_>, LeagueError>>()?;
    Ok(LeaguePairingRun {
        league_id: run.try_get("league_id").map_err(storage_error)?,
        league_round_no: run.try_get("league_round_no").map_err(storage_error)?,
        market_round_id: run.try_get("market_round_id").map_err(storage_error)?,
        pairing_policy_version: run
            .try_get("pairing_policy_version")
            .map_err(storage_error)?,
        pairing_seed_hex: hex::encode(seed),
        standings_input_hash_hex: hex::encode(standings_input_hash),
        seed_source_slot: run.try_get("seed_source_slot").map_err(storage_error)?,
        seed_source_blockhash: run
            .try_get("seed_source_blockhash")
            .map_err(storage_error)?,
        bye_wallet: run.try_get("bye_wallet").map_err(storage_error)?,
        pairings,
    })
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

fn parse_standings_wallet(row: &PgRow, column: &str) -> Result<[u8; 32], LeagueError> {
    let wallet: String = row.try_get(column).map_err(storage_error)?;
    parse_player_pubkey(&wallet).map_err(|_| LeagueError::InvalidStandings)
}

fn parse_standings_result(result: &str) -> Option<LeagueBattleResult> {
    match result {
        "PLAYER_A" => Some(LeagueBattleResult::PlayerA),
        "PLAYER_B" => Some(LeagueBattleResult::PlayerB),
        "DRAW" => Some(LeagueBattleResult::Draw),
        "FORFEIT_A" => Some(LeagueBattleResult::ForfeitA),
        "FORFEIT_B" => Some(LeagueBattleResult::ForfeitB),
        "BOTH_FORFEIT" => Some(LeagueBattleResult::BothForfeit),
        "VOIDED" => Some(LeagueBattleResult::Voided),
        _ => None,
    }
}

fn standings_final_seed(league_pubkey: [u8; 32], latest_pairing_seed: [u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"TICKERSIX_LEAGUE_STANDINGS_V1\0");
    hasher.update(league_pubkey);
    hasher.update(latest_pairing_seed);
    hasher.finalize().into()
}

fn standing_view(standing: LeagueStanding) -> LeagueStandingView {
    LeagueStandingView {
        rank: standing.rank,
        wallet: bs58::encode(standing.wallet).into_string(),
        league_points: standing.league_points,
        wins: standing.wins,
        draws: standing.draws,
        losses: standing.losses,
        byes: standing.byes,
        buchholz_sos: standing.buchholz_sos,
        head_to_head_points: standing.head_to_head_points,
        cumulative_margin_q9: standing.cumulative_margin_q9,
    }
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

    #[test]
    fn entropy_selection_chooses_the_first_finalized_slot_at_or_after_cutoff() {
        let candidates = vec![
            FinalizedEntropyCandidate {
                slot: 12,
                block_time: 1_000,
                blockhash: bs58::encode([12; 32]).into_string(),
                finalized: true,
            },
            FinalizedEntropyCandidate {
                slot: 10,
                block_time: 999,
                blockhash: bs58::encode([10; 32]).into_string(),
                finalized: true,
            },
            FinalizedEntropyCandidate {
                slot: 11,
                block_time: 1_000,
                blockhash: bs58::encode([11; 32]).into_string(),
                finalized: false,
            },
        ];

        let selected = select_pairing_entropy(&candidates, 1_000).unwrap();
        assert_eq!(selected.slot, 12);
        assert_eq!(selected.blockhash, bs58::encode([12; 32]).into_string());
    }
}
