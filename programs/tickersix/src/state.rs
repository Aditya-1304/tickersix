use anchor_lang::prelude::*;

use crate::constants::{ATTESTOR_COUNT, LINEUP_SIZE};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum PriceSourceKind {
    JupiterTokenSpotV1,
    Pyth247IndexV1,
    VerifiedIssuerOracleV1,
}

#[derive(Debug, AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum MarketRoundState {
    Preparing,
    Scheduled,
    CommitOpen,
    RevealOpen,
    Live,
    Ended,
    Settling,
    Finalized,
    Voided,
}

#[derive(Debug, AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum BattleMode {
    Ranked,
    League,
    Exhibition,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum SideStatus {
    AwaitingCommit,
    Committed,
    Revealed,
    ScoreFinalized,
    Forfeited,
}

#[derive(Debug, AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum BattleResult {
    Pending,
    PlayerA,
    PlayerB,
    Draw,
    ForfeitA,
    ForfeitB,
    BothForfeit,
    Voided,
}

#[derive(Debug, AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum VoidReason {
    None,
    PriceUnavailable,
    RegistryFault,
    SystemIncident,
    InvalidRound,
    AdminEmergency,
}

#[account]
#[derive(InitSpace)]
pub struct Config {
    pub admin_authority: Pubkey,
    pub coordinator_authority: Pubkey,
    pub paused: bool,
    pub current_registry_version: u32,
    pub registry_frozen: bool,
    pub last_market_round_id: u64,
    pub last_market_round_end_at: i64,
    pub protocol_version: u16,
    pub current_price_policy_version: u16,
    pub current_market_quality_policy_version: u16,
    pub current_attestor_set_version: u16,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct PricePolicy {
    pub version: u16,
    pub source_kind: PriceSourceKind,
    pub observation_window_secs: u16,
    pub attestation_grace_secs: u16,
    pub sample_interval_secs: u16,
    pub max_attestor_spread_bps: u16,
    pub min_accepted_observations: u16,
    pub min_unique_source_blocks: u16,
    pub max_source_block_lag: u64,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct MarketQualityPolicy {
    pub version: u16,
    pub canonical_policy_hash: [u8; 32],
    pub min_eligible_assets: u16,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct AttestorSet {
    pub version: u16,
    pub attestors: [Pubkey; ATTESTOR_COUNT],
    pub quorum: u8,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct AssetRegistryEntry {
    pub registry_version: u32,
    pub asset_id: u16,
    pub symbol: [u8; 8],
    pub scoring_mint: Pubkey,
    pub issuer_kind: u8,
    pub active: bool,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct MarketRound {
    pub round_id: u64,
    pub registry_version: u32,
    pub price_policy_version: u16,
    pub market_quality_policy_version: u16,
    pub attestor_set_version: u16,
    pub market_quality_policy_hash: [u8; 32],
    pub eligibility_snapshot_hash: [u8; 32],
    pub eligibility_frozen_at: i64,
    pub queue_close_at: i64,
    pub commit_deadline: i64,
    pub reveal_deadline: i64,
    pub start_target_at: i64,
    pub end_target_at: i64,
    pub observation_window_secs: u16,
    pub attestation_grace_secs: u16,
    pub max_attestor_spread_bps: u16,
    pub eligible_asset_bitmap: [u64; 4],
    pub round_asset_count: u16,
    /// Number of rated Battles admitted into this frozen round. The count is
    /// used by the permissionless completion instruction so a caller cannot
    /// finalize a round while a created Battle is still unresolved.
    pub rated_battle_count: u32,
    /// Number of admitted Battles that reached a terminal result, including
    /// player results, forfeits, price-unavailable voids, and system voids.
    pub resolved_battle_count: u32,
    pub state: MarketRoundState,
    pub is_replay: bool,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct RoundAsset {
    pub market_round: Pubkey,
    pub asset_id: u16,
    pub scoring_mint: Pubkey,
    pub issuer_kind: u8,
    pub price_source_kind: PriceSourceKind,
    pub price_policy_version: u16,
    pub market_quality_policy_version: u16,
    pub start_price_q9: i64,
    pub start_finalized: bool,
    pub start_unavailable: bool,
    pub start_evidence_commitment: [u8; 32],
    pub end_price_q9: i64,
    pub end_finalized: bool,
    pub end_unavailable: bool,
    pub end_evidence_commitment: [u8; 32],
    pub return_q9: i64,
    pub available: bool,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace)]
pub struct BattleSide {
    pub player: Pubkey,
    pub commitment: [u8; 32],
    pub committed: bool,
    pub commit_slot: u64,
    pub asset_ids: [u16; LINEUP_SIZE],
    pub captain_asset_id: u16,
    pub revealed: bool,
    pub reveal_slot: u64,
    pub score_q9: i64,
    pub score_finalized: bool,
    pub status: SideStatus,
}

#[account]
#[derive(InitSpace)]
pub struct Battle {
    pub battle_id: u64,
    pub market_round: Pubkey,
    pub mode: BattleMode,
    pub rated: bool,
    pub league: Pubkey,
    pub league_round_no: u16,
    pub rating_a_before: i32,
    pub rating_b_before: i32,
    pub rating_formula_version: u16,
    pub a: BattleSide,
    pub b: BattleSide,
    pub result: BattleResult,
    pub void_reason: VoidReason,
    pub created_at: i64,
    pub finalized_at: i64,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct RatedSlot {
    pub market_round: Pubkey,
    pub player: Pubkey,
    pub battle: Pubkey,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum LeagueState {
    Registration,
    Active,
    Completed,
    Cancelled,
}

#[account]
#[derive(InitSpace)]
pub struct League {
    pub league_id: u64,
    pub creator: Pubkey,
    pub max_players: u16,
    pub joined_players: u16,
    pub total_rounds: u16,
    pub current_round: u16,
    pub pairing_policy_version: u16,
    pub rated: bool,
    pub registration_close_at: i64,
    pub state: LeagueState,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct LeagueMember {
    pub league: Pubkey,
    pub player: Pubkey,
    pub joined_at: i64,
    pub active: bool,
    pub bye_count: u16,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum PricePhase {
    Start,
    End,
}

#[account]
#[derive(InitSpace)]
pub struct PriceAttestation {
    pub round_asset: Pubkey,
    pub phase: PricePhase,
    pub attestor: Pubkey,
    pub median_price_q9: i64,
    pub accepted_observation_count: u16,
    pub unique_source_block_count: u16,
    pub first_source_block_id: u64,
    pub last_source_block_id: u64,
    pub evidence_root: [u8; 32],
    pub report_created_at: i64,
    pub bump: u8,
}
