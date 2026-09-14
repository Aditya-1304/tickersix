use anchor_lang::prelude::*;

use crate::{
    constants::{
        ASSET_SEED, ATTESTOR_COUNT, ATTESTOR_QUORUM, ATTESTOR_SET_SEED, CONFIG_SEED,
        MAX_ASSETS_PER_REGISTRY, PRICE_POLICY_SEED, PROTOCOL_VERSION, QUALITY_POLICY_SEED,
    },
    error::ErrorCode,
    state::{
        AssetRegistryEntry, AttestorSet, Config, MarketQualityPolicy, PricePolicy, PriceSourceKind,
    },
};

#[derive(Accounts)]
pub struct InitializeConfig<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(init, payer = payer, space = 8 + Config::INIT_SPACE, seeds = [CONFIG_SEED], bump)]
    pub config: Account<'info, Config>,
    pub system_program: Program<'info, System>,
}

pub fn handle_initialize_config(ctx: Context<InitializeConfig>) -> Result<()> {
    let config = &mut ctx.accounts.config;
    config.admin_authority = ctx.accounts.payer.key();
    config.coordinator_authority = ctx.accounts.payer.key();
    config.paused = false;
    config.current_registry_version = 0;
    config.registry_frozen = false;
    config.last_market_round_id = 0;
    config.last_market_round_end_at = 0;
    config.protocol_version = PROTOCOL_VERSION;
    config.current_price_policy_version = 0;
    config.current_market_quality_policy_version = 0;
    config.current_attestor_set_version = 0;
    config.bump = ctx.bumps.config;
    Ok(())
}

#[derive(Accounts)]
pub struct RotateAdmin<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    pub admin: Signer<'info>,
    /// CHECK: The new authority is only stored as a public key and must be non-default.
    pub new_admin: UncheckedAccount<'info>,
}

pub fn handle_rotate_admin(ctx: Context<RotateAdmin>) -> Result<()> {
    require_admin(&ctx.accounts.config, &ctx.accounts.admin)?;
    require!(
        ctx.accounts.new_admin.key() != Pubkey::default(),
        ErrorCode::UnauthorizedAdmin
    );
    ctx.accounts.config.admin_authority = ctx.accounts.new_admin.key();
    Ok(())
}

#[derive(Accounts)]
pub struct RotateCoordinator<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    pub admin: Signer<'info>,
    /// CHECK: The new coordinator is only stored as a public key and must be non-default.
    pub new_coordinator: UncheckedAccount<'info>,
}

pub fn handle_rotate_coordinator(ctx: Context<RotateCoordinator>) -> Result<()> {
    require_admin(&ctx.accounts.config, &ctx.accounts.admin)?;
    require!(
        ctx.accounts.new_coordinator.key() != Pubkey::default(),
        ErrorCode::UnauthorizedCoordinator
    );
    ctx.accounts.config.coordinator_authority = ctx.accounts.new_coordinator.key();
    Ok(())
}

#[derive(Accounts)]
#[instruction(version: u16)]
pub struct CreatePricePolicy<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + PricePolicy::INIT_SPACE,
        seeds = [PRICE_POLICY_SEED, &version.to_le_bytes()],
        bump
    )]
    pub price_policy: Account<'info, PricePolicy>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_price_policy(
    ctx: Context<CreatePricePolicy>,
    version: u16,
    source_kind: PriceSourceKind,
    observation_window_secs: u16,
    attestation_grace_secs: u16,
    sample_interval_secs: u16,
    max_attestor_spread_bps: u16,
    min_accepted_observations: u16,
    min_unique_source_blocks: u16,
    max_source_block_lag: u64,
) -> Result<()> {
    require_admin(&ctx.accounts.config, &ctx.accounts.payer)?;
    require!(
        version > ctx.accounts.config.current_price_policy_version,
        ErrorCode::InvalidPolicyVersion
    );
    require!(
        observation_window_secs > 0
            && attestation_grace_secs > 0
            && sample_interval_secs > 0
            && sample_interval_secs <= observation_window_secs
            && max_attestor_spread_bps > 0
            && max_attestor_spread_bps <= 10_000
            && min_accepted_observations > 0
            && min_unique_source_blocks > 0
            && min_unique_source_blocks <= min_accepted_observations
            && max_source_block_lag > 0,
        ErrorCode::UncalibratedPolicy
    );

    let policy = &mut ctx.accounts.price_policy;
    policy.version = version;
    policy.source_kind = source_kind;
    policy.observation_window_secs = observation_window_secs;
    policy.attestation_grace_secs = attestation_grace_secs;
    policy.sample_interval_secs = sample_interval_secs;
    policy.max_attestor_spread_bps = max_attestor_spread_bps;
    policy.min_accepted_observations = min_accepted_observations;
    policy.min_unique_source_blocks = min_unique_source_blocks;
    policy.max_source_block_lag = max_source_block_lag;
    policy.bump = ctx.bumps.price_policy;
    ctx.accounts.config.current_price_policy_version = version;
    Ok(())
}

#[derive(Accounts)]
#[instruction(version: u16)]
pub struct CreateMarketQualityPolicy<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + MarketQualityPolicy::INIT_SPACE,
        seeds = [QUALITY_POLICY_SEED, &version.to_le_bytes()],
        bump
    )]
    pub market_quality_policy: Account<'info, MarketQualityPolicy>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_market_quality_policy(
    ctx: Context<CreateMarketQualityPolicy>,
    version: u16,
    canonical_policy_hash: [u8; 32],
    min_eligible_assets: u16,
) -> Result<()> {
    require_admin(&ctx.accounts.config, &ctx.accounts.payer)?;
    require!(
        version > ctx.accounts.config.current_market_quality_policy_version,
        ErrorCode::InvalidPolicyVersion
    );
    require!(
        canonical_policy_hash != [0; 32],
        ErrorCode::UncalibratedPolicy
    );
    require!(
        (6..=MAX_ASSETS_PER_REGISTRY).contains(&min_eligible_assets),
        ErrorCode::InsufficientEligibleAssets
    );

    let policy = &mut ctx.accounts.market_quality_policy;
    policy.version = version;
    policy.canonical_policy_hash = canonical_policy_hash;
    policy.min_eligible_assets = min_eligible_assets;
    policy.bump = ctx.bumps.market_quality_policy;
    ctx.accounts.config.current_market_quality_policy_version = version;
    Ok(())
}

#[derive(Accounts)]
#[instruction(version: u16)]
pub struct CreateAttestorSet<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + AttestorSet::INIT_SPACE,
        seeds = [ATTESTOR_SET_SEED, &version.to_le_bytes()],
        bump
    )]
    pub attestor_set: Account<'info, AttestorSet>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_attestor_set(
    ctx: Context<CreateAttestorSet>,
    version: u16,
    attestors: [Pubkey; ATTESTOR_COUNT],
) -> Result<()> {
    require_admin(&ctx.accounts.config, &ctx.accounts.payer)?;
    require!(
        version > ctx.accounts.config.current_attestor_set_version,
        ErrorCode::InvalidPolicyVersion
    );
    require!(
        attestors[0] != Pubkey::default()
            && attestors[1] != Pubkey::default()
            && attestors[2] != Pubkey::default()
            && attestors[0] != attestors[1]
            && attestors[0] != attestors[2]
            && attestors[1] != attestors[2],
        ErrorCode::InvalidAttestorSet
    );

    let set = &mut ctx.accounts.attestor_set;
    set.version = version;
    set.attestors = attestors;
    set.quorum = ATTESTOR_QUORUM;
    set.bump = ctx.bumps.attestor_set;
    ctx.accounts.config.current_attestor_set_version = version;
    Ok(())
}

#[derive(Accounts)]
pub struct SetPause<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    pub admin: Signer<'info>,
}

pub fn handle_set_pause(ctx: Context<SetPause>, paused: bool) -> Result<()> {
    require_admin(&ctx.accounts.config, &ctx.accounts.admin)?;
    ctx.accounts.config.paused = paused;
    Ok(())
}

#[derive(Accounts)]
#[instruction(registry_version: u32, asset_id: u16)]
pub struct CreateRegistryEntry<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + AssetRegistryEntry::INIT_SPACE,
        seeds = [ASSET_SEED, &registry_version.to_le_bytes(), &asset_id.to_le_bytes()],
        bump
    )]
    pub asset: Account<'info, AssetRegistryEntry>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_registry_entry(
    ctx: Context<CreateRegistryEntry>,
    registry_version: u32,
    asset_id: u16,
    symbol: [u8; 8],
    scoring_mint: Pubkey,
    issuer_kind: u8,
) -> Result<()> {
    require_admin(&ctx.accounts.config, &ctx.accounts.payer)?;
    require!(registry_version > 0, ErrorCode::InvalidRegistryVersion);
    require!(
        registry_version >= ctx.accounts.config.current_registry_version,
        ErrorCode::InvalidRegistryVersion
    );
    require!(
        !ctx.accounts.config.registry_frozen
            || registry_version > ctx.accounts.config.current_registry_version,
        ErrorCode::RegistryNotFrozen
    );
    require!(
        asset_id < MAX_ASSETS_PER_REGISTRY,
        ErrorCode::InvalidAssetId
    );
    require!(
        scoring_mint != Pubkey::default(),
        ErrorCode::InvalidScoringMint
    );

    let asset = &mut ctx.accounts.asset;
    asset.registry_version = registry_version;
    asset.asset_id = asset_id;
    asset.symbol = symbol;
    asset.scoring_mint = scoring_mint;
    asset.issuer_kind = issuer_kind;
    asset.active = true;
    asset.bump = ctx.bumps.asset;
    if registry_version > ctx.accounts.config.current_registry_version {
        ctx.accounts.config.current_registry_version = registry_version;
        ctx.accounts.config.registry_frozen = false;
    }
    Ok(())
}

#[derive(Accounts)]
pub struct FreezeRegistryVersion<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    pub admin: Signer<'info>,
}

pub fn handle_freeze_registry_version(ctx: Context<FreezeRegistryVersion>) -> Result<()> {
    require_admin(&ctx.accounts.config, &ctx.accounts.admin)?;
    require!(
        ctx.accounts.config.current_registry_version > 0,
        ErrorCode::InvalidRegistryVersion
    );
    ctx.accounts.config.registry_frozen = true;
    Ok(())
}

fn require_admin(config: &Config, signer: &Signer) -> Result<()> {
    require_keys_eq!(
        config.admin_authority,
        signer.key(),
        ErrorCode::UnauthorizedAdmin
    );
    Ok(())
}
