//! Client-side construction for the price-report relay path.
//!
//! The relay is intentionally a transaction builder, not a key custodian. An
//! attestor signs a report in the market-data worker, while any relayer can
//! assemble the native Ed25519 verification instruction and the TickerSix
//! submission instruction. A wallet or external signer may then sign and send
//! the resulting transaction.

use std::{error::Error, fmt};

use anchor_lang::{prelude::Pubkey as AnchorPubkey, InstructionData};
use market_data::SignedAttestorReport;
use solana_address::Address;
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_sdk_ids::{ed25519_program, system_program};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use tickersix::PricePhase;

const ED25519_HEADER_LEN: u16 = 16;
const ED25519_SIGNATURE_LEN: u16 = 64;
const ED25519_PUBLIC_KEY_LEN: u16 = 32;
const MAX_ED25519_MESSAGE_LEN: usize = u16::MAX as usize;
const RELAYER_ACCOUNT_INDEX: usize = 8;

/// Accounts needed to relay one signed report. These are public addresses;
/// the relayer key itself is held by the caller's wallet or signer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayAccounts {
    pub price_policy: [u8; 32],
    pub jupiter_source_config: [u8; 32],
    pub market_quality_policy: [u8; 32],
    pub attestor_set: [u8; 32],
    pub relayer: [u8; 32],
}

/// Accounts needed by permissionless phase finalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizeAccounts {
    pub price_policy: [u8; 32],
    pub jupiter_source_config: [u8; 32],
    pub market_quality_policy: [u8; 32],
    pub attestor_set: [u8; 32],
    pub finalizer: [u8; 32],
}

/// Public accounts for one Pyth evidence submission. The signed payload is
/// deliberately not accepted as a trusted boolean or price; the builder only
/// carries the semantic fields and the hash of the exact signed payload,
/// while the program rechecks the pinned verifier receipt.
#[cfg(feature = "pyth-pro")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PythEvidenceAccounts {
    pub round_asset: [u8; 32],
    pub market_round: [u8; 32],
    pub settlement_policy: [u8; 32],
    pub pyth_source_config: [u8; 32],
    pub relayer: [u8; 32],
}

/// Public accounts for permissionless Pyth phase finalization.
#[cfg(feature = "pyth-pro")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PythFinalizeAccounts {
    pub round_asset: [u8; 32],
    pub market_round: [u8; 32],
    pub settlement_policy: [u8; 32],
    pub pyth_source_config: [u8; 32],
    pub pyth_price_evidence: [u8; 32],
    pub finalizer: [u8; 32],
}

/// Public accounts for the source-specific Pyth fail-closed transition.
#[cfg(feature = "pyth-pro")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PythUnavailableAccounts {
    pub round_asset: [u8; 32],
    pub market_round: [u8; 32],
    pub settlement_policy: [u8; 32],
    pub pyth_source_config: [u8; 32],
    pub marker: [u8; 32],
}

/// Public accounts for permissionless completion of a Pyth-backed round.
#[cfg(feature = "pyth-pro")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PythMarketRoundFinalizeAccounts {
    pub market_round: [u8; 32],
    pub settlement_policy: [u8; 32],
    pub pyth_source_config: [u8; 32],
    pub market_quality_policy: [u8; 32],
    pub keeper: [u8; 32],
}

/// Accounts required by the coordinator to admit one official Ranked Battle.
/// The coordinator signer is supplied by the caller; this crate never stores
/// or selects a coordinator private key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateRatedBattleAccounts {
    pub config: [u8; 32],
    pub coordinator: [u8; 32],
    pub market_round: [u8; 32],
    pub player_a: [u8; 32],
    pub player_b: [u8; 32],
    pub battle: [u8; 32],
    pub rated_slot_a: [u8; 32],
    pub rated_slot_b: [u8; 32],
}

/// Accounts required by the coordinator to create one official rated League.
/// The coordinator signer is supplied by the caller and is never persisted by
/// this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateOfficialLeagueAccounts {
    pub config: [u8; 32],
    pub coordinator: [u8; 32],
    pub league: [u8; 32],
}

/// Accounts required to request or complete one League membership lifecycle
/// transaction. The player signs both operations; the backend never receives
/// or stores the player's private key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeagueMembershipAccounts {
    pub config: [u8; 32],
    pub player: [u8; 32],
    pub league: [u8; 32],
    pub member: [u8; 32],
}

/// Accounts required by the coordinator to create one rated League Battle and
/// both per-round RatedSlot accounts atomically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeagueRatedBattleAccounts {
    pub config: [u8; 32],
    pub coordinator: [u8; 32],
    pub market_round: [u8; 32],
    pub player_a: [u8; 32],
    pub player_b: [u8; 32],
    pub league: [u8; 32],
    pub league_member_a: [u8; 32],
    pub league_member_b: [u8; 32],
    pub battle: [u8; 32],
    pub rated_slot_a: [u8; 32],
    pub rated_slot_b: [u8; 32],
}

/// The two instructions that must be adjacent in a relay transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceRelayPlan {
    /// Native Ed25519 verification. This must execute before `submission`.
    pub verification: Instruction,
    /// TickerSix report-account initialization and persistence instruction.
    pub submission: Instruction,
    /// PDA initialized by `submission` for this `(RoundAsset, phase, attestor)`.
    pub price_attestation: [u8; 32],
}

impl PriceRelayPlan {
    /// Returns the transaction instruction order required by the on-chain
    /// Instructions-sysvar verifier.
    pub fn instructions(&self) -> [Instruction; 2] {
        [self.verification.clone(), self.submission.clone()]
    }
}

/// Errors raised before a relay transaction is handed to a signer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayError {
    InvalidReport,
    InvalidPhase,
    ReportContextMismatch,
    DuplicateReport,
    InvalidRoundAssets,
    InvalidSide,
    InsufficientReports,
    TooManyReports,
    MessageTooLarge,
    TransactionBuildFailed(String),
}

impl fmt::Display for RelayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidReport => "signed report failed verification",
            Self::InvalidPhase => "price phase must be START (0) or END (1)",
            Self::ReportContextMismatch => "reports do not share the same frozen context",
            Self::DuplicateReport => "finalization contains a duplicate attestor report",
            Self::InvalidRoundAssets => "settlement requires six unique RoundAsset accounts",
            Self::InvalidSide => "Battle side index must be 0 or 1",
            Self::InsufficientReports => "finalization requires at least two reports",
            Self::TooManyReports => "finalization accepts at most three reports",
            Self::MessageTooLarge => "canonical report message exceeds Ed25519 message size",
            Self::TransactionBuildFailed(error) => error,
        };
        formatter.write_str(message)
    }
}

impl Error for RelayError {}

/// Builds the exact native-Ed25519-plus-submit instruction pair for one
/// signed report. The report is verified locally first, but the on-chain
/// program remains the authority because it inspects the native instruction.
pub fn build_submit_price_attestation_plan(
    report: &SignedAttestorReport,
    accounts: RelayAccounts,
) -> Result<PriceRelayPlan, RelayError> {
    report.verify().map_err(|_| RelayError::InvalidReport)?;
    let phase = phase_from_u8(report.report.context.phase)?;
    let message = report.report.canonical_bytes();
    if message.len() > MAX_ED25519_MESSAGE_LEN {
        return Err(RelayError::MessageTooLarge);
    }

    let context = report.report.context;
    if context.program_id != tickersix::ID.to_bytes() {
        return Err(RelayError::ReportContextMismatch);
    }

    let round_asset = AnchorPubkey::new_from_array(context.round_asset);
    let attestor = AnchorPubkey::new_from_array(report.attestor);
    let price_attestation = price_attestation_pda(round_asset, phase, attestor);

    let verification = build_ed25519_instruction(&message, report.attestor, &report.signature)?;
    let submission = Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: vec![
            readonly(address(context.round_asset)),
            readonly(address(context.market_round)),
            readonly(address(accounts.price_policy)),
            readonly(address(accounts.jupiter_source_config)),
            readonly(address(accounts.market_quality_policy)),
            readonly(address(accounts.attestor_set)),
            readonly(address(report.attestor)),
            writable(address(price_attestation.to_bytes())),
            signer_writable(address(accounts.relayer)),
            readonly(address(solana_instructions_sysvar::ID.to_bytes())),
            readonly(address(system_program::ID.to_bytes())),
        ],
        data: tickersix::instruction::SubmitPriceAttestation {
            phase,
            median_price_q9: report.report.median_price_q9,
            accepted_observation_count: report.report.accepted_observation_count,
            unique_source_block_count: report.report.unique_source_block_count,
            first_source_block_id: report.report.first_source_block_id,
            last_source_block_id: report.report.last_source_block_id,
            evidence_root: report.report.evidence_root,
            report_created_at: context.report_created_at,
        }
        .data(),
    };

    Ok(PriceRelayPlan {
        verification,
        submission,
        price_attestation: price_attestation.to_bytes(),
    })
}

/// Builds the Pyth verifier-receipt evidence instruction. The actual Pyth
/// verifier instruction must be placed immediately before this instruction by
/// the caller; the on-chain program checks both its pinned program id and its
/// exact data hash through the Instructions sysvar.
#[cfg(feature = "pyth-pro")]
#[allow(clippy::too_many_arguments)]
pub fn build_submit_or_record_pyth_evidence_instruction(
    accounts: PythEvidenceAccounts,
    phase: PricePhase,
    feed_id: u32,
    payload_timestamp_us: u64,
    feed_update_timestamp_us: u64,
    price_mantissa: i64,
    confidence_mantissa: u64,
    exponent: i16,
    normalized_price_q9: i64,
    payload_hash: [u8; 32],
) -> Result<Instruction, RelayError> {
    if feed_id == 0 || price_mantissa <= 0 || normalized_price_q9 <= 0 || payload_hash == [0; 32] {
        return Err(RelayError::InvalidReport);
    }

    let round_asset = AnchorPubkey::new_from_array(accounts.round_asset);
    let evidence = pyth_price_evidence_pda(round_asset, phase);
    Ok(Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: vec![
            readonly(address(accounts.round_asset)),
            readonly(address(accounts.market_round)),
            readonly(address(accounts.settlement_policy)),
            readonly(address(accounts.pyth_source_config)),
            writable(address(evidence.to_bytes())),
            signer_writable(address(accounts.relayer)),
            readonly(address(solana_instructions_sysvar::ID.to_bytes())),
            readonly(address(system_program::ID.to_bytes())),
        ],
        data: tickersix::instruction::SubmitOrRecordPythEvidence {
            phase,
            feed_id,
            payload_timestamp_us,
            feed_update_timestamp_us,
            price_mantissa,
            confidence_mantissa,
            exponent,
            normalized_price_q9,
            payload_hash,
        }
        .data(),
    })
}

/// Builds the permissionless Pyth phase-finalization instruction. The
/// program, not this client helper, rechecks the evidence PDA and source
/// binding before copying the verified Q9 price into the RoundAsset.
#[cfg(feature = "pyth-pro")]
pub fn build_finalize_pyth_price_phase_instruction(
    accounts: PythFinalizeAccounts,
    phase: PricePhase,
) -> Instruction {
    Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: vec![
            writable(address(accounts.round_asset)),
            readonly(address(accounts.market_round)),
            readonly(address(accounts.settlement_policy)),
            readonly(address(accounts.pyth_source_config)),
            readonly(address(accounts.pyth_price_evidence)),
            signer(address(accounts.finalizer)),
        ],
        data: tickersix::instruction::FinalizePythPricePhase { phase }.data(),
    }
}

/// Builds the permissionless Pyth unavailable transition. It contains no
/// price payload and cannot route a frozen Pyth round through Jupiter.
#[cfg(feature = "pyth-pro")]
pub fn build_mark_pyth_price_phase_unavailable_instruction(
    accounts: PythUnavailableAccounts,
    phase: PricePhase,
) -> Instruction {
    Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: vec![
            writable(address(accounts.round_asset)),
            readonly(address(accounts.market_round)),
            readonly(address(accounts.settlement_policy)),
            readonly(address(accounts.pyth_source_config)),
            signer(address(accounts.marker)),
        ],
        data: tickersix::instruction::MarkPythPricePhaseUnavailable { phase }.data(),
    }
}

/// Builds the source-specific Pyth round finalizer and appends the complete
/// frozen RoundAsset set in canonical caller order.
#[cfg(feature = "pyth-pro")]
pub fn build_finalize_pyth_market_round_instruction(
    accounts: PythMarketRoundFinalizeAccounts,
    round_assets: &[[u8; 32]],
) -> Instruction {
    let mut account_metas = vec![
        writable(address(accounts.market_round)),
        readonly(address(accounts.settlement_policy)),
        readonly(address(accounts.pyth_source_config)),
        readonly(address(accounts.market_quality_policy)),
        signer(address(accounts.keeper)),
    ];
    account_metas.extend(
        round_assets
            .iter()
            .copied()
            .map(|round_asset| readonly(address(round_asset))),
    );
    Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: account_metas,
        data: tickersix::instruction::FinalizePythMarketRound {}.data(),
    }
}

/// Derives the canonical Pyth evidence PDA used by both relay instructions.
#[cfg(feature = "pyth-pro")]
pub fn pyth_price_evidence_address(round_asset: [u8; 32], phase: PricePhase) -> [u8; 32] {
    pyth_price_evidence_pda(AnchorPubkey::new_from_array(round_asset), phase).to_bytes()
}

/// Builds the exact Anchor instruction used by the coordinator to create one
/// Ranked Battle and both RatedSlot PDAs atomically. The program remains the
/// final authority: it checks the frozen round, coordinator signer, player
/// identities, replay flag, and whether either RatedSlot already exists.
pub fn build_create_rated_battle_instruction(
    battle_id: u64,
    rating_a_before: i32,
    rating_b_before: i32,
    rating_formula_version: u16,
    accounts: CreateRatedBattleAccounts,
) -> Instruction {
    Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: vec![
            readonly(address(accounts.config)),
            signer_writable(address(accounts.coordinator)),
            writable(address(accounts.market_round)),
            readonly(address(accounts.player_a)),
            readonly(address(accounts.player_b)),
            // Anchor uses the program id as the explicit `None` marker for
            // each optional League account when required accounts follow.
            readonly(address(tickersix::ID.to_bytes())),
            readonly(address(tickersix::ID.to_bytes())),
            readonly(address(tickersix::ID.to_bytes())),
            writable(address(accounts.battle)),
            writable(address(accounts.rated_slot_a)),
            writable(address(accounts.rated_slot_b)),
            readonly(address(system_program::ID.to_bytes())),
        ],
        data: tickersix::instruction::CreateRatedBattle {
            battle_id,
            mode: tickersix::BattleMode::Ranked,
            league: AnchorPubkey::default(),
            league_round_no: 0,
            rating_a_before,
            rating_b_before,
            rating_formula_version,
        }
        .data(),
    }
}

/// Builds the coordinator instruction that creates the canonical on-chain
/// official League account. Backend catalog data should be inserted or
/// reconciled using the resulting League PDA only after this transaction is
/// submitted and confirmed.
pub fn build_create_official_league_instruction(
    league_id: u64,
    max_players: u16,
    total_rounds: u16,
    pairing_policy_version: u16,
    registration_close_at: i64,
    accounts: CreateOfficialLeagueAccounts,
) -> Instruction {
    Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: vec![
            readonly(address(accounts.config)),
            signer_writable(address(accounts.coordinator)),
            writable(address(accounts.league)),
            readonly(address(system_program::ID.to_bytes())),
        ],
        data: tickersix::instruction::CreateOfficialLeague {
            league_id,
            max_players,
            total_rounds,
            pairing_policy_version,
            registration_close_at,
        }
        .data(),
    }
}

/// Builds the coordinator instruction for a League pairing. The optional
/// League accounts used by the shared Anchor instruction are supplied in the
/// exact order expected by `CreateRatedBattle` so the program can validate the
/// League PDA, both LeagueMember PDAs, current round, and active membership.
pub fn build_create_league_rated_battle_instruction(
    battle_id: u64,
    league_round_no: u16,
    rating_a_before: i32,
    rating_b_before: i32,
    rating_formula_version: u16,
    accounts: LeagueRatedBattleAccounts,
) -> Instruction {
    Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: vec![
            readonly(address(accounts.config)),
            signer_writable(address(accounts.coordinator)),
            writable(address(accounts.market_round)),
            readonly(address(accounts.player_a)),
            readonly(address(accounts.player_b)),
            readonly(address(accounts.league)),
            readonly(address(accounts.league_member_a)),
            readonly(address(accounts.league_member_b)),
            writable(address(accounts.battle)),
            writable(address(accounts.rated_slot_a)),
            writable(address(accounts.rated_slot_b)),
            readonly(address(system_program::ID.to_bytes())),
        ],
        data: tickersix::instruction::CreateRatedBattle {
            battle_id,
            mode: tickersix::BattleMode::League,
            league: AnchorPubkey::new_from_array(accounts.league),
            league_round_no,
            rating_a_before,
            rating_b_before,
            rating_formula_version,
        }
        .data(),
    }
}

/// Builds the wallet-signed instruction that creates a LeagueMember PDA.
/// PostgreSQL may track the request as pending, but membership becomes active
/// only after a chain indexer confirms this instruction succeeded.
pub fn build_join_league_instruction(accounts: LeagueMembershipAccounts) -> Instruction {
    Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: vec![
            readonly(address(accounts.config)),
            signer_writable(address(accounts.player)),
            writable(address(accounts.league)),
            writable(address(accounts.member)),
            readonly(address(system_program::ID.to_bytes())),
        ],
        data: tickersix::instruction::JoinLeague {}.data(),
    }
}

/// Builds the wallet-signed instruction that closes a LeagueMember PDA before
/// registration closes. The backend keeps the reservation blocked until the
/// chain indexer confirms this instruction and marks the membership as left.
pub fn build_leave_league_instruction(accounts: LeagueMembershipAccounts) -> Instruction {
    Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: vec![
            readonly(address(accounts.config)),
            signer_writable(address(accounts.player)),
            writable(address(accounts.league)),
            writable(address(accounts.member)),
        ],
        data: tickersix::instruction::LeaveLeagueBeforeClose {}.data(),
    }
}

/// Derives the canonical Config PDA used by all player-facing League
/// membership instructions.
pub fn config_pda() -> [u8; 32] {
    AnchorPubkey::find_program_address(&[tickersix::CONFIG_SEED], &tickersix::ID)
        .0
        .to_bytes()
}

/// Derives the canonical League PDA for an on-chain league id.
pub fn league_pda(league_id: u64) -> [u8; 32] {
    AnchorPubkey::find_program_address(
        &[tickersix::LEAGUE_SEED, &league_id.to_le_bytes()],
        &tickersix::ID,
    )
    .0
    .to_bytes()
}

/// Derives the canonical LeagueMember PDA for a League and wallet.
pub fn league_member_pda(league: [u8; 32], player: [u8; 32]) -> [u8; 32] {
    AnchorPubkey::find_program_address(
        &[
            tickersix::LEAGUE_MEMBER_SEED,
            league.as_ref(),
            player.as_ref(),
        ],
        &tickersix::ID,
    )
    .0
    .to_bytes()
}

/// Derives the Battle and RatedSlot addresses used by the ranked coordinator
/// transaction. Keeping PDA derivation beside instruction construction avoids
/// backend/client disagreement about seed order or integer encoding.
pub fn create_rated_battle_pdas(
    market_round: [u8; 32],
    battle_id: u64,
    player_a: [u8; 32],
    player_b: [u8; 32],
) -> ([u8; 32], [u8; 32], [u8; 32]) {
    let market_round = AnchorPubkey::new_from_array(market_round);
    let player_a = AnchorPubkey::new_from_array(player_a);
    let player_b = AnchorPubkey::new_from_array(player_b);
    let battle = AnchorPubkey::find_program_address(
        &[
            tickersix::BATTLE_SEED,
            market_round.as_ref(),
            &battle_id.to_le_bytes(),
        ],
        &tickersix::ID,
    )
    .0;
    let rated_slot_a = AnchorPubkey::find_program_address(
        &[
            tickersix::RATED_SLOT_SEED,
            market_round.as_ref(),
            player_a.as_ref(),
        ],
        &tickersix::ID,
    )
    .0;
    let rated_slot_b = AnchorPubkey::find_program_address(
        &[
            tickersix::RATED_SLOT_SEED,
            market_round.as_ref(),
            player_b.as_ref(),
        ],
        &tickersix::ID,
    )
    .0;
    (
        battle.to_bytes(),
        rated_slot_a.to_bytes(),
        rated_slot_b.to_bytes(),
    )
}

/// Builds the permissionless finalization instruction for two or three signed
/// reports. Reports are verified and sorted by attestor before their PDAs are
/// added, making the generated transaction stable even when input arrival
/// order differs.
pub fn build_finalize_price_phase_instruction(
    phase_number: u8,
    reports: &[SignedAttestorReport],
    accounts: FinalizeAccounts,
) -> Result<Instruction, RelayError> {
    build_finalize_price_phase_instruction_with_name(phase_number, reports, accounts, false)
}

/// Builds the canonical Jupiter-specific phase finalizer. The legacy generic
/// builder remains available for compatibility, but new settlement workers
/// should use this name so source selection is explicit in the instruction
/// discriminator and in the resulting proof transaction.
pub fn build_finalize_jupiter_price_phase_instruction(
    phase_number: u8,
    reports: &[SignedAttestorReport],
    accounts: FinalizeAccounts,
) -> Result<Instruction, RelayError> {
    build_finalize_price_phase_instruction_with_name(phase_number, reports, accounts, true)
}

fn build_finalize_price_phase_instruction_with_name(
    phase_number: u8,
    reports: &[SignedAttestorReport],
    accounts: FinalizeAccounts,
    jupiter_name: bool,
) -> Result<Instruction, RelayError> {
    let phase = phase_from_u8(phase_number)?;
    if reports.len() < 2 {
        return Err(RelayError::InsufficientReports);
    }
    if reports.len() > 3 {
        return Err(RelayError::TooManyReports);
    }

    let first_context = reports[0].report.context;
    if first_context.program_id != tickersix::ID.to_bytes() {
        return Err(RelayError::ReportContextMismatch);
    }
    let mut attestation_accounts = Vec::with_capacity(reports.len());
    for report in reports {
        report.verify().map_err(|_| RelayError::InvalidReport)?;
        let context = report.report.context;
        if !same_frozen_context(first_context, context) || context.phase != phase_number {
            return Err(RelayError::ReportContextMismatch);
        }
        let round_asset = AnchorPubkey::new_from_array(context.round_asset);
        let attestor = AnchorPubkey::new_from_array(report.attestor);
        let price_attestation = price_attestation_pda(round_asset, phase, attestor);
        if attestation_accounts
            .iter()
            .any(|(_, existing)| *existing == price_attestation.to_bytes())
        {
            return Err(RelayError::DuplicateReport);
        }
        attestation_accounts.push((report.attestor, price_attestation.to_bytes()));
    }
    attestation_accounts.sort_unstable_by_key(|(attestor, _)| *attestor);

    let mut account_metas = vec![
        writable(address(first_context.round_asset)),
        readonly(address(first_context.market_round)),
        readonly(address(accounts.price_policy)),
        readonly(address(accounts.jupiter_source_config)),
        readonly(address(accounts.market_quality_policy)),
        readonly(address(accounts.attestor_set)),
        signer(address(accounts.finalizer)),
    ];
    account_metas.extend(
        attestation_accounts
            .into_iter()
            .map(|(_, account)| readonly(address(account))),
    );

    Ok(Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: account_metas,
        data: if jupiter_name {
            tickersix::instruction::FinalizeJupiterPricePhase { phase }.data()
        } else {
            tickersix::instruction::FinalizePricePhase { phase }.data()
        },
    })
}

/// Builds the explicit fail-closed transition used when a phase misses its
/// deadline or has no compatible quorum. This instruction never supplies a
/// replacement price or alternate source.
#[allow(clippy::too_many_arguments)]
pub fn build_mark_price_phase_unavailable_instruction(
    phase_number: u8,
    round_asset: [u8; 32],
    market_round: [u8; 32],
    price_policy: [u8; 32],
    jupiter_source_config: [u8; 32],
    attestor_set: [u8; 32],
    marker: [u8; 32],
    attestors: [[u8; 32]; 3],
) -> Result<Instruction, RelayError> {
    build_mark_price_phase_unavailable_instruction_with_name(
        phase_number,
        round_asset,
        market_round,
        price_policy,
        jupiter_source_config,
        attestor_set,
        marker,
        attestors,
        false,
    )
}

/// Builds the canonical Jupiter-specific fail-closed phase transition.
#[allow(clippy::too_many_arguments)]
pub fn build_mark_jupiter_price_phase_unavailable_instruction(
    phase_number: u8,
    round_asset: [u8; 32],
    market_round: [u8; 32],
    price_policy: [u8; 32],
    jupiter_source_config: [u8; 32],
    attestor_set: [u8; 32],
    marker: [u8; 32],
    attestors: [[u8; 32]; 3],
) -> Result<Instruction, RelayError> {
    build_mark_price_phase_unavailable_instruction_with_name(
        phase_number,
        round_asset,
        market_round,
        price_policy,
        jupiter_source_config,
        attestor_set,
        marker,
        attestors,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_mark_price_phase_unavailable_instruction_with_name(
    phase_number: u8,
    round_asset: [u8; 32],
    market_round: [u8; 32],
    price_policy: [u8; 32],
    jupiter_source_config: [u8; 32],
    attestor_set: [u8; 32],
    marker: [u8; 32],
    attestors: [[u8; 32]; 3],
    jupiter_name: bool,
) -> Result<Instruction, RelayError> {
    let phase = phase_from_u8(phase_number)?;
    let mut report_accounts = Vec::with_capacity(attestors.len());
    for attestor in attestors {
        let report = price_attestation_pda(
            AnchorPubkey::new_from_array(round_asset),
            phase,
            AnchorPubkey::new_from_array(attestor),
        )
        .to_bytes();
        if report_accounts.contains(&report) {
            return Err(RelayError::DuplicateReport);
        }
        report_accounts.push(report);
    }

    let mut accounts = vec![
        writable(address(round_asset)),
        readonly(address(market_round)),
        readonly(address(price_policy)),
        readonly(address(jupiter_source_config)),
        readonly(address(attestor_set)),
        signer(address(marker)),
    ];
    accounts.extend(
        report_accounts
            .into_iter()
            .map(|report| readonly(address(report))),
    );

    Ok(Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts,
        data: if jupiter_name {
            tickersix::instruction::MarkJupiterPricePhaseUnavailable { phase }.data()
        } else {
            tickersix::instruction::MarkPricePhaseUnavailable { phase }.data()
        },
    })
}

/// Builds the account layout for one side of the exact Q9 score settlement.
/// The six RoundAsset addresses are passed in the caller's lineup order; the
/// program binds each account to the revealed asset id stored in the Battle.
pub fn build_settle_side_score_instruction(
    side_index: u8,
    battle: [u8; 32],
    market_round: [u8; 32],
    settler: [u8; 32],
    round_assets: &[[u8; 32]],
) -> Result<Instruction, RelayError> {
    if side_index > 1 {
        return Err(RelayError::InvalidSide);
    }
    let round_assets = unique_round_assets(round_assets, true)?;
    let mut accounts = vec![
        writable(address(battle)),
        readonly(address(market_round)),
        signer(address(settler)),
    ];
    accounts.extend(
        round_assets
            .into_iter()
            .map(|asset| readonly(address(asset))),
    );
    Ok(Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts,
        data: tickersix::instruction::SettleSideScore { side_index }.data(),
    })
}

/// Builds the permissionless Battle result finalization instruction.
pub fn build_finalize_battle_instruction(
    battle: [u8; 32],
    market_round: [u8; 32],
    finalizer: [u8; 32],
) -> Instruction {
    Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: vec![
            writable(address(battle)),
            writable(address(market_round)),
            signer(address(finalizer)),
        ],
        data: tickersix::instruction::FinalizeBattle {}.data(),
    }
}

/// Builds the post-reveal-window player-forfeit finalization instruction.
pub fn build_finalize_forfeit_instruction(
    battle: [u8; 32],
    market_round: [u8; 32],
    finalizer: [u8; 32],
) -> Instruction {
    Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: vec![
            writable(address(battle)),
            signer(address(finalizer)),
            writable(address(market_round)),
        ],
        data: tickersix::instruction::FinalizeForfeit {}.data(),
    }
}

/// Builds the price-unavailable Battle void instruction. No replacement price
/// or alternate oracle account can be carried by this instruction.
pub fn build_void_battle_price_unavailable_instruction(
    battle: [u8; 32],
    market_round: [u8; 32],
    marker: [u8; 32],
    unavailable_round_assets: &[[u8; 32]],
) -> Result<Instruction, RelayError> {
    let round_assets = unique_round_assets(unavailable_round_assets, false)?;
    let mut accounts = vec![
        writable(address(battle)),
        writable(address(market_round)),
        signer(address(marker)),
    ];
    accounts.extend(
        round_assets
            .into_iter()
            .map(|asset| readonly(address(asset))),
    );
    Ok(Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts,
        data: tickersix::instruction::VoidBattleIfPriceUnavailable {}.data(),
    })
}

/// Builds the coordinator-controlled system-incident void instruction.
pub fn build_void_battle_system_incident_instruction(
    config: [u8; 32],
    battle: [u8; 32],
    market_round: [u8; 32],
    coordinator: [u8; 32],
) -> Instruction {
    Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts: vec![
            readonly(address(config)),
            writable(address(market_round)),
            writable(address(battle)),
            signer(address(coordinator)),
        ],
        data: tickersix::instruction::VoidBattleForSystemIncident {}.data(),
    }
}

/// Builds the final round transition after all assets and Battles are
/// terminal. The caller supplies the complete frozen RoundAsset set.
pub fn build_finalize_market_round_instruction(
    market_round: [u8; 32],
    price_policy: [u8; 32],
    jupiter_source_config: [u8; 32],
    market_quality_policy: [u8; 32],
    keeper: [u8; 32],
    round_assets: &[[u8; 32]],
) -> Result<Instruction, RelayError> {
    let round_assets = unique_round_assets(round_assets, false)?;
    let mut accounts = vec![
        writable(address(market_round)),
        readonly(address(price_policy)),
        readonly(address(jupiter_source_config)),
        readonly(address(market_quality_policy)),
        signer(address(keeper)),
    ];
    accounts.extend(
        round_assets
            .into_iter()
            .map(|asset| readonly(address(asset))),
    );
    Ok(Instruction {
        program_id: address(tickersix::ID.to_bytes()),
        accounts,
        data: tickersix::instruction::FinalizeMarketRound {}.data(),
    })
}

/// Signs a relay plan with an in-memory relayer key and returns a legacy
/// transaction. The caller must obtain a fresh blockhash and submit the
/// transaction through its chosen wallet/RPC boundary.
pub fn build_signed_legacy_relay_transaction(
    plan: &PriceRelayPlan,
    recent_blockhash: Hash,
    relayer: &Keypair,
) -> Result<VersionedTransaction, RelayError> {
    let Some(relayer_account) = plan.submission.accounts.get(RELAYER_ACCOUNT_INDEX) else {
        return Err(RelayError::TransactionBuildFailed(
            "relay submission instruction is missing its relayer account".to_owned(),
        ));
    };
    if relayer.pubkey().to_bytes() != relayer_account.pubkey.to_bytes() {
        return Err(RelayError::TransactionBuildFailed(
            "relayer key does not match the relay account".to_owned(),
        ));
    }
    let instructions = plan.instructions();
    let message =
        Message::new_with_blockhash(&instructions, Some(&relayer.pubkey()), &recent_blockhash);
    VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[relayer])
        .map_err(|error| RelayError::TransactionBuildFailed(error.to_string()))
}

fn build_ed25519_instruction(
    message: &[u8],
    public_key: [u8; 32],
    signature: &[u8],
) -> Result<Instruction, RelayError> {
    if signature.len() != usize::from(ED25519_SIGNATURE_LEN) {
        return Err(RelayError::InvalidReport);
    }
    let message_offset = ED25519_HEADER_LEN
        .checked_add(ED25519_SIGNATURE_LEN)
        .and_then(|offset| offset.checked_add(ED25519_PUBLIC_KEY_LEN))
        .ok_or(RelayError::MessageTooLarge)?;
    let message_offset = usize::from(message_offset);
    let message_len = u16::try_from(message.len()).map_err(|_| RelayError::MessageTooLarge)?;
    let mut data = Vec::with_capacity(message_offset + message.len());
    data.extend_from_slice(&[1, 0]);
    data.extend_from_slice(&ED25519_HEADER_LEN.to_le_bytes());
    data.extend_from_slice(&u16::MAX.to_le_bytes());
    data.extend_from_slice(&(ED25519_HEADER_LEN + ED25519_SIGNATURE_LEN).to_le_bytes());
    data.extend_from_slice(&u16::MAX.to_le_bytes());
    data.extend_from_slice(&(message_offset as u16).to_le_bytes());
    data.extend_from_slice(&message_len.to_le_bytes());
    data.extend_from_slice(&u16::MAX.to_le_bytes());
    data.extend_from_slice(signature);
    data.extend_from_slice(&public_key);
    data.extend_from_slice(message);

    Ok(Instruction {
        program_id: address(ed25519_program::ID.to_bytes()),
        accounts: Vec::new(),
        data,
    })
}

fn unique_round_assets(
    round_assets: &[[u8; 32]],
    require_six: bool,
) -> Result<Vec<[u8; 32]>, RelayError> {
    if require_six && round_assets.len() != 6 {
        return Err(RelayError::InvalidRoundAssets);
    }
    if round_assets.is_empty() {
        return Err(RelayError::InvalidRoundAssets);
    }
    let mut unique = Vec::with_capacity(round_assets.len());
    for asset in round_assets {
        if unique.contains(asset) {
            return Err(RelayError::InvalidRoundAssets);
        }
        unique.push(*asset);
    }
    Ok(unique)
}

fn phase_from_u8(value: u8) -> Result<PricePhase, RelayError> {
    match value {
        0 => Ok(PricePhase::Start),
        1 => Ok(PricePhase::End),
        _ => Err(RelayError::InvalidPhase),
    }
}

fn same_frozen_context(
    left: market_data::PriceReportContext,
    right: market_data::PriceReportContext,
) -> bool {
    left.program_id == right.program_id
        && left.market_round == right.market_round
        && left.round_asset == right.round_asset
        && left.asset_id == right.asset_id
        && left.phase == right.phase
        && left.scoring_mint == right.scoring_mint
        && left.price_policy_version == right.price_policy_version
        && left.market_quality_policy_version == right.market_quality_policy_version
        && left.attestor_set_version == right.attestor_set_version
        && left.observation_window_start == right.observation_window_start
        && left.observation_window_end == right.observation_window_end
}

fn price_attestation_pda(
    round_asset: AnchorPubkey,
    phase: PricePhase,
    attestor: AnchorPubkey,
) -> AnchorPubkey {
    AnchorPubkey::find_program_address(
        &[
            tickersix::PRICE_ATTESTATION_SEED,
            round_asset.as_ref(),
            &[phase as u8],
            attestor.as_ref(),
        ],
        &tickersix::ID,
    )
    .0
}

#[cfg(feature = "pyth-pro")]
fn pyth_price_evidence_pda(round_asset: AnchorPubkey, phase: PricePhase) -> AnchorPubkey {
    AnchorPubkey::find_program_address(
        &[
            tickersix::PYTH_PRICE_EVIDENCE_SEED,
            round_asset.as_ref(),
            &[phase as u8],
        ],
        &tickersix::ID,
    )
    .0
}

fn address(bytes: [u8; 32]) -> Address {
    Address::from(bytes)
}

fn readonly(address: Address) -> AccountMeta {
    AccountMeta::new_readonly(address, false)
}

fn writable(address: Address) -> AccountMeta {
    AccountMeta::new(address, false)
}

fn signer(address: Address) -> AccountMeta {
    AccountMeta::new_readonly(address, true)
}

fn signer_writable(address: Address) -> AccountMeta {
    AccountMeta::new(address, true)
}
