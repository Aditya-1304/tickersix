//! Read-only Private Markets catalog and comparison boundary.
//!
//! This module adapts the already-validated PreStocks/Tessera descriptors into
//! HTTP-facing Reference Asset and Representation views. It intentionally has
//! no rating, settlement, wallet, or Battle mutation capability: provider
//! metadata can disappear without changing Public Ranked behavior.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fmt,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use market_data::{
    private_market_exhibition_ready, PrivateMarketActivation, PrivateMarketCatalogAssessment,
    PrivateRepresentationDescriptor, SponsorClient, DEFAULT_PRESTOCKS_URL, DEFAULT_TESSERA_URL,
};
use serde::Serialize;

pub const PRIVATE_MARKET_DOMAIN: &str = "PRIVATE_MARKET";
pub const COMPARISON_UNAVAILABLE: &str = "COMPARISON_UNAVAILABLE";
pub const MIN_PRIVATE_EXHIBITION_ASSETS: usize = 6;

#[derive(Debug, Clone, Serialize)]
pub struct PrivateMarketCatalogResponse {
    pub competition_domain: &'static str,
    pub activation: PrivateMarketActivation,
    pub captured_at_unix: i64,
    pub exhibition: PrivateExhibitionReadiness,
    pub assets: Vec<PrivateMarketAsset>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivateMarketAsset {
    pub id: String,
    pub reference_symbol: String,
    pub display_name: String,
    pub providers: Vec<String>,
    pub representation_count: usize,
    pub competition_domain: &'static str,
    pub exhibition_eligible: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivateMarketAssetDetail {
    pub asset: PrivateMarketAsset,
    pub captured_at_unix: i64,
    pub exhibition: PrivateExhibitionReadiness,
    pub representations: Vec<PrivateMarketRepresentation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivateMarketRepresentationsResponse {
    pub asset_id: String,
    pub reference_symbol: String,
    pub competition_domain: &'static str,
    pub captured_at_unix: i64,
    pub exhibition: PrivateExhibitionReadiness,
    pub representations: Vec<PrivateMarketRepresentation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivateMarketRepresentation {
    pub id: String,
    pub provider: String,
    pub reference_asset_id: String,
    pub reference_symbol: String,
    pub representation_symbol: String,
    pub display_name: String,
    pub structure_kind: String,
    pub provider_disclosure: String,
    pub lifecycle_status: String,
    pub source_url: Option<String>,
    pub mint_or_contract: String,
    pub mark_price_q9: i64,
    pub mark_valuation_q9: Option<String>,
    pub holder_count: Option<u64>,
    pub comparability: String,
    pub rated_settlement_eligible: bool,
    pub competition_domain: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivateMarketComparison {
    pub asset_id: String,
    pub reference_symbol: String,
    pub status: &'static str,
    pub reason: &'static str,
    pub numeric_basis_bps: Option<i64>,
    pub competition_domain: &'static str,
    pub representations: Vec<PrivateMarketRepresentation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivateExhibitionReadiness {
    pub eligible: bool,
    pub status: &'static str,
    pub reason: &'static str,
    pub usable_reference_asset_count: usize,
    pub minimum_reference_asset_count: usize,
    pub competition_domain: &'static str,
    pub rated: bool,
}

#[derive(Debug)]
pub enum PrivateMarketsError {
    Unavailable(String),
    InvalidAssetId,
    NotFound,
}

impl fmt::Display for PrivateMarketsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable(_) => "Private Markets provider data is unavailable",
            Self::InvalidAssetId => "Private Markets asset ID is invalid",
            Self::NotFound => "Private Markets asset was not found",
        })
    }
}

impl std::error::Error for PrivateMarketsError {}

#[derive(Clone)]
pub struct PrivateMarketsService {
    client: Option<Arc<SponsorClient>>,
    quality_measured: bool,
    initialization_error: Option<String>,
}

impl PrivateMarketsService {
    pub fn from_env() -> Self {
        let prestocks_url = env::var("TICKERSIX_PRESTOCKS_URL")
            .unwrap_or_else(|_| DEFAULT_PRESTOCKS_URL.to_owned());
        let tessera_url =
            env::var("TICKERSIX_TESSERA_URL").unwrap_or_else(|_| DEFAULT_TESSERA_URL.to_owned());
        let quality_measured = env::var("TICKERSIX_PRIVATE_MARKET_QUALITY_MEASURED")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        match SponsorClient::with_urls(DEFAULT_PRESTOCKS_URL, prestocks_url, tessera_url, None) {
            Ok(client) => Self {
                client: Some(Arc::new(client)),
                quality_measured,
                initialization_error: None,
            },
            Err(error) => Self {
                client: None,
                quality_measured,
                initialization_error: Some(error.to_string()),
            },
        }
    }

    pub async fn catalog(&self) -> Result<PrivateMarketCatalogResponse, PrivateMarketsError> {
        let assessment = self.load_assessment().await?;
        Ok(build_snapshot(
            &assessment,
            self.quality_measured,
            unix_now(),
        ))
    }

    pub async fn asset(
        &self,
        asset_id: &str,
    ) -> Result<PrivateMarketAssetDetail, PrivateMarketsError> {
        let assessment = self.load_assessment().await?;
        let snapshot = build_snapshot(&assessment, self.quality_measured, unix_now());
        let asset = snapshot
            .assets
            .iter()
            .find(|asset| asset.id == asset_id)
            .cloned()
            .ok_or(PrivateMarketsError::NotFound)?;
        let representations = representations_for_asset(&assessment, asset_id);
        Ok(PrivateMarketAssetDetail {
            asset,
            captured_at_unix: snapshot.captured_at_unix,
            exhibition: snapshot.exhibition,
            representations,
        })
    }

    pub async fn representations(
        &self,
        asset_id: &str,
    ) -> Result<PrivateMarketRepresentationsResponse, PrivateMarketsError> {
        let detail = self.asset(asset_id).await?;
        Ok(PrivateMarketRepresentationsResponse {
            asset_id: detail.asset.id.clone(),
            reference_symbol: detail.asset.reference_symbol.clone(),
            competition_domain: PRIVATE_MARKET_DOMAIN,
            captured_at_unix: detail.captured_at_unix,
            exhibition: detail.exhibition,
            representations: detail.representations,
        })
    }

    pub async fn comparison(
        &self,
        asset_id: &str,
    ) -> Result<PrivateMarketComparison, PrivateMarketsError> {
        let detail = self.asset(asset_id).await?;
        Ok(PrivateMarketComparison {
            asset_id: detail.asset.id,
            reference_symbol: detail.asset.reference_symbol,
            status: COMPARISON_UNAVAILABLE,
            reason: "PROVIDER_CLAIMS_NOT_CANONICALLY_COMPARABLE",
            numeric_basis_bps: None,
            competition_domain: PRIVATE_MARKET_DOMAIN,
            representations: detail.representations,
        })
    }

    async fn load_assessment(&self) -> Result<PrivateMarketCatalogAssessment, PrivateMarketsError> {
        let client = self.client.as_ref().ok_or_else(|| {
            PrivateMarketsError::Unavailable(
                self.initialization_error
                    .clone()
                    .unwrap_or_else(|| "provider client initialization failed".to_owned()),
            )
        })?;
        client
            .fetch_private_catalogs(self.quality_measured)
            .await
            .map_err(|error| PrivateMarketsError::Unavailable(error.to_string()))
    }
}

pub fn build_snapshot(
    assessment: &PrivateMarketCatalogAssessment,
    quality_measured: bool,
    captured_at_unix: i64,
) -> PrivateMarketCatalogResponse {
    let exhibition = build_exhibition_readiness(assessment, quality_measured);
    let mut groups: BTreeMap<String, Vec<PrivateMarketRepresentation>> = BTreeMap::new();

    for descriptor in assessment.prestocks.iter().chain(assessment.tessera.iter()) {
        let reference_symbol = descriptor.reference_symbol.trim().to_ascii_uppercase();
        if reference_symbol.is_empty() {
            continue;
        }
        let asset_id = private_asset_id(&reference_symbol);
        groups
            .entry(asset_id.clone())
            .or_default()
            .push(to_representation(descriptor, &asset_id, &reference_symbol));
    }

    let assets = groups
        .iter()
        .map(|(asset_id, representations)| {
            let providers = representations
                .iter()
                .map(|representation| representation.provider.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let reference_symbol = representations
                .first()
                .map(|representation| representation.reference_symbol.clone())
                .unwrap_or_default();
            PrivateMarketAsset {
                id: asset_id.clone(),
                reference_symbol: reference_symbol.clone(),
                display_name: reference_symbol,
                providers,
                representation_count: representations.len(),
                competition_domain: PRIVATE_MARKET_DOMAIN,
                exhibition_eligible: exhibition.eligible,
            }
        })
        .collect();

    PrivateMarketCatalogResponse {
        competition_domain: PRIVATE_MARKET_DOMAIN,
        activation: assessment.activation,
        captured_at_unix,
        exhibition,
        assets,
    }
}

fn build_exhibition_readiness(
    assessment: &PrivateMarketCatalogAssessment,
    quality_measured: bool,
) -> PrivateExhibitionReadiness {
    let usable_reference_asset_count = usable_reference_asset_count(assessment);
    let eligible = private_market_exhibition_ready(assessment, quality_measured);
    let (status, reason) = if eligible {
        ("READY", "QUALITY_MEASURED_AND_SIX_USABLE_ASSETS")
    } else if !quality_measured {
        ("UNAVAILABLE", "QUALITY_NOT_MEASURED")
    } else {
        ("UNAVAILABLE", "FEWER_THAN_SIX_USABLE_ASSETS")
    };

    PrivateExhibitionReadiness {
        eligible,
        status,
        reason,
        usable_reference_asset_count,
        minimum_reference_asset_count: MIN_PRIVATE_EXHIBITION_ASSETS,
        competition_domain: PRIVATE_MARKET_DOMAIN,
        rated: false,
    }
}

fn usable_reference_asset_count(assessment: &PrivateMarketCatalogAssessment) -> usize {
    let mut references = BTreeSet::new();
    assessment
        .prestocks
        .iter()
        .chain(assessment.tessera.iter())
        .filter(|descriptor| {
            descriptor.mark_price_q9 > 0 && !descriptor.mint_or_contract.trim().is_empty()
        })
        .for_each(|descriptor| {
            references.insert(descriptor.reference_symbol.trim().to_ascii_uppercase());
        });
    references.len()
}

fn representations_for_asset(
    assessment: &PrivateMarketCatalogAssessment,
    asset_id: &str,
) -> Vec<PrivateMarketRepresentation> {
    let reference_symbol = asset_id
        .strip_prefix("private-market-")
        .unwrap_or_default()
        .to_ascii_uppercase();
    assessment
        .prestocks
        .iter()
        .chain(assessment.tessera.iter())
        .filter(|descriptor| {
            descriptor
                .reference_symbol
                .trim()
                .eq_ignore_ascii_case(&reference_symbol)
        })
        .map(|descriptor| to_representation(descriptor, asset_id, &reference_symbol))
        .collect()
}

fn to_representation(
    descriptor: &PrivateRepresentationDescriptor,
    asset_id: &str,
    reference_symbol: &str,
) -> PrivateMarketRepresentation {
    PrivateMarketRepresentation {
        id: format!(
            "private-market/{}/{}",
            descriptor.provider.to_ascii_lowercase(),
            descriptor.representation_symbol.to_ascii_lowercase()
        ),
        provider: descriptor.provider.clone(),
        reference_asset_id: asset_id.to_owned(),
        reference_symbol: reference_symbol.to_owned(),
        representation_symbol: descriptor.representation_symbol.clone(),
        display_name: descriptor.display_name.clone(),
        structure_kind: descriptor.structure_kind.clone(),
        provider_disclosure: descriptor.provider_disclosure.clone(),
        lifecycle_status: descriptor.lifecycle_status.clone(),
        source_url: descriptor.source_url.clone(),
        mint_or_contract: descriptor.mint_or_contract.clone(),
        mark_price_q9: descriptor.mark_price_q9,
        mark_valuation_q9: descriptor.mark_valuation_q9.clone(),
        holder_count: descriptor.holder_count,
        comparability: descriptor.comparability.clone(),
        rated_settlement_eligible: false,
        competition_domain: PRIVATE_MARKET_DOMAIN,
    }
}

fn private_asset_id(reference_symbol: &str) -> String {
    format!("private-market-{}", reference_symbol.to_ascii_lowercase())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use market_data::{PrivateMarketActivation, PrivateRepresentationDescriptor};

    fn sample_assessment() -> PrivateMarketCatalogAssessment {
        let descriptor = PrivateRepresentationDescriptor {
            provider: "PreStocks".to_owned(),
            reference_symbol: "OPENAI".to_owned(),
            representation_symbol: "OPENAI".to_owned(),
            display_name: "OpenAI PreStocks".to_owned(),
            lifecycle_status: "UNSPECIFIED".to_owned(),
            provider_disclosure:
                "Provider-described SPV economic exposure; not ordinary shareholder rights"
                    .to_owned(),
            source_url: None,
            structure_kind: "SpvEconomicExposure".to_owned(),
            mint_or_contract: "contract".to_owned(),
            mark_price_q9: 1_000_000_000,
            mark_valuation_q9: None,
            holder_count: None,
            comparability: "Unsupported".to_owned(),
            rated_settlement_eligible: false,
        };
        PrivateMarketCatalogAssessment {
            prestocks: vec![descriptor],
            tessera: Vec::new(),
            activation: PrivateMarketActivation::MetadataOnly,
        }
    }

    #[test]
    fn private_snapshot_is_private_and_never_rating_eligible() {
        let snapshot = build_snapshot(&sample_assessment(), false, 123);
        assert_eq!(snapshot.competition_domain, PRIVATE_MARKET_DOMAIN);
        assert!(!snapshot.exhibition.eligible);
        assert!(!snapshot.assets[0].exhibition_eligible);
    }

    #[test]
    fn comparison_fails_closed_without_numeric_basis() {
        let snapshot = build_snapshot(&sample_assessment(), false, 123);
        let asset_id = snapshot.assets[0].id.clone();
        let assessment = sample_assessment();
        let representations = representations_for_asset(&assessment, &asset_id);
        let comparison = PrivateMarketComparison {
            asset_id,
            reference_symbol: "OPENAI".to_owned(),
            status: COMPARISON_UNAVAILABLE,
            reason: "PROVIDER_CLAIMS_NOT_CANONICALLY_COMPARABLE",
            numeric_basis_bps: None,
            competition_domain: PRIVATE_MARKET_DOMAIN,
            representations,
        };
        assert_eq!(comparison.status, COMPARISON_UNAVAILABLE);
        assert_eq!(comparison.numeric_basis_bps, None);
    }
}
