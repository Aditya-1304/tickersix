use market_data::{
    decide_private_market_activation, decide_pyth_activation, is_excluded_phase0_sponsor_provider,
    parse_prestocks_catalog, parse_tessera_catalog, validate_pyth_payload, PrivateMarketActivation,
    PrivateRepresentationError, PythActivationDecision, PythActivationInputs, PythPayloadError,
    PythValidationPolicy,
};

const PYTH_PAYLOAD: &str = r#"{
    "type": "streamUpdated",
    "parsed": {
        "timestampUs": "1790071800000000",
        "priceFeeds": [{
            "priceFeedId": 1398,
            "price": "77410050",
            "exponent": -5,
            "confidence": "1000",
            "marketSession": "regular",
            "feedUpdateTimestamp": 1790071800000000
        }]
    },
    "solana": {
        "encoding": "hex",
        "data": "b9011a82d239c094c52016990d6ca2b261dbb1157ad503cbd3ea0679493316150cf3457624d19ec3f6e0a0e94373ab0971e39d939beda15cc02eb3c5454eb700f1f7310df65210bee4fcf5b1cee1e537fabcfd95010297653b94af04d454fc473e94834f2a0075d3c7938094b99e52260600030201000000010000b5ea6fea00000002000000010000c58f44d3010000"
    }
}"#;

const PRESTOCKS_CATALOG: &str = r#"[
    {
        "name": "OpenAI PreStocks",
        "symbol": "OPENAI",
        "description": "OPENAI is a PreStocks issued token backed 1:1 by SPV exposure that tracks the price of the underlying private company.",
        "external_url": "https://www.prestocks.com/openai",
        "contract_address": "PreweJYECqtQwBtpxHL171nL2K6umo692gTm7Q3rpgF",
        "markPrice": 1001.4540065446288,
        "markValuation": 1240731710609,
        "tokenPrice": 1152.9663546323225,
        "impliedValuation": 1428444949153,
        "supply": 2826.4156228188335
    }
]"#;

const TESSERA_CATALOG: &str = r#"[
    {
        "id": "T-OpenAI",
        "name": "T-OpenAI",
        "symbol": "T-OpenAI",
        "code": "tOpenAI",
        "sector": "Artificial Intelligence",
        "mint": "oPAiAikWTaFj9RYoRFD35ccfwhnMcB3ThgBZRHSkjTZ",
        "markPrice": 812.79,
        "holders": 8259,
        "markValuation": 950000000000
    }
]"#;

fn valid_pyth_policy() -> PythValidationPolicy {
    PythValidationPolicy {
        expected_feed_id: 1398,
        target_timestamp_us: 1_790_071_800_000_000,
        max_feed_age_us: 1_000_000,
        max_confidence_bps: 100,
        require_fresh_update: true,
        reject_closed_session: true,
    }
}

#[test]
fn pyth_preflight_normalizes_mantissa_exponent_and_binds_target() {
    let assessment = validate_pyth_payload(PYTH_PAYLOAD, valid_pyth_policy()).unwrap();

    assert_eq!(assessment.feed_id, 1398);
    assert_eq!(assessment.price_q9, 774_100_500_000);
    assert_eq!(assessment.feed_update_timestamp_us, 1_790_071_800_000_000);
    assert!(!assessment.carried_forward);
}

#[test]
fn pyth_preflight_rejects_a_carried_forward_price_when_freshness_is_required() {
    let payload = PYTH_PAYLOAD.replace(
        "\"feedUpdateTimestamp\": 1790071800000000",
        "\"feedUpdateTimestamp\": 1790071799000000",
    );

    let error = validate_pyth_payload(&payload, valid_pyth_policy()).unwrap_err();

    assert_eq!(error, PythPayloadError::CarriedForwardPrice);
}

#[test]
fn pyth_activation_does_not_enable_ranked_settlement_before_devnet_verification() {
    let inputs = PythActivationInputs {
        trial_token_active: true,
        required_feed_count: 10,
        available_feed_count: 10,
        payload_policy_passed: true,
        devnet_verification_passed: false,
        q9_vectors_passed: true,
        cost_evidence_recorded: true,
    };

    assert_eq!(
        decide_pyth_activation(inputs),
        PythActivationDecision::AnalyticsOnly
    );
}

#[test]
fn pyth_activation_requires_every_gate_before_enablement() {
    let inputs = PythActivationInputs {
        trial_token_active: true,
        required_feed_count: 10,
        available_feed_count: 10,
        payload_policy_passed: true,
        devnet_verification_passed: true,
        q9_vectors_passed: true,
        cost_evidence_recorded: true,
    };

    assert_eq!(
        decide_pyth_activation(inputs),
        PythActivationDecision::Enabled
    );
}

#[test]
fn prestocks_is_parsed_as_spv_economic_exposure_not_ordinary_equity() {
    let descriptors = parse_prestocks_catalog(PRESTOCKS_CATALOG).unwrap();
    let descriptor = &descriptors[0];

    assert_eq!(descriptor.reference_symbol, "OPENAI");
    assert_eq!(descriptor.structure_kind, "SpvEconomicExposure");
    assert_eq!(
        descriptor.mark_valuation_q9.as_deref(),
        Some("1240731710609000000000")
    );
    assert!(!descriptor.rated_settlement_eligible);
}

#[test]
fn tessera_is_parsed_as_a_loan_participation_representation() {
    let descriptors = parse_tessera_catalog(TESSERA_CATALOG).unwrap();
    let descriptor = &descriptors[0];

    assert_eq!(descriptor.representation_symbol, "T-OpenAI");
    assert_eq!(descriptor.structure_kind, "LoanParticipationRight");
    assert_eq!(descriptor.comparability, "Unsupported");
    assert_eq!(
        descriptor.mark_valuation_q9.as_deref(),
        Some("950000000000000000000")
    );
    assert!(!descriptor.rated_settlement_eligible);
}

#[test]
fn pyth_untrusted_exponent_is_rejected_without_integer_overflow_or_panic() {
    let payload = PYTH_PAYLOAD.replace("\"exponent\": -5", "\"exponent\": -32768");

    let error = validate_pyth_payload(&payload, valid_pyth_policy()).unwrap_err();

    assert_eq!(error, PythPayloadError::InvalidQ9Conversion);
}

#[test]
fn private_market_activation_stays_metadata_only_until_quality_is_measured() {
    assert_eq!(
        decide_private_market_activation(true, true, false),
        PrivateMarketActivation::MetadataOnly
    );
}

#[test]
fn private_market_parser_rejects_missing_provider_disclosure() {
    let invalid = PRESTOCKS_CATALOG.replace("SPV exposure", "ordinary shares");

    let error = parse_prestocks_catalog(&invalid).unwrap_err();

    assert_eq!(error, PrivateRepresentationError::MissingEconomicDisclosure);
}

#[test]
fn phase0_explicitly_excludes_clawpump_and_meteora_from_sponsor_discovery() {
    assert!(is_excluded_phase0_sponsor_provider("ClawPump"));
    assert!(is_excluded_phase0_sponsor_provider("meteora"));
    assert!(!is_excluded_phase0_sponsor_provider("PreStocks"));
}
