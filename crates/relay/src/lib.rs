//! Client-side construction for the Phase 2.2 price-report relay path.
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
const RELAYER_ACCOUNT_INDEX: usize = 7;

/// Accounts needed to relay one signed report. These are public addresses;
/// the relayer key itself is held by the caller's wallet or signer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayAccounts {
    pub price_policy: [u8; 32],
    pub market_quality_policy: [u8; 32],
    pub attestor_set: [u8; 32],
    pub relayer: [u8; 32],
}

/// Accounts needed by permissionless phase finalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizeAccounts {
    pub price_policy: [u8; 32],
    pub market_quality_policy: [u8; 32],
    pub attestor_set: [u8; 32],
    pub finalizer: [u8; 32],
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
            readonly(address(accounts.market_quality_policy)),
            readonly(address(accounts.attestor_set)),
            readonly(address(report.attestor)),
            writable(address(price_attestation.to_bytes())),
            signer(address(accounts.relayer)),
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

/// Builds the permissionless finalization instruction for two or three signed
/// reports. Reports are verified and sorted by attestor before their PDAs are
/// added, making the generated transaction stable even when input arrival
/// order differs.
pub fn build_finalize_price_phase_instruction(
    phase_number: u8,
    reports: &[SignedAttestorReport],
    accounts: FinalizeAccounts,
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
        data: tickersix::instruction::FinalizePricePhase { phase }.data(),
    })
}

/// Builds the explicit fail-closed transition used when a phase misses its
/// deadline or has no compatible quorum. This instruction never supplies a
/// replacement price or alternate source.
pub fn build_mark_price_phase_unavailable_instruction(
    phase_number: u8,
    round_asset: [u8; 32],
    market_round: [u8; 32],
    price_policy: [u8; 32],
    attestor_set: [u8; 32],
    marker: [u8; 32],
    attestors: [[u8; 32]; 3],
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
        data: tickersix::instruction::MarkPricePhaseUnavailable { phase }.data(),
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
