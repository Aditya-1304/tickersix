// Anchor instruction handlers expose their serialized wire schema directly;
// their argument count is therefore intentionally larger than a Rust helper.
#![allow(clippy::too_many_arguments)]

pub mod constants;
pub mod error;
pub mod instructions;
pub mod math;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("8sehrRxpnLbvpzgJx8MqB5YApZAh69z5yVvdK1Zeyj6Z");

#[program]
pub mod tickersix {
    use super::*;

    pub fn initialize_config(ctx: Context<InitializeConfig>) -> Result<()> {
        instructions::admin::handle_initialize_config(ctx)
    }

    pub fn create_price_policy(
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
        canonical_policy_hash: [u8; 32],
        source_config: Pubkey,
    ) -> Result<()> {
        instructions::admin::handle_create_price_policy(
            ctx,
            version,
            source_kind,
            observation_window_secs,
            attestation_grace_secs,
            sample_interval_secs,
            max_attestor_spread_bps,
            min_accepted_observations,
            min_unique_source_blocks,
            max_source_block_lag,
            canonical_policy_hash,
            source_config,
        )
    }

    pub fn create_settlement_policy(
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
        canonical_policy_hash: [u8; 32],
        source_config: Pubkey,
    ) -> Result<()> {
        instructions::admin::handle_create_settlement_policy(
            ctx,
            version,
            source_kind,
            observation_window_secs,
            attestation_grace_secs,
            sample_interval_secs,
            max_attestor_spread_bps,
            min_accepted_observations,
            min_unique_source_blocks,
            max_source_block_lag,
            canonical_policy_hash,
            source_config,
        )
    }

    pub fn create_jupiter_source_config(
        ctx: Context<CreateJupiterSourceConfig>,
        version: u16,
        attestation_grace_secs: u16,
        sample_interval_secs: u16,
        max_attestor_spread_bps: u16,
        min_accepted_observations: u16,
        min_unique_source_blocks: u16,
        max_source_block_lag: u64,
    ) -> Result<()> {
        instructions::admin::handle_create_jupiter_source_config(
            ctx,
            version,
            attestation_grace_secs,
            sample_interval_secs,
            max_attestor_spread_bps,
            min_accepted_observations,
            min_unique_source_blocks,
            max_source_block_lag,
        )
    }

    pub fn create_market_quality_policy(
        ctx: Context<CreateMarketQualityPolicy>,
        version: u16,
        canonical_policy_hash: [u8; 32],
        min_eligible_assets: u16,
        competition_domain: CompetitionDomain,
    ) -> Result<()> {
        instructions::admin::handle_create_market_quality_policy(
            ctx,
            version,
            canonical_policy_hash,
            min_eligible_assets,
            competition_domain,
        )
    }

    pub fn create_attestor_set(
        ctx: Context<CreateAttestorSet>,
        version: u16,
        attestors: [Pubkey; ATTESTOR_COUNT],
    ) -> Result<()> {
        instructions::admin::handle_create_attestor_set(ctx, version, attestors)
    }

    pub fn set_pause(ctx: Context<SetPause>, paused: bool) -> Result<()> {
        instructions::admin::handle_set_pause(ctx, paused)
    }

    pub fn rotate_admin(ctx: Context<RotateAdmin>) -> Result<()> {
        instructions::admin::handle_rotate_admin(ctx)
    }

    pub fn rotate_coordinator(ctx: Context<RotateCoordinator>) -> Result<()> {
        instructions::admin::handle_rotate_coordinator(ctx)
    }

    pub fn freeze_registry_version(ctx: Context<FreezeRegistryVersion>) -> Result<()> {
        instructions::admin::handle_freeze_registry_version(ctx)
    }

    pub fn create_official_league(
        ctx: Context<CreateOfficialLeague>,
        league_id: u64,
        max_players: u16,
        total_rounds: u16,
        pairing_policy_version: u16,
        registration_close_at: i64,
    ) -> Result<()> {
        instructions::league::handle_create_official_league(
            ctx,
            league_id,
            max_players,
            total_rounds,
            pairing_policy_version,
            registration_close_at,
        )
    }

    pub fn activate_league(ctx: Context<ActivateLeague>) -> Result<()> {
        instructions::league::handle_activate_league(ctx)
    }

    pub fn cancel_league(ctx: Context<CancelLeague>) -> Result<()> {
        instructions::league::handle_cancel_league(ctx)
    }

    pub fn join_league(ctx: Context<JoinLeague>) -> Result<()> {
        instructions::league::handle_join_league(ctx)
    }

    pub fn leave_league_before_close(ctx: Context<LeaveLeagueBeforeClose>) -> Result<()> {
        instructions::league::handle_leave_league_before_close(ctx)
    }

    pub fn deactivate_league_member(ctx: Context<DeactivateLeagueMember>) -> Result<()> {
        instructions::league::handle_deactivate_league_member(ctx)
    }

    pub fn create_registry_entry(
        ctx: Context<CreateRegistryEntry>,
        registry_version: u32,
        asset_id: u16,
        symbol: [u8; 8],
        scoring_mint: Pubkey,
        issuer_kind: u8,
    ) -> Result<()> {
        instructions::admin::handle_create_registry_entry(
            ctx,
            registry_version,
            asset_id,
            symbol,
            scoring_mint,
            issuer_kind,
        )
    }

    pub fn create_market_round_draft(
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
        instructions::round::handle_create_market_round_draft(
            ctx,
            round_id,
            registry_version,
            eligibility_snapshot_hash,
            eligibility_frozen_at,
            queue_close_at,
            commit_deadline,
            reveal_deadline,
            start_target_at,
            end_target_at,
            is_replay,
        )
    }

    pub fn add_round_asset(
        ctx: Context<AddRoundAsset>,
        asset_id: u16,
        scoring_mint: Pubkey,
        issuer_kind: u8,
        price_source_kind: PriceSourceKind,
        price_policy_version: u16,
        market_quality_policy_version: u16,
    ) -> Result<()> {
        instructions::round::handle_add_round_asset(
            ctx,
            asset_id,
            scoring_mint,
            issuer_kind,
            price_source_kind,
            price_policy_version,
            market_quality_policy_version,
        )
    }

    pub fn freeze_market_round(ctx: Context<FreezeMarketRound>) -> Result<()> {
        instructions::round::handle_freeze_market_round(ctx)
    }

    pub fn advance_market_round(ctx: Context<AdvanceMarketRound>) -> Result<()> {
        instructions::round::handle_advance_market_round(ctx)
    }

    pub fn finalize_market_round(ctx: Context<FinalizeMarketRound>) -> Result<()> {
        instructions::round::handle_finalize_market_round(ctx)
    }

    pub fn create_rated_battle(
        ctx: Context<CreateRatedBattle>,
        battle_id: u64,
        mode: BattleMode,
        league: Pubkey,
        league_round_no: u16,
        rating_a_before: i32,
        rating_b_before: i32,
        rating_formula_version: u16,
    ) -> Result<()> {
        instructions::battle::handle_create_rated_battle(
            ctx,
            battle_id,
            mode,
            league,
            league_round_no,
            rating_a_before,
            rating_b_before,
            rating_formula_version,
        )
    }

    pub fn commit_lineup(ctx: Context<CommitLineup>, commitment: [u8; 32]) -> Result<()> {
        instructions::battle::handle_commit_lineup(ctx, commitment)
    }

    pub fn cancel_uncommitted_battle(ctx: Context<CancelUncommittedBattle>) -> Result<()> {
        instructions::battle::handle_cancel_uncommitted_battle(ctx)
    }

    pub fn reveal_lineup(
        ctx: Context<RevealLineup>,
        asset_ids: [u16; LINEUP_SIZE],
        captain_asset_id: u16,
        salt: [u8; 32],
    ) -> Result<()> {
        instructions::battle::handle_reveal_lineup(ctx, asset_ids, captain_asset_id, salt)
    }

    pub fn submit_price_attestation(
        ctx: Context<SubmitPriceAttestation>,
        phase: PricePhase,
        median_price_q9: i64,
        accepted_observation_count: u16,
        unique_source_block_count: u16,
        first_source_block_id: u64,
        last_source_block_id: u64,
        evidence_root: [u8; 32],
        report_created_at: i64,
    ) -> Result<()> {
        instructions::price::handle_submit_price_attestation(
            ctx,
            phase,
            median_price_q9,
            accepted_observation_count,
            unique_source_block_count,
            first_source_block_id,
            last_source_block_id,
            evidence_root,
            report_created_at,
        )
    }

    pub fn finalize_price_phase(ctx: Context<FinalizePricePhase>, phase: PricePhase) -> Result<()> {
        instructions::price::handle_finalize_price_phase(ctx, phase)
    }

    pub fn mark_price_phase_unavailable(
        ctx: Context<MarkPricePhaseUnavailable>,
        phase: PricePhase,
    ) -> Result<()> {
        instructions::price::handle_mark_price_phase_unavailable(ctx, phase)
    }

    pub fn settle_side_score(ctx: Context<SettleSideScore>, side_index: u8) -> Result<()> {
        instructions::battle::handle_settle_side_score(ctx, side_index)
    }

    pub fn finalize_battle(ctx: Context<FinalizeBattle>) -> Result<()> {
        instructions::battle::handle_finalize_battle(ctx)
    }

    pub fn void_battle_for_system_incident(
        ctx: Context<VoidBattleForSystemIncident>,
    ) -> Result<()> {
        instructions::battle::handle_void_battle_for_system_incident(ctx)
    }

    pub fn finalize_forfeit(ctx: Context<FinalizeForfeit>) -> Result<()> {
        instructions::battle::handle_finalize_forfeit(ctx)
    }

    pub fn void_battle_if_price_unavailable(
        ctx: Context<VoidBattleIfPriceUnavailable>,
    ) -> Result<()> {
        instructions::battle::handle_void_battle_if_price_unavailable(ctx)
    }
}
