use anchor_lang::prelude::*;

use crate::{
    constants::{CONFIG_SEED, LEAGUE_MEMBER_SEED, LEAGUE_SEED},
    error::ErrorCode,
    state::{Config, League, LeagueMember, LeagueState},
};

const MAX_OFFICIAL_LEAGUE_PLAYERS: u16 = 100;

#[derive(Accounts)]
#[instruction(league_id: u64)]
pub struct CreateOfficialLeague<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub coordinator: Signer<'info>,
    #[account(
        init,
        payer = coordinator,
        space = 8 + League::INIT_SPACE,
        seeds = [LEAGUE_SEED, &league_id.to_le_bytes()],
        bump
    )]
    pub league: Account<'info, League>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_official_league(
    ctx: Context<CreateOfficialLeague>,
    league_id: u64,
    max_players: u16,
    total_rounds: u16,
    pairing_policy_version: u16,
    registration_close_at: i64,
) -> Result<()> {
    require_coordinator(&ctx.accounts.config, &ctx.accounts.coordinator)?;
    require!(!ctx.accounts.config.paused, ErrorCode::ProtocolPaused);
    require!(
        (2..=MAX_OFFICIAL_LEAGUE_PLAYERS).contains(&max_players),
        ErrorCode::InvalidLeague
    );
    require!(
        total_rounds > 0 && pairing_policy_version > 0,
        ErrorCode::InvalidLeague
    );
    require!(
        registration_close_at > Clock::get()?.unix_timestamp,
        ErrorCode::InvalidLeague
    );

    let league = &mut ctx.accounts.league;
    league.league_id = league_id;
    league.creator = ctx.accounts.coordinator.key();
    league.max_players = max_players;
    league.joined_players = 0;
    league.total_rounds = total_rounds;
    league.current_round = 0;
    league.pairing_policy_version = pairing_policy_version;
    league.rated = true;
    league.registration_close_at = registration_close_at;
    league.state = LeagueState::Registration;
    league.bump = ctx.bumps.league;
    Ok(())
}

#[derive(Accounts)]
pub struct ActivateLeague<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    pub coordinator: Signer<'info>,
    #[account(mut, seeds = [LEAGUE_SEED, &league.league_id.to_le_bytes()], bump = league.bump)]
    pub league: Account<'info, League>,
}

pub fn handle_activate_league(ctx: Context<ActivateLeague>) -> Result<()> {
    require_coordinator(&ctx.accounts.config, &ctx.accounts.coordinator)?;
    let league = &mut ctx.accounts.league;
    require!(
        league.state == LeagueState::Registration,
        ErrorCode::InvalidLeague
    );
    require!(
        Clock::get()?.unix_timestamp >= league.registration_close_at,
        ErrorCode::LeagueRegistrationClosed
    );
    require!(league.joined_players >= 2, ErrorCode::InvalidLeague);
    league.state = LeagueState::Active;
    league.current_round = 1;
    Ok(())
}

#[derive(Accounts)]
pub struct CancelLeague<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    pub coordinator: Signer<'info>,
    #[account(mut, seeds = [LEAGUE_SEED, &league.league_id.to_le_bytes()], bump = league.bump)]
    pub league: Account<'info, League>,
}

pub fn handle_cancel_league(ctx: Context<CancelLeague>) -> Result<()> {
    require_coordinator(&ctx.accounts.config, &ctx.accounts.coordinator)?;
    require!(
        matches!(
            ctx.accounts.league.state,
            LeagueState::Registration | LeagueState::Active
        ),
        ErrorCode::InvalidLeague
    );
    ctx.accounts.league.state = LeagueState::Cancelled;
    Ok(())
}

#[derive(Accounts)]
pub struct JoinLeague<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub player: Signer<'info>,
    #[account(mut, seeds = [LEAGUE_SEED, &league.league_id.to_le_bytes()], bump = league.bump)]
    pub league: Account<'info, League>,
    #[account(
        init,
        payer = player,
        space = 8 + LeagueMember::INIT_SPACE,
        seeds = [LEAGUE_MEMBER_SEED, league.key().as_ref(), player.key().as_ref()],
        bump
    )]
    pub member: Account<'info, LeagueMember>,
    pub system_program: Program<'info, System>,
}

pub fn handle_join_league(ctx: Context<JoinLeague>) -> Result<()> {
    require!(!ctx.accounts.config.paused, ErrorCode::ProtocolPaused);
    let league = &mut ctx.accounts.league;
    require!(
        league.state == LeagueState::Registration,
        ErrorCode::InvalidLeague
    );
    require!(
        Clock::get()?.unix_timestamp <= league.registration_close_at,
        ErrorCode::LeagueRegistrationClosed
    );
    require!(
        league.joined_players < league.max_players,
        ErrorCode::LeagueFull
    );

    league.joined_players += 1;
    let member = &mut ctx.accounts.member;
    member.league = league.key();
    member.player = ctx.accounts.player.key();
    member.joined_at = Clock::get()?.unix_timestamp;
    member.active = true;
    member.bye_count = 0;
    member.bump = ctx.bumps.member;
    Ok(())
}

#[derive(Accounts)]
pub struct LeaveLeagueBeforeClose<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub player: Signer<'info>,
    #[account(mut, seeds = [LEAGUE_SEED, &league.league_id.to_le_bytes()], bump = league.bump)]
    pub league: Account<'info, League>,
    #[account(
        mut,
        close = player,
        seeds = [LEAGUE_MEMBER_SEED, league.key().as_ref(), player.key().as_ref()],
        bump = member.bump,
        constraint = member.league == league.key() @ ErrorCode::InvalidLeague,
        constraint = member.player == player.key() @ ErrorCode::NotLeagueMember
    )]
    pub member: Account<'info, LeagueMember>,
}

pub fn handle_leave_league_before_close(ctx: Context<LeaveLeagueBeforeClose>) -> Result<()> {
    let league = &mut ctx.accounts.league;
    require!(
        league.state == LeagueState::Registration,
        ErrorCode::InvalidLeague
    );
    require!(
        Clock::get()?.unix_timestamp <= league.registration_close_at,
        ErrorCode::LeagueRegistrationClosed
    );
    require!(ctx.accounts.member.active, ErrorCode::NotLeagueMember);
    league.joined_players = league
        .joined_players
        .checked_sub(1)
        .ok_or_else(|| error!(ErrorCode::InvalidLeague))?;
    Ok(())
}

#[derive(Accounts)]
pub struct DeactivateLeagueMember<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    pub coordinator: Signer<'info>,
    #[account(seeds = [LEAGUE_SEED, &league.league_id.to_le_bytes()], bump = league.bump)]
    pub league: Account<'info, League>,
    #[account(mut)]
    pub member: Account<'info, LeagueMember>,
}

pub fn handle_deactivate_league_member(ctx: Context<DeactivateLeagueMember>) -> Result<()> {
    require_coordinator(&ctx.accounts.config, &ctx.accounts.coordinator)?;
    require_keys_eq!(
        ctx.accounts.member.league,
        ctx.accounts.league.key(),
        ErrorCode::InvalidLeague
    );
    require!(ctx.accounts.member.active, ErrorCode::NotLeagueMember);
    ctx.accounts.member.active = false;
    Ok(())
}

fn require_coordinator(config: &Config, signer: &Signer) -> Result<()> {
    require_keys_eq!(
        config.coordinator_authority,
        signer.key(),
        ErrorCode::UnauthorizedCoordinator
    );
    Ok(())
}
