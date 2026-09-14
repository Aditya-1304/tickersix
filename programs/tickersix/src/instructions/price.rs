use anchor_lang::prelude::*;
use solana_instructions_sysvar::{load_current_index_checked, load_instruction_at_checked};
use solana_sdk_ids::ed25519_program;

use crate::{
    constants::{
        ATTESTATION_DOMAIN, ATTESTOR_QUORUM, ATTESTOR_SET_SEED, MARKET_ROUND_SEED,
        PRICE_ATTESTATION_SEED, PRICE_POLICY_SEED, QUALITY_POLICY_SEED, ROUND_ASSET_SEED,
    },
    error::ErrorCode,
    math::return_q9,
    state::{
        AttestorSet, MarketQualityPolicy, MarketRound, PriceAttestation, PricePhase, PricePolicy,
        RoundAsset,
    },
};

#[derive(Debug, Clone, Copy)]
struct AttestationSummary {
    attestor: Pubkey,
    price_q9: i64,
    evidence_root: [u8; 32],
}

#[derive(Debug)]
struct QuorumResult {
    selected: Vec<AttestationSummary>,
    finalized_price_q9: i64,
    spread_bps: u64,
}

#[derive(Accounts)]
#[instruction(phase: PricePhase)]
pub struct SubmitPriceAttestation<'info> {
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
        seeds = [PRICE_POLICY_SEED, &market_round.price_policy_version.to_le_bytes()],
        bump = price_policy.bump
    )]
    pub price_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [QUALITY_POLICY_SEED, &market_round.market_quality_policy_version.to_le_bytes()],
        bump = market_quality_policy.bump
    )]
    pub market_quality_policy: Account<'info, MarketQualityPolicy>,
    #[account(
        seeds = [ATTESTOR_SET_SEED, &market_round.attestor_set_version.to_le_bytes()],
        bump = attestor_set.bump
    )]
    pub attestor_set: Account<'info, AttestorSet>,
    /// CHECK: The public key is checked against the frozen attestor set and the
    /// native Ed25519 instruction is inspected below.
    pub attestor: UncheckedAccount<'info>,
    #[account(
        init,
        payer = relayer,
        space = 8 + PriceAttestation::INIT_SPACE,
        seeds = [PRICE_ATTESTATION_SEED, round_asset.key().as_ref(), &[phase as u8], attestor.key().as_ref()],
        bump
    )]
    pub price_attestation: Account<'info, PriceAttestation>,
    #[account(mut)]
    pub relayer: Signer<'info>,
    /// CHECK: This is the runtime instructions sysvar used to verify the native
    /// Ed25519 instruction that precedes this instruction.
    #[account(address = solana_instructions_sysvar::ID)]
    pub instructions_sysvar: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn handle_submit_price_attestation(
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
    let round = &ctx.accounts.market_round;
    let policy = &ctx.accounts.price_policy;
    let round_asset = &ctx.accounts.round_asset;
    let now = Clock::get()?.unix_timestamp;
    let (window_start, window_end) = phase_window(round, phase)?;
    let submission_deadline = window_end
        .checked_add(i64::from(policy.attestation_grace_secs))
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;

    require!(
        now >= window_end && now <= submission_deadline,
        ErrorCode::AttestationWindowClosed
    );
    require!(
        report_created_at >= window_start && report_created_at <= window_end,
        ErrorCode::AttestationWindowClosed
    );
    require!(median_price_q9 > 0, ErrorCode::InvalidAttestationEvidence);
    require!(
        accepted_observation_count >= policy.min_accepted_observations
            && unique_source_block_count >= policy.min_unique_source_blocks
            && unique_source_block_count <= accepted_observation_count
            && first_source_block_id <= last_source_block_id,
        ErrorCode::InvalidAttestationEvidence
    );
    let current_slot = Clock::get()?.slot;
    require!(
        last_source_block_id <= current_slot
            && current_slot - last_source_block_id <= policy.max_source_block_lag,
        ErrorCode::InvalidAttestationEvidence
    );
    require!(
        round.price_policy_version == policy.version
            && round_asset.price_policy_version == policy.version
            && round_asset.price_source_kind == policy.source_kind,
        ErrorCode::PricePolicyMismatch
    );
    require!(
        round.market_quality_policy_version == ctx.accounts.market_quality_policy.version
            && round_asset.market_quality_policy_version
                == ctx.accounts.market_quality_policy.version
            && round.market_quality_policy_hash
                == ctx.accounts.market_quality_policy.canonical_policy_hash,
        ErrorCode::QualityPolicyMismatch
    );
    require!(
        round.attestor_set_version == ctx.accounts.attestor_set.version,
        ErrorCode::AttestorSetMismatch
    );
    require!(
        ctx.accounts.attestor_set.quorum == ATTESTOR_QUORUM
            && ctx
                .accounts
                .attestor_set
                .attestors
                .contains(&ctx.accounts.attestor.key()),
        ErrorCode::UnregisteredAttestor
    );

    let message = canonical_attestation_message(
        crate::id(),
        round.key(),
        round_asset.key(),
        round_asset.asset_id,
        round_asset.scoring_mint,
        round_asset.price_policy_version,
        round_asset.market_quality_policy_version,
        round.attestor_set_version,
        phase,
        median_price_q9,
        accepted_observation_count,
        unique_source_block_count,
        first_source_block_id,
        last_source_block_id,
        evidence_root,
        window_start,
        window_end,
        report_created_at,
    );
    verify_ed25519_instruction(
        &ctx.accounts.instructions_sysvar.to_account_info(),
        &ctx.accounts.attestor.key(),
        &message,
    )?;

    let attestation = &mut ctx.accounts.price_attestation;
    attestation.round_asset = round_asset.key();
    attestation.phase = phase;
    attestation.attestor = ctx.accounts.attestor.key();
    attestation.median_price_q9 = median_price_q9;
    attestation.accepted_observation_count = accepted_observation_count;
    attestation.unique_source_block_count = unique_source_block_count;
    attestation.first_source_block_id = first_source_block_id;
    attestation.last_source_block_id = last_source_block_id;
    attestation.evidence_root = evidence_root;
    attestation.report_created_at = report_created_at;
    attestation.bump = ctx.bumps.price_attestation;
    Ok(())
}

#[derive(Accounts)]
pub struct FinalizePricePhase<'info> {
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
        seeds = [PRICE_POLICY_SEED, &market_round.price_policy_version.to_le_bytes()],
        bump = price_policy.bump
    )]
    pub price_policy: Account<'info, PricePolicy>,
    #[account(
        seeds = [ATTESTOR_SET_SEED, &market_round.attestor_set_version.to_le_bytes()],
        bump = attestor_set.bump
    )]
    pub attestor_set: Account<'info, AttestorSet>,
    pub finalizer: Signer<'info>,
}

pub fn handle_finalize_price_phase(
    ctx: Context<FinalizePricePhase>,
    phase: PricePhase,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let (_, window_end) = phase_window(&ctx.accounts.market_round, phase)?;
    let deadline = window_end
        .checked_add(i64::from(ctx.accounts.price_policy.attestation_grace_secs))
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    require!(now > deadline, ErrorCode::AttestationWindowClosed);
    require!(
        ctx.accounts.round_asset.price_policy_version == ctx.accounts.price_policy.version,
        ErrorCode::PricePolicyMismatch
    );
    require!(
        ctx.accounts.round_asset.price_source_kind == ctx.accounts.price_policy.source_kind,
        ErrorCode::PricePolicyMismatch
    );
    require!(
        !phase_is_finalized(&ctx.accounts.round_asset, phase)
            && !phase_is_unavailable(&ctx.accounts.round_asset, phase),
        ErrorCode::PricePhaseResolved
    );

    let reports = read_attestation_summaries(
        ctx.remaining_accounts,
        ctx.accounts.round_asset.key(),
        phase,
        &ctx.accounts.attestor_set,
    )?;
    let quorum =
        choose_compatible_quorum(&reports, ctx.accounts.market_round.max_attestor_spread_bps)?;
    let evidence_commitment =
        selected_evidence_commitment(ctx.accounts.round_asset.key(), phase, &quorum.selected);
    let round_asset = &mut ctx.accounts.round_asset;
    match phase {
        PricePhase::Start => {
            round_asset.start_price_q9 = quorum.finalized_price_q9;
            round_asset.start_finalized = true;
            round_asset.start_unavailable = false;
            round_asset.start_evidence_commitment = evidence_commitment;
        }
        PricePhase::End => {
            round_asset.end_price_q9 = quorum.finalized_price_q9;
            round_asset.end_finalized = true;
            round_asset.end_unavailable = false;
            round_asset.end_evidence_commitment = evidence_commitment;
            require!(round_asset.start_finalized, ErrorCode::InvalidBattleState);
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
pub struct MarkPricePhaseUnavailable<'info> {
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
    pub marker: Signer<'info>,
}

pub fn handle_mark_price_phase_unavailable(
    ctx: Context<MarkPricePhaseUnavailable>,
    phase: PricePhase,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let target = match phase {
        PricePhase::Start => ctx.accounts.market_round.start_target_at,
        PricePhase::End => ctx.accounts.market_round.end_target_at,
    };
    let window_end = target
        .checked_add(i64::from(ctx.accounts.market_round.observation_window_secs))
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    let deadline = window_end
        .checked_add(i64::from(ctx.accounts.market_round.attestation_grace_secs))
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    require!(now > deadline, ErrorCode::AttestationWindowClosed);
    require!(
        !phase_is_finalized(&ctx.accounts.round_asset, phase)
            && !phase_is_unavailable(&ctx.accounts.round_asset, phase),
        ErrorCode::PricePhaseResolved
    );
    match phase {
        PricePhase::Start => ctx.accounts.round_asset.start_unavailable = true,
        PricePhase::End => ctx.accounts.round_asset.end_unavailable = true,
    }
    ctx.accounts.round_asset.available = false;
    Ok(())
}

fn phase_window(round: &MarketRound, phase: PricePhase) -> Result<(i64, i64)> {
    let start = match phase {
        PricePhase::Start => round.start_target_at,
        PricePhase::End => round.end_target_at,
    };
    let end = start
        .checked_add(i64::from(round.observation_window_secs))
        .ok_or_else(|| error!(ErrorCode::MathOverflow))?;
    Ok((start, end))
}

fn phase_is_finalized(round_asset: &RoundAsset, phase: PricePhase) -> bool {
    match phase {
        PricePhase::Start => round_asset.start_finalized,
        PricePhase::End => round_asset.end_finalized,
    }
}

fn phase_is_unavailable(round_asset: &RoundAsset, phase: PricePhase) -> bool {
    match phase {
        PricePhase::Start => round_asset.start_unavailable,
        PricePhase::End => round_asset.end_unavailable,
    }
}

fn read_attestation_summaries(
    accounts: &[AccountInfo<'_>],
    round_asset: Pubkey,
    phase: PricePhase,
    attestor_set: &AttestorSet,
) -> Result<Vec<AttestationSummary>> {
    require!(accounts.len() <= 3, ErrorCode::InvalidAttestation);
    let mut reports = Vec::with_capacity(accounts.len());
    for account in accounts {
        require_keys_eq!(*account.owner, crate::id(), ErrorCode::InvalidAttestation);
        let data = account.try_borrow_data()?;
        let mut slice: &[u8] = &data;
        let report = PriceAttestation::try_deserialize(&mut slice)
            .map_err(|_| error!(ErrorCode::InvalidAttestation))?;
        require_keys_eq!(report.round_asset, round_asset, ErrorCode::WrongMarketRound);
        require!(report.phase == phase, ErrorCode::InvalidAttestation);
        require!(
            attestor_set.attestors.contains(&report.attestor),
            ErrorCode::UnregisteredAttestor
        );
        let (expected_key, _) = Pubkey::find_program_address(
            &[
                PRICE_ATTESTATION_SEED,
                round_asset.as_ref(),
                &[phase as u8],
                report.attestor.as_ref(),
            ],
            &crate::id(),
        );
        require_keys_eq!(*account.key, expected_key, ErrorCode::InvalidAttestation);
        require!(
            !reports
                .iter()
                .any(|existing: &AttestationSummary| existing.attestor == report.attestor),
            ErrorCode::InvalidAttestation
        );
        reports.push(AttestationSummary {
            attestor: report.attestor,
            price_q9: report.median_price_q9,
            evidence_root: report.evidence_root,
        });
    }
    require!(
        reports.len() >= usize::from(ATTESTOR_QUORUM),
        ErrorCode::NoCompatibleQuorum
    );
    Ok(reports)
}

fn choose_compatible_quorum(
    reports: &[AttestationSummary],
    max_spread_bps: u16,
) -> Result<QuorumResult> {
    let combinations = 1usize << reports.len();
    let mut best: Option<QuorumResult> = None;
    for mask in 0..combinations {
        let selected: Vec<AttestationSummary> = (0..reports.len())
            .filter(|index| mask & (1 << index) != 0)
            .map(|index| reports[index])
            .collect();
        if selected.len() < usize::from(ATTESTOR_QUORUM) {
            continue;
        }

        let minimum = selected.iter().map(|report| report.price_q9).min().unwrap();
        let maximum = selected.iter().map(|report| report.price_q9).max().unwrap();
        require!(minimum > 0, ErrorCode::InvalidAttestationEvidence);
        let midpoint = (i128::from(minimum) + i128::from(maximum)) / 2;
        let spread_bps = u64::try_from(
            i128::from(maximum - minimum)
                .checked_mul(10_000)
                .and_then(|value| value.checked_div(midpoint))
                .ok_or_else(|| error!(ErrorCode::MathOverflow))?,
        )
        .map_err(|_| error!(ErrorCode::MathOverflow))?;
        if spread_bps > u64::from(max_spread_bps) {
            continue;
        }

        let finalized_price_q9 = if selected.len() == 2 {
            i64::try_from(midpoint).map_err(|_| error!(ErrorCode::MathOverflow))?
        } else {
            let mut prices: Vec<i64> = selected.iter().map(|report| report.price_q9).collect();
            prices.sort_unstable();
            prices[prices.len() / 2]
        };
        let mut selected = selected;
        selected.sort_unstable_by_key(|report| report.attestor);
        let candidate = QuorumResult {
            selected,
            finalized_price_q9,
            spread_bps,
        };
        if best.as_ref().is_none_or(|current| {
            candidate.selected.len() > current.selected.len()
                || (candidate.selected.len() == current.selected.len()
                    && (candidate.spread_bps < current.spread_bps
                        || (candidate.spread_bps == current.spread_bps
                            && candidate
                                .selected
                                .iter()
                                .map(|report| report.attestor)
                                .lt(current.selected.iter().map(|report| report.attestor)))))
        }) {
            best = Some(candidate);
        }
    }
    best.ok_or_else(|| error!(ErrorCode::NoCompatibleQuorum))
}

fn selected_evidence_commitment(
    round_asset: Pubkey,
    phase: PricePhase,
    selected: &[AttestationSummary],
) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(64 + selected.len() * 72);
    bytes.extend_from_slice(b"TICKERSIX_SELECTED_ATTESTATIONS_V1\0");
    bytes.extend_from_slice(round_asset.as_ref());
    bytes.push(phase as u8);
    for report in selected {
        bytes.extend_from_slice(report.attestor.as_ref());
        bytes.extend_from_slice(&report.price_q9.to_le_bytes());
        bytes.extend_from_slice(&report.evidence_root);
    }
    solana_sha256_hasher::hash(&bytes).to_bytes()
}

/// Canonical bytes signed by an attestor. The attestor public key is identified
/// by the native Ed25519 instruction and is intentionally not duplicated in the
/// signed payload, matching the source-of-truth message layout.
pub fn canonical_attestation_message(
    program_id: Pubkey,
    market_round: Pubkey,
    round_asset: Pubkey,
    asset_id: u16,
    scoring_mint: Pubkey,
    price_policy_version: u16,
    market_quality_policy_version: u16,
    attestor_set_version: u16,
    phase: PricePhase,
    median_price_q9: i64,
    accepted_observation_count: u16,
    unique_source_block_count: u16,
    first_source_block_id: u64,
    last_source_block_id: u64,
    evidence_root: [u8; 32],
    observation_window_start: i64,
    observation_window_end: i64,
    report_created_at: i64,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(256);
    bytes.extend_from_slice(ATTESTATION_DOMAIN);
    bytes.extend_from_slice(program_id.as_ref());
    bytes.extend_from_slice(market_round.as_ref());
    bytes.extend_from_slice(round_asset.as_ref());
    bytes.extend_from_slice(&asset_id.to_le_bytes());
    bytes.push(phase as u8);
    bytes.extend_from_slice(scoring_mint.as_ref());
    bytes.extend_from_slice(&price_policy_version.to_le_bytes());
    bytes.extend_from_slice(&market_quality_policy_version.to_le_bytes());
    bytes.extend_from_slice(&attestor_set_version.to_le_bytes());
    bytes.extend_from_slice(&median_price_q9.to_le_bytes());
    bytes.extend_from_slice(&accepted_observation_count.to_le_bytes());
    bytes.extend_from_slice(&unique_source_block_count.to_le_bytes());
    bytes.extend_from_slice(&first_source_block_id.to_le_bytes());
    bytes.extend_from_slice(&last_source_block_id.to_le_bytes());
    bytes.extend_from_slice(&evidence_root);
    bytes.extend_from_slice(&observation_window_start.to_le_bytes());
    bytes.extend_from_slice(&observation_window_end.to_le_bytes());
    bytes.extend_from_slice(&report_created_at.to_le_bytes());
    bytes
}

fn verify_ed25519_instruction(
    instructions_sysvar: &AccountInfo<'_>,
    expected_public_key: &Pubkey,
    expected_message: &[u8],
) -> Result<()> {
    let current_index = load_current_index_checked(instructions_sysvar)
        .map_err(|_| error!(ErrorCode::InvalidAttestationInstruction))?;
    for index in 0..current_index {
        let instruction = load_instruction_at_checked(index as usize, instructions_sysvar)
            .map_err(|_| error!(ErrorCode::InvalidAttestationInstruction))?;
        if instruction.program_id != ed25519_program::ID || instruction.data.len() < 16 {
            continue;
        }

        let signatures = usize::from(instruction.data[0]);
        for signature_number in 0..signatures {
            let offset = 2 + signature_number * 14;
            if instruction.data.len() < offset + 14 {
                continue;
            }
            let signature_offset = read_u16(&instruction.data, offset);
            let signature_instruction = read_u16(&instruction.data, offset + 2);
            let public_key_offset = read_u16(&instruction.data, offset + 4);
            let public_key_instruction = read_u16(&instruction.data, offset + 6);
            let message_offset = read_u16(&instruction.data, offset + 8);
            let message_size = read_u16(&instruction.data, offset + 10);
            let message_instruction = read_u16(&instruction.data, offset + 12);

            // The client uses the native Ed25519 instruction's inline payload.
            // Rejecting cross-instruction offsets keeps the inspected bytes
            // unambiguous and avoids treating a later instruction as evidence.
            if signature_instruction != u16::MAX
                || public_key_instruction != u16::MAX
                || message_instruction != u16::MAX
            {
                continue;
            }
            let signature_data = &instruction.data;
            let public_key_data = &instruction.data;
            let message_data = &instruction.data;
            let signature_end = usize::from(signature_offset).saturating_add(64);
            let public_key_end = usize::from(public_key_offset).saturating_add(32);
            let message_end = usize::from(message_offset).saturating_add(usize::from(message_size));
            if signature_end > signature_data.len()
                || public_key_end > public_key_data.len()
                || message_end > message_data.len()
            {
                continue;
            }
            if public_key_data[usize::from(public_key_offset)..public_key_end]
                == expected_public_key.to_bytes()
                && &message_data[usize::from(message_offset)..message_end] == expected_message
            {
                return Ok(());
            }
        }
    }
    err!(ErrorCode::InvalidAttestationInstruction)
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}
