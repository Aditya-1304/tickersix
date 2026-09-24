use market_data::{
    assess_private_market_catalogs, private_market_exhibition_ready, PrivateMarketActivation,
};

const PRESTOCKS_CATALOG: &str = r#"[
    {
        "name": "OpenAI PreStocks",
        "symbol": "OPENAI",
        "description": "OPENAI is a PreStocks issued token backed 1:1 by SPV exposure that tracks the price of the underlying private company.",
        "external_url": "https://prestocks.com/openai",
        "contract_address": "PreweJYECqtQwBtpxHL171nL2K6umo692gTm7Q3rpgF",
        "markPrice": 1001.45,
        "markValuation": 1240731710609
    },
    {
        "name": "Anduril PreStocks",
        "symbol": "ANDURIL",
        "description": "ANDURIL is a PreStocks issued token backed 1:1 by SPV exposure that tracks the price of the underlying private company.",
        "contract_address": "PreAndurilExampleContract111111111111111111",
        "markPrice": 10.0,
        "markValuation": 30000000000
    }
]"#;

const TESSERA_CATALOG: &str = r#"[
    {
        "id": "T-OpenAI",
        "name": "T-OpenAI",
        "symbol": "T-OpenAI",
        "mint": "oPAiAikWTaFj9RYoRFD35ccfwhnMcB3ThgBZRHSkjTZ",
        "markPrice": 812.79,
        "holders": 8259,
        "markValuation": 950000000000
    },
    {
        "id": "T-Kalshi",
        "name": "T-Kalshi",
        "symbol": "T-Kalshi",
        "mint": "tKalshiExampleMint111111111111111111111111",
        "markPrice": 1.0,
        "holders": 100,
        "markValuation": 10000000000
    }
]"#;

#[test]
fn provider_descriptors_preserve_structure_disclosure_and_lifecycle() {
    let assessment = assess_private_market_catalogs(PRESTOCKS_CATALOG, TESSERA_CATALOG, false)
        .expect("fixture catalogs should parse");

    let prestocks = &assessment.prestocks[0];
    assert_eq!(prestocks.structure_kind, "SpvEconomicExposure");
    assert_eq!(prestocks.lifecycle_status, "UNSPECIFIED");
    assert!(prestocks.provider_disclosure.contains("not ordinary"));
    assert_eq!(
        prestocks.source_url.as_deref(),
        Some("https://prestocks.com/openai")
    );

    let tessera = &assessment.tessera[0];
    assert_eq!(tessera.structure_kind, "LoanParticipationRight");
    assert_eq!(tessera.lifecycle_status, "UNSPECIFIED");
    assert!(tessera.provider_disclosure.contains("loan participation"));
    assert_eq!(assessment.activation, PrivateMarketActivation::MetadataOnly);
}

#[test]
fn private_exhibition_requires_quality_and_six_distinct_usable_references() {
    let mut assessment = assess_private_market_catalogs(PRESTOCKS_CATALOG, TESSERA_CATALOG, true)
        .expect("fixture catalogs should parse");
    assert!(!private_market_exhibition_ready(&assessment, true));

    for symbol in ["ANTHROPIC", "SPACEX", "POLYMARKET", "NEURALINK"] {
        let mut descriptor = assessment.prestocks[0].clone();
        descriptor.reference_symbol = symbol.to_owned();
        descriptor.representation_symbol = symbol.to_owned();
        assessment.prestocks.push(descriptor);
    }

    assert!(private_market_exhibition_ready(&assessment, true));
    assert!(!private_market_exhibition_ready(&assessment, false));
}
