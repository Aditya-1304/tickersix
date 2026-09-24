use market_data::{
    evaluate_xstocks_baseline, evaluate_xstocks_baseline_with_verified_token_program,
    validate_jupiter_baseline, BaselineError, JupiterBaselineConfig, XStocksBaselineError,
};

const VALID_ASSET: &str = r#"{
    "id": "SPYx",
    "name": "SPDR S&P 500 ETF Token",
    "symbol": "SPYx",
    "underlying": {"symbol": "SPY", "isin": "US78462F1030"},
    "isTradingHalted": false,
    "deployments": [{
        "address": "So11111111111111111111111111111111111111112",
        "network": "Solana",
        "solanaTokenProgram": "Token2022Program"
    }]
}"#;

const LIVE_SHAPED_ASSET_WITHOUT_PROGRAM_FIELD: &str = r#"{
    "id": "365fea03-d6ef-41fc-8a43-08d014e5e4c7",
    "name": "SP500 xStock",
    "symbol": "SPYx",
    "underlyingSymbol": "SPY",
    "underlying": {"symbol": "SPY"},
    "isTradingHalted": false,
    "deployments": [{
        "address": "XsoCS1TfEyfFhfvj8EtZ528L3CaKBDBRqRapnBbDF2W",
        "network": "Solana"
    }]
}"#;

const VALID_PRICE: &str = r#"{"quote": 512.1234567891}"#;
const VALID_MULTIPLIER: &str = r#"{
    "currentMultiplier": 1.0,
    "newMultiplier": null,
    "activationDateTime": null,
    "reason": null
}"#;
const LIVE_MULTIPLIER_WITH_NO_PENDING_CHANGE: &str = r#"{
    "currentMultiplier": 1.005714560286254,
    "newMultiplier": 0,
    "activationDateTime": 0,
    "reason": null
}"#;
const VALID_CORPORATE_ACTIONS: &str = r#"{"page": {"currentPage": 1}, "nodes": []}"#;

#[test]
fn jupiter_baseline_rejects_a_plan_that_exceeds_the_free_tier() {
    let error = validate_jupiter_baseline(JupiterBaselineConfig {
        eligible_asset_count: 51,
        attestor_count: 3,
        sample_interval_secs: 5,
        observation_window_secs: 60,
        provider_limit_milli_rps: 1_000,
    })
    .unwrap_err();

    assert!(matches!(
        error,
        BaselineError::ProviderRateLimitExceeded { .. }
    ));
}

#[test]
fn jupiter_baseline_rejects_a_public_round_with_fewer_than_ten_assets() {
    let error = validate_jupiter_baseline(JupiterBaselineConfig {
        eligible_asset_count: 9,
        attestor_count: 3,
        sample_interval_secs: 5,
        observation_window_secs: 60,
        provider_limit_milli_rps: 1_000,
    })
    .unwrap_err();

    assert_eq!(
        error,
        BaselineError::InsufficientEligibleAssets { actual: 9 }
    );
}

#[test]
fn xstocks_baseline_requires_the_exact_solana_token2022_deployment() {
    let asset = VALID_ASSET.replace("Token2022Program", "TokenProgram");

    let error = evaluate_xstocks_baseline(
        &asset,
        VALID_PRICE,
        VALID_MULTIPLIER,
        VALID_CORPORATE_ACTIONS,
        "SPYx",
    )
    .unwrap_err();

    assert_eq!(
        error,
        XStocksBaselineError::MissingSolanaToken2022Deployment
    );
}

#[test]
fn xstocks_baseline_accepts_live_shape_when_chain_verifies_token2022() {
    let snapshot = evaluate_xstocks_baseline_with_verified_token_program(
        LIVE_SHAPED_ASSET_WITHOUT_PROGRAM_FIELD,
        VALID_PRICE,
        VALID_MULTIPLIER,
        VALID_CORPORATE_ACTIONS,
        "SPYx",
        "Token2022Program",
    )
    .unwrap();

    assert_eq!(
        snapshot.solana_mint,
        "XsoCS1TfEyfFhfvj8EtZ528L3CaKBDBRqRapnBbDF2W"
    );
    assert_eq!(snapshot.solana_token_program, "Token2022Program");
}

#[test]
fn xstocks_baseline_treats_provider_zero_multiplier_sentinel_as_no_pending_change() {
    let snapshot = evaluate_xstocks_baseline_with_verified_token_program(
        LIVE_SHAPED_ASSET_WITHOUT_PROGRAM_FIELD,
        r#"{"quote": 774.1005}"#,
        LIVE_MULTIPLIER_WITH_NO_PENDING_CHANGE,
        VALID_CORPORATE_ACTIONS,
        "SPYx",
        "Token2022Program",
    )
    .unwrap();

    assert_eq!(snapshot.pending_multiplier_q9, None);
    assert_eq!(snapshot.pending_multiplier_activation, None);
}

#[test]
fn xstocks_baseline_rejects_missing_or_non_positive_price_data() {
    for body in [r#"{"quote": null}"#, r#"{"quote": 0}"#] {
        let error = evaluate_xstocks_baseline(
            VALID_ASSET,
            body,
            VALID_MULTIPLIER,
            VALID_CORPORATE_ACTIONS,
            "SPYx",
        )
        .unwrap_err();

        assert_eq!(error, XStocksBaselineError::InvalidPrice);
    }
}

#[test]
fn xstocks_baseline_preserves_exact_decimal_values_and_fixture_coverage() {
    let snapshot = evaluate_xstocks_baseline(
        VALID_ASSET,
        VALID_PRICE,
        VALID_MULTIPLIER,
        VALID_CORPORATE_ACTIONS,
        "SPYx",
    )
    .unwrap();

    assert_eq!(snapshot.symbol, "SPYx");
    assert_eq!(snapshot.underlying_symbol.as_deref(), Some("SPY"));
    assert_eq!(
        snapshot.solana_mint,
        "So11111111111111111111111111111111111111112"
    );
    assert_eq!(snapshot.price_q9, 512_123_456_789);
    assert_eq!(snapshot.current_multiplier_q9, 1_000_000_000);
    assert_eq!(snapshot.upcoming_corporate_action_count, 0);
}
