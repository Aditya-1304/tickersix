//! Idempotent PostgreSQL projections for Solana account state.
//!
//! The indexer owns no competitive decisions. It accepts decoded account facts
//! from a chain reader, writes them with unique keys, and can safely replay
//! the same slot/account update after a process restart.

use std::fmt;

use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{
    battle_facts::{self, IndexedBattleFacts},
    league,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexerError {
    InvalidRound,
    InvalidRoundAsset,
    InvalidLeague,
    InvalidLeagueMember,
    InvalidBattle,
    InvalidBattleFacts,
    BattleFactsConflict,
    LeagueMembershipConflict,
    ExposureConflict,
    Storage(String),
}

impl fmt::Display for IndexerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRound => "indexed Market Round has invalid timing",
            Self::InvalidRoundAsset => "indexed RoundAsset has incomplete canonical identity",
            Self::InvalidLeague => "indexed League has invalid lifecycle fields",
            Self::InvalidLeagueMember => {
                "indexed LeagueMember does not match the canonical League and wallet"
            }
            Self::InvalidBattle => "indexed Battle contains invalid terminal result or score data",
            Self::InvalidBattleFacts => "indexed Battle contains invalid finalized lineup evidence",
            Self::BattleFactsConflict => {
                "indexed Battle finalized evidence conflicts with stored facts"
            }
            Self::LeagueMembershipConflict => {
                "indexed LeagueMember conflicts with the pending membership lifecycle"
            }
            Self::ExposureConflict => "wallet already has a different rated exposure in the round",
            Self::Storage(_) => "indexer storage operation failed",
        })
    }
}

impl std::error::Error for IndexerError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedMarketRound {
    pub chain_pubkey: String,
    pub round_sequence: i64,
    pub state: String,
    pub is_replay: bool,
    pub competition_domain: String,
    pub settlement_source_kind: String,
    pub queue_close_at: i64,
    pub start_target_at: i64,
    pub end_target_at: i64,
    pub indexed_at: i64,
}

/// Consumer metadata decoded from a canonical on-chain RoundAsset account.
///
/// The indexer stores this projection so clients can render the exact frozen
/// MarketRound universe without inventing symbols, providers, or scoring mints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedRoundAsset {
    pub market_round_id: i64,
    pub round_asset_pubkey: String,
    pub asset_id: i64,
    pub symbol: String,
    pub name: String,
    pub representation: String,
    pub provider: String,
    pub scoring_mint: String,
    pub status: String,
    pub indexed_at: i64,
}

/// Decoded lifecycle fields from an on-chain official League account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedLeague {
    pub chain_pubkey: String,
    pub state: String,
    pub joined_players: i32,
    pub current_round: i32,
    pub indexed_at: i64,
}

pub fn validate_league(league: &IndexedLeague) -> Result<(), IndexerError> {
    league::validate_league_chain_pubkey(&league.chain_pubkey)
        .map_err(|_| IndexerError::InvalidLeague)?;
    if !matches!(
        league.state.as_str(),
        "REGISTRATION" | "ACTIVE" | "COMPLETED" | "CANCELLED"
    ) || !(0..=league::MAX_LEAGUE_PLAYERS).contains(&league.joined_players)
        || league.current_round < 0
    {
        return Err(IndexerError::InvalidLeague);
    }
    Ok(())
}

pub async fn upsert_league(pool: &PgPool, league: &IndexedLeague) -> Result<(), IndexerError> {
    validate_league(league)?;
    crate::league::reconcile_league_state(
        pool,
        &league.chain_pubkey,
        &league.state,
        league.joined_players,
        league.current_round,
        league.indexed_at,
    )
    .await
    .map_err(|error| match error {
        crate::league::LeagueError::Storage(message) => IndexerError::Storage(message),
        crate::league::LeagueError::NotFound => IndexerError::InvalidLeague,
        _ => IndexerError::InvalidLeague,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedBattle {
    pub chain_pubkey: String,
    pub market_round_id: i64,
    pub mode: String,
    pub rated: bool,
    pub settlement_source_kind: Option<String>,
    pub player_a: String,
    pub player_b: String,
    pub state: String,
    pub result: Option<String>,
    pub score_a_q9: Option<i64>,
    pub score_b_q9: Option<i64>,
    /// Finalized lineup, captain, return, and chain-finalization facts.
    ///
    /// This remains optional for non-played and non-rated Battles, but it is
    /// mandatory for terminal played rated League Battles.
    pub finalized_facts: Option<IndexedBattleFacts>,
    pub indexed_at: i64,
}

/// Validates the portion of a decoded Battle account that is required before
/// it can enter the durable projection. Terminal played League Battles must
/// carry both exact Q9 scores; forfeits and voids intentionally remain
/// scoreless because they are not played-score events.
pub fn validate_battle(battle: &IndexedBattle) -> Result<(), IndexerError> {
    if battle.player_a == battle.player_b
        || battle
            .score_a_q9
            .into_iter()
            .chain(battle.score_b_q9)
            .any(|score| score < 0)
    {
        return Err(IndexerError::InvalidBattle);
    }

    let terminal_state = matches!(battle.state.as_str(), "FINALIZED" | "SETTLED" | "VOIDED");
    let terminal_league_battle = battle.mode == "LEAGUE" && battle.rated && terminal_state;
    if terminal_league_battle
        && !matches!(
            battle.result.as_deref(),
            Some("PLAYER_A")
                | Some("PLAYER_B")
                | Some("DRAW")
                | Some("FORFEIT_A")
                | Some("FORFEIT_B")
                | Some("BOTH_FORFEIT")
                | Some("VOIDED")
        )
    {
        return Err(IndexerError::InvalidBattle);
    }

    let played_result = matches!(
        battle.result.as_deref(),
        Some("PLAYER_A") | Some("PLAYER_B") | Some("DRAW")
    );
    if terminal_league_battle && played_result {
        let (Some(score_a), Some(score_b)) = (battle.score_a_q9, battle.score_b_q9) else {
            return Err(IndexerError::InvalidBattle);
        };
        let score_order = score_a.cmp(&score_b);
        let result_matches = match battle.result.as_deref() {
            Some("PLAYER_A") => score_order.is_gt(),
            Some("PLAYER_B") => score_order.is_lt(),
            Some("DRAW") => score_order.is_eq(),
            _ => false,
        };
        if !result_matches {
            return Err(IndexerError::InvalidBattle);
        }

        let Some(facts) = battle.finalized_facts.as_ref() else {
            return Err(IndexerError::InvalidBattleFacts);
        };
        battle_facts::validate(facts, score_a, score_b)
            .map_err(|_| IndexerError::InvalidBattleFacts)?;
    }

    if let Some(facts) = battle.finalized_facts.as_ref() {
        // Rated League played Battles were validated above; this branch covers
        // other terminal played Battle projections that carry the same facts.
        if !(terminal_league_battle && played_result) {
            if !terminal_state || !played_result {
                return Err(IndexerError::InvalidBattleFacts);
            }
            let (Some(score_a), Some(score_b)) = (battle.score_a_q9, battle.score_b_q9) else {
                return Err(IndexerError::InvalidBattleFacts);
            };
            battle_facts::validate(facts, score_a, score_b)
                .map_err(|_| IndexerError::InvalidBattleFacts)?;
        }
    }
    Ok(())
}

/// Decoded on-chain LeagueMember state supplied by a chain reader.
///
/// `league_id` is the backend projection id; `league_chain_pubkey` and
/// `member_pubkey` are the authoritative Solana identities used for PDA
/// validation before any membership state is promoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedLeagueMember {
    pub league_id: i64,
    pub league_chain_pubkey: String,
    pub member_pubkey: String,
    pub wallet: String,
    pub active: bool,
    pub indexed_at: i64,
}

pub fn validate_league_member(member: &IndexedLeagueMember) -> Result<(), IndexerError> {
    let expected = league::expected_member_pubkey(&member.league_chain_pubkey, &member.wallet)
        .map_err(|_| IndexerError::InvalidLeagueMember)?;
    if member.league_id <= 0 || expected != member.member_pubkey {
        return Err(IndexerError::InvalidLeagueMember);
    }
    Ok(())
}

pub fn validate_market_round(round: &IndexedMarketRound) -> Result<(), IndexerError> {
    if round.queue_close_at >= round.start_target_at || round.start_target_at >= round.end_target_at
    {
        return Err(IndexerError::InvalidRound);
    }
    if !matches!(
        round.competition_domain.as_str(),
        "PUBLIC_EQUITY" | "PRIVATE_MARKET"
    ) || !matches!(
        round.settlement_source_kind.as_str(),
        "JUPITER_TOKEN_SPOT_V1" | "PYTH_PRO_VERIFIED_V1" | "PYTH_247_INDEX_V1"
    ) {
        return Err(IndexerError::InvalidRound);
    }
    Ok(())
}

pub async fn upsert_market_round(
    pool: &PgPool,
    round: &IndexedMarketRound,
) -> Result<i64, IndexerError> {
    validate_market_round(round)?;
    let row = sqlx::query(
        "INSERT INTO market_rounds
            (chain_pubkey, round_sequence, state, is_replay,
             competition_domain, settlement_source_kind,
             queue_close_at, start_target_at, end_target_at, indexed_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (chain_pubkey) DO UPDATE SET
             round_sequence = EXCLUDED.round_sequence,
             state = EXCLUDED.state,
             is_replay = EXCLUDED.is_replay,
             competition_domain = EXCLUDED.competition_domain,
             settlement_source_kind = EXCLUDED.settlement_source_kind,
             queue_close_at = EXCLUDED.queue_close_at,
             start_target_at = EXCLUDED.start_target_at,
             end_target_at = EXCLUDED.end_target_at,
             indexed_at = EXCLUDED.indexed_at
         RETURNING id",
    )
    .bind(&round.chain_pubkey)
    .bind(round.round_sequence)
    .bind(&round.state)
    .bind(round.is_replay)
    .bind(&round.competition_domain)
    .bind(&round.settlement_source_kind)
    .bind(round.queue_close_at)
    .bind(round.start_target_at)
    .bind(round.end_target_at)
    .bind(round.indexed_at)
    .fetch_one(pool)
    .await
    .map_err(storage_error)?;
    row.try_get("id").map_err(storage_error)
}

pub fn validate_round_asset(asset: &IndexedRoundAsset) -> Result<(), IndexerError> {
    if asset.market_round_id <= 0
        || asset.round_asset_pubkey.trim().is_empty()
        || asset.symbol.trim().is_empty()
        || asset.name.trim().is_empty()
        || asset.representation.trim().is_empty()
        || asset.provider.trim().is_empty()
        || asset.scoring_mint.trim().is_empty()
        || asset.status.trim().is_empty()
    {
        return Err(IndexerError::InvalidRoundAsset);
    }
    Ok(())
}

pub async fn upsert_round_asset(
    pool: &PgPool,
    asset: &IndexedRoundAsset,
) -> Result<(), IndexerError> {
    validate_round_asset(asset)?;
    sqlx::query(
        "INSERT INTO round_assets
            (market_round_id, round_asset_pubkey, asset_id, symbol, name,
             representation, provider, scoring_mint, status, indexed_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (market_round_id, asset_id) DO UPDATE SET
             round_asset_pubkey = EXCLUDED.round_asset_pubkey,
             symbol = EXCLUDED.symbol,
             name = EXCLUDED.name,
             representation = EXCLUDED.representation,
             provider = EXCLUDED.provider,
             scoring_mint = EXCLUDED.scoring_mint,
             status = EXCLUDED.status,
             indexed_at = EXCLUDED.indexed_at",
    )
    .bind(asset.market_round_id)
    .bind(&asset.round_asset_pubkey)
    .bind(asset.asset_id)
    .bind(&asset.symbol)
    .bind(&asset.name)
    .bind(&asset.representation)
    .bind(&asset.provider)
    .bind(&asset.scoring_mint)
    .bind(&asset.status)
    .bind(asset.indexed_at)
    .execute(pool)
    .await
    .map_err(storage_error)?;
    Ok(())
}

pub async fn upsert_battle(pool: &PgPool, battle: &IndexedBattle) -> Result<(), IndexerError> {
    validate_battle(battle)?;
    let mut transaction = pool.begin().await.map_err(storage_error)?;

    sqlx::query(
        "INSERT INTO users (wallet, created_at)
         VALUES ($1, $3), ($2, $3)
         ON CONFLICT (wallet) DO NOTHING",
    )
    .bind(&battle.player_a)
    .bind(&battle.player_b)
    .bind(battle.indexed_at)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;

    sqlx::query(
        "INSERT INTO battles
            (chain_pubkey, market_round_id, mode, rated,
             settlement_source_kind, player_a, player_b, state, result,
             score_a_q9, score_b_q9, indexed_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         ON CONFLICT (chain_pubkey) DO UPDATE SET
             market_round_id = EXCLUDED.market_round_id,
             mode = EXCLUDED.mode,
             rated = EXCLUDED.rated,
             settlement_source_kind = COALESCE(
                 EXCLUDED.settlement_source_kind, battles.settlement_source_kind
             ),
             player_a = EXCLUDED.player_a,
             player_b = EXCLUDED.player_b,
             state = EXCLUDED.state,
             result = EXCLUDED.result,
             score_a_q9 = EXCLUDED.score_a_q9,
             score_b_q9 = EXCLUDED.score_b_q9,
             indexed_at = EXCLUDED.indexed_at
         WHERE battles.indexed_at <= EXCLUDED.indexed_at",
    )
    .bind(&battle.chain_pubkey)
    .bind(battle.market_round_id)
    .bind(&battle.mode)
    .bind(battle.rated)
    .bind(&battle.settlement_source_kind)
    .bind(&battle.player_a)
    .bind(&battle.player_b)
    .bind(&battle.state)
    .bind(&battle.result)
    .bind(battle.score_a_q9)
    .bind(battle.score_b_q9)
    .bind(battle.indexed_at)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;

    if let Some(facts) = battle.finalized_facts.as_ref() {
        persist_battle_facts(
            &mut transaction,
            &battle.chain_pubkey,
            facts,
            battle.indexed_at,
        )
        .await?;
    }
    transaction.commit().await.map_err(storage_error)?;

    if matches!(battle.state.as_str(), "FINALIZED" | "SETTLED" | "VOIDED") {
        crate::replay::record_final_event(
            pool,
            &battle.chain_pubkey,
            battle.indexed_at,
            battle.indexed_at,
            battle.score_a_q9,
            battle.score_b_q9,
        )
        .await
        .map_err(|error| IndexerError::Storage(error.to_string()))?;
    }
    if battle.state == "VOIDED" {
        let event_key = format!("battle-void:{}:{}", battle.chain_pubkey, battle.indexed_at);
        crate::hardening::record_incident(
            pool,
            crate::hardening::IncidentInput {
                event_key: &event_key,
                class: crate::hardening::FailureClass::SystemVoid,
                competition_domain: None,
                market_round_id: Some(battle.market_round_id),
                battle_pubkey: Some(&battle.chain_pubkey),
                settlement_source_kind: battle.settlement_source_kind.as_deref(),
                evidence: serde_json::json!({
                    "result": battle.result,
                    "state": battle.state,
                    "indexed_at": battle.indexed_at,
                }),
                occurred_at: battle.indexed_at,
            },
        )
        .await
        .map_err(|error| IndexerError::Storage(error.to_string()))?;
    }
    Ok(())
}

struct SerializedBattleFacts {
    side_a_lineup: String,
    side_a_returns_q9: String,
    side_b_lineup: String,
    side_b_returns_q9: String,
}

fn serialize_battle_facts(
    facts: &IndexedBattleFacts,
) -> Result<SerializedBattleFacts, IndexerError> {
    Ok(SerializedBattleFacts {
        side_a_lineup: serde_json::to_string(&facts.side_a_lineup)
            .map_err(|_| IndexerError::InvalidBattleFacts)?,
        side_a_returns_q9: serde_json::to_string(&facts.side_a_returns_q9)
            .map_err(|_| IndexerError::InvalidBattleFacts)?,
        side_b_lineup: serde_json::to_string(&facts.side_b_lineup)
            .map_err(|_| IndexerError::InvalidBattleFacts)?,
        side_b_returns_q9: serde_json::to_string(&facts.side_b_returns_q9)
            .map_err(|_| IndexerError::InvalidBattleFacts)?,
    })
}

/// Stores finalized evidence immutably while allowing a newer index timestamp
/// for an identical replay. The conflict predicate is evaluated inside the
/// PostgreSQL upsert, so concurrent indexer workers cannot silently replace
/// authoritative facts with a different lineup or return set.
async fn persist_battle_facts(
    transaction: &mut Transaction<'_, Postgres>,
    battle_pubkey: &str,
    facts: &IndexedBattleFacts,
    indexed_at: i64,
) -> Result<(), IndexerError> {
    let serialized = serialize_battle_facts(facts)?;
    let result = sqlx::query(
        "INSERT INTO battle_competitive_facts
            (battle_pubkey, facts_version, finalized_slot,
             side_a_lineup, side_a_captain, side_a_returns_q9,
             side_b_lineup, side_b_captain, side_b_returns_q9, indexed_at)
         VALUES ($1, $2, $3, $4::jsonb, $5, $6::jsonb,
                 $7::jsonb, $8, $9::jsonb, $10)
         ON CONFLICT (battle_pubkey) DO UPDATE SET
             indexed_at = GREATEST(
                 battle_competitive_facts.indexed_at, EXCLUDED.indexed_at
             )
         WHERE battle_competitive_facts.facts_version = EXCLUDED.facts_version
           AND battle_competitive_facts.finalized_slot = EXCLUDED.finalized_slot
           AND battle_competitive_facts.side_a_lineup = EXCLUDED.side_a_lineup
           AND battle_competitive_facts.side_a_captain = EXCLUDED.side_a_captain
           AND battle_competitive_facts.side_a_returns_q9 = EXCLUDED.side_a_returns_q9
           AND battle_competitive_facts.side_b_lineup = EXCLUDED.side_b_lineup
           AND battle_competitive_facts.side_b_captain = EXCLUDED.side_b_captain
           AND battle_competitive_facts.side_b_returns_q9 = EXCLUDED.side_b_returns_q9",
    )
    .bind(battle_pubkey)
    .bind(facts.facts_version)
    .bind(facts.finalized_slot)
    .bind(serialized.side_a_lineup)
    .bind(i32::from(facts.side_a_captain))
    .bind(serialized.side_a_returns_q9)
    .bind(serialized.side_b_lineup)
    .bind(i32::from(facts.side_b_captain))
    .bind(serialized.side_b_returns_q9)
    .bind(indexed_at)
    .execute(&mut **transaction)
    .await
    .map_err(storage_error)?;

    if result.rows_affected() == 0 {
        return Err(IndexerError::BattleFactsConflict);
    }
    Ok(())
}

/// Applies an indexed membership transition only after the wallet lifecycle
/// has produced the matching pending intent. Replaying the same chain update
/// is safe because the League module performs the state transition inside a
/// wallet-locked transaction.
pub async fn upsert_league_member(
    pool: &PgPool,
    member: &IndexedLeagueMember,
) -> Result<(), IndexerError> {
    validate_league_member(member)?;
    let result = if member.active {
        league::confirm_join(
            pool,
            member.league_id,
            &member.wallet,
            &member.member_pubkey,
            member.indexed_at,
        )
        .await
    } else {
        league::confirm_leave(pool, member.league_id, &member.wallet, member.indexed_at).await
    };
    result.map(|_| ()).map_err(|error| match error {
        league::LeagueError::Storage(message) => IndexerError::Storage(message),
        league::LeagueError::InvalidWallet
        | league::LeagueError::ChainIdentityUnavailable
        | league::LeagueError::InvalidLeagueId => IndexerError::InvalidLeagueMember,
        _ => IndexerError::LeagueMembershipConflict,
    })
}

pub async fn upsert_rated_exposure(
    pool: &PgPool,
    market_round_id: i64,
    wallet: &str,
    battle_pubkey: &str,
) -> Result<(), IndexerError> {
    let result = sqlx::query(
        "INSERT INTO rated_exposures (market_round_id, wallet, battle_pubkey)
         VALUES ($1, $2, $3)
         ON CONFLICT (market_round_id, wallet) DO UPDATE
             SET battle_pubkey = EXCLUDED.battle_pubkey
             WHERE rated_exposures.battle_pubkey = EXCLUDED.battle_pubkey",
    )
    .bind(market_round_id)
    .bind(wallet)
    .bind(battle_pubkey)
    .execute(pool)
    .await
    .map_err(storage_error)?;

    if result.rows_affected() == 0 {
        return Err(IndexerError::ExposureConflict);
    }
    Ok(())
}

pub async fn record_account_observation(
    pool: &PgPool,
    account_pubkey: &str,
    owner_program: &str,
    account_kind: &str,
    slot: i64,
    data_hash: &[u8],
    indexed_at: i64,
) -> Result<(), IndexerError> {
    sqlx::query(
        "INSERT INTO indexed_accounts
            (account_pubkey, owner_program, account_kind, slot, data_hash, indexed_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (account_pubkey) DO UPDATE SET
             owner_program = EXCLUDED.owner_program,
             account_kind = EXCLUDED.account_kind,
             slot = EXCLUDED.slot,
             data_hash = EXCLUDED.data_hash,
             indexed_at = EXCLUDED.indexed_at
             indexed_at = EXCLUDED.indexed_at",
    )
    .bind(account_pubkey)
    .bind(owner_program)
    .bind(account_kind)
    .bind(slot)
    .bind(data_hash)
    .bind(indexed_at)
    .execute(pool)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn storage_error(error: sqlx::Error) -> IndexerError {
    IndexerError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round() -> IndexedMarketRound {
        IndexedMarketRound {
            chain_pubkey: "round".to_owned(),
            round_sequence: 1,
            state: "SCHEDULED".to_owned(),
            is_replay: false,
            competition_domain: "PUBLIC_EQUITY".to_owned(),
            settlement_source_kind: "JUPITER_TOKEN_SPOT_V1".to_owned(),
            queue_close_at: 100,
            start_target_at: 200,
            end_target_at: 300,
            indexed_at: 1,
        }
    }

    #[test]
    fn indexer_rejects_round_asset_without_canonical_identity() {
        let mut asset = round_asset();
        asset.round_asset_pubkey.clear();

        assert_eq!(
            validate_round_asset(&asset),
            Err(IndexerError::InvalidRoundAsset)
        );
    }

    fn round_asset() -> IndexedRoundAsset {
        IndexedRoundAsset {
            market_round_id: 7,
            round_asset_pubkey: "round-asset-pda".to_owned(),
            asset_id: 11,
            symbol: "NVDA".to_owned(),
            name: "NVIDIA".to_owned(),
            representation: "NVDAx".to_owned(),
            provider: "xStocks".to_owned(),
            scoring_mint: "scoring-mint".to_owned(),
            status: "ELIGIBLE".to_owned(),
            indexed_at: 1,
        }
    }

    #[test]
    fn indexer_rejects_invalid_round_timing_before_database_access() {
        let mut invalid = round();
        invalid.queue_close_at = invalid.start_target_at;

        assert_eq!(
            validate_market_round(&invalid),
            Err(IndexerError::InvalidRound)
        );
        assert!(validate_market_round(&round()).is_ok());
    }

    #[test]
    fn indexer_rejects_unknown_market_round_consumer_metadata_before_database_access() {
        let mut invalid = round();
        invalid.competition_domain = "UNREGISTERED_DOMAIN".to_owned();
        assert_eq!(
            validate_market_round(&invalid),
            Err(IndexerError::InvalidRound)
        );

        let mut invalid = round();
        invalid.settlement_source_kind = "UNREGISTERED_SOURCE".to_owned();
        assert_eq!(
            validate_market_round(&invalid),
            Err(IndexerError::InvalidRound)
        );
    }

    #[test]
    fn indexer_rejects_a_member_pda_bound_to_another_wallet() {
        let member = IndexedLeagueMember {
            league_id: 7,
            league_chain_pubkey: bs58::encode([12; 32]).into_string(),
            member_pubkey: bs58::encode([99; 32]).into_string(),
            wallet: bs58::encode([11; 32]).into_string(),
            active: true,
            indexed_at: 1,
        };

        assert_eq!(
            validate_league_member(&member),
            Err(IndexerError::InvalidLeagueMember)
        );
    }

    #[test]
    fn indexer_requires_exact_scores_for_terminal_played_league_battles() {
        let mut battle = IndexedBattle {
            chain_pubkey: bs58::encode([7; 32]).into_string(),
            market_round_id: 3,
            mode: "LEAGUE".to_owned(),
            rated: true,
            settlement_source_kind: Some("JUPITER_TOKEN_SPOT_V1".to_owned()),
            player_a: bs58::encode([1; 32]).into_string(),
            player_b: bs58::encode([2; 32]).into_string(),
            state: "FINALIZED".to_owned(),
            result: Some("PLAYER_A".to_owned()),
            score_a_q9: None,
            score_b_q9: None,
            finalized_facts: None,
            indexed_at: 10,
        };

        assert_eq!(validate_battle(&battle), Err(IndexerError::InvalidBattle));

        battle.score_a_q9 = Some(60_000_000);
        battle.score_b_q9 = Some(40_000_000);
        assert_eq!(
            validate_battle(&battle),
            Err(IndexerError::InvalidBattleFacts)
        );

        battle.result = Some("DRAW".to_owned());
        assert_eq!(validate_battle(&battle), Err(IndexerError::InvalidBattle));
    }

    #[test]
    fn indexer_accepts_a_terminal_played_battle_only_with_matching_finalized_facts() {
        let mut battle = IndexedBattle {
            chain_pubkey: bs58::encode([8; 32]).into_string(),
            market_round_id: 3,
            mode: "LEAGUE".to_owned(),
            rated: true,
            settlement_source_kind: Some("JUPITER_TOKEN_SPOT_V1".to_owned()),
            player_a: bs58::encode([1; 32]).into_string(),
            player_b: bs58::encode([2; 32]).into_string(),
            state: "SETTLED".to_owned(),
            result: Some("PLAYER_A".to_owned()),
            score_a_q9: Some(48),
            score_b_q9: Some(10),
            finalized_facts: Some(IndexedBattleFacts {
                facts_version: 1,
                finalized_slot: 900,
                side_a_lineup: vec![1, 2, 3, 4, 5, 6],
                side_a_captain: 1,
                side_a_returns_q9: [70, 60, 50, 40, 30, 20],
                side_b_lineup: vec![7, 8, 9, 10, 11, 12],
                side_b_captain: 7,
                side_b_returns_q9: [10, 10, 10, 10, 10, 10],
            }),
            indexed_at: 10,
        };

        assert!(validate_battle(&battle).is_ok());

        battle.score_a_q9 = Some(49);
        assert_eq!(
            validate_battle(&battle),
            Err(IndexerError::InvalidBattleFacts)
        );
    }
}
