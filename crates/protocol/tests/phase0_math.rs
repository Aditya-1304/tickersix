use protocol::{
    commitment, evidence_root, lineup_score_q9, parse_decimal_q9, price_sample_leaf, return_q9,
    select_compatible_quorum, AttestorReport, MathError, PriceSample,
};

#[test]
fn decimal_prices_are_parsed_exactly_and_truncated_toward_zero() {
    // This catches the real settlement bug where binary floating-point conversion
    // changes the ninth decimal place and produces different cross-language scores.
    assert_eq!(parse_decimal_q9("1.2345678919").unwrap(), 1_234_567_891);
    assert_eq!(parse_decimal_q9("1.2e1").unwrap(), 12_000_000_000);
    assert_eq!(parse_decimal_q9("0.0000000019").unwrap(), 1);
}

#[test]
fn non_positive_prices_are_rejected_before_settlement() {
    assert!(matches!(
        parse_decimal_q9("0"),
        Err(MathError::NonPositivePrice)
    ));
    assert!(matches!(
        parse_decimal_q9("-1.0"),
        Err(MathError::NonPositivePrice)
    ));
}

#[test]
fn return_and_captain_score_use_the_canonical_q9_formula() {
    assert_eq!(
        return_q9(100_000_000_000, 102_000_000_000).unwrap(),
        20_000_000
    );
    assert_eq!(
        return_q9(100_000_000_000, 99_000_000_000).unwrap(),
        -10_000_000
    );

    let asset_ids = [1, 3, 4, 8, 9, 12];
    let returns = [10, 20, 30, 40, 50, 60];
    assert_eq!(lineup_score_q9(returns, asset_ids, 3).unwrap(), 32);
}

#[test]
fn commitment_is_independent_of_input_order_but_bound_to_battle_and_player() {
    let program_id = [1u8; 32];
    let battle = [2u8; 32];
    let player = [3u8; 32];
    let salt = [4u8; 32];

    let first = commitment(program_id, battle, player, 1, [12, 1, 9, 4, 8, 3], 9, salt);
    let reordered = commitment(program_id, battle, player, 1, [1, 3, 4, 8, 9, 12], 9, salt);
    let other_battle = commitment(
        program_id,
        [5u8; 32],
        player,
        1,
        [1, 3, 4, 8, 9, 12],
        9,
        salt,
    );

    assert_eq!(first, reordered);
    assert_ne!(first, other_battle);
}

#[test]
fn quorum_selection_ignores_one_outlier_and_uses_checked_midpoint() {
    let reports = [
        AttestorReport {
            attestor: [1u8; 32],
            median_price_q9: 100,
        },
        AttestorReport {
            attestor: [2u8; 32],
            median_price_q9: 102,
        },
        AttestorReport {
            attestor: [3u8; 32],
            median_price_q9: 1_000,
        },
    ];

    let selection = select_compatible_quorum(&reports, 200).unwrap();

    assert_eq!(selection.selected_attestors, vec![[1u8; 32], [2u8; 32]]);
    assert_eq!(selection.finalized_price_q9, 101);
    assert_eq!(selection.spread_bps, 198);
}

#[test]
fn three_compatible_reports_finalize_at_the_integer_median() {
    let reports = [
        AttestorReport {
            attestor: [3u8; 32],
            median_price_q9: 103,
        },
        AttestorReport {
            attestor: [1u8; 32],
            median_price_q9: 100,
        },
        AttestorReport {
            attestor: [2u8; 32],
            median_price_q9: 102,
        },
    ];

    let selection = select_compatible_quorum(&reports, 300).unwrap();

    assert_eq!(
        selection.selected_attestors,
        vec![[1u8; 32], [2u8; 32], [3u8; 32]]
    );
    assert_eq!(selection.finalized_price_q9, 102);
}

#[test]
fn evidence_root_is_order_independent_but_bound_to_every_sample_field() {
    let first = PriceSample {
        market_round: [1; 32],
        asset_id: 7,
        phase: 0,
        scoring_mint: [2; 32],
        source_block_id: 100,
        observed_at_unix_ms: 1_700_000_000_000,
        price_q9: 123,
    };
    let second = PriceSample {
        source_block_id: 101,
        observed_at_unix_ms: 1_700_000_005_000,
        price_q9: 124,
        ..first
    };

    assert_eq!(
        evidence_root(&[first, second]),
        evidence_root(&[second, first])
    );
    assert_ne!(
        price_sample_leaf(&first),
        price_sample_leaf(&PriceSample {
            price_q9: 125,
            ..first
        })
    );
}
