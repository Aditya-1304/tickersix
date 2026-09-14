use market_data::{AttestorSigner, CanonicalPriceReport, PriceReportContext, SignedAttestorReport};
use relay::{
    build_finalize_price_phase_instruction, build_mark_price_phase_unavailable_instruction,
    build_signed_legacy_relay_transaction, build_submit_price_attestation_plan, FinalizeAccounts,
    RelayAccounts, RelayError,
};
use solana_address::Address;
use solana_hash::Hash;
use solana_keypair::Keypair;
use solana_sdk_ids::ed25519_program;
use solana_signer::Signer;

fn report(phase: u8, attestor_seed: u8, price_q9: i64) -> SignedAttestorReport {
    report_for_program(phase, attestor_seed, price_q9, tickersix::ID.to_bytes())
}

fn report_for_program(
    phase: u8,
    attestor_seed: u8,
    price_q9: i64,
    program_id: [u8; 32],
) -> SignedAttestorReport {
    let context = PriceReportContext {
        program_id,
        market_round: [2; 32],
        round_asset: [3; 32],
        asset_id: 7,
        phase,
        scoring_mint: [4; 32],
        price_policy_version: 1,
        market_quality_policy_version: 2,
        attestor_set_version: 3,
        observation_window_start: 100,
        observation_window_end: 110,
        report_created_at: 109,
    };
    AttestorSigner::from_secret_key([attestor_seed; 32]).sign(CanonicalPriceReport {
        context,
        median_price_q9: price_q9,
        accepted_observation_count: 3,
        unique_source_block_count: 3,
        first_source_block_id: 10,
        last_source_block_id: 12,
        evidence_root: [5; 32],
    })
}

fn accounts() -> RelayAccounts {
    RelayAccounts {
        price_policy: [10; 32],
        market_quality_policy: [11; 32],
        attestor_set: [12; 32],
        relayer: [13; 32],
    }
}

#[test]
fn submit_plan_places_native_verification_before_program_instruction() {
    let signed = report(0, 9, 100_000);
    let plan = build_submit_price_attestation_plan(&signed, accounts()).unwrap();

    assert_eq!(
        plan.verification.program_id,
        Address::from(ed25519_program::ID.to_bytes())
    );
    assert_eq!(
        plan.submission.program_id,
        Address::from(tickersix::ID.to_bytes())
    );
    assert_eq!(plan.submission.accounts.len(), 10);
    assert_eq!(
        plan.submission.accounts[6].pubkey,
        Address::from(plan.price_attestation)
    );
    assert!(plan.submission.accounts[7].is_signer);
    assert_eq!(
        plan.submission.accounts[8].pubkey,
        Address::from(solana_instructions_sysvar::ID.to_bytes())
    );
    assert_eq!(
        plan.verification.data[16 + 64 + 32..],
        signed.report.canonical_bytes()
    );
}

#[test]
fn submit_plan_rejects_a_report_with_a_mutated_signature() {
    let mut signed = report(0, 9, 100_000);
    signed.signature[0] ^= 1;

    assert!(matches!(
        build_submit_price_attestation_plan(&signed, accounts()),
        Err(RelayError::InvalidReport)
    ));
}

#[test]
fn finalize_plan_sorts_reports_and_rejects_mixed_phases() {
    let first = report(0, 9, 100_000);
    let second = report(0, 10, 100_010);
    let instruction = build_finalize_price_phase_instruction(
        0,
        &[second.clone(), first.clone()],
        FinalizeAccounts {
            price_policy: [10; 32],
            market_quality_policy: [11; 32],
            attestor_set: [12; 32],
            finalizer: [13; 32],
        },
    )
    .unwrap();
    let reverse = build_finalize_price_phase_instruction(
        0,
        &[first.clone(), second.clone()],
        FinalizeAccounts {
            price_policy: [10; 32],
            market_quality_policy: [11; 32],
            attestor_set: [12; 32],
            finalizer: [13; 32],
        },
    )
    .unwrap();

    assert_eq!(instruction.accounts.len(), 8);
    assert_eq!(instruction.accounts[0].pubkey, Address::from([3; 32]));
    assert_eq!(instruction.accounts, reverse.accounts);
    assert!(build_finalize_price_phase_instruction(
        0,
        &[first, report(1, 10, 100_010)],
        FinalizeAccounts {
            price_policy: [10; 32],
            market_quality_policy: [11; 32],
            attestor_set: [12; 32],
            finalizer: [13; 32],
        },
    )
    .is_err());
}

#[test]
fn finalize_plan_rejects_reports_for_another_program() {
    let bad_report = report_for_program(0, 9, 100_000, [99; 32]);

    assert!(matches!(
        build_finalize_price_phase_instruction(
            0,
            &[bad_report, report(0, 10, 100_010)],
            FinalizeAccounts {
                price_policy: [10; 32],
                market_quality_policy: [11; 32],
                attestor_set: [12; 32],
                finalizer: [13; 32],
            },
        ),
        Err(RelayError::ReportContextMismatch)
    ));
}

#[test]
fn signed_relay_transaction_uses_the_plan_relayer_and_exact_instruction_order() {
    let relayer = Keypair::new();
    let mut relay_accounts = accounts();
    relay_accounts.relayer = relayer.pubkey().to_bytes();
    let plan = build_submit_price_attestation_plan(&report(0, 9, 100_000), relay_accounts).unwrap();

    let transaction =
        build_signed_legacy_relay_transaction(&plan, Hash::new_from_array([14; 32]), &relayer)
            .unwrap();

    assert_eq!(transaction.signatures.len(), 1);
    assert_eq!(transaction.message.instructions().len(), 2);
    assert_eq!(
        transaction.message.instructions()[0].data,
        plan.verification.data
    );
    assert_eq!(
        transaction.message.instructions()[1].data,
        plan.submission.data
    );
}

#[test]
fn unavailable_plan_is_explicit_and_has_no_price_payload() {
    let instruction = build_mark_price_phase_unavailable_instruction(
        1,
        [3; 32],
        [2; 32],
        [10; 32],
        [12; 32],
        [13; 32],
        [[9; 32], [10; 32], [11; 32]],
    )
    .unwrap();

    assert_eq!(instruction.accounts.len(), 8);
    assert!(instruction.accounts[0].is_writable);
    assert!(instruction.accounts[4].is_signer);
    assert!(build_mark_price_phase_unavailable_instruction(
        2,
        [3; 32],
        [2; 32],
        [10; 32],
        [12; 32],
        [13; 32],
        [[9; 32], [10; 32], [11; 32]],
    )
    .is_err());
}
