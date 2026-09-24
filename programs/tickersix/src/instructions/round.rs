use anchor_lang::prelude::*;

use crate::{
    constants::{
        ASSET_SEED, CONFIG_SEED, JUPITER_SOURCE_CONFIG_SEED, LINEUP_SIZE, MARKET_ROUND_SEED,
        MAX_ELIGIBLE_ASSETS, PRICE_POLICY_SEED, QUALITY_POLICY_SEED, ROUND_ASSET_SEED,
    },
    error::ErrorCode,
    state::{
        AssetRegistryEntry, AttestorSet, Config, JupiterSourceConfig, MarketQualityPolicy,
        MarketRound, MarketRoundState, PricePolicy, RoundAsset, SettlementSourceKind,
    },
};

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct CreateMarketRoundDraft<'info> {
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
        bump = price_policy.bump
    )]
    pub price_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [JUPITER_SOURCE_CONFIG_SEED, &config.current_jupiter_source_config_version.to_le_bytes()],
        bump = jupiter_source_config.bump
    )]
    pub jupiter_source_config: Account<'info, JupiterSourceConfig>,
    #[account(
        seeds = [QUALITY_POLICY_SEED, &config.current_market_quality_policy_version.to_le_bytes()],
        bump = market_quality_policy.bump
    )]
    pub market_quality_policy: Account<'info, MarketQualityPolicy>,
    #[account(
        seeds = [crate::constants::ATTESTOR_SET_SEED, &config.current_attestor_set_version.to_le_bytes()],
        bump = attestor_set.bump
    )]
    pub attestor_set: Account<'info, AttestorSet>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_market_round_draft(
    ctx: Context<CreateMarketRoundDraft>,
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
    require_coordinator(&ctx.accounts.config, &ctx.accounts.coordinator)?;
    require!(!ctx.accounts.config.paused, ErrorCode::ProtocolPaused);
    validate_eligibility_snapshot_hash(eligibility_snapshot_hash)?;
    require!(
        eligibility_frozen_at < queue_close_at
            && queue_close_at < commit_deadline
            && commit_deadline < reveal_deadline
            && reveal_deadline < start_target_at
            && start_target_at < end_target_at,
        ErrorCode::InvalidRoundTiming
    );
    require!(
        round_id > ctx.accounts.config.last_market_round_id,
        ErrorCode::InvalidRoundSequence
    );
    require!(
        start_target_at >= ctx.accounts.config.last_market_round_end_at,
        ErrorCode::InvalidRoundSequence
    );
    require!(
        registry_version > 0 && registry_version == ctx.accounts.config.current_registry_version,
        ErrorCode::InvalidRegistryVersion
    );
    require!(
        ctx.accounts.config.registry_frozen,
        ErrorCode::RegistryNotFrozen
    );
    require!(
        ctx.accounts.price_policy.version == ctx.accounts.config.current_settlement_policy_version
            && ctx.accounts.market_quality_policy.version
                == ctx.accounts.config.current_market_quality_policy_version
            && ctx.accounts.attestor_set.version
                == ctx.accounts.config.current_attestor_set_version
            && ctx.accounts.jupiter_source_config.version
                == ctx.accounts.config.current_jupiter_source_config_version,
        ErrorCode::InvalidPolicyVersion
    );
    require!(
        ctx.accounts.price_policy.source_kind == SettlementSourceKind::JupiterTokenSpotV1
            && ctx.accounts.price_policy.source_config == ctx.accounts.jupiter_source_config.key()
            && ctx.accounts.jupiter_source_config.attestor_set_version
                == ctx.accounts.attestor_set.version,
        ErrorCode::SourceConfigMismatch
    );

    let round = &mut ctx.accounts.market_round;
    round.round_id = round_id;
    round.registry_version = registry_version;
    round.competition_domain = ctx.accounts.market_quality_policy.competition_domain;
    round.settlement_policy_version = ctx.accounts.price_policy.version;
    round.settlement_source_kind = ctx.accounts.price_policy.source_kind;
    round.settlement_source_config = ctx.accounts.price_policy.source_config;
    round.jupiter_source_config_version = ctx.accounts.jupiter_source_config.version;
    round.pyth_source_config_version = 0;
    round.price_policy_version = ctx.accounts.price_policy.version;
    round.market_quality_policy_version = ctx.accounts.market_quality_policy.version;
    round.attestor_set_version = ctx.accounts.attestor_set.version;
    round.market_quality_policy_hash = ctx.accounts.market_quality_policy.canonical_policy_hash;
    round.eligibility_snapshot_hash = eligibility_snapshot_hash;
    round.eligibility_frozen_at = eligibility_frozen_at;
    round.queue_close_at = queue_close_at;
    round.commit_deadline = commit_deadline;
    round.reveal_deadline = reveal_deadline;
    round.start_target_at = start_target_at;
    round.end_target_at = end_target_at;
    round.observation_window_secs = ctx.accounts.price_policy.observation_window_secs;
    round.attestation_grace_secs = ctx.accounts.jupiter_source_config.attestation_grace_secs;
    round.max_attestor_spread_bps = ctx.accounts.jupiter_source_config.max_attestor_spread_bps;
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
pub struct AddRoundAsset<'info> {
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
        seeds = [ASSET_SEED, &market_round.registry_version.to_le_bytes(), &asset_id.to_le_bytes()],
        bump = registry_entry.bump,
        constraint = registry_entry.registry_version == market_round.registry_version @ ErrorCode::InvalidRegistryVersion,
        constraint = registry_entry.asset_id == asset_id @ ErrorCode::InvalidAssetId
    )]
    pub registry_entry: Account<'info, AssetRegistryEntry>,
    #[account(
        seeds = [PRICE_POLICY_SEED, &market_round.settlement_policy_version.to_le_bytes()],
        bump = price_policy.bump
    )]
    pub price_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [JUPITER_SOURCE_CONFIG_SEED, &market_round.jupiter_source_config_version.to_le_bytes()],
        bump = jupiter_source_config.bump
    )]
    pub jupiter_source_config: Account<'info, JupiterSourceConfig>,
    #[account(
        seeds = [QUALITY_POLICY_SEED, &market_round.market_quality_policy_version.to_le_bytes()],
        bump = market_quality_policy.bump
    )]
    pub market_quality_policy: Account<'info, MarketQualityPolicy>,
    pub system_program: Program<'info, System>,
}

pub fn handle_add_round_asset(
    ctx: Context<AddRoundAsset>,
    asset_id: u16,
    scoring_mint: Pubkey,
    issuer_kind: u8,
    price_source_kind: crate::state::PriceSourceKind,
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
    require!(asset_id < MAX_ELIGIBLE_ASSETS, ErrorCode::InvalidAssetId);
    require!(
        ctx.accounts.registry_entry.active,
        ErrorCode::RegistryEntryMismatch
    );
    require!(
        scoring_mint == ctx.accounts.registry_entry.scoring_mint
            && issuer_kind == ctx.accounts.registry_entry.issuer_kind,
        ErrorCode::RegistryEntryMismatch
    );
    require!(
        price_policy_version == ctx.accounts.market_round.price_policy_version
            && market_quality_policy_version
                == ctx.accounts.market_round.market_quality_policy_version,
        ErrorCode::InvalidPolicyVersion
    );
    require!(
        price_source_kind == ctx.accounts.price_policy.source_kind,
        ErrorCode::PricePolicyMismatch
    );
    require!(
        ctx.accounts.market_round.settlement_source_kind == price_source_kind
            && ctx.accounts.market_round.settlement_policy_version
                == ctx.accounts.price_policy.version
            && ctx.accounts.market_round.settlement_source_config
                == ctx.accounts.jupiter_source_config.key()
            && ctx.accounts.price_policy.source_config == ctx.accounts.jupiter_source_config.key(),
        ErrorCode::SourceConfigMismatch
    );
    require!(
        ctx.accounts.registry_entry.descriptor.enabled
            && ctx.accounts.registry_entry.descriptor.lifecycle_state
                == crate::state::LifecycleState::Active
            && ctx.accounts.registry_entry.descriptor.comparability_kind
                != crate::state::ComparabilityKind::Unsupported
            && ctx.accounts.registry_entry.descriptor.registry_version
                == ctx.accounts.market_round.registry_version
            && ctx.accounts.registry_entry.descriptor.asset_id == asset_id,
        ErrorCode::RegistryEntryMismatch
    );
    require!(
        ctx.accounts.market_round.round_asset_count < MAX_ELIGIBLE_ASSETS,
        ErrorCode::InsufficientEligibleAssets
    );

    validate_round_assets(
        ctx.remaining_accounts,
        ctx.accounts.market_round.key(),
        &ctx.accounts.market_round,
        ctx.accounts.market_round.round_asset_count,
        &ctx.accounts.price_policy,
        &ctx.accounts.market_quality_policy,
    )?;
    for account in ctx.remaining_accounts {
        let data = account.try_borrow_data()?;
        let mut input_bytes: &[u8] = &data;
        let existing = RoundAsset::try_deserialize(&mut input_bytes)
            .map_err(|_| error!(ErrorCode::DuplicateRoundAsset))?;
        require!(
            existing.scoring_mint != scoring_mint,
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

    let round_asset = &mut ctx.accounts.round_asset;
    round_asset.market_round = ctx.accounts.market_round.key();
    round_asset.asset_id = asset_id;
    round_asset.scoring_mint = scoring_mint;
    round_asset.token_program = ctx.accounts.registry_entry.descriptor.token_program;
    round_asset.representation_id = ctx.accounts.registry_entry.descriptor.representation_id;
    round_asset.provider_kind = ctx.accounts.registry_entry.descriptor.provider_kind;
    round_asset.pyth_feed_id = ctx.accounts.registry_entry.descriptor.pyth_feed_id;
    round_asset.issuer_kind = issuer_kind;
    round_asset.settlement_source_kind = price_source_kind;
    round_asset.price_source_kind = price_source_kind;
    round_asset.settlement_policy_version = price_policy_version;
    round_asset.price_policy_version = price_policy_version;
    round_asset.market_quality_policy_version = market_quality_policy_version;
    round_asset.start_price_q9 = 0;
    round_asset.start_finalized = false;
    round_asset.start_unavailable = false;
    round_asset.start_evidence_kind = crate::state::EvidenceKind::None;
    round_asset.start_evidence_commitment = [0; 32];
    round_asset.end_price_q9 = 0;
    round_asset.end_finalized = false;
    round_asset.end_unavailable = false;
    round_asset.end_evidence_kind = crate::state::EvidenceKind::None;
    round_asset.end_evidence_commitment = [0; 32];
    round_asset.return_q9 = 0;
    round_asset.available = false;
    round_asset.bump = ctx.bumps.round_asset;
    Ok(())
}

#[derive(Accounts)]
pub struct FreezeMarketRound<'info> {
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
        bump = price_policy.bump
    )]
    pub price_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [JUPITER_SOURCE_CONFIG_SEED, &market_round.jupiter_source_config_version.to_le_bytes()],
        bump = jupiter_source_config.bump
    )]
    pub jupiter_source_config: Account<'info, JupiterSourceConfig>,
    #[account(
        seeds = [QUALITY_POLICY_SEED, &market_round.market_quality_policy_version.to_le_bytes()],
        bump = market_quality_policy.bump
    )]
    pub market_quality_policy: Account<'info, MarketQualityPolicy>,
    #[account(
        seeds = [crate::constants::ATTESTOR_SET_SEED, &market_round.attestor_set_version.to_le_bytes()],
        bump = attestor_set.bump
    )]
    pub attestor_set: Account<'info, AttestorSet>,
    pub coordinator: Signer<'info>,
}

pub fn handle_freeze_market_round(ctx: Context<FreezeMarketRound>) -> Result<()> {
    require_coordinator(&ctx.accounts.config, &ctx.accounts.coordinator)?;
    require!(!ctx.accounts.config.paused, ErrorCode::ProtocolPaused);
    let round = &mut ctx.accounts.market_round;
    require!(
        round.state == MarketRoundState::Preparing,
        ErrorCode::InvalidRoundState
    );
    require!(
        round.price_policy_version == ctx.accounts.price_policy.version,
        ErrorCode::PricePolicyMismatch
    );
    validate_jupiter_source_binding(
        round,
        &ctx.accounts.price_policy,
        &ctx.accounts.jupiter_source_config,
        &ctx.accounts.attestor_set,
        ctx.accounts.jupiter_source_config.key(),
    )?;
    require!(
        round.market_quality_policy_version == ctx.accounts.market_quality_policy.version,
        ErrorCode::QualityPolicyMismatch
    );
    require!(
        round.competition_domain == ctx.accounts.market_quality_policy.competition_domain,
        ErrorCode::QualityPolicyMismatch
    );
    require!(
        round.attestor_set_version == ctx.accounts.attestor_set.version,
        ErrorCode::AttestorSetMismatch
    );
    require!(
        round.round_asset_count >= ctx.accounts.market_quality_policy.min_eligible_assets,
        ErrorCode::InsufficientEligibleAssets
    );
    require!(
        round.round_asset_count >= LINEUP_SIZE as u16,
        ErrorCode::InsufficientEligibleAssets
    );
    validate_round_assets(
        ctx.remaining_accounts,
        round.key(),
        round,
        round.round_asset_count,
        &ctx.accounts.price_policy,
        &ctx.accounts.market_quality_policy,
    )?;
    round.state = MarketRoundState::Scheduled;
    Ok(())
}

#[derive(Accounts)]
pub struct AdvanceMarketRound<'info> {
    #[account(
        mut,
        seeds = [MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    pub keeper: Signer<'info>,
}

/// Advances one clock-driven Market Round transition.
///
/// Transitions are deliberately single-step and permissionless. A keeper can
/// retry the instruction safely, while each boundary remains explicit and
/// cannot be skipped by a malformed timestamp or an out-of-order call.
pub fn handle_advance_market_round(ctx: Context<AdvanceMarketRound>) -> Result<()> {
    let next = next_market_round_state(&ctx.accounts.market_round, Clock::get()?.unix_timestamp)?;
    ctx.accounts.market_round.state = next;
    Ok(())
}

#[derive(Accounts)]
pub struct FinalizeMarketRound<'info> {
    #[account(
        mut,
        seeds = [MARKET_ROUND_SEED, &market_round.round_id.to_le_bytes()],
        bump = market_round.bump
    )]
    pub market_round: Account<'info, MarketRound>,
    #[account(
        seeds = [PRICE_POLICY_SEED, &market_round.settlement_policy_version.to_le_bytes()],
        bump = price_policy.bump
    )]
    pub price_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [JUPITER_SOURCE_CONFIG_SEED, &market_round.jupiter_source_config_version.to_le_bytes()],
        bump = jupiter_source_config.bump
    )]
    pub jupiter_source_config: Account<'info, JupiterSourceConfig>,
    #[account(
        seeds = [QUALITY_POLICY_SEED, &market_round.market_quality_policy_version.to_le_bytes()],
        bump = market_quality_policy.bump
    )]
    pub market_quality_policy: Account<'info, MarketQualityPolicy>,
    pub keeper: Signer<'info>,
}

/// Completes a round only after every frozen asset phase and every admitted
/// Battle has reached a terminal state. The transition is permissionless and
/// deterministic: one unavailable asset makes the whole round `Voided`.
pub fn handle_finalize_market_round(ctx: Context<FinalizeMarketRound>) -> Result<()> {
    let round = &ctx.accounts.market_round;
    require!(
        round.state == MarketRoundState::Settling,
        ErrorCode::InvalidRoundState
    );
    require!(
        round.rated_battle_count == round.resolved_battle_count,
        ErrorCode::InvalidBattleState
    );
    validate_jupiter_policy_binding(
        round,
        &ctx.accounts.price_policy,
        &ctx.accounts.jupiter_source_config,
        ctx.accounts.jupiter_source_config.key(),
    )?;

    validate_round_assets(
        ctx.remaining_accounts,
        round.key(),
        round,
        round.round_asset_count,
        &ctx.accounts.price_policy,
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

    let round = &mut ctx.accounts.market_round;
    round.state = if round_is_voided {
        MarketRoundState::Voided
    } else {
        MarketRoundState::Finalized
    };
    Ok(())
}

/// Verifies that the caller supplied the complete, canonical set of RoundAsset
/// accounts for the draft. This prevents a coordinator from omitting an older
/// asset while adding a duplicate mint or freezing a partial account set.
pub(crate) fn validate_round_assets(
    accounts: &[AccountInfo<'_>],
    round_key: Pubkey,
    round: &MarketRound,
    expected_count: u16,
    price_policy: &PricePolicy,
    market_quality_policy: &MarketQualityPolicy,
) -> Result<()> {
    require!(
        accounts.len() == usize::from(expected_count),
        ErrorCode::InvalidRoundState
    );

    let mut seen_asset_ids = [0u64; 4];
    let mut seen_mints = Vec::with_capacity(accounts.len());
    let mut seen_representations = Vec::with_capacity(accounts.len());
    for account in accounts {
        require_keys_eq!(*account.owner, crate::id(), ErrorCode::WrongMarketRound);
        let data = account.try_borrow_data()?;
        let mut input_bytes: &[u8] = &data;
        let asset = RoundAsset::try_deserialize(&mut input_bytes)
            .map_err(|_| error!(ErrorCode::WrongMarketRound))?;
        require_keys_eq!(asset.market_round, round_key, ErrorCode::WrongMarketRound);
        require!(
            asset.asset_id < MAX_ELIGIBLE_ASSETS,
            ErrorCode::InvalidAssetId
        );

        let (expected_key, _) = Pubkey::find_program_address(
            &[
                ROUND_ASSET_SEED,
                round_key.as_ref(),
                &asset.asset_id.to_le_bytes(),
            ],
            &crate::id(),
        );
        require_keys_eq!(*account.key, expected_key, ErrorCode::WrongMarketRound);

        let (word, bit) = crate::constants::asset_bit(asset.asset_id);
        require!(
            round.eligible_asset_bitmap[word] & bit != 0,
            ErrorCode::InvalidRoundState
        );
        require!(
            asset.price_policy_version == price_policy.version
                && asset.price_source_kind == price_policy.source_kind
                && asset.settlement_policy_version == round.settlement_policy_version
                && asset.settlement_source_kind == round.settlement_source_kind
                && asset.market_quality_policy_version == market_quality_policy.version,
            ErrorCode::InvalidPolicyVersion
        );
        require!(
            asset.token_program != Pubkey::default(),
            ErrorCode::RegistryEntryMismatch
        );
        if asset.settlement_source_kind == SettlementSourceKind::PythProVerifiedV1 {
            require!(asset.pyth_feed_id != 0, ErrorCode::RegistryEntryMismatch);
        }
        require!(
            seen_asset_ids[word] & bit == 0,
            ErrorCode::DuplicateRoundAsset
        );
        require!(
            !seen_mints.contains(&asset.scoring_mint),
            ErrorCode::DuplicateRoundAsset
        );
        require!(
            !seen_representations.contains(&asset.representation_id),
            ErrorCode::DuplicateRoundAsset
        );
        seen_asset_ids[word] |= bit;
        seen_mints.push(asset.scoring_mint);
        seen_representations.push(asset.representation_id);
    }
    require!(
        seen_asset_ids == round.eligible_asset_bitmap,
        ErrorCode::InvalidRoundState
    );
    Ok(())
}

fn validate_eligibility_snapshot_hash(hash: [u8; 32]) -> Result<()> {
    require!(hash != [0; 32], ErrorCode::InvalidEligibilitySnapshot);
    Ok(())
}

fn next_market_round_state(round: &MarketRound, now: i64) -> Result<MarketRoundState> {
    match round.state {
        MarketRoundState::Scheduled => {
            require!(now >= round.queue_close_at, ErrorCode::InvalidRoundState);
            Ok(MarketRoundState::CommitOpen)
        }
        MarketRoundState::CommitOpen => {
            require!(now >= round.commit_deadline, ErrorCode::InvalidRoundState);
            Ok(MarketRoundState::RevealOpen)
        }
        MarketRoundState::RevealOpen => {
            require!(now >= round.start_target_at, ErrorCode::InvalidRoundState);
            Ok(MarketRoundState::Live)
        }
        MarketRoundState::Live => {
            require!(now >= round.end_target_at, ErrorCode::InvalidRoundState);
            Ok(MarketRoundState::Ended)
        }
        MarketRoundState::Ended => Ok(MarketRoundState::Settling),
        _ => err!(ErrorCode::InvalidRoundState),
    }
}

pub(crate) fn require_coordinator(config: &Config, signer: &Signer) -> Result<()> {
    require_keys_eq!(
        config.coordinator_authority,
        signer.key(),
        ErrorCode::UnauthorizedCoordinator
    );
    Ok(())
}

/// Binds the frozen round to exactly one immutable Jupiter source config and
/// the attestor set referenced by that config. This is the source-abstraction
/// gate: later callers may only supply the PDAs whose versions were copied at
/// draft creation and rechecked at freeze/finalization.
pub(crate) fn validate_jupiter_source_binding(
    round: &MarketRound,
    policy: &PricePolicy,
    jupiter_config: &JupiterSourceConfig,
    attestor_set: &AttestorSet,
    jupiter_config_key: Pubkey,
) -> Result<()> {
    validate_jupiter_policy_binding(round, policy, jupiter_config, jupiter_config_key)?;
    require!(
        jupiter_config.attestor_set_version == attestor_set.version
            && round.attestor_set_version == attestor_set.version,
        ErrorCode::SourceConfigMismatch
    );
    Ok(())
}

pub(crate) fn validate_jupiter_policy_binding(
    round: &MarketRound,
    policy: &PricePolicy,
    jupiter_config: &JupiterSourceConfig,
    jupiter_config_key: Pubkey,
) -> Result<()> {
    require!(
        round.settlement_source_kind == SettlementSourceKind::JupiterTokenSpotV1
            && policy.source_kind == SettlementSourceKind::JupiterTokenSpotV1,
        ErrorCode::UnsupportedSettlementSource
    );
    require!(
        round.settlement_policy_version == policy.version
            && round.price_policy_version == policy.version
            && round.settlement_source_config == jupiter_config_key
            && policy.source_config == jupiter_config_key
            && round.jupiter_source_config_version == jupiter_config.version,
        ErrorCode::SourceConfigMismatch
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatched_jupiter_source_config_is_rejected_before_freeze() {
        let round = MarketRound {
            settlement_source_kind: crate::state::SettlementSourceKind::JupiterTokenSpotV1,
            settlement_source_config: Pubkey::new_from_array([1; 32]),
            ..round()
        };
        let policy = PricePolicy {
            version: 1,
            source_kind: crate::state::SettlementSourceKind::JupiterTokenSpotV1,
            source_config: Pubkey::new_from_array([1; 32]),
            observation_window_secs: 10,
            canonical_policy_hash: [3; 32],
            bump: 1,
        };
        let jupiter_config = JupiterSourceConfig {
            version: 1,
            attestor_set_version: 1,
            attestation_grace_secs: 10,
            sample_interval_secs: 5,
            max_attestor_spread_bps: 100,
            min_accepted_observations: 1,
            min_unique_source_blocks: 1,
            max_source_block_lag: 100,
            bump: 1,
        };
        let attestor_set = AttestorSet {
            version: 1,
            attestors: [Pubkey::new_from_array([11; 32]); crate::constants::ATTESTOR_COUNT],
            quorum: crate::constants::ATTESTOR_QUORUM,
            bump: 1,
        };

        assert!(validate_jupiter_source_binding(
            &round,
            &policy,
            &jupiter_config,
            &attestor_set,
            Pubkey::new_from_array([2; 32]),
        )
        .is_err());
    }

    fn round() -> MarketRound {
        MarketRound {
            round_id: 1,
            registry_version: 1,
            competition_domain: crate::state::CompetitionDomain::PublicEquity,
            settlement_policy_version: 1,
            settlement_source_kind: crate::state::SettlementSourceKind::JupiterTokenSpotV1,
            settlement_source_config: Pubkey::new_from_array([1; 32]),
            jupiter_source_config_version: 1,
            pyth_source_config_version: 0,
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
    fn market_round_state_advances_only_at_the_configured_boundary() {
        let round = round();

        assert!(next_market_round_state(&round, 19).is_err());
        assert_eq!(
            next_market_round_state(&round, 20).unwrap(),
            MarketRoundState::CommitOpen
        );
    }

    #[test]
    fn eligibility_snapshot_hash_cannot_be_the_uninitialized_sentinel() {
        assert!(validate_eligibility_snapshot_hash([0; 32]).is_err());
        assert!(validate_eligibility_snapshot_hash([1; 32]).is_ok());
    }
}
