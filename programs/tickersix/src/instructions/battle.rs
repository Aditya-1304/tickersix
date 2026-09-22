use anchor_lang::prelude::*;

use crate::{
    constants::{BATTLE_SEED, CONFIG_SEED, LINEUP_SIZE, RATED_SLOT_SEED, RATING_FLOOR},
    error::ErrorCode,
    math::{lineup_score_q9, validate_lineup},
    state::{
        Battle, BattleMode, BattleResult, BattleSide, Config, League, LeagueMember, LeagueState,
        MarketRound, MarketRoundState, RatedSlot, RoundAsset, SideStatus, VoidReason,
    },
};

#[derive(Accounts)]
#[instruction(battle_id: u64)]
pub struct CreateRatedBattle<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub coordinator: Signer<'info>,
    #[account(
        mut,
        seeds = [crate::constants::MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    /// CHECK: Player identities are stored as public keys; they do not sign coordinator pairing.
    pub player_a: UncheckedAccount<'info>,
    /// CHECK: Player identities are stored as public keys; they do not sign coordinator pairing.
    pub player_b: UncheckedAccount<'info>,
    /// CHECK: This optional account is validated against the League PDA and mode below.
    pub league_account: Option<Account<'info, League>>,
    /// CHECK: These optional accounts are validated against the LeagueMember PDAs below.
    pub league_member_a: Option<Account<'info, LeagueMember>>,
    /// CHECK: These optional accounts are validated against the LeagueMember PDAs below.
    pub league_member_b: Option<Account<'info, LeagueMember>>,
    #[account(
        init,
        payer = coordinator,
        space = 8 + Battle::INIT_SPACE,
        seeds = [BATTLE_SEED, market_round.key().as_ref(), &battle_id.to_le_bytes()],
        bump
    )]
    pub battle: Account<'info, Battle>,
    #[account(
        init,
        payer = coordinator,
        space = 8 + RatedSlot::INIT_SPACE,
        seeds = [RATED_SLOT_SEED, market_round.key().as_ref(), player_a.key().as_ref()],
        bump
    )]
    pub rated_slot_a: Account<'info, RatedSlot>,
    #[account(
        init,
        payer = coordinator,
        space = 8 + RatedSlot::INIT_SPACE,
        seeds = [RATED_SLOT_SEED, market_round.key().as_ref(), player_b.key().as_ref()],
        bump
    )]
    pub rated_slot_b: Account<'info, RatedSlot>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_rated_battle(
    ctx: Context<CreateRatedBattle>,
    battle_id: u64,
    mode: BattleMode,
    league: Pubkey,
    league_round_no: u16,
    rating_a_before: i32,
    rating_b_before: i32,
    rating_formula_version: u16,
) -> Result<()> {
    require_keys_eq!(
        ctx.accounts.config.coordinator_authority,
        ctx.accounts.coordinator.key(),
        ErrorCode::UnauthorizedCoordinator
    );
    require!(!ctx.accounts.config.paused, ErrorCode::ProtocolPaused);
    validate_rated_battle_window(&ctx.accounts.market_round, Clock::get()?.unix_timestamp)?;
    require!(
        !ctx.accounts.market_round.is_replay,
        ErrorCode::ReplayCannotBeRated
    );
    require!(
        matches!(mode, BattleMode::Ranked | BattleMode::League),
        ErrorCode::InvalidRatedBattleMode
    );
    require!(
        rating_a_before >= RATING_FLOOR
            && rating_b_before >= RATING_FLOOR
            && rating_formula_version > 0,
        ErrorCode::InvalidBattleState
    );
    require!(
        ctx.accounts.player_a.key() != ctx.accounts.player_b.key(),
        ErrorCode::SameBattlePlayer
    );
    require!(
        ctx.accounts.player_a.key() != Pubkey::default()
            && ctx.accounts.player_b.key() != Pubkey::default(),
        ErrorCode::InvalidBattleState
    );
    require!(
        (matches!(mode, BattleMode::Ranked) && league == Pubkey::default())
            || (matches!(mode, BattleMode::League) && league != Pubkey::default()),
        ErrorCode::InvalidRatedBattleMode
    );
    match mode {
        BattleMode::Ranked => require!(
            ctx.accounts.league_account.is_none()
                && ctx.accounts.league_member_a.is_none()
                && ctx.accounts.league_member_b.is_none()
                && league_round_no == 0,
            ErrorCode::InvalidRatedBattleMode
        ),
        BattleMode::League => {
            require!(league_round_no > 0, ErrorCode::InvalidRatedBattleMode);
            let league_account = ctx
                .accounts
                .league_account
                .as_ref()
                .ok_or_else(|| error!(ErrorCode::InvalidLeague))?;
            require_keys_eq!(league_account.key(), league, ErrorCode::InvalidLeague);
            let (expected_league, _) = Pubkey::find_program_address(
                &[
                    crate::constants::LEAGUE_SEED,
                    &league_account.league_id.to_le_bytes(),
                ],
                &crate::id(),
            );
            require_keys_eq!(
                league_account.key(),
                expected_league,
                ErrorCode::InvalidLeague
            );
            require!(
                league_account.rated
                    && league_account.state == LeagueState::Active
                    && league_account.current_round == league_round_no
                    && league_round_no <= league_account.total_rounds,
                ErrorCode::InvalidLeague
            );

            let member_a = ctx
                .accounts
                .league_member_a
                .as_ref()
                .ok_or_else(|| error!(ErrorCode::NotLeagueMember))?;
            let member_b = ctx
                .accounts
                .league_member_b
                .as_ref()
                .ok_or_else(|| error!(ErrorCode::NotLeagueMember))?;
            require_active_league_member(
                member_a,
                league_account.key(),
                ctx.accounts.player_a.key(),
            )?;
            require_active_league_member(
                member_b,
                league_account.key(),
                ctx.accounts.player_b.key(),
            )?;
            require_keys_eq!(
                member_a.key(),
                expected_member(league_account.key(), ctx.accounts.player_a.key()),
                ErrorCode::NotLeagueMember
            );
            require_keys_eq!(
                member_b.key(),
                expected_member(league_account.key(), ctx.accounts.player_b.key()),
                ErrorCode::NotLeagueMember
            );
        }
        BattleMode::Exhibition => unreachable!(),
    }

    let battle = &mut ctx.accounts.battle;
    battle.battle_id = battle_id;
    battle.market_round = ctx.accounts.market_round.key();
    battle.mode = mode;
    battle.rated = true;
    battle.league = league;
    battle.league_round_no = league_round_no;
    battle.rating_a_before = rating_a_before;
    battle.rating_b_before = rating_b_before;
    battle.rating_formula_version = rating_formula_version;
    battle.a = BattleSide::new(ctx.accounts.player_a.key());
    battle.b = BattleSide::new(ctx.accounts.player_b.key());
    battle.result = BattleResult::Pending;
    battle.void_reason = crate::state::VoidReason::None;
    battle.created_at = Clock::get()?.unix_timestamp;
    battle.finalized_at = 0;
    battle.bump = ctx.bumps.battle;

    ctx.accounts.market_round.rated_battle_count = ctx
        .accounts
        .market_round
        .rated_battle_count
        .checked_add(1)
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;

    let market_round = ctx.accounts.market_round.key();
    let battle_key = battle.key();
    for (slot, player) in [
        (&mut ctx.accounts.rated_slot_a, ctx.accounts.player_a.key()),
        (&mut ctx.accounts.rated_slot_b, ctx.accounts.player_b.key()),
    ] {
        slot.market_round = market_round;
        slot.player = player;
        slot.battle = battle_key;
        slot.bump = if player == ctx.accounts.player_a.key() {
            ctx.bumps.rated_slot_a
        } else {
            ctx.bumps.rated_slot_b
        };
    }
    Ok(())
}

impl BattleSide {
    fn new(player: Pubkey) -> Self {
        Self {
            player,
            commitment: [0; 32],
            committed: false,
            commit_slot: 0,
            asset_ids: [0; LINEUP_SIZE],
            captain_asset_id: 0,
            revealed: false,
            reveal_slot: 0,
            score_q9: 0,
            score_finalized: false,
            status: SideStatus::AwaitingCommit,
        }
    }
}

#[derive(Accounts)]
pub struct CommitLineup<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [
            BATTLE_SEED,
            battle.market_round.as_ref(),
            &battle.battle_id.to_le_bytes()
        ],
        bump = battle.bump,
        constraint = battle.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub battle: Account<'info, Battle>,
    #[account(
        seeds = [
            crate::constants::MARKET_ROUND_SEED,
            &market_round.round_id.to_le_bytes()
        ],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    pub player: Signer<'info>,
}

#[derive(Accounts)]
pub struct CancelUncommittedBattle<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [crate::constants::MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    pub coordinator: Signer<'info>,
    #[account(
        mut,
        seeds = [BATTLE_SEED, market_round.key().as_ref(), &battle.battle_id.to_le_bytes()],
        bump = battle.bump,
        constraint = battle.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub battle: Account<'info, Battle>,
}

pub fn handle_cancel_uncommitted_battle(ctx: Context<CancelUncommittedBattle>) -> Result<()> {
    require_keys_eq!(
        ctx.accounts.config.coordinator_authority,
        ctx.accounts.coordinator.key(),
        ErrorCode::UnauthorizedCoordinator
    );
    let battle = &mut ctx.accounts.battle;
    require!(
        battle.result == BattleResult::Pending,
        ErrorCode::BattleAlreadyFinalized
    );
    require!(
        !battle.a.committed && !battle.b.committed,
        ErrorCode::InvalidBattleState
    );
    battle.result = BattleResult::Voided;
    battle.void_reason = VoidReason::SystemIncident;
    battle.finalized_at = Clock::get()?.unix_timestamp;
    ctx.accounts.market_round.resolved_battle_count = ctx
        .accounts
        .market_round
        .resolved_battle_count
        .checked_add(1)
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    Ok(())
}

pub fn handle_commit_lineup(ctx: Context<CommitLineup>, commitment: [u8; 32]) -> Result<()> {
    let clock = Clock::get()?;
    require!(!ctx.accounts.config.paused, ErrorCode::ProtocolPaused);
    require!(
        ctx.accounts.market_round.state == MarketRoundState::CommitOpen,
        ErrorCode::InvalidRoundState
    );
    require!(
        ctx.accounts.battle.result == BattleResult::Pending,
        ErrorCode::BattleAlreadyFinalized
    );
    require!(
        clock.unix_timestamp <= ctx.accounts.market_round.commit_deadline,
        ErrorCode::CommitWindowClosed
    );
    require!(commitment != [0; 32], ErrorCode::InvalidCommitmentState);

    let side = side_for_player_mut(&mut ctx.accounts.battle, ctx.accounts.player.key())?;
    require!(!side.committed, ErrorCode::InvalidCommitmentState);
    side.commitment = commitment;
    side.committed = true;
    side.commit_slot = clock.slot;
    side.status = SideStatus::Committed;
    Ok(())
}

#[derive(Accounts)]
pub struct RevealLineup<'info> {
    #[account(
        mut,
        seeds = [
            BATTLE_SEED,
            battle.market_round.as_ref(),
            &battle.battle_id.to_le_bytes()
        ],
        bump = battle.bump,
        constraint = battle.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub battle: Account<'info, Battle>,
    #[account(
        seeds = [
            crate::constants::MARKET_ROUND_SEED,
            &market_round.round_id.to_le_bytes()
        ],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    pub player: Signer<'info>,
}

pub fn handle_reveal_lineup(
    ctx: Context<RevealLineup>,
    asset_ids: [u16; LINEUP_SIZE],
    captain_asset_id: u16,
    salt: [u8; 32],
) -> Result<()> {
    let clock = Clock::get()?;
    require!(
        ctx.accounts.market_round.state == MarketRoundState::RevealOpen,
        ErrorCode::InvalidRoundState
    );
    require!(
        ctx.accounts.battle.result == BattleResult::Pending,
        ErrorCode::BattleAlreadyFinalized
    );
    require!(
        clock.unix_timestamp >= ctx.accounts.market_round.commit_deadline
            && clock.unix_timestamp <= ctx.accounts.market_round.reveal_deadline,
        ErrorCode::RevealWindowClosed
    );

    validate_lineup(
        asset_ids,
        captain_asset_id,
        &ctx.accounts.market_round.eligible_asset_bitmap,
    )?;
    let player_key = ctx.accounts.player.key();
    let battle_key = ctx.accounts.battle.key();
    let expected = crate::math::canonical_lineup_commitment(
        crate::id(),
        battle_key,
        player_key,
        ctx.accounts.market_round.registry_version,
        asset_ids,
        captain_asset_id,
        salt,
    );

    let side = side_for_player_mut(&mut ctx.accounts.battle, player_key)?;
    require!(
        side.committed && !side.revealed,
        ErrorCode::InvalidCommitmentState
    );
    require!(side.commitment == expected, ErrorCode::CommitmentMismatch);
    let mut canonical_ids = asset_ids;
    canonical_ids.sort_unstable();
    side.asset_ids = canonical_ids;
    side.captain_asset_id = captain_asset_id;
    side.revealed = true;
    side.reveal_slot = clock.slot;
    side.status = SideStatus::Revealed;
    Ok(())
}

fn side_for_player_mut(battle: &mut Battle, player: Pubkey) -> Result<&mut BattleSide> {
    if battle.a.player == player {
        Ok(&mut battle.a)
    } else if battle.b.player == player {
        Ok(&mut battle.b)
    } else {
        err!(ErrorCode::NotBattleParticipant)
    }
}

#[derive(Accounts)]
pub struct SettleSideScore<'info> {
    #[account(
        mut,
        seeds = [BATTLE_SEED, market_round.key().as_ref(), &battle.battle_id.to_le_bytes()],
        bump = battle.bump,
        constraint = battle.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub battle: Account<'info, Battle>,
    #[account(
        seeds = [crate::constants::MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump,
        constraint = battle.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub market_round: Account<'info, MarketRound>,
    pub settler: Signer<'info>,
}

pub fn handle_settle_side_score(ctx: Context<SettleSideScore>, side_index: u8) -> Result<()> {
    require!(side_index <= 1, ErrorCode::InvalidBattleState);
    let (asset_ids, captain_asset_id, revealed, score_finalized) = {
        let side = side_for_index(&ctx.accounts.battle, side_index)?;
        (
            side.asset_ids,
            side.captain_asset_id,
            side.revealed,
            side.score_finalized,
        )
    };
    require!(
        ctx.accounts.battle.result == BattleResult::Pending,
        ErrorCode::BattleAlreadyFinalized
    );
    require!(revealed, ErrorCode::InvalidBattleState);
    require!(!score_finalized, ErrorCode::InvalidBattleState);
    require!(
        ctx.remaining_accounts.len() == LINEUP_SIZE,
        ErrorCode::RoundAssetUnavailable
    );

    let mut returns = [0i64; LINEUP_SIZE];
    let mut found = [false; LINEUP_SIZE];
    for account in ctx.remaining_accounts {
        require_keys_eq!(
            *account.owner,
            crate::id(),
            ErrorCode::RoundAssetUnavailable
        );
        let data = account.try_borrow_data()?;
        let mut slice: &[u8] = &data;
        let round_asset = RoundAsset::try_deserialize(&mut slice)
            .map_err(|_| error!(ErrorCode::RoundAssetUnavailable))?;
        require_keys_eq!(
            round_asset.market_round,
            ctx.accounts.market_round.key(),
            ErrorCode::WrongMarketRound
        );
        let (expected_key, _) = Pubkey::find_program_address(
            &[
                crate::constants::ROUND_ASSET_SEED,
                ctx.accounts.market_round.key().as_ref(),
                &round_asset.asset_id.to_le_bytes(),
            ],
            &crate::id(),
        );
        require_keys_eq!(*account.key, expected_key, ErrorCode::RoundAssetUnavailable);
        require!(
            round_asset.start_finalized
                && round_asset.end_finalized
                && round_asset.available
                && !round_asset.start_unavailable
                && !round_asset.end_unavailable,
            ErrorCode::RoundAssetUnavailable
        );
        let position = asset_ids
            .iter()
            .position(|asset_id| *asset_id == round_asset.asset_id)
            .ok_or_else(|| error!(ErrorCode::RoundAssetUnavailable))?;
        require!(!found[position], ErrorCode::RoundAssetUnavailable);
        found[position] = true;
        returns[position] = round_asset.return_q9;
    }
    require!(
        found.into_iter().all(|present| present),
        ErrorCode::RoundAssetUnavailable
    );
    let score_q9 = lineup_score_q9(returns, asset_ids, captain_asset_id)?;
    let side = side_for_index_mut(&mut ctx.accounts.battle, side_index)?;
    side.score_q9 = score_q9;
    side.score_finalized = true;
    side.status = SideStatus::ScoreFinalized;
    Ok(())
}

#[derive(Accounts)]
pub struct FinalizeBattle<'info> {
    #[account(
        mut,
        seeds = [BATTLE_SEED, market_round.key().as_ref(), &battle.battle_id.to_le_bytes()],
        bump = battle.bump,
        constraint = battle.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub battle: Account<'info, Battle>,
    #[account(
        mut,
        seeds = [crate::constants::MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    pub finalizer: Signer<'info>,
}

pub fn handle_finalize_battle(ctx: Context<FinalizeBattle>) -> Result<()> {
    let battle = &mut ctx.accounts.battle;
    require!(
        battle.result == BattleResult::Pending,
        ErrorCode::BattleAlreadyFinalized
    );
    require!(
        battle.a.score_finalized && battle.b.score_finalized,
        ErrorCode::InvalidBattleState
    );
    battle.result = if battle.a.score_q9 > battle.b.score_q9 {
        BattleResult::PlayerA
    } else if battle.a.score_q9 < battle.b.score_q9 {
        BattleResult::PlayerB
    } else {
        BattleResult::Draw
    };
    battle.finalized_at = Clock::get()?.unix_timestamp;
    ctx.accounts.market_round.resolved_battle_count = ctx
        .accounts
        .market_round
        .resolved_battle_count
        .checked_add(1)
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    Ok(())
}

#[derive(Accounts)]
pub struct FinalizeForfeit<'info> {
    #[account(
        mut,
        seeds = [BATTLE_SEED, market_round.key().as_ref(), &battle.battle_id.to_le_bytes()],
        bump = battle.bump,
        constraint = battle.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub battle: Account<'info, Battle>,
    pub finalizer: Signer<'info>,
    #[account(
        mut,
        seeds = [crate::constants::MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
}

pub fn handle_finalize_forfeit(ctx: Context<FinalizeForfeit>) -> Result<()> {
    require_keys_eq!(
        ctx.accounts.battle.market_round,
        ctx.accounts.market_round.key(),
        ErrorCode::WrongMarketRound
    );
    require!(
        Clock::get()?.unix_timestamp > ctx.accounts.market_round.reveal_deadline,
        ErrorCode::RevealWindowClosed
    );
    let battle = &mut ctx.accounts.battle;
    require!(
        battle.result == BattleResult::Pending,
        ErrorCode::BattleAlreadyFinalized
    );
    let a_failed = !battle.a.revealed;
    let b_failed = !battle.b.revealed;
    require!(a_failed || b_failed, ErrorCode::InvalidBattleState);
    battle.result = match (a_failed, b_failed) {
        (true, false) => BattleResult::ForfeitA,
        (false, true) => BattleResult::ForfeitB,
        (true, true) => BattleResult::BothForfeit,
        (false, false) => unreachable!(),
    };
    if a_failed {
        battle.a.status = SideStatus::Forfeited;
    }
    if b_failed {
        battle.b.status = SideStatus::Forfeited;
    }
    battle.finalized_at = Clock::get()?.unix_timestamp;
    ctx.accounts.market_round.resolved_battle_count = ctx
        .accounts
        .market_round
        .resolved_battle_count
        .checked_add(1)
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    Ok(())
}

#[derive(Accounts)]
pub struct VoidBattleIfPriceUnavailable<'info> {
    #[account(
        mut,
        seeds = [BATTLE_SEED, market_round.key().as_ref(), &battle.battle_id.to_le_bytes()],
        bump = battle.bump,
        constraint = battle.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub battle: Account<'info, Battle>,
    #[account(
        mut,
        seeds = [crate::constants::MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump,
        constraint = battle.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub market_round: Account<'info, MarketRound>,
    pub marker: Signer<'info>,
}

pub fn handle_void_battle_if_price_unavailable(
    ctx: Context<VoidBattleIfPriceUnavailable>,
) -> Result<()> {
    let battle = &mut ctx.accounts.battle;
    require!(
        battle.result == BattleResult::Pending,
        ErrorCode::BattleAlreadyFinalized
    );
    require!(
        battle.a.revealed || battle.b.revealed,
        ErrorCode::InvalidBattleState
    );
    let mut selected_asset_ids = Vec::new();
    if battle.a.revealed {
        selected_asset_ids.extend_from_slice(&battle.a.asset_ids);
    }
    if battle.b.revealed {
        selected_asset_ids.extend_from_slice(&battle.b.asset_ids);
    }

    let mut unavailable = false;
    for account in ctx.remaining_accounts {
        require_keys_eq!(
            *account.owner,
            crate::id(),
            ErrorCode::RoundAssetUnavailable
        );
        let data = account.try_borrow_data()?;
        let mut slice: &[u8] = &data;
        let round_asset = RoundAsset::try_deserialize(&mut slice)
            .map_err(|_| error!(ErrorCode::RoundAssetUnavailable))?;
        require_keys_eq!(
            round_asset.market_round,
            ctx.accounts.market_round.key(),
            ErrorCode::WrongMarketRound
        );
        let (expected_key, _) = Pubkey::find_program_address(
            &[
                crate::constants::ROUND_ASSET_SEED,
                ctx.accounts.market_round.key().as_ref(),
                &round_asset.asset_id.to_le_bytes(),
            ],
            &crate::id(),
        );
        require_keys_eq!(*account.key, expected_key, ErrorCode::RoundAssetUnavailable);
        unavailable |= selected_asset_ids.contains(&round_asset.asset_id)
            && (round_asset.start_unavailable || round_asset.end_unavailable);
    }
    require!(unavailable, ErrorCode::RoundAssetUnavailable);
    battle.result = BattleResult::Voided;
    battle.void_reason = VoidReason::PriceUnavailable;
    battle.finalized_at = Clock::get()?.unix_timestamp;
    ctx.accounts.market_round.resolved_battle_count = ctx
        .accounts
        .market_round
        .resolved_battle_count
        .checked_add(1)
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    Ok(())
}

/// Voids a pending Battle for a declared platform or settlement incident.
/// This path is coordinator-controlled and requires the protocol pause, which
/// keeps infrastructure failure distinct from a player's reveal forfeit.
#[derive(Accounts)]
pub struct VoidBattleForSystemIncident<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [crate::constants::MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    #[account(
        mut,
        seeds = [BATTLE_SEED, market_round.key().as_ref(), &battle.battle_id.to_le_bytes()],
        bump = battle.bump,
        constraint = battle.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub battle: Account<'info, Battle>,
    pub coordinator: Signer<'info>,
}

pub fn handle_void_battle_for_system_incident(
    ctx: Context<VoidBattleForSystemIncident>,
) -> Result<()> {
    require_keys_eq!(
        ctx.accounts.config.coordinator_authority,
        ctx.accounts.coordinator.key(),
        ErrorCode::UnauthorizedCoordinator
    );
    require!(ctx.accounts.config.paused, ErrorCode::InvalidBattleState);
    let battle = &mut ctx.accounts.battle;
    require!(
        battle.result == BattleResult::Pending,
        ErrorCode::BattleAlreadyFinalized
    );
    battle.result = BattleResult::Voided;
    battle.void_reason = VoidReason::SystemIncident;
    battle.finalized_at = Clock::get()?.unix_timestamp;
    ctx.accounts.market_round.resolved_battle_count = ctx
        .accounts
        .market_round
        .resolved_battle_count
        .checked_add(1)
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    Ok(())
}

fn side_for_index(battle: &Battle, side_index: u8) -> Result<&BattleSide> {
    match side_index {
        0 => Ok(&battle.a),
        1 => Ok(&battle.b),
        _ => err!(ErrorCode::InvalidBattleState),
    }
}

fn side_for_index_mut(battle: &mut Battle, side_index: u8) -> Result<&mut BattleSide> {
    match side_index {
        0 => Ok(&mut battle.a),
        1 => Ok(&mut battle.b),
        _ => err!(ErrorCode::InvalidBattleState),
    }
}

fn expected_member(league: Pubkey, player: Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            crate::constants::LEAGUE_MEMBER_SEED,
            league.as_ref(),
            player.as_ref(),
        ],
        &crate::id(),
    )
    .0
}

fn require_active_league_member(
    member: &LeagueMember,
    league: Pubkey,
    player: Pubkey,
) -> Result<()> {
    require!(member.active, ErrorCode::NotLeagueMember);
    require_keys_eq!(member.league, league, ErrorCode::NotLeagueMember);
    require_keys_eq!(member.player, player, ErrorCode::NotLeagueMember);
    Ok(())
}

/// Enforces the only period in which a coordinator may admit a rated Battle.
///
/// A Market Round remains in its pre-start state while queue results are
/// materialized. The clock check is therefore required in addition to the
/// account state check: without it, a stale `Scheduled` account could accept
/// new rated exposure after the commit deadline.
fn validate_rated_battle_window(round: &MarketRound, now: i64) -> Result<()> {
    require!(
        matches!(
            round.state,
            MarketRoundState::Scheduled | MarketRoundState::CommitOpen
        ),
        ErrorCode::InvalidRoundState
    );
    require!(
        now <= round.commit_deadline,
        ErrorCode::RatedBattleWindowClosed
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheduled_round() -> MarketRound {
        MarketRound {
            round_id: 1,
            registry_version: 1,
            competition_domain: crate::state::CompetitionDomain::PublicEquity,
            settlement_policy_version: 1,
            settlement_source_kind: crate::state::SettlementSourceKind::JupiterTokenSpotV1,
            settlement_source_config: Pubkey::new_from_array([1; 32]),
            jupiter_source_config_version: 1,
            price_policy_version: 1,
            market_quality_policy_version: 1,
            attestor_set_version: 1,
            market_quality_policy_hash: [1; 32],
            eligibility_snapshot_hash: [2; 32],
            eligibility_frozen_at: 10,
            queue_close_at: 20,
            commit_deadline: 30,
            reveal_deadline: 40,
            start_target_at: 50,
            end_target_at: 60,
            observation_window_secs: 10,
            attestation_grace_secs: 10,
            max_attestor_spread_bps: 100,
            eligible_asset_bitmap: [1; 4],
            round_asset_count: 1,
            rated_battle_count: 0,
            resolved_battle_count: 0,
            state: MarketRoundState::Scheduled,
            is_replay: false,
            bump: 1,
        }
    }

    #[test]
    fn rated_battle_creation_rejects_after_commit_deadline() {
        let round = scheduled_round();

        assert!(validate_rated_battle_window(&round, 30).is_ok());
        assert!(validate_rated_battle_window(&round, 31).is_err());
    }
}
