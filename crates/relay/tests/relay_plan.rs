use market_data::{AttestorSigner, CanonicalPriceReport, PriceReportContext, SignedAttestorReport};
use relay::{
    build_create_league_rated_battle_instruction, build_create_official_league_instruction,
    build_create_rated_battle_instruction, build_finalize_battle_instruction,
    build_finalize_forfeit_instruction, build_finalize_market_round_instruction,
    build_finalize_price_phase_instruction, build_join_league_instruction,
    build_leave_league_instruction, build_mark_price_phase_unavailable_instruction,
    build_settle_side_score_instruction, build_signed_legacy_relay_transaction,
    build_submit_price_attestation_plan, build_void_battle_price_unavailable_instruction,
    build_void_battle_system_incident_instruction, league_member_pda, league_pda,
    CreateOfficialLeagueAccounts, CreateRatedBattleAccounts, FinalizeAccounts,
    LeagueMembershipAccounts, LeagueRatedBattleAccounts, RelayAccounts, RelayError,
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
        jupiter_source_config: [14; 32],
        market_quality_policy: [11; 32],
        attestor_set: [12; 32],
        relayer: [13; 32],
    }
}

#[test]
fn coordinator_battle_builder_preserves_ranked_account_authority_and_writability() {
    let instruction = build_create_rated_battle_instruction(
        7,
        1500,
        1510,
        1,
        CreateRatedBattleAccounts {
            config: [10; 32],
            coordinator: [11; 32],
            market_round: [12; 32],
            player_a: [13; 32],
            player_b: [14; 32],
            battle: [15; 32],
            rated_slot_a: [16; 32],
            rated_slot_b: [17; 32],
        },
    );

    assert_eq!(
        instruction.program_id,
        Address::from(tickersix::ID.to_bytes())
    );
    assert_eq!(instruction.accounts.len(), 12);
    assert!(instruction.accounts[1].is_signer);
    assert!(instruction.accounts[1].is_writable);
    assert!(instruction.accounts[2].is_writable);
    assert!(!instruction.accounts[3].is_writable);
    assert!(!instruction.accounts[4].is_writable);
    assert_eq!(
        instruction.accounts[5].pubkey,
        Address::from(tickersix::ID.to_bytes())
    );
    assert_eq!(
        instruction.accounts[6].pubkey,
        Address::from(tickersix::ID.to_bytes())
    );
    assert_eq!(
        instruction.accounts[7].pubkey,
        Address::from(tickersix::ID.to_bytes())
    );
    assert!(instruction.accounts[8].is_writable);
    assert!(instruction.accounts[9].is_writable);
    assert!(instruction.accounts[10].is_writable);
    assert!(!instruction.data.is_empty());
}

#[test]
fn official_league_builder_uses_the_canonical_league_pda_and_coordinator_signer() {
    let league_id = 42;
    let instruction = build_create_official_league_instruction(
        league_id,
        100,
        5,
        1,
        1_800_000_000,
        CreateOfficialLeagueAccounts {
            config: [10; 32],
            coordinator: [11; 32],
            league: league_pda(league_id),
        },
    );

    assert_eq!(instruction.accounts.len(), 4);
    assert!(!instruction.accounts[0].is_writable);
    assert!(instruction.accounts[1].is_signer && instruction.accounts[1].is_writable);
    assert!(instruction.accounts[2].is_writable);
    assert_eq!(
        instruction.accounts[2].pubkey,
        Address::from(league_pda(league_id))
    );
    assert!(!instruction.data.is_empty());
}

#[test]
fn league_membership_builders_bind_the_wallet_to_the_canonical_member_pda() {
    let accounts = LeagueMembershipAccounts {
        config: [10; 32],
        player: [11; 32],
        league: [12; 32],
        member: league_member_pda([12; 32], [11; 32]),
    };
    let join = build_join_league_instruction(accounts);
    let leave = build_leave_league_instruction(accounts);

    assert_eq!(join.accounts.len(), 5);
    assert!(!join.accounts[0].is_writable);
    assert!(join.accounts[1].is_signer && join.accounts[1].is_writable);
    assert!(join.accounts[2].is_writable && join.accounts[3].is_writable);
    assert_eq!(join.accounts[3].pubkey, Address::from(accounts.member));
    assert_eq!(leave.accounts.len(), 4);
    assert!(leave.accounts[1].is_signer && leave.accounts[1].is_writable);
    assert_eq!(leave.accounts[3].pubkey, Address::from(accounts.member));
}

#[test]
fn league_battle_builder_binds_both_members_and_preserves_readonly_identity_accounts() {
    let league = [12; 32];
    let player_a = [13; 32];
    let player_b = [14; 32];
    let instruction = build_create_league_rated_battle_instruction(
        9,
        2,
        1500,
        1510,
        1,
        LeagueRatedBattleAccounts {
            config: [10; 32],
            coordinator: [11; 32],
            market_round: [15; 32],
            player_a,
            player_b,
            league,
            league_member_a: league_member_pda(league, player_a),
            league_member_b: league_member_pda(league, player_b),
            battle: [16; 32],
            rated_slot_a: [17; 32],
            rated_slot_b: [18; 32],
        },
    );

    assert_eq!(instruction.accounts.len(), 12);
    assert!(instruction.accounts[1].is_signer && instruction.accounts[1].is_writable);
    assert!(instruction.accounts[2].is_writable);
    assert!(!instruction.accounts[3].is_writable);
    assert!(!instruction.accounts[4].is_writable);
    assert!(!instruction.accounts[5].is_writable);
    assert!(!instruction.accounts[6].is_writable);
    assert!(!instruction.accounts[7].is_writable);
    assert_eq!(instruction.accounts[5].pubkey, Address::from(league));
    assert_eq!(
        instruction.accounts[6].pubkey,
        Address::from(league_member_pda(league, player_a))
    );
    assert_eq!(
        instruction.accounts[7].pubkey,
        Address::from(league_member_pda(league, player_b))
    );
    assert!(!instruction.data.is_empty());
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
    assert_eq!(plan.submission.accounts.len(), 11);
    assert_eq!(
        plan.submission.accounts[7].pubkey,
        Address::from(plan.price_attestation)
    );
    assert!(plan.submission.accounts[8].is_signer);
    assert_eq!(
        plan.submission.accounts[9].pubkey,
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
            jupiter_source_config: [14; 32],
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
            jupiter_source_config: [14; 32],
            market_quality_policy: [11; 32],
            attestor_set: [12; 32],
            finalizer: [13; 32],
        },
    )
    .unwrap();

    assert_eq!(instruction.accounts.len(), 9);
    assert_eq!(instruction.accounts[0].pubkey, Address::from([3; 32]));
    assert_eq!(instruction.accounts, reverse.accounts);
    assert!(build_finalize_price_phase_instruction(
        0,
        &[first, report(1, 10, 100_010)],
        FinalizeAccounts {
            price_policy: [10; 32],
            jupiter_source_config: [14; 32],
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
                jupiter_source_config: [14; 32],
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
        [14; 32],
        [12; 32],
        [13; 32],
        [[9; 32], [10; 32], [11; 32]],
    )
    .unwrap();

    assert_eq!(instruction.accounts.len(), 9);
    assert!(instruction.accounts[0].is_writable);
    assert!(instruction.accounts[5].is_signer);
    assert!(build_mark_price_phase_unavailable_instruction(
        2,
        [3; 32],
        [2; 32],
        [10; 32],
        [14; 32],
        [12; 32],
        [13; 32],
        [[9; 32], [10; 32], [11; 32]],
    )
    .is_err());
}

#[test]
fn battle_builders_preserve_mutability_and_reject_malformed_asset_sets() {
    let round_assets = [[1; 32], [2; 32], [3; 32], [4; 32], [5; 32], [6; 32]];
    let settle =
        build_settle_side_score_instruction(0, [20; 32], [21; 32], [22; 32], &round_assets)
            .unwrap();
    assert_eq!(settle.accounts.len(), 9);
    assert!(settle.accounts[0].is_writable);
    assert!(settle.accounts[2].is_signer);

    let finalize = build_finalize_battle_instruction([20; 32], [21; 32], [22; 32]);
    assert!(finalize.accounts[0].is_writable && finalize.accounts[1].is_writable);
    let forfeit = build_finalize_forfeit_instruction([20; 32], [21; 32], [22; 32]);
    assert!(forfeit.accounts[0].is_writable && forfeit.accounts[2].is_writable);
    let void =
        build_void_battle_price_unavailable_instruction([20; 32], [21; 32], [22; 32], &[[1; 32]])
            .unwrap();
    assert_eq!(void.accounts.len(), 4);
    let incident =
        build_void_battle_system_incident_instruction([23; 32], [20; 32], [21; 32], [22; 32]);
    assert!(incident.accounts[1].is_writable && incident.accounts[2].is_writable);
    let round = build_finalize_market_round_instruction(
        [21; 32],
        [24; 32],
        [26; 32],
        [25; 32],
        [22; 32],
        &round_assets,
    )
    .unwrap();
    assert_eq!(round.accounts.len(), 11);
    assert!(round.accounts[0].is_writable);

    assert!(matches!(
        build_settle_side_score_instruction(2, [20; 32], [21; 32], [22; 32], &round_assets),
        Err(RelayError::InvalidSide)
    ));
    assert!(matches!(
        build_settle_side_score_instruction(0, [20; 32], [21; 32], [22; 32], &round_assets[..5]),
        Err(RelayError::InvalidRoundAssets)
    ));
}
