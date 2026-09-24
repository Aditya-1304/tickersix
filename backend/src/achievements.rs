//! Achievement authority and idempotent derived projections.
//!
//! The chain/indexer and rating history remain the source of truth. This
//! module derives cosmetic, non-economic unlocks from finalized public facts.
//! Pure rule evaluation is kept separate from SQL so the critical exclusions
//!—replays, private matches, voids, and forfeits—are testable without a DB.

use std::{collections::HashSet, fmt};

use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{postgres::PgRow, PgPool, Postgres, Row, Transaction};

use crate::{league, metrics};

pub const FIRST_BLOOD: &str = "FIRST_BLOOD";
pub const HAT_TRICK: &str = "HAT_TRICK";
pub const UNSTOPPABLE: &str = "UNSTOPPABLE";
pub const GIANT_SLAYER: &str = "GIANT_SLAYER";
pub const PERFECT_CAPTAIN: &str = "PERFECT_CAPTAIN";
pub const GREEN_SIX: &str = "GREEN_SIX";
pub const PHOTO_FINISH: &str = "PHOTO_FINISH";
pub const LANDSLIDE: &str = "LANDSLIDE";
pub const LEAGUE_PODIUM: &str = "LEAGUE_PODIUM";
pub const LEAGUE_CHAMPION: &str = "LEAGUE_CHAMPION";
pub const PERFECT_LEAGUE: &str = "PERFECT_LEAGUE";
pub const VETERAN_25: &str = "VETERAN_25";
pub const CENTURY_100: &str = "CENTURY_100";
pub const DIAMOND: &str = "DIAMOND";
pub const MASTER: &str = "MASTER";

const ACHIEVEMENT_WORKER_LOCK_KEY: i64 = 6_782_419;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AchievementError {
    InvalidWallet,
    Storage(String),
    League(String),
}

impl fmt::Display for AchievementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidWallet => "wallet must be a base58-encoded 32-byte Solana public key",
            Self::Storage(_) => "achievement storage operation failed",
            Self::League(_) => "League achievement derivation failed",
        })
    }
}

impl std::error::Error for AchievementError {}

#[derive(Debug, Clone, Serialize)]
pub struct AchievementView {
    pub code: String,
    pub name: String,
    pub rarity: String,
    pub rule_version: i32,
    pub scope_type: String,
    pub scope_id: String,
    pub unlocked_at: i64,
    pub evidence: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleOutcome {
    PlayedWin,
    PlayedLoss,
    PlayedDraw,
    OwnForfeit,
    OpponentForfeit,
    BothForfeit,
    SystemVoid,
    Ineligible,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AchievementState {
    pub win_streak: u32,
    pub fully_played_rated_battles: u32,
    pub fully_played_rated_wins: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleAchievementFact {
    pub battle_pubkey: String,
    pub round_sequence: i64,
    pub official: bool,
    pub rated: bool,
    pub replay: bool,
    pub voided: bool,
    pub settled: bool,
    pub outcome: BattleOutcome,
    pub own_rating_before: Option<i32>,
    pub opponent_rating_before: Option<i32>,
    pub rating_after: Option<i32>,
    pub margin_bps: Option<i64>,
    pub returns_q9: Option<[i64; 6]>,
    pub captain_return_q9: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AchievementUnlock {
    pub code: &'static str,
    pub scope_type: &'static str,
    pub scope_id: String,
    pub evidence: Value,
}

/// Applies one ordered Battle fact to a player's derived achievement state.
///
/// The known-code set is loaded from durable unlocks. PostgreSQL's unique key
/// remains the final idempotency guard if two scheduler processes overlap.
pub fn derive_battle_unlocks(
    fact: &BattleAchievementFact,
    mut state: AchievementState,
    known_codes: &HashSet<String>,
) -> (AchievementState, Vec<AchievementUnlock>) {
    if !fact.official || !fact.rated || fact.replay || fact.voided {
        return (state, Vec::new());
    }

    let played = matches!(
        fact.outcome,
        BattleOutcome::PlayedWin | BattleOutcome::PlayedLoss | BattleOutcome::PlayedDraw
    );
    if played {
        state.fully_played_rated_battles = state.fully_played_rated_battles.saturating_add(1);
    }
    match fact.outcome {
        BattleOutcome::PlayedWin => {
            state.win_streak = state.win_streak.saturating_add(1);
            state.fully_played_rated_wins = state.fully_played_rated_wins.saturating_add(1);
        }
        BattleOutcome::PlayedLoss | BattleOutcome::PlayedDraw | BattleOutcome::OwnForfeit => {
            state.win_streak = 0;
        }
        BattleOutcome::OpponentForfeit
        | BattleOutcome::BothForfeit
        | BattleOutcome::SystemVoid
        | BattleOutcome::Ineligible => {}
    }
    if !played {
        return (state, Vec::new());
    }

    let mut unlocks = Vec::new();
    if fact.outcome == BattleOutcome::PlayedWin {
        if state.fully_played_rated_wins == 1 {
            add_unlock(
                &mut unlocks,
                known_codes,
                fact,
                FIRST_BLOOD,
                "first_fully_played_rated_win",
                &state,
            );
        }
        if state.win_streak >= 3 {
            add_unlock(
                &mut unlocks,
                known_codes,
                fact,
                HAT_TRICK,
                "three_consecutive_fully_played_rated_wins",
                &state,
            );
        }
        if state.win_streak >= 5 {
            add_unlock(
                &mut unlocks,
                known_codes,
                fact,
                UNSTOPPABLE,
                "five_consecutive_fully_played_rated_wins",
                &state,
            );
        }
        if state.fully_played_rated_battles >= 25 {
            add_unlock(
                &mut unlocks,
                known_codes,
                fact,
                VETERAN_25,
                "twenty_five_fully_played_rated_battles",
                &state,
            );
        }
        if state.fully_played_rated_battles >= 100 {
            add_unlock(
                &mut unlocks,
                known_codes,
                fact,
                CENTURY_100,
                "one_hundred_fully_played_rated_battles",
                &state,
            );
        }
        if fact
            .opponent_rating_before
            .zip(fact.own_rating_before)
            .is_some_and(|(opponent, own)| opponent >= own.saturating_add(200))
        {
            add_unlock(
                &mut unlocks,
                known_codes,
                fact,
                GIANT_SLAYER,
                "opponent_pre_battle_rating_at_least_200_higher",
                &state,
            );
        }
        if fact
            .margin_bps
            .is_some_and(|margin| (1..=10).contains(&margin))
        {
            add_unlock(
                &mut unlocks,
                known_codes,
                fact,
                PHOTO_FINISH,
                "winning_margin_between_1_and_10_bps",
                &state,
            );
        }
        if fact.margin_bps.is_some_and(|margin| margin >= 300) {
            add_unlock(
                &mut unlocks,
                known_codes,
                fact,
                LANDSLIDE,
                "winning_margin_at_least_300_bps",
                &state,
            );
        }
    }
    if fact.rating_after.is_some_and(|value| value >= 1_850) {
        add_unlock(
            &mut unlocks,
            known_codes,
            fact,
            DIAMOND,
            "rating_reached_1850",
            &state,
        );
    }
    if fact.rating_after.is_some_and(|value| value >= 2_000) {
        add_unlock(
            &mut unlocks,
            known_codes,
            fact,
            MASTER,
            "rating_reached_2000",
            &state,
        );
    }
    if fact.settled {
        if let (Some(returns), Some(captain_return)) = (fact.returns_q9, fact.captain_return_q9) {
            if captain_return >= returns.into_iter().max().unwrap_or(captain_return) {
                add_unlock(
                    &mut unlocks,
                    known_codes,
                    fact,
                    PERFECT_CAPTAIN,
                    "captain_return_at_least_every_other_lineup_return",
                    &state,
                );
            }
            if returns.into_iter().all(|value| value > 0) {
                add_unlock(
                    &mut unlocks,
                    known_codes,
                    fact,
                    GREEN_SIX,
                    "all_six_asset_returns_positive",
                    &state,
                );
            }
        }
    }
    (state, unlocks)
}

fn add_unlock(
    unlocks: &mut Vec<AchievementUnlock>,
    known_codes: &HashSet<String>,
    fact: &BattleAchievementFact,
    code: &'static str,
    rule: &'static str,
    state: &AchievementState,
) {
    if known_codes.contains(code) {
        return;
    }
    unlocks.push(AchievementUnlock {
        code,
        scope_type: "BATTLE",
        scope_id: fact.battle_pubkey.clone(),
        evidence: json!({
            "rule": rule,
            "rule_version": 1,
            "battle_pubkey": &fact.battle_pubkey,
            "round_sequence": fact.round_sequence,
            "win_streak": state.win_streak,
            "fully_played_rated_battles": state.fully_played_rated_battles,
        }),
    });
}

/// Replays authoritative wallet history and materializes missing unlocks.
/// Full replay keeps recovery deterministic after a DB restore or indexer
/// rewind; unique keys make reruns safe.
pub async fn reconcile_wallet(
    pool: &PgPool,
    wallet: &str,
    now: i64,
) -> Result<usize, AchievementError> {
    crate::auth::parse_wallet(wallet).map_err(|_| AchievementError::InvalidWallet)?;
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ACHIEVEMENT_WORKER_LOCK_KEY)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
    let rows = sqlx::query(
        "SELECT b.chain_pubkey, mr.round_sequence, b.mode, b.rated,
                mr.competition_domain, mr.is_replay, b.state, b.result,
                b.player_a, b.player_b, b.score_a_q9, b.score_b_q9,
                re.rating_before, re.opponent_rating_snapshot, re.rating_after
         FROM battles b
         JOIN market_rounds mr ON mr.id = b.market_round_id
         LEFT JOIN rating_events re
           ON re.battle_pubkey = b.chain_pubkey AND re.wallet = $1
         WHERE (b.player_a = $1 OR b.player_b = $1) AND b.rated
         ORDER BY mr.round_sequence ASC, b.chain_pubkey ASC",
    )
    .bind(wallet)
    .fetch_all(&mut *transaction)
    .await
    .map_err(storage_error)?;
    let mut known_codes = load_known_codes(&mut transaction, wallet).await?;
    let mut state = AchievementState::default();
    let mut created = 0;
    for row in rows {
        let fact = battle_fact_from_row(&row, wallet)?;
        let (next_state, unlocks) = derive_battle_unlocks(&fact, state, &known_codes);
        state = next_state;
        for unlock in unlocks {
            if insert_unlock(&mut transaction, wallet, &unlock, now).await? {
                known_codes.insert(unlock.code.to_owned());
                created += 1;
                metrics::increment("achievement_unlocks_total", 1);
            }
        }
    }
    let season_id =
        sqlx::query_scalar::<_, i64>("SELECT id FROM seasons WHERE status = 'ACTIVE' LIMIT 1")
            .fetch_optional(&mut *transaction)
            .await
            .map_err(storage_error)?;
    sqlx::query(
        "INSERT INTO achievement_player_state
            (wallet, season_id, win_streak, fully_played_rated_battles,
             fully_played_rated_wins, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (wallet) DO UPDATE SET
            season_id = EXCLUDED.season_id,
            win_streak = EXCLUDED.win_streak,
            fully_played_rated_battles = EXCLUDED.fully_played_rated_battles,
            fully_played_rated_wins = EXCLUDED.fully_played_rated_wins,
            updated_at = EXCLUDED.updated_at",
    )
    .bind(wallet)
    .bind(season_id)
    .bind(
        i32::try_from(state.win_streak)
            .map_err(|_| AchievementError::Storage("streak overflow".to_owned()))?,
    )
    .bind(
        i32::try_from(state.fully_played_rated_battles)
            .map_err(|_| AchievementError::Storage("battle count overflow".to_owned()))?,
    )
    .bind(
        i32::try_from(state.fully_played_rated_wins)
            .map_err(|_| AchievementError::Storage("win count overflow".to_owned()))?,
    )
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;
    transaction.commit().await.map_err(storage_error)?;
    Ok(created)
}

/// Reconciles all player histories and completed official Leagues in bounded
/// per-wallet transactions. Every write is idempotent.
pub async fn reconcile_all(pool: &PgPool, now: i64) -> Result<usize, AchievementError> {
    let wallet_rows = sqlx::query("SELECT wallet FROM users ORDER BY wallet")
        .fetch_all(pool)
        .await
        .map_err(storage_error)?;
    let mut created = 0;
    for row in wallet_rows {
        let wallet: String = row.try_get("wallet").map_err(storage_error)?;
        created += reconcile_wallet(pool, &wallet, now).await?;
    }
    created += reconcile_leagues(pool, now).await?;
    Ok(created)
}

pub async fn list_for_wallet(
    pool: &PgPool,
    wallet: &str,
) -> Result<Vec<AchievementView>, AchievementError> {
    crate::auth::parse_wallet(wallet).map_err(|_| AchievementError::InvalidWallet)?;
    let rows = sqlx::query(
        "SELECT a.code, a.name, a.rarity, a.rule_version,
                pa.scope_type, pa.scope_id, pa.unlocked_at, pa.evidence
         FROM player_achievements pa
         JOIN achievements a ON a.id = pa.achievement_id
         WHERE pa.wallet = $1 AND a.visible AND a.enabled
         ORDER BY pa.unlocked_at ASC, a.id ASC, pa.scope_id ASC",
    )
    .bind(wallet)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?;
    rows.into_iter()
        .map(|row| {
            Ok(AchievementView {
                code: row.try_get("code").map_err(storage_error)?,
                name: row.try_get("name").map_err(storage_error)?,
                rarity: row.try_get("rarity").map_err(storage_error)?,
                rule_version: row.try_get("rule_version").map_err(storage_error)?,
                scope_type: row.try_get("scope_type").map_err(storage_error)?,
                scope_id: row.try_get("scope_id").map_err(storage_error)?,
                unlocked_at: row.try_get("unlocked_at").map_err(storage_error)?,
                evidence: row.try_get("evidence").map_err(storage_error)?,
            })
        })
        .collect()
}

fn battle_fact_from_row(
    row: &PgRow,
    wallet: &str,
) -> Result<BattleAchievementFact, AchievementError> {
    let result: Option<String> = row.try_get("result").map_err(storage_error)?;
    let player_a: String = row.try_get("player_a").map_err(storage_error)?;
    let player_b: String = row.try_get("player_b").map_err(storage_error)?;
    let outcome = match result.as_deref() {
        Some("PLAYER_A") if wallet == player_a => BattleOutcome::PlayedWin,
        Some("PLAYER_A") => BattleOutcome::PlayedLoss,
        Some("PLAYER_B") if wallet == player_b => BattleOutcome::PlayedWin,
        Some("PLAYER_B") => BattleOutcome::PlayedLoss,
        Some("DRAW") => BattleOutcome::PlayedDraw,
        Some("FORFEIT_A") if wallet == player_a => BattleOutcome::OwnForfeit,
        Some("FORFEIT_A") => BattleOutcome::OpponentForfeit,
        Some("FORFEIT_B") if wallet == player_b => BattleOutcome::OwnForfeit,
        Some("FORFEIT_B") => BattleOutcome::OpponentForfeit,
        Some("BOTH_FORFEIT") => BattleOutcome::BothForfeit,
        Some("VOIDED") => BattleOutcome::SystemVoid,
        _ => BattleOutcome::Ineligible,
    };
    let score_a: Option<i64> = row.try_get("score_a_q9").map_err(storage_error)?;
    let score_b: Option<i64> = row.try_get("score_b_q9").map_err(storage_error)?;
    let margin_bps = score_a.zip(score_b).and_then(|(a, b)| {
        i128::from(a)
            .abs_diff(i128::from(b))
            .checked_mul(10_000)
            .and_then(|value| i64::try_from(value / 1_000_000_000).ok())
    });
    let state: String = row.try_get("state").map_err(storage_error)?;
    let mode: String = row.try_get("mode").map_err(storage_error)?;
    let domain: String = row.try_get("competition_domain").map_err(storage_error)?;
    Ok(BattleAchievementFact {
        battle_pubkey: row.try_get("chain_pubkey").map_err(storage_error)?,
        round_sequence: row.try_get("round_sequence").map_err(storage_error)?,
        official: mode != "PRIVATE_MARKET" && domain == "PUBLIC_EQUITY",
        rated: row.try_get("rated").map_err(storage_error)?,
        replay: row.try_get("is_replay").map_err(storage_error)?,
        voided: state == "VOIDED" || outcome == BattleOutcome::SystemVoid,
        settled: state == "SETTLED",
        outcome,
        own_rating_before: row.try_get("rating_before").map_err(storage_error)?,
        opponent_rating_before: row
            .try_get("opponent_rating_snapshot")
            .map_err(storage_error)?,
        rating_after: row.try_get("rating_after").map_err(storage_error)?,
        margin_bps,
        returns_q9: None,
        captain_return_q9: None,
    })
}

async fn load_known_codes(
    transaction: &mut Transaction<'_, Postgres>,
    wallet: &str,
) -> Result<HashSet<String>, AchievementError> {
    let rows = sqlx::query(
        "SELECT a.code FROM player_achievements pa
         JOIN achievements a ON a.id = pa.achievement_id
         WHERE pa.wallet = $1",
    )
    .bind(wallet)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage_error)?;
    rows.into_iter()
        .map(|row| row.try_get("code").map_err(storage_error))
        .collect()
}

async fn insert_unlock(
    transaction: &mut Transaction<'_, Postgres>,
    wallet: &str,
    unlock: &AchievementUnlock,
    now: i64,
) -> Result<bool, AchievementError> {
    let result = sqlx::query(
        "INSERT INTO player_achievements
            (wallet, achievement_id, scope_type, scope_id, unlocked_at, evidence)
         SELECT $1, id, $3, $4, $5, $6
         FROM achievements
         WHERE code = $2 AND enabled
         ON CONFLICT (wallet, achievement_id, scope_type, scope_id) DO NOTHING",
    )
    .bind(wallet)
    .bind(unlock.code)
    .bind(unlock.scope_type)
    .bind(&unlock.scope_id)
    .bind(now)
    .bind(&unlock.evidence)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;
    Ok(result.rows_affected() == 1)
}

async fn reconcile_leagues(pool: &PgPool, now: i64) -> Result<usize, AchievementError> {
    let rows =
        sqlx::query("SELECT id FROM leagues WHERE rated AND status = 'COMPLETED' ORDER BY id")
            .fetch_all(pool)
            .await
            .map_err(storage_error)?;
    let mut created = 0;
    for row in rows {
        let league_id: i64 = row.try_get("id").map_err(storage_error)?;
        let standings = league::get_league_standings(pool, league_id)
            .await
            .map_err(|error| AchievementError::League(error.to_string()))?;
        if !standings.complete || standings.standings.len() < 16 {
            continue;
        }
        for standing in standings
            .standings
            .iter()
            .filter(|standing| standing.rank <= 3)
        {
            let perfect = standing.rank == 1
                && standings.resolved_rounds >= 5
                && standing.losses == 0
                && standing.draws == 0
                && no_league_forfeits(pool, league_id, &standing.wallet).await?;
            let codes = [
                (LEAGUE_PODIUM, true),
                (LEAGUE_CHAMPION, standing.rank == 1),
                (PERFECT_LEAGUE, perfect),
            ];
            let mut transaction = pool.begin().await.map_err(storage_error)?;
            for (code, enabled) in codes {
                if !enabled {
                    continue;
                }
                let unlock = AchievementUnlock {
                    code,
                    scope_type: "LEAGUE",
                    scope_id: league_id.to_string(),
                    evidence: json!({
                        "rule_version": 1,
                        "league_id": league_id,
                        "rank": standing.rank,
                        "participants": standings.standings.len(),
                        "resolved_rounds": standings.resolved_rounds,
                    }),
                };
                if insert_unlock(&mut transaction, &standing.wallet, &unlock, now).await? {
                    created += 1;
                    metrics::increment("achievement_unlocks_total", 1);
                }
            }
            transaction.commit().await.map_err(storage_error)?;
        }
    }
    Ok(created)
}

async fn no_league_forfeits(
    pool: &PgPool,
    league_id: i64,
    wallet: &str,
) -> Result<bool, AchievementError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM battles
         WHERE league_id = $1 AND (player_a = $2 OR player_b = $2)
           AND result IN ('FORFEIT_A', 'FORFEIT_B', 'BOTH_FORFEIT')",
    )
    .bind(league_id)
    .bind(wallet)
    .fetch_one(pool)
    .await
    .map_err(storage_error)?;
    Ok(count == 0)
}

fn storage_error(error: sqlx::Error) -> AchievementError {
    AchievementError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(outcome: BattleOutcome) -> BattleAchievementFact {
        BattleAchievementFact {
            battle_pubkey: "battle-1".to_owned(),
            round_sequence: 1,
            official: true,
            rated: true,
            replay: false,
            voided: false,
            settled: true,
            outcome,
            own_rating_before: Some(1500),
            opponent_rating_before: Some(1750),
            rating_after: Some(1517),
            margin_bps: Some(5),
            returns_q9: Some([1, 2, 3, 4, 5, 6]),
            captain_return_q9: Some(6),
        }
    }

    #[test]
    fn replay_private_and_voided_battles_cannot_unlock_competitive_achievements() {
        for (replay, official, voided) in [
            (true, true, false),
            (false, false, false),
            (false, true, true),
        ] {
            let mut battle = fact(BattleOutcome::PlayedWin);
            battle.replay = replay;
            battle.official = official;
            battle.voided = voided;
            let (state, unlocks) =
                derive_battle_unlocks(&battle, AchievementState::default(), &HashSet::new());
            assert_eq!(state, AchievementState::default());
            assert!(unlocks.is_empty());
        }
    }

    #[test]
    fn forfeit_semantics_do_not_turn_no_shows_into_played_wins() {
        let (opponent_state, opponent_unlocks) = derive_battle_unlocks(
            &fact(BattleOutcome::OpponentForfeit),
            AchievementState {
                win_streak: 2,
                ..AchievementState::default()
            },
            &HashSet::new(),
        );
        assert_eq!(opponent_state.win_streak, 2);
        assert!(opponent_unlocks.is_empty());
        let (own_state, own_unlocks) = derive_battle_unlocks(
            &fact(BattleOutcome::OwnForfeit),
            AchievementState {
                win_streak: 2,
                ..AchievementState::default()
            },
            &HashSet::new(),
        );
        assert_eq!(own_state.win_streak, 0);
        assert!(own_unlocks.is_empty());
    }

    #[test]
    fn played_win_emits_rule_evidence_once_and_preserves_streak_state() {
        let (state, unlocks) = derive_battle_unlocks(
            &fact(BattleOutcome::PlayedWin),
            AchievementState::default(),
            &HashSet::new(),
        );
        let codes: HashSet<_> = unlocks.iter().map(|unlock| unlock.code).collect();
        assert_eq!(state.win_streak, 1);
        assert!(codes.contains(&FIRST_BLOOD));
        assert!(codes.contains(&GIANT_SLAYER));
        assert!(codes.contains(&PERFECT_CAPTAIN));
        assert!(codes.contains(&GREEN_SIX));
        assert!(codes.contains(&PHOTO_FINISH));
        assert_eq!(
            unlocks
                .iter()
                .filter(|unlock| unlock.code == FIRST_BLOOD)
                .count(),
            1
        );
        let (_, repeated) = derive_battle_unlocks(
            &fact(BattleOutcome::PlayedWin),
            AchievementState::default(),
            &HashSet::from([FIRST_BLOOD.to_owned()]),
        );
        assert!(!repeated.iter().any(|unlock| unlock.code == FIRST_BLOOD));
    }

    #[test]
    fn streak_thresholds_and_rating_thresholds_are_derived_from_ordered_state() {
        let mut state = AchievementState::default();
        let mut known = HashSet::new();
        let mut all_codes = HashSet::new();
        for index in 1..=5 {
            let mut battle = fact(BattleOutcome::PlayedWin);
            battle.battle_pubkey = format!("battle-{index}");
            battle.round_sequence = index;
            battle.rating_after = Some(if index == 5 { 2_000 } else { 1_500 });
            let (next, unlocks) = derive_battle_unlocks(&battle, state, &known);
            state = next;
            for unlock in unlocks {
                known.insert(unlock.code.to_owned());
                all_codes.insert(unlock.code);
            }
        }
        assert_eq!(state.win_streak, 5);
        assert!(all_codes.contains(&HAT_TRICK));
        assert!(all_codes.contains(&UNSTOPPABLE));
        assert!(all_codes.contains(&MASTER));
    }
}
