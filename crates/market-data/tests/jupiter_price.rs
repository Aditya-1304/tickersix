use market_data::{
    build_attestor_price_report, build_canonical_price_report, parse_jupiter_price_response,
    parse_jupiter_token_response, read_jsonl, stagger_offsets_millis, summarize_observation_window,
    validate_batched_sampling_plan, validate_sampling_plan, AcceptedPriceSample,
    AttestorEvidencePolicy, AttestorReportError, AttestorSigner, CanonicalPriceReport,
    EvidenceBatchRecord, EvidenceLedger, JsonlStore, JupiterParseError, MarketDataObservation,
    PriceReportContext, SamplingPlanError, MAX_PRICE_REQUEST_MINTS,
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
fn batched_sampling_rate_counts_provider_requests_for_large_universes() {
    // Regression target: an eligible universe larger than Jupiter's request
    // limit must be chunked and rate-limited by chunk count, not by asset count
    // or by the mistaken assumption that one tick is always one request.
    assert!(validate_batched_sampling_plan(3, 2, 5, 1_200).is_ok());
    assert!(matches!(
        validate_batched_sampling_plan(3, 2, 5, 1_000),
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
fn observation_summary_separates_stale_polls_from_independent_source_blocks() {
    let requested = vec!["mint_a".to_owned(), "mint_b".to_owned()];
    let observations = vec![
        MarketDataObservation::new("attestor-a", "mint_a", 100, 1, 0),
        MarketDataObservation::new("attestor-a", "mint_a", 101, 1, 1_000),
        MarketDataObservation::new("attestor-a", "mint_a", 102, 2, 2_000),
        MarketDataObservation::new("attestor-b", "mint_a", 103, 1, 3_000),
        MarketDataObservation::new("attestor-a", "mint_a", 104, 3, 60_000),
    ];

    let summary = summarize_observation_window(&requested, &observations, 0, 60).unwrap();
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

#[test]
fn canonical_report_rejects_stale_samples_and_keeps_only_fresh_evidence() {
    // Regression target: a repeated old Jupiter block must not enter the
    // signed report merely because it was polled again during the window.
    let context = PriceReportContext {
        program_id: [1; 32],
        market_round: [2; 32],
        round_asset: [3; 32],
        asset_id: 7,
        phase: 0,
        scoring_mint: [4; 32],
        price_policy_version: 11,
        market_quality_policy_version: 12,
        attestor_set_version: 13,
        observation_window_start: 1_000,
        observation_window_end: 61_000,
        report_created_at: 60_000,
    };
    let samples = [
        AcceptedPriceSample::new(10, 10_000_000, 100),
        AcceptedPriceSample::new(11, 20_000_000, 101),
        AcceptedPriceSample::new(12, 30_000_000, 103),
        AcceptedPriceSample::new(12, 35_000_000, 104),
    ];
    let policy = AttestorEvidencePolicy {
        min_accepted_observations: 3,
        min_unique_source_blocks: 2,
        max_source_block_lag: 1,
    };

    let report = build_canonical_price_report(context, &samples, policy, 12).unwrap();

    assert_eq!(report.median_price_q9, 103);
    assert_eq!(report.accepted_observation_count, 3);
    assert_eq!(report.unique_source_block_count, 2);
    assert_eq!(report.first_source_block_id, 11);
    assert_eq!(report.last_source_block_id, 12);
}

#[test]
fn canonical_report_rejects_zero_evidence_thresholds() {
    // Regression target: an invalid zero-threshold policy must fail closed
    // instead of allowing an empty report or panicking while reading bounds.
    let context = PriceReportContext {
        program_id: [1; 32],
        market_round: [2; 32],
        round_asset: [3; 32],
        asset_id: 7,
        phase: 0,
        scoring_mint: [4; 32],
        price_policy_version: 11,
        market_quality_policy_version: 12,
        attestor_set_version: 13,
        observation_window_start: 1,
        observation_window_end: 2,
        report_created_at: 1,
    };

    let error = build_canonical_price_report(
        context,
        &[],
        AttestorEvidencePolicy {
            min_accepted_observations: 0,
            min_unique_source_blocks: 0,
            max_source_block_lag: 1,
        },
        1,
    )
    .unwrap_err();

    assert_eq!(error, AttestorReportError::InvalidPolicy);
}

#[test]
fn signed_report_verifies_exact_canonical_bytes_and_rejects_mutation() {
    // Regression target: the relay must be able to verify the exact bytes that
    // the Solana program will reconstruct, and a changed price must invalidate
    // the signature rather than being treated as metadata-only mutation.
    let report = CanonicalPriceReport {
        context: PriceReportContext {
            program_id: [1; 32],
            market_round: [2; 32],
            round_asset: [3; 32],
            asset_id: 7,
            phase: 1,
            scoring_mint: [4; 32],
            price_policy_version: 11,
            market_quality_policy_version: 12,
            attestor_set_version: 13,
            observation_window_start: 1_000,
            observation_window_end: 61_000,
            report_created_at: 60_000,
        },
        median_price_q9: 103,
        accepted_observation_count: 3,
        unique_source_block_count: 2,
        first_source_block_id: 11,
        last_source_block_id: 12,
        evidence_root: [5; 32],
    };
    let signer = AttestorSigner::from_secret_key([9; 32]);
    let signed = signer.sign(report.clone());

    assert_eq!(signed.attestor, signer.public_key_bytes());
    signed.verify().unwrap();

    let mut mutated = signed.clone();
    mutated.report.median_price_q9 += 1;
    assert!(mutated.verify().is_err());
}

#[test]
fn evidence_and_report_logs_round_trip_and_append_durably() {
    // Regression target: a worker restart must preserve prior evidence and
    // signed reports instead of silently replacing the audit trail.
    let path = std::env::temp_dir().join(format!(
        "tickersix-market-data-test-{}-{}.ndjson",
        std::process::id(),
        1
    ));
    let _ = std::fs::remove_file(&path);

    let record = EvidenceBatchRecord {
        schema_version: 1,
        market_round: [2; 32],
        phase: 0,
        attestor: [9; 32],
        requested_mints: vec!["MintA".to_owned()],
        request_started_at_unix_ms: 1_000,
        request_completed_at_unix_ms: 1_250,
        http_status: Some(200),
        observations: Vec::new(),
        missing_mints: vec!["MintA".to_owned()],
        raw_response: Some("{}".to_owned()),
        error: None,
    };

    {
        let mut store = JsonlStore::open_append(&path).unwrap();
        store.append(&record).unwrap();
    }

    let records: Vec<EvidenceBatchRecord> = read_jsonl(&path).unwrap();
    assert_eq!(records, vec![record]);
    let _ = std::fs::remove_file(&path);
}
