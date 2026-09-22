#![cfg(feature = "pyth-pro")]

use anchor_lang::InstructionData;
use relay::{
    build_finalize_pyth_market_round_instruction,
    build_mark_pyth_price_phase_unavailable_instruction, PythEvidenceAccounts,
    PythFinalizeAccounts, PythMarketRoundFinalizeAccounts, PythUnavailableAccounts,
};

#[test]
fn pyth_unavailable_builder_is_source_specific_and_payload_free() {
    let instruction = build_mark_pyth_price_phase_unavailable_instruction(
        PythUnavailableAccounts {
            round_asset: [3; 32],
            market_round: [2; 32],
            settlement_policy: [10; 32],
            pyth_source_config: [14; 32],
            marker: [13; 32],
        },
        tickersix::PricePhase::End,
    );

    assert_eq!(instruction.accounts.len(), 5);
    assert!(instruction.accounts[0].is_writable);
    assert!(instruction.accounts[4].is_signer);
    assert_eq!(
        instruction.data,
        tickersix::instruction::MarkPythPricePhaseUnavailable {
            phase: tickersix::PricePhase::End,
        }
        .data()
    );
}

#[test]
fn pyth_finalize_builder_binds_the_evidence_pda_and_phase() {
    let instruction = relay::build_finalize_pyth_price_phase_instruction(
        PythFinalizeAccounts {
            round_asset: [3; 32],
            market_round: [2; 32],
            settlement_policy: [10; 32],
            pyth_source_config: [14; 32],
            pyth_price_evidence: relay::pyth_price_evidence_address(
                [3; 32],
                tickersix::PricePhase::Start,
            ),
            finalizer: [13; 32],
        },
        tickersix::PricePhase::Start,
    );

    assert_eq!(instruction.accounts.len(), 6);
    assert_eq!(
        instruction.accounts[4].pubkey.to_bytes(),
        relay::pyth_price_evidence_address([3; 32], tickersix::PricePhase::Start)
    );
}

#[test]
fn pyth_evidence_builder_rejects_a_zero_payload_hash() {
    let result = relay::build_submit_or_record_pyth_evidence_instruction(
        PythEvidenceAccounts {
            round_asset: [3; 32],
            market_round: [2; 32],
            settlement_policy: [10; 32],
            pyth_source_config: [14; 32],
            relayer: [13; 32],
        },
        tickersix::PricePhase::Start,
        42,
        1_000_000,
        1_000_000,
        100,
        1,
        -9,
        100_000_000_000,
        [0; 32],
    );

    assert!(matches!(result, Err(relay::RelayError::InvalidReport)));
}

#[test]
fn pyth_round_finalizer_appends_the_complete_round_asset_set() {
    let round_assets = [[1; 32], [2; 32], [3; 32], [4; 32], [5; 32], [6; 32]];
    let instruction = build_finalize_pyth_market_round_instruction(
        PythMarketRoundFinalizeAccounts {
            market_round: [20; 32],
            settlement_policy: [21; 32],
            pyth_source_config: [22; 32],
            market_quality_policy: [23; 32],
            keeper: [24; 32],
        },
        &round_assets,
    );

    assert_eq!(instruction.accounts.len(), 11);
    assert!(instruction.accounts[0].is_writable);
    assert!(instruction.accounts[4].is_signer);
    assert_eq!(instruction.accounts[10].pubkey.to_bytes(), [6; 32]);
}
