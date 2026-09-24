//! Feature-gated Pyth Pro settlement path.
//!
//! Pyth is intentionally isolated from the permanent Jupiter path. This file
//! owns the Pyth source configuration, round admission, verifier-receipt
//! evidence, and phase finalization instructions so a default build cannot
//! accidentally advertise or execute Pyth settlement.

use anchor_lang::prelude::*;
use solana_instructions_sysvar::{load_current_index_checked, load_instruction_at_checked};

use crate::{
    constants::{
        CONFIG_SEED, MARKET_ROUND_SEED, PRICE_POLICY_SEED, PYTH_PRICE_EVIDENCE_SEED,
        PYTH_PRO_SOURCE_CONFIG_SEED, QUALITY_POLICY_SEED, ROUND_ASSET_SEED,
    },
    error::ErrorCode,
    math::return_q9,
    state::{
        AssetRegistryEntry, Config, EvidenceKind, LifecycleState, MarketQualityPolicy, MarketRound,
        MarketRoundState, PricePhase, PricePolicy, PythPriceEvidence, PythProSourceConfig,
        RoundAsset, SettlementSourceKind,
    },
};

const PYTH_PAYLOAD_DOMAIN: &[u8] = b"TICKERSIX_PYTH_VERIFIED_PAYLOAD_V1\0";
const MICROS_PER_SECOND: u64 = 1_000_000;

#[derive(Accounts)]
#[instruction(version: u16)]
pub struct CreatePythProSourceConfig<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init,
        payer = payer,
        space = 8 + PythProSourceConfig::INIT_SPACE,
        seeds = [PYTH_PRO_SOURCE_CONFIG_SEED, &version.to_le_bytes()],
        bump
    )]
    pub pyth_source_config: Account<'info, PythProSourceConfig>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_pyth_pro_source_config(
    ctx: Context<CreatePythProSourceConfig>,
    version: u16,
    verifier_program: Pubkey,
    max_payload_timestamp_delta_us: u64,
    max_feed_age_us: u64,
    max_confidence_bps: u16,
    channel_kind: u8,
    canonical_feed_set_hash: [u8; 32],
) -> Result<()> {
    require_admin(&ctx.accounts.config, &ctx.accounts.payer)?;
    require!(
        version > ctx.accounts.config.current_pyth_source_config_version,
        ErrorCode::InvalidPolicyVersion
    );
    require!(
        verifier_program == crate::constants::PYTH_PRO_DEVNET_VERIFIER_PROGRAM
            && max_payload_timestamp_delta_us > 0
            && max_feed_age_us > 0
            && max_confidence_bps > 0
            && max_confidence_bps <= 10_000
            && channel_kind > 0
            && canonical_feed_set_hash != [0; 32],
        ErrorCode::InvalidPythSourceConfig
    );

    let source_config = &mut ctx.accounts.pyth_source_config;
    source_config.version = version;
    source_config.verifier_program = verifier_program;
    source_config.max_payload_timestamp_delta_us = max_payload_timestamp_delta_us;
    source_config.max_feed_age_us = max_feed_age_us;
    source_config.max_confidence_bps = max_confidence_bps;
    source_config.channel_kind = channel_kind;
    source_config.canonical_feed_set_hash = canonical_feed_set_hash;
    source_config.bump = ctx.bumps.pyth_source_config;
    ctx.accounts.config.current_pyth_source_config_version = version;
    Ok(())
}

#[derive(Accounts)]
#[instruction(version: u16, source_config_version: u16)]
pub struct CreatePythSettlementPolicy<'info> {
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
    pub settlement_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [PYTH_PRO_SOURCE_CONFIG_SEED, &source_config_version.to_le_bytes()],
        bump = pyth_source_config.bump,
        constraint = pyth_source_config.version == config.current_pyth_source_config_version @ ErrorCode::InvalidPolicyVersion
    )]
    pub pyth_source_config: Account<'info, PythProSourceConfig>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_pyth_settlement_policy(
    ctx: Context<CreatePythSettlementPolicy>,
    version: u16,
    source_config_version: u16,
    observation_window_secs: u16,
    canonical_policy_hash: [u8; 32],
) -> Result<()> {
    require_admin(&ctx.accounts.config, &ctx.accounts.payer)?;
    require!(
        version > ctx.accounts.config.current_settlement_policy_version
            && source_config_version == ctx.accounts.pyth_source_config.version
            && observation_window_secs > 0
            && canonical_policy_hash != [0; 32],
        ErrorCode::UncalibratedPolicy
    );

    let policy = &mut ctx.accounts.settlement_policy;
    policy.version = version;
    policy.source_kind = SettlementSourceKind::PythProVerifiedV1;
    policy.source_config = ctx.accounts.pyth_source_config.key();
    policy.observation_window_secs = observation_window_secs;
    policy.canonical_policy_hash = canonical_policy_hash;
    policy.bump = ctx.bumps.settlement_policy;
    ctx.accounts.config.current_settlement_policy_version = version;
    ctx.accounts.config.current_price_policy_version = version;
    Ok(())
}

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct CreatePythMarketRoundDraft<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub coordinator: Signer<'info>,
    #[account(
        init,
        payer = coordinator,
        space = 8 + MarketRound::INIT_SPACE,
        seeds = [MARKET_ROUND_SEED, &round_id.to_le_bytes()],
        bump
    )]
    pub market_round: Account<'info, MarketRound>,
    #[account(
        seeds = [PRICE_POLICY_SEED, &config.current_settlement_policy_version.to_le_bytes()],
        bump = settlement_policy.bump
    )]
    pub settlement_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [PYTH_PRO_SOURCE_CONFIG_SEED, &config.current_pyth_source_config_version.to_le_bytes()],
        bump = pyth_source_config.bump
    )]
    pub pyth_source_config: Account<'info, PythProSourceConfig>,
    #[account(
        seeds = [QUALITY_POLICY_SEED, &config.current_market_quality_policy_version.to_le_bytes()],
        bump = market_quality_policy.bump
    )]
    pub market_quality_policy: Account<'info, MarketQualityPolicy>,
    pub system_program: Program<'info, System>,
}

#[allow(clippy::too_many_arguments)]
pub fn handle_create_pyth_market_round_draft(
    ctx: Context<CreatePythMarketRoundDraft>,
    round_id: u64,
    registry_version: u32,
    eligibility_snapshot_hash: [u8; 32],
    eligibility_frozen_at: i64,
    queue_close_at: i64,
    commit_deadline: i64,
    reveal_deadline: i64,
    start_target_at: i64,
    end_target_at: i64,
    is_replay: bool,
) -> Result<()> {
    crate::instructions::round::require_coordinator(
        &ctx.accounts.config,
        &ctx.accounts.coordinator,
    )?;
    require!(!ctx.accounts.config.paused, ErrorCode::ProtocolPaused);
    require!(
        eligibility_snapshot_hash != [0; 32],
        ErrorCode::InvalidEligibilitySnapshot
    );
    require!(
        eligibility_frozen_at < queue_close_at
            && queue_close_at < commit_deadline
            && commit_deadline < reveal_deadline
            && reveal_deadline < start_target_at
            && start_target_at < end_target_at,
        ErrorCode::InvalidRoundTiming
    );
    require!(
        round_id > ctx.accounts.config.last_market_round_id
            && start_target_at >= ctx.accounts.config.last_market_round_end_at,
        ErrorCode::InvalidRoundSequence
    );
    require!(
        registry_version > 0
            && registry_version == ctx.accounts.config.current_registry_version
            && ctx.accounts.config.registry_frozen,
        ErrorCode::InvalidRegistryVersion
    );
    require!(
        ctx.accounts.settlement_policy.version
            == ctx.accounts.config.current_settlement_policy_version
            && ctx.accounts.settlement_policy.source_kind
                == SettlementSourceKind::PythProVerifiedV1
            && ctx.accounts.settlement_policy.source_config
                == ctx.accounts.pyth_source_config.key()
            && ctx.accounts.pyth_source_config.version
                == ctx.accounts.config.current_pyth_source_config_version
            && ctx.accounts.market_quality_policy.version
                == ctx.accounts.config.current_market_quality_policy_version,
        ErrorCode::SourceConfigMismatch
    );

    let round = &mut ctx.accounts.market_round;
    round.round_id = round_id;
    round.registry_version = registry_version;
    round.competition_domain = ctx.accounts.market_quality_policy.competition_domain;
    round.settlement_policy_version = ctx.accounts.settlement_policy.version;
    round.settlement_source_kind = SettlementSourceKind::PythProVerifiedV1;
    round.settlement_source_config = ctx.accounts.pyth_source_config.key();
    round.jupiter_source_config_version = 0;
    round.pyth_source_config_version = ctx.accounts.pyth_source_config.version;
    round.price_policy_version = ctx.accounts.settlement_policy.version;
    round.market_quality_policy_version = ctx.accounts.market_quality_policy.version;
    round.attestor_set_version = 0;
    round.market_quality_policy_hash = ctx.accounts.market_quality_policy.canonical_policy_hash;
    round.eligibility_snapshot_hash = eligibility_snapshot_hash;
    round.eligibility_frozen_at = eligibility_frozen_at;
    round.queue_close_at = queue_close_at;
    round.commit_deadline = commit_deadline;
    round.reveal_deadline = reveal_deadline;
    round.start_target_at = start_target_at;
    round.end_target_at = end_target_at;
    round.observation_window_secs = ctx.accounts.settlement_policy.observation_window_secs;
    round.attestation_grace_secs = 0;
    round.max_attestor_spread_bps = 0;
    round.eligible_asset_bitmap = [0; 4];
    round.round_asset_count = 0;
    round.rated_battle_count = 0;
    round.resolved_battle_count = 0;
    round.state = MarketRoundState::Preparing;
    round.is_replay = is_replay;
    round.bump = ctx.bumps.market_round;
    ctx.accounts.config.last_market_round_id = round_id;
    ctx.accounts.config.last_market_round_end_at = end_target_at;
    Ok(())
}

#[derive(Accounts)]
#[instruction(asset_id: u16)]
pub struct AddPythRoundAsset<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    #[account(mut)]
    pub coordinator: Signer<'info>,
    #[account(
        init,
        payer = coordinator,
        space = 8 + RoundAsset::INIT_SPACE,
        seeds = [ROUND_ASSET_SEED, market_round.key().as_ref(), &asset_id.to_le_bytes()],
        bump
    )]
    pub round_asset: Account<'info, RoundAsset>,
    #[account(
        seeds = [crate::constants::ASSET_SEED, &market_round.registry_version.to_le_bytes(), &asset_id.to_le_bytes()],
        bump = registry_entry.bump,
        constraint = registry_entry.registry_version == market_round.registry_version @ ErrorCode::InvalidRegistryVersion,
        constraint = registry_entry.asset_id == asset_id @ ErrorCode::InvalidAssetId
    )]
    pub registry_entry: Account<'info, AssetRegistryEntry>,
    #[account(
        seeds = [PRICE_POLICY_SEED, &market_round.settlement_policy_version.to_le_bytes()],
        bump = settlement_policy.bump
    )]
    pub settlement_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [PYTH_PRO_SOURCE_CONFIG_SEED, &market_round.pyth_source_config_version.to_le_bytes()],
        bump = pyth_source_config.bump
    )]
    pub pyth_source_config: Account<'info, PythProSourceConfig>,
    #[account(
        seeds = [QUALITY_POLICY_SEED, &market_round.market_quality_policy_version.to_le_bytes()],
        bump = market_quality_policy.bump
    )]
    pub market_quality_policy: Account<'info, MarketQualityPolicy>,
    pub system_program: Program<'info, System>,
}

pub fn handle_add_pyth_round_asset(
    ctx: Context<AddPythRoundAsset>,
    asset_id: u16,
    scoring_mint: Pubkey,
    issuer_kind: u8,
    price_policy_version: u16,
    market_quality_policy_version: u16,
) -> Result<()> {
    require_keys_eq!(
        ctx.accounts.config.coordinator_authority,
        ctx.accounts.coordinator.key(),
        ErrorCode::UnauthorizedCoordinator
    );
    require!(!ctx.accounts.config.paused, ErrorCode::ProtocolPaused);
    require!(
        ctx.accounts.market_round.state == MarketRoundState::Preparing,
        ErrorCode::InvalidRoundState
    );
    require!(
        ctx.accounts.market_round.settlement_source_kind == SettlementSourceKind::PythProVerifiedV1
            && ctx.accounts.settlement_policy.source_kind
                == SettlementSourceKind::PythProVerifiedV1
            && ctx.accounts.settlement_policy.source_config
                == ctx.accounts.pyth_source_config.key(),
        ErrorCode::SourceConfigMismatch
    );
    require!(
        scoring_mint == ctx.accounts.registry_entry.scoring_mint
            && issuer_kind == ctx.accounts.registry_entry.issuer_kind
            && ctx.accounts.registry_entry.descriptor.pyth_feed_id != 0,
        ErrorCode::RegistryEntryMismatch
    );
    require!(
        ctx.accounts.registry_entry.active
            && ctx.accounts.registry_entry.descriptor.enabled
            && ctx.accounts.registry_entry.descriptor.lifecycle_state == LifecycleState::Active
            && ctx.accounts.registry_entry.descriptor.registry_version
                == ctx.accounts.market_round.registry_version
            && ctx.accounts.registry_entry.descriptor.asset_id == asset_id,
        ErrorCode::RegistryEntryMismatch
    );
    require!(
        price_policy_version == ctx.accounts.market_round.settlement_policy_version
            && market_quality_policy_version
                == ctx.accounts.market_round.market_quality_policy_version,
        ErrorCode::InvalidPolicyVersion
    );
    require!(
        ctx.accounts.market_round.round_asset_count < crate::constants::MAX_ELIGIBLE_ASSETS,
        ErrorCode::InsufficientEligibleAssets
    );

    for account in ctx.remaining_accounts {
        let data = account.try_borrow_data()?;
        let mut input_bytes: &[u8] = &data;
        let existing = RoundAsset::try_deserialize(&mut input_bytes)
            .map_err(|_| error!(ErrorCode::DuplicateRoundAsset))?;
        require!(
            existing.scoring_mint != scoring_mint
                && existing.pyth_feed_id != ctx.accounts.registry_entry.descriptor.pyth_feed_id,
            ErrorCode::DuplicateRoundAsset
        );
    }

    let (word, bit) = crate::constants::asset_bit(asset_id);
    require!(
        ctx.accounts.market_round.eligible_asset_bitmap[word] & bit == 0,
        ErrorCode::DuplicateRoundAsset
    );
    ctx.accounts.market_round.eligible_asset_bitmap[word] |= bit;
    ctx.accounts.market_round.round_asset_count += 1;

    let asset = &mut ctx.accounts.round_asset;
    asset.market_round = ctx.accounts.market_round.key();
    asset.asset_id = asset_id;
    asset.scoring_mint = scoring_mint;
    asset.token_program = ctx.accounts.registry_entry.descriptor.token_program;
    asset.representation_id = ctx.accounts.registry_entry.descriptor.representation_id;
    asset.provider_kind = ctx.accounts.registry_entry.descriptor.provider_kind;
    asset.pyth_feed_id = ctx.accounts.registry_entry.descriptor.pyth_feed_id;
    asset.issuer_kind = issuer_kind;
    asset.settlement_source_kind = SettlementSourceKind::PythProVerifiedV1;
    asset.price_source_kind = SettlementSourceKind::PythProVerifiedV1;
    asset.settlement_policy_version = price_policy_version;
    asset.price_policy_version = price_policy_version;
    asset.market_quality_policy_version = market_quality_policy_version;
    asset.start_price_q9 = 0;
    asset.start_finalized = false;
    asset.start_unavailable = false;
    asset.start_evidence_kind = EvidenceKind::None;
    asset.start_evidence_commitment = [0; 32];
    asset.end_price_q9 = 0;
    asset.end_finalized = false;
    asset.end_unavailable = false;
    asset.end_evidence_kind = EvidenceKind::None;
    asset.end_evidence_commitment = [0; 32];
    asset.return_q9 = 0;
    asset.available = false;
    asset.bump = ctx.bumps.round_asset;
    Ok(())
}

#[derive(Accounts)]
pub struct FreezePythMarketRound<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    #[account(
        seeds = [PRICE_POLICY_SEED, &market_round.settlement_policy_version.to_le_bytes()],
        bump = settlement_policy.bump
    )]
    pub settlement_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [PYTH_PRO_SOURCE_CONFIG_SEED, &market_round.pyth_source_config_version.to_le_bytes()],
        bump = pyth_source_config.bump
    )]
    pub pyth_source_config: Account<'info, PythProSourceConfig>,
    #[account(
        seeds = [QUALITY_POLICY_SEED, &market_round.market_quality_policy_version.to_le_bytes()],
        bump = market_quality_policy.bump
    )]
    pub market_quality_policy: Account<'info, MarketQualityPolicy>,
    pub coordinator: Signer<'info>,
}

pub fn handle_freeze_pyth_market_round(ctx: Context<FreezePythMarketRound>) -> Result<()> {
    crate::instructions::round::require_coordinator(
        &ctx.accounts.config,
        &ctx.accounts.coordinator,
    )?;
    require!(!ctx.accounts.config.paused, ErrorCode::ProtocolPaused);
    let round = &mut ctx.accounts.market_round;
    require!(
        round.state == MarketRoundState::Preparing,
        ErrorCode::InvalidRoundState
    );
    validate_pyth_policy_binding(
        round,
        &ctx.accounts.settlement_policy,
        &ctx.accounts.pyth_source_config,
    )?;
    require!(
        round.market_quality_policy_version == ctx.accounts.market_quality_policy.version
            && round.competition_domain == ctx.accounts.market_quality_policy.competition_domain
            && round.round_asset_count >= ctx.accounts.market_quality_policy.min_eligible_assets
            && round.round_asset_count >= crate::constants::LINEUP_SIZE as u16,
        ErrorCode::InsufficientEligibleAssets
    );
    crate::instructions::round::validate_round_assets(
        ctx.remaining_accounts,
        round.key(),
        round,
        round.round_asset_count,
        &ctx.accounts.settlement_policy,
        &ctx.accounts.market_quality_policy,
    )?;
    round.state = MarketRoundState::Scheduled;
    Ok(())
}

#[derive(Accounts)]
pub struct FinalizePythMarketRound<'info> {
    #[account(
        mut,
        seeds = [MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    #[account(
        seeds = [PRICE_POLICY_SEED, &market_round.settlement_policy_version.to_le_bytes()],
        bump = settlement_policy.bump
    )]
    pub settlement_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [PYTH_PRO_SOURCE_CONFIG_SEED, &market_round.pyth_source_config_version.to_le_bytes()],
        bump = pyth_source_config.bump
    )]
    pub pyth_source_config: Account<'info, PythProSourceConfig>,
    #[account(
        seeds = [QUALITY_POLICY_SEED, &market_round.market_quality_policy_version.to_le_bytes()],
        bump = market_quality_policy.bump
    )]
    pub market_quality_policy: Account<'info, MarketQualityPolicy>,
    pub keeper: Signer<'info>,
}

/// Finalizes a Pyth-backed Market Round only after every source-specific phase
/// is resolved. The transition is permissionless and mirrors the Jupiter
/// finalization invariant without accepting Jupiter accounts for a Pyth round.
pub fn handle_finalize_pyth_market_round(ctx: Context<FinalizePythMarketRound>) -> Result<()> {
    let round = &ctx.accounts.market_round;
    require!(
        round.state == MarketRoundState::Settling,
        ErrorCode::InvalidRoundState
    );
    require!(
        round.rated_battle_count == round.resolved_battle_count,
        ErrorCode::InvalidBattleState
    );
    validate_pyth_policy_binding(
        round,
        &ctx.accounts.settlement_policy,
        &ctx.accounts.pyth_source_config,
    )?;
    crate::instructions::round::validate_round_assets(
        ctx.remaining_accounts,
        round.key(),
        round,
        round.round_asset_count,
        &ctx.accounts.settlement_policy,
        &ctx.accounts.market_quality_policy,
    )?;

    let mut round_is_voided = false;
    for account in ctx.remaining_accounts {
        let data = account.try_borrow_data()?;
        let mut input_bytes: &[u8] = &data;
        let asset = RoundAsset::try_deserialize(&mut input_bytes)
            .map_err(|_| error!(ErrorCode::RoundAssetUnavailable))?;
        require!(
            (asset.start_finalized || asset.start_unavailable)
                && (asset.end_finalized || asset.end_unavailable),
            ErrorCode::RoundAssetUnavailable
        );
        round_is_voided |= asset.start_unavailable || asset.end_unavailable;
    }

    ctx.accounts.market_round.state = if round_is_voided {
        MarketRoundState::Voided
    } else {
        MarketRoundState::Finalized
    };
    Ok(())
}

#[derive(Accounts)]
#[instruction(phase: PricePhase)]
pub struct SubmitOrRecordPythEvidence<'info> {
    #[account(
        seeds = [ROUND_ASSET_SEED, market_round.key().as_ref(), &round_asset.asset_id.to_le_bytes()],
        bump = round_asset.bump,
        constraint = round_asset.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub round_asset: Account<'info, RoundAsset>,
    #[account(
        seeds = [MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    #[account(
        seeds = [PRICE_POLICY_SEED, &market_round.settlement_policy_version.to_le_bytes()],
        bump = settlement_policy.bump
    )]
    pub settlement_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [PYTH_PRO_SOURCE_CONFIG_SEED, &market_round.pyth_source_config_version.to_le_bytes()],
        bump = pyth_source_config.bump
    )]
    pub pyth_source_config: Account<'info, PythProSourceConfig>,
    #[account(
        init,
        payer = relayer,
        space = 8 + PythPriceEvidence::INIT_SPACE,
        seeds = [PYTH_PRICE_EVIDENCE_SEED, round_asset.key().as_ref(), &[phase as u8]],
        bump
    )]
    pub pyth_price_evidence: Account<'info, PythPriceEvidence>,
    #[account(mut)]
    pub relayer: Signer<'info>,
    /// CHECK: The program only reads the Instructions sysvar to bind the
    /// evidence to the preceding pinned verifier instruction.
    #[account(address = solana_instructions_sysvar::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[allow(clippy::too_many_arguments)]
pub fn handle_submit_or_record_pyth_evidence(
    ctx: Context<SubmitOrRecordPythEvidence>,
    phase: PricePhase,
    feed_id: u32,
    payload_timestamp_us: u64,
    feed_update_timestamp_us: u64,
    price_mantissa: i64,
    confidence_mantissa: u64,
    exponent: i16,
    normalized_price_q9: i64,
    payload_hash: [u8; 32],
) -> Result<()> {
    validate_pyth_policy_binding(
        &ctx.accounts.market_round,
        &ctx.accounts.settlement_policy,
        &ctx.accounts.pyth_source_config,
    )?;
    require!(
        ctx.accounts.round_asset.pyth_feed_id == feed_id && feed_id != 0,
        ErrorCode::InvalidPythEvidence
    );
    let phase_resolved = match phase {
        PricePhase::Start => ctx.accounts.round_asset.start_finalized,
        PricePhase::End => ctx.accounts.round_asset.end_finalized,
    };
    require!(!phase_resolved, ErrorCode::PricePhaseResolved);
    require!(payload_hash != [0; 32], ErrorCode::InvalidPythEvidence);
    verify_pinned_verifier_instruction(
        &ctx.accounts.instructions_sysvar.to_account_info(),
        ctx.accounts.pyth_source_config.verifier_program,
        payload_hash,
    )?;

    let target_seconds = match phase {
        PricePhase::Start => ctx.accounts.market_round.start_target_at,
        PricePhase::End => ctx.accounts.market_round.end_target_at,
    };
    validate_pyth_semantics(
        target_seconds,
        ctx.accounts
            .pyth_source_config
            .max_payload_timestamp_delta_us,
        ctx.accounts.pyth_source_config.max_feed_age_us,
        ctx.accounts.pyth_source_config.max_confidence_bps,
        ctx.accounts.round_asset.pyth_feed_id,
        feed_id,
        payload_timestamp_us,
        feed_update_timestamp_us,
        price_mantissa,
        confidence_mantissa,
        exponent,
        normalized_price_q9,
    )?;

    let evidence = &mut ctx.accounts.pyth_price_evidence;
    evidence.round_asset = ctx.accounts.round_asset.key();
    evidence.phase = phase;
    evidence.feed_id = feed_id;
    evidence.payload_timestamp_us = payload_timestamp_us;
    evidence.feed_update_timestamp_us = feed_update_timestamp_us;
    evidence.price_mantissa = price_mantissa;
    evidence.confidence_mantissa = confidence_mantissa;
    evidence.exponent = exponent;
    evidence.normalized_price_q9 = normalized_price_q9;
    evidence.payload_hash = payload_hash;
    evidence.bump = ctx.bumps.pyth_price_evidence;
    Ok(())
}

#[derive(Accounts)]
#[instruction(phase: PricePhase)]
pub struct FinalizePythPricePhase<'info> {
    #[account(
        mut,
        seeds = [ROUND_ASSET_SEED, market_round.key().as_ref(), &round_asset.asset_id.to_le_bytes()],
        bump = round_asset.bump,
        constraint = round_asset.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub round_asset: Account<'info, RoundAsset>,
    #[account(
        seeds = [MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    #[account(
        seeds = [PRICE_POLICY_SEED, &market_round.settlement_policy_version.to_le_bytes()],
        bump = settlement_policy.bump
    )]
    pub settlement_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [PYTH_PRO_SOURCE_CONFIG_SEED, &market_round.pyth_source_config_version.to_le_bytes()],
        bump = pyth_source_config.bump
    )]
    pub pyth_source_config: Account<'info, PythProSourceConfig>,
    #[account(
        seeds = [PYTH_PRICE_EVIDENCE_SEED, round_asset.key().as_ref(), &[phase as u8]],
        bump = pyth_price_evidence.bump
    )]
    pub pyth_price_evidence: Account<'info, PythPriceEvidence>,
    pub finalizer: Signer<'info>,
}

pub fn handle_finalize_pyth_price_phase(
    ctx: Context<FinalizePythPricePhase>,
    phase: PricePhase,
) -> Result<()> {
    validate_pyth_policy_binding(
        &ctx.accounts.market_round,
        &ctx.accounts.settlement_policy,
        &ctx.accounts.pyth_source_config,
    )?;
    require!(
        ctx.accounts.pyth_price_evidence.round_asset == ctx.accounts.round_asset.key()
            && ctx.accounts.pyth_price_evidence.phase == phase
            && ctx.accounts.pyth_price_evidence.feed_id == ctx.accounts.round_asset.pyth_feed_id,
        ErrorCode::InvalidPythEvidence
    );
    let round_asset = &mut ctx.accounts.round_asset;
    match phase {
        PricePhase::Start => {
            require!(!round_asset.start_finalized, ErrorCode::PricePhaseResolved);
            round_asset.start_price_q9 = ctx.accounts.pyth_price_evidence.normalized_price_q9;
            round_asset.start_finalized = true;
            round_asset.start_unavailable = false;
            round_asset.start_evidence_kind = EvidenceKind::PythProVerifiedV1;
            round_asset.start_evidence_commitment =
                pyth_evidence_commitment(&ctx.accounts.pyth_price_evidence);
        }
        PricePhase::End => {
            require!(
                round_asset.start_finalized && !round_asset.end_finalized,
                ErrorCode::InvalidBattleState
            );
            round_asset.end_price_q9 = ctx.accounts.pyth_price_evidence.normalized_price_q9;
            round_asset.end_finalized = true;
            round_asset.end_unavailable = false;
            round_asset.end_evidence_kind = EvidenceKind::PythProVerifiedV1;
            round_asset.end_evidence_commitment =
                pyth_evidence_commitment(&ctx.accounts.pyth_price_evidence);
            round_asset.return_q9 =
                return_q9(round_asset.start_price_q9, round_asset.end_price_q9)?;
        }
    }
    if round_asset.start_finalized && round_asset.end_finalized {
        round_asset.available = true;
    }
    Ok(())
}

#[derive(Accounts)]
pub struct MarkPythPricePhaseUnavailable<'info> {
    #[account(
        mut,
        seeds = [ROUND_ASSET_SEED, market_round.key().as_ref(), &round_asset.asset_id.to_le_bytes()],
        bump = round_asset.bump,
        constraint = round_asset.market_round == market_round.key() @ ErrorCode::WrongMarketRound
    )]
    pub round_asset: Account<'info, RoundAsset>,
    #[account(
        seeds = [MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    #[account(
        seeds = [PRICE_POLICY_SEED, &market_round.settlement_policy_version.to_le_bytes()],
        bump = settlement_policy.bump
    )]
    pub settlement_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [PYTH_PRO_SOURCE_CONFIG_SEED, &market_round.pyth_source_config_version.to_le_bytes()],
        bump = pyth_source_config.bump
    )]
    pub pyth_source_config: Account<'info, PythProSourceConfig>,
    pub marker: Signer<'info>,
}

/// Marks a Pyth phase unavailable after its deterministic target/tolerance
/// boundary. This is the only failure path: no Jupiter fallback can mutate a
/// frozen Pyth round after the market outcome may be known.
pub fn handle_mark_pyth_price_phase_unavailable(
    ctx: Context<MarkPythPricePhaseUnavailable>,
    phase: PricePhase,
) -> Result<()> {
    validate_pyth_policy_binding(
        &ctx.accounts.market_round,
        &ctx.accounts.settlement_policy,
        &ctx.accounts.pyth_source_config,
    )?;
    let target_seconds = match phase {
        PricePhase::Start => ctx.accounts.market_round.start_target_at,
        PricePhase::End => ctx.accounts.market_round.end_target_at,
    };
    let tolerance_seconds = i64::try_from(
        ctx.accounts
            .pyth_source_config
            .max_payload_timestamp_delta_us
            .div_ceil(MICROS_PER_SECOND),
    )
    .map_err(|_| error!(ErrorCode::MathOverflow))?;
    let deadline = target_seconds
        .checked_add(tolerance_seconds)
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    require!(
        Clock::get()?.unix_timestamp > deadline,
        ErrorCode::AttestationWindowClosed
    );
    let round_asset = &mut ctx.accounts.round_asset;
    match phase {
        PricePhase::Start => {
            require!(!round_asset.start_finalized, ErrorCode::PricePhaseResolved);
            round_asset.start_unavailable = true;
        }
        PricePhase::End => {
            require!(!round_asset.end_finalized, ErrorCode::PricePhaseResolved);
            round_asset.end_unavailable = true;
        }
    }
    Ok(())
}

fn validate_pyth_policy_binding(
    round: &MarketRound,
    policy: &PricePolicy,
    source_config: &PythProSourceConfig,
) -> Result<()> {
    require!(
        round.settlement_source_kind == SettlementSourceKind::PythProVerifiedV1
            && policy.source_kind == SettlementSourceKind::PythProVerifiedV1,
        ErrorCode::UnsupportedSettlementSource
    );
    require!(
        round.settlement_policy_version == policy.version
            && round.price_policy_version == policy.version
            && round.settlement_source_config == policy.source_config
            && policy.source_config
                == Pubkey::find_program_address(
                    &[
                        PYTH_PRO_SOURCE_CONFIG_SEED,
                        &source_config.version.to_le_bytes()
                    ],
                    &crate::id(),
                )
                .0
            && round.pyth_source_config_version == source_config.version,
        ErrorCode::SourceConfigMismatch
    );
    Ok(())
}

fn verify_pinned_verifier_instruction(
    instructions_sysvar: &AccountInfo<'_>,
    verifier_program: Pubkey,
    payload_hash: [u8; 32],
) -> Result<()> {
    let current_index = load_current_index_checked(instructions_sysvar)
        .map_err(|_| error!(ErrorCode::InvalidPythVerifierInstruction))?;
    require!(current_index > 0, ErrorCode::InvalidPythVerifierInstruction);
    let instruction =
        load_instruction_at_checked(usize::from(current_index - 1), instructions_sysvar)
            .map_err(|_| error!(ErrorCode::InvalidPythVerifierInstruction))?;
    require_keys_eq!(
        instruction.program_id,
        verifier_program,
        ErrorCode::InvalidPythVerifierInstruction
    );
    require!(
        solana_sha256_hasher::hash(&instruction.data).to_bytes() == payload_hash,
        ErrorCode::InvalidPythVerifierInstruction
    );
    Ok(())
}

fn pyth_evidence_commitment(evidence: &PythPriceEvidence) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(160);
    bytes.extend_from_slice(PYTH_PAYLOAD_DOMAIN);
    bytes.extend_from_slice(evidence.round_asset.as_ref());
    bytes.push(evidence.phase as u8);
    bytes.extend_from_slice(&evidence.feed_id.to_le_bytes());
    bytes.extend_from_slice(&evidence.payload_timestamp_us.to_le_bytes());
    bytes.extend_from_slice(&evidence.feed_update_timestamp_us.to_le_bytes());
    bytes.extend_from_slice(&evidence.price_mantissa.to_le_bytes());
    bytes.extend_from_slice(&evidence.confidence_mantissa.to_le_bytes());
    bytes.extend_from_slice(&evidence.exponent.to_le_bytes());
    bytes.extend_from_slice(&evidence.normalized_price_q9.to_le_bytes());
    bytes.extend_from_slice(&evidence.payload_hash);
    solana_sha256_hasher::hash(&bytes).to_bytes()
}

fn confidence_bps(price_mantissa: i64, confidence_mantissa: u64) -> Option<u64> {
    if price_mantissa <= 0 {
        return None;
    }
    // Round upward so a fractional ratio above the configured limit cannot
    // pass because integer division truncated the excess away.
    let denominator = i128::from(price_mantissa);
    let numerator = i128::from(confidence_mantissa).checked_mul(10_000)?;
    u64::try_from(
        numerator
            .checked_add(denominator.checked_sub(1)?)?
            .checked_div(denominator)?,
    )
    .ok()
}

/// Validates all application-level Pyth facts after the pinned verifier has
/// accepted the signed payload bytes. Keeping this separate from account
/// plumbing makes the fail-closed policy independently testable and prevents
/// a client-provided normalized price from becoming settlement truth.
#[allow(clippy::too_many_arguments)]
fn validate_pyth_semantics(
    target_seconds: i64,
    max_payload_timestamp_delta_us: u64,
    max_feed_age_us: u64,
    max_confidence_bps: u16,
    expected_feed_id: u32,
    feed_id: u32,
    payload_timestamp_us: u64,
    feed_update_timestamp_us: u64,
    price_mantissa: i64,
    confidence_mantissa: u64,
    exponent: i16,
    normalized_price_q9: i64,
) -> Result<()> {
    require!(target_seconds >= 0, ErrorCode::InvalidPythEvidence);
    let target_timestamp_us = u64::try_from(target_seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(MICROS_PER_SECOND))
        .ok_or_else(|| error!(ErrorCode::InvalidPythEvidence))?;
    require!(
        feed_id != 0
            && feed_id == expected_feed_id
            && payload_timestamp_us.abs_diff(target_timestamp_us) <= max_payload_timestamp_delta_us,
        ErrorCode::InvalidPythEvidence
    );
    require!(
        feed_update_timestamp_us <= payload_timestamp_us
            && payload_timestamp_us - feed_update_timestamp_us <= max_feed_age_us,
        ErrorCode::InvalidPythEvidence
    );
    require!(price_mantissa > 0, ErrorCode::InvalidPythEvidence);
    let confidence_bps = confidence_bps(price_mantissa, confidence_mantissa)
        .ok_or_else(|| error!(ErrorCode::InvalidPythEvidence))?;
    require!(
        confidence_bps <= u64::from(max_confidence_bps),
        ErrorCode::InvalidPythEvidence
    );
    require!(
        pyth_mantissa_to_q9(price_mantissa, exponent)? == normalized_price_q9,
        ErrorCode::InvalidPythEvidence
    );
    Ok(())
}

fn pyth_mantissa_to_q9(mantissa: i64, exponent: i16) -> Result<i64> {
    require!(mantissa > 0, ErrorCode::InvalidPythEvidence);
    let scale = i32::from(exponent) + 9;
    require!((-38..=38).contains(&scale), ErrorCode::InvalidPythEvidence);
    let value = if scale >= 0 {
        i128::from(mantissa)
            .checked_mul(power_of_ten(scale as u32))
            .ok_or_else(|| error!(ErrorCode::MathOverflow))?
    } else {
        i128::from(mantissa)
            .checked_div(power_of_ten(scale.unsigned_abs()))
            .ok_or_else(|| error!(ErrorCode::MathOverflow))?
    };
    require!(value > 0, ErrorCode::InvalidPythEvidence);
    i64::try_from(value).map_err(|_| error!(ErrorCode::MathOverflow))
}

fn power_of_ten(exponent: u32) -> i128 {
    (0..exponent).fold(1i128, |value, _| value.saturating_mul(10))
}

fn require_admin(config: &Config, signer: &Signer) -> Result<()> {
    require_keys_eq!(
        config.admin_authority,
        signer.key(),
        ErrorCode::UnauthorizedAdmin
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pyth_q9_conversion_rejects_non_positive_and_overflowing_values() {
        assert!(pyth_mantissa_to_q9(0, -8).is_err());
        assert!(pyth_mantissa_to_q9(-1, -8).is_err());
        assert!(pyth_mantissa_to_q9(i64::MAX, 9).is_err());
    }

    #[test]
    fn pyth_confidence_bound_uses_integer_basis_points() {
        assert_eq!(confidence_bps(1_000, 1), Some(10));
        assert_eq!(confidence_bps(1_000, 0), Some(0));
        assert_eq!(confidence_bps(0, 1), None);
    }

    #[test]
    fn pyth_confidence_bound_rounds_up_instead_of_accepting_a_fractional_overage() {
        // A 1/3 confidence ratio is 3333.33 basis points. Rounding down to
        // 3333 would incorrectly pass a policy capped at exactly 3333 bps.
        assert_eq!(confidence_bps(3, 1), Some(3_334));
    }

    #[test]
    fn pyth_semantics_fail_closed_for_target_feed_age_confidence_and_q9() {
        let valid = || {
            validate_pyth_semantics(
                10, 100, 500, 100, 42, 42, 10_000_050, 10_000_050, 123, 1, -9, 123,
            )
        };
        assert!(valid().is_ok());

        assert!(validate_pyth_semantics(
            10, 100, 500, 100, 42, 7, 10_000_050, 10_000_050, 123, 1, -9, 123
        )
        .is_err());
        assert!(validate_pyth_semantics(
            10, 100, 500, 100, 42, 42, 10_000_050, 9_999_000, 123, 1, -9, 123
        )
        .is_err());
        assert!(validate_pyth_semantics(
            10, 100, 500, 100, 42, 42, 10_000_050, 10_000_050, 100, 2, -9, 100
        )
        .is_err());
        assert!(validate_pyth_semantics(
            10, 100, 500, 100, 42, 42, 10_000_050, 10_000_050, 123, 1, -9, 124
        )
        .is_err());
    }
}
