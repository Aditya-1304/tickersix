use market_data::{
    build_attestor_price_report, parse_jupiter_price_response, parse_jupiter_token_response,
    stagger_offsets_millis, summarize_phase0_window, validate_sampling_plan, AcceptedPriceSample,
    EvidenceLedger, JupiterParseError, Phase0Observation, SamplingPlanError,
    MAX_PRICE_REQUEST_MINTS,
};

#[test]
fn price_v3_response_keeps_decimal_lexeme_for_exact_q9_parsing() {
    let requested = vec!["MintA".to_owned(), "MintB".to_owned()];
    let body = r#"{
        "MintA": {
            "usdPrice": 1.2345678919,
            "blockId": 348004026,
            "decimals": 6,
            "priceChange24h": 0.529
        }
    }"#;

    let batch = parse_jupiter_price_response(&requested, body, 1_700_000_000_000).unwrap();

    assert_eq!(batch.observations.len(), 1);
    assert_eq!(batch.observations[0].mint, "MintA");
    assert_eq!(batch.observations[0].price_q9, 1_234_567_891);
    assert_eq!(batch.observations[0].source_block_id, 348004026);
    assert_eq!(batch.missing_mints, vec!["MintB"]);
}

#[test]
fn malformed_or_non_positive_jupiter_prices_fail_closed() {
    let requested = vec!["MintA".to_owned()];
    let body = r#"{
        "MintA": {
            "usdPrice": 0,
            "blockId": 1,
            "decimals": 6
        }
    }"#;

    let error = parse_jupiter_price_response(&requested, body, 1).unwrap_err();
    assert!(matches!(error, JupiterParseError::Price(_)));
}

#[test]
fn evidence_counts_polls_and_unique_source_blocks_separately() {
    let mut ledger = EvidenceLedger::default();

    ledger.record("MintA", 100, 10);
    ledger.record("MintA", 100, 11);
    ledger.record("MintA", 101, 12);

    assert_eq!(ledger.accepted_observation_count(), 3);
    assert_eq!(ledger.unique_source_block_count("MintA"), 2);
    assert!(!ledger.has_enough_unique_source_blocks("MintA", 3));
    assert!(ledger.has_enough_unique_source_blocks("MintA", 2));
}

#[test]
fn price_batch_request_limit_matches_current_jupiter_contract() {
    assert_eq!(MAX_PRICE_REQUEST_MINTS, 50);
}

#[test]
fn attestor_sampling_is_staggered_and_fits_the_one_rps_free_plan() {
    let offsets = stagger_offsets_millis(3, 5).unwrap();

    assert_eq!(offsets, vec![0, 1_666, 3_333]);
    assert!(validate_sampling_plan(3, 5, 1_000).is_ok());
    assert!(matches!(
        validate_sampling_plan(3, 2, 1_000),
        Err(SamplingPlanError::ProviderRateLimitExceeded { .. })
    ));
}

#[test]
fn token_information_keeps_exact_mint_identity_and_reports_metadata() {
    let requested = vec!["mint_a".to_owned(), "mint_b".to_owned()];
    let body = r#"[
        {"id":"mint_a","name":"Alpha","symbol":"ALP","decimals":9,"isVerified":true,"tags":["stocks"],"liquidity":123.5,"priceBlockId":42},
        {"id":"unexpected","name":"Other","symbol":"OTH","decimals":6}
    ]"#;

    let result = parse_jupiter_token_response(&requested, body).unwrap();
    assert_eq!(result.tokens.len(), 1);
    assert_eq!(result.tokens[0].mint, "mint_a");
    assert_eq!(result.tokens[0].symbol, "ALP");
    assert!(result.tokens[0].is_verified);
    assert_eq!(result.tokens[0].price_block_id, Some(42));
    assert_eq!(result.missing_mints, vec!["mint_b"]);
    assert_eq!(result.raw_response, body);
}

#[test]
fn phase0_summary_separates_stale_polls_from_independent_source_blocks() {
    let requested = vec!["mint_a".to_owned(), "mint_b".to_owned()];
    let observations = vec![
        Phase0Observation::new("attestor-a", "mint_a", 100, 1, 0),
        Phase0Observation::new("attestor-a", "mint_a", 101, 1, 1_000),
        Phase0Observation::new("attestor-a", "mint_a", 102, 2, 2_000),
        Phase0Observation::new("attestor-b", "mint_a", 103, 1, 3_000),
        Phase0Observation::new("attestor-a", "mint_a", 104, 3, 60_000),
    ];

    let summary = summarize_phase0_window(&requested, &observations, 0, 60).unwrap();
    assert_eq!(summary.total_observations, 4);
    assert_eq!(summary.assets[0].unique_source_block_count, 3);
    assert_eq!(summary.assets[0].stale_observation_count, 1);
    assert_eq!(summary.assets[0].attestor_count, 2);
    assert_eq!(summary.assets[0].price_range_bps, Some(300));
    assert!(summary.assets[1].missing);
}

#[test]
fn attestor_report_uses_exact_median_and_unique_block_evidence() {
    let samples = vec![
        AcceptedPriceSample::new(100, 1_700_000_000_000, 100),
        AcceptedPriceSample::new(100, 1_700_000_005_000, 101),
        AcceptedPriceSample::new(101, 1_700_000_010_000, 102),
    ];

    let report = build_attestor_price_report([1; 32], 7, 0, [2; 32], &samples).unwrap();
    assert_eq!(report.median_price_q9, 101);
    assert_eq!(report.accepted_observation_count, 3);
    assert_eq!(report.unique_source_block_count, 2);
    assert_eq!(report.first_source_block_id, 100);
    assert_eq!(report.last_source_block_id, 101);
    assert_ne!(report.evidence_root, [0; 32]);
}
