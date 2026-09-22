use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Protocol is paused")]
    ProtocolPaused,
    #[msg("Only the configured administrator may perform this action")]
    UnauthorizedAdmin,
    #[msg("Only the configured coordinator may perform this action")]
    UnauthorizedCoordinator,
    #[msg("Protocol version is invalid")]
    InvalidProtocolVersion,
    #[msg("Policy version must be non-zero")]
    InvalidPolicyVersion,
    #[msg("Policy parameters must be calibrated and bounded")]
    UncalibratedPolicy,
    #[msg("Settlement source is not enabled in this protocol build")]
    UnsupportedSettlementSource,
    #[msg("Settlement policy and source configuration are not bound together")]
    SourceConfigMismatch,
    #[msg("Attestor set must contain three unique keys and a two-report quorum")]
    InvalidAttestorSet,
    #[msg("Asset identifier is outside the supported registry range")]
    InvalidAssetId,
    #[msg("Scoring mint cannot be the default public key")]
    InvalidScoringMint,
    #[msg("Round asset does not match its frozen registry entry")]
    RegistryEntryMismatch,
    #[msg("Registry version is invalid or not the configured current version")]
    InvalidRegistryVersion,
    #[msg("The current registry version must be frozen before rated use")]
    RegistryNotFrozen,
    #[msg("This operation is not valid in the current market-round state")]
    InvalidRoundState,
    #[msg("Market-round timing is not strictly ordered")]
    InvalidRoundTiming,
    #[msg("Eligibility snapshot hash must commit to a non-empty snapshot")]
    InvalidEligibilitySnapshot,
    #[msg("Market-round sequence or schedule overlaps an earlier official round")]
    InvalidRoundSequence,
    #[msg("Rated Battle creation is outside the round admission window")]
    RatedBattleWindowClosed,
    #[msg("Market round does not contain enough eligible assets")]
    InsufficientEligibleAssets,
    #[msg("Market-round asset identity is duplicated")]
    DuplicateRoundAsset,
    #[msg("Market-round asset account belongs to a different round")]
    WrongMarketRound,
    #[msg("Market round is replay-only and cannot be rated")]
    ReplayCannotBeRated,
    #[msg("Rated Battle participants must be distinct")]
    SameBattlePlayer,
    #[msg("Rated Battle must use an official ranked or league mode")]
    InvalidRatedBattleMode,
    #[msg("Player is not a participant in this Battle")]
    NotBattleParticipant,
    #[msg("Lineup commitment is missing or already submitted")]
    InvalidCommitmentState,
    #[msg("Lineup commitment is outside its submission window")]
    CommitWindowClosed,
    #[msg("Lineup reveal is outside its submission window")]
    RevealWindowClosed,
    #[msg("Lineup must contain six unique eligible assets")]
    InvalidLineup,
    #[msg("Captain must be one of the selected assets")]
    CaptainNotInLineup,
    #[msg("Lineup reveal does not match the stored commitment")]
    CommitmentMismatch,
    #[msg("Price policy does not match the frozen market round")]
    PricePolicyMismatch,
    #[msg("Market-quality policy does not match the frozen market round")]
    QualityPolicyMismatch,
    #[msg("Attestor set does not match the frozen market round")]
    AttestorSetMismatch,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Attestation submission is outside the frozen observation window")]
    AttestationWindowClosed,
    #[msg("Attestation report does not meet the frozen quality minimums")]
    InvalidAttestationEvidence,
    #[msg("Attestor is not registered in the frozen attestor set")]
    UnregisteredAttestor,
    #[msg("Attestation report is malformed or has already been submitted")]
    InvalidAttestation,
    #[msg("The native Ed25519 verification instruction does not match the report")]
    InvalidAttestationInstruction,
    #[msg("No compatible attestor quorum exists")]
    NoCompatibleQuorum,
    #[msg("Price phase has already been resolved")]
    PricePhaseResolved,
    #[msg("Selected RoundAsset is not finalized and available")]
    RoundAssetUnavailable,
    #[msg("Battle side is not ready for this operation")]
    InvalidBattleState,
    #[msg("Battle result has already been finalized")]
    BattleAlreadyFinalized,
    #[msg("League configuration or state is invalid")]
    InvalidLeague,
    #[msg("League registration is closed")]
    LeagueRegistrationClosed,
    #[msg("League has reached its player limit")]
    LeagueFull,
    #[msg("Wallet is already a member of this League")]
    AlreadyLeagueMember,
    #[msg("Wallet is not an active member of this League")]
    NotLeagueMember,
}
