//! Read-only xStocks public contract checks for the baseline baseline.
//!
//! xStocks metadata, price, multiplier, and corporate-action responses are
//! provider inputs only. This module never promotes them into rated
//! settlement evidence. Its job is to prove that the public API shape and the
//! exact Solana Token-2022 deployment identity are usable before later
//! eligibility and representation work is attempted.

use std::{fmt, time::Duration};

use protocol::parse_decimal_q9;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

pub const DEFAULT_XSTOCKS_BASE_URL: &str = "https://api.xstocks.fi/api/v2";
pub const DEFAULT_SOLANA_RPC_URL: &str = "https://api.mainnet-beta.solana.com";
pub const SPL_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const SPL_TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct XStocksBaselineSnapshot {
    pub symbol: String,
    pub name: String,
    pub underlying_symbol: Option<String>,
    pub solana_mint: String,
    pub solana_token_program: String,
    pub is_trading_halted: bool,
    pub price_q9: i64,
    pub current_multiplier_q9: i64,
    pub pending_multiplier_q9: Option<i64>,
    pub pending_multiplier_activation: Option<String>,
    pub upcoming_corporate_action_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XStocksBaselineError {
    InvalidJson,
    SymbolMismatch,
    MissingUnderlying,
    MissingSolanaToken2022Deployment,
    AmbiguousSolanaToken2022Deployment,
    InvalidPrice,
    InvalidMultiplier,
    InvalidCorporateActions,
}

impl fmt::Display for XStocksBaselineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidJson => "xStocks response is not valid JSON",
            Self::SymbolMismatch => "xStocks response symbol does not match the requested asset",
            Self::MissingUnderlying => "xStocks asset is missing its underlying identity",
            Self::MissingSolanaToken2022Deployment => {
                "xStocks asset has no Solana Token-2022 deployment"
            }
            Self::AmbiguousSolanaToken2022Deployment => {
                "xStocks asset has multiple Solana Token-2022 deployments"
            }
            Self::InvalidPrice => "xStocks public price is missing or non-positive",
            Self::InvalidMultiplier => "xStocks multiplier is missing or non-positive",
            Self::InvalidCorporateActions => {
                "xStocks corporate-action response is missing its nodes array"
            }
        })
    }
}

impl std::error::Error for XStocksBaselineError {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetDocument {
    name: String,
    symbol: String,
    underlying: Option<UnderlyingDocument>,
    is_trading_halted: bool,
    deployments: Vec<DeploymentDocument>,
}

#[derive(Debug, Deserialize)]
struct UnderlyingDocument {
    symbol: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeploymentDocument {
    address: String,
    network: String,
    solana_token_program: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PriceDocument {
    quote: Option<Box<RawValue>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MultiplierDocument {
    current_multiplier: Option<Box<RawValue>>,
    new_multiplier: Option<Box<RawValue>>,
    activation_date_time: Option<Box<RawValue>>,
}

/// Validates the four public xStocks payloads needed by the permanent
/// baseline. The result is a review snapshot, not a settlement authority.
pub fn evaluate_xstocks_baseline(
    asset_body: &str,
    price_body: &str,
    multiplier_body: &str,
    corporate_actions_body: &str,
    expected_symbol: &str,
) -> Result<XStocksBaselineSnapshot, XStocksBaselineError> {
    evaluate_xstocks_baseline_internal(
        asset_body,
        price_body,
        multiplier_body,
        corporate_actions_body,
        expected_symbol,
        None,
    )
}

/// Evaluates the xStocks payloads with a token-program identity verified from
/// the Solana mint account owner. This is required when the provider metadata
/// omits the asset deployment's own `solanaTokenProgram` field.
pub fn evaluate_xstocks_baseline_with_verified_token_program(
    asset_body: &str,
    price_body: &str,
    multiplier_body: &str,
    corporate_actions_body: &str,
    expected_symbol: &str,
    verified_token_program: &str,
) -> Result<XStocksBaselineSnapshot, XStocksBaselineError> {
    evaluate_xstocks_baseline_internal(
        asset_body,
        price_body,
        multiplier_body,
        corporate_actions_body,
        expected_symbol,
        Some(verified_token_program),
    )
}

fn evaluate_xstocks_baseline_internal(
    asset_body: &str,
    price_body: &str,
    multiplier_body: &str,
    corporate_actions_body: &str,
    expected_symbol: &str,
    verified_token_program: Option<&str>,
) -> Result<XStocksBaselineSnapshot, XStocksBaselineError> {
    let asset: AssetDocument =
        serde_json::from_str(asset_body).map_err(|_| XStocksBaselineError::InvalidJson)?;
    if asset.symbol != expected_symbol {
        return Err(XStocksBaselineError::SymbolMismatch);
    }
    let underlying_symbol = asset
        .underlying
        .as_ref()
        .and_then(|underlying| underlying.symbol.as_deref())
        .filter(|symbol| !symbol.trim().is_empty())
        .map(str::to_owned)
        .ok_or(XStocksBaselineError::MissingUnderlying)?;

    let deployment = select_solana_deployment(&asset)?;
    let solana_mint = deployment.address.clone();
    let reported_token_program = deployment.solana_token_program.clone();
    let solana_token_program = match (reported_token_program.as_deref(), verified_token_program) {
        (Some(reported), Some(verified)) if reported != verified => {
            return Err(XStocksBaselineError::MissingSolanaToken2022Deployment)
        }
        (Some(reported), _) => reported.to_owned(),
        (None, Some(verified)) => verified.to_owned(),
        (None, None) => return Err(XStocksBaselineError::MissingSolanaToken2022Deployment),
    };
    if solana_token_program != "Token2022Program" {
        return Err(XStocksBaselineError::MissingSolanaToken2022Deployment);
    }

    let price: PriceDocument =
        serde_json::from_str(price_body).map_err(|_| XStocksBaselineError::InvalidJson)?;
    let price_q9 = price
        .quote
        .as_deref()
        .and_then(parse_raw_decimal_q9)
        .filter(|price| *price > 0)
        .ok_or(XStocksBaselineError::InvalidPrice)?;

    let multiplier: MultiplierDocument =
        serde_json::from_str(multiplier_body).map_err(|_| XStocksBaselineError::InvalidJson)?;
    let current_multiplier_q9 = multiplier
        .current_multiplier
        .as_deref()
        .and_then(parse_raw_decimal_q9)
        .filter(|multiplier| *multiplier > 0)
        .ok_or(XStocksBaselineError::InvalidMultiplier)?;
    let pending_multiplier_q9 = parse_pending_multiplier(multiplier.new_multiplier.as_deref())?;
    let pending_multiplier_activation =
        parse_optional_activation(multiplier.activation_date_time.as_deref())?;

    let corporate_actions: serde_json::Value = serde_json::from_str(corporate_actions_body)
        .map_err(|_| XStocksBaselineError::InvalidJson)?;
    let upcoming_corporate_action_count = corporate_actions
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .ok_or(XStocksBaselineError::InvalidCorporateActions)?;

    Ok(XStocksBaselineSnapshot {
        symbol: asset.symbol,
        name: asset.name,
        underlying_symbol: Some(underlying_symbol),
        solana_mint,
        solana_token_program,
        is_trading_halted: asset.is_trading_halted,
        price_q9,
        current_multiplier_q9,
        pending_multiplier_q9,
        pending_multiplier_activation,
        upcoming_corporate_action_count,
    })
}

fn select_solana_deployment(
    asset: &AssetDocument,
) -> Result<&DeploymentDocument, XStocksBaselineError> {
    let solana_deployments: Vec<&DeploymentDocument> = asset
        .deployments
        .iter()
        .filter(|deployment| {
            deployment.network == "Solana" && !deployment.address.trim().is_empty()
        })
        .collect();
    match solana_deployments.as_slice() {
        [] => Err(XStocksBaselineError::MissingSolanaToken2022Deployment),
        [deployment] => Ok(*deployment),
        _ => Err(XStocksBaselineError::AmbiguousSolanaToken2022Deployment),
    }
}

/// Errors returned by the optional live xStocks smoke client.
#[derive(Debug)]
pub enum XStocksClientError {
    InvalidRequest(String),
    Http(reqwest::Error),
    HttpStatus { status: StatusCode, body: String },
    Parse(XStocksBaselineError),
    SolanaRpc(String),
    TokenProgramMismatch { provider: String, chain: String },
}

impl fmt::Display for XStocksClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => formatter.write_str(message),
            Self::Http(error) => write!(formatter, "xStocks request failed: {error}"),
            Self::HttpStatus { status, body } => {
                write!(
                    formatter,
                    "xStocks returned {status}: {}",
                    truncate_body(body)
                )
            }
            Self::Parse(error) => error.fmt(formatter),
            Self::SolanaRpc(message) => {
                write!(formatter, "Solana RPC verification failed: {message}")
            }
            Self::TokenProgramMismatch { provider, chain } => write!(
                formatter,
                "xStocks provider token program {provider} disagrees with Solana owner {chain}"
            ),
        }
    }
}

impl std::error::Error for XStocksClientError {}

/// Read-only client for the documented unauthenticated xStocks public API.
pub struct XStocksClient {
    client: reqwest::Client,
    base_url: String,
    solana_rpc_url: String,
}

impl XStocksClient {
    pub fn new() -> Result<Self, XStocksClientError> {
        Self::with_base_url(DEFAULT_XSTOCKS_BASE_URL)
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Result<Self, XStocksClientError> {
        Self::with_base_url_and_rpc_url(base_url, DEFAULT_SOLANA_RPC_URL)
    }

    pub fn with_base_url_and_rpc_url(
        base_url: impl Into<String>,
        solana_rpc_url: impl Into<String>,
    ) -> Result<Self, XStocksClientError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if base_url.is_empty() {
            return Err(XStocksClientError::InvalidRequest(
                "xStocks base URL cannot be empty".to_owned(),
            ));
        }
        let solana_rpc_url = solana_rpc_url.into().trim_end_matches('/').to_owned();
        if solana_rpc_url.is_empty() {
            return Err(XStocksClientError::InvalidRequest(
                "Solana RPC URL cannot be empty".to_owned(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(XStocksClientError::Http)?;
        Ok(Self {
            client,
            base_url,
            solana_rpc_url,
        })
    }

    /// Fetches and validates the public identity, price, multiplier, and
    /// upcoming-corporate-action payloads for one symbol.
    pub async fn fetch_baseline(
        &self,
        symbol: &str,
        network: &str,
    ) -> Result<XStocksBaselineSnapshot, XStocksClientError> {
        validate_path_component(symbol)?;
        validate_path_component(network)?;
        if network != "Solana" {
            return Err(XStocksClientError::InvalidRequest(
                "xStocks baseline verification currently requires network=Solana".to_owned(),
            ));
        }
        let asset = self.fetch_body(&format!("/public/assets/{symbol}")).await?;
        let asset_document: AssetDocument = serde_json::from_str(&asset)
            .map_err(|_| XStocksClientError::Parse(XStocksBaselineError::InvalidJson))?;
        if asset_document.symbol != symbol {
            return Err(XStocksClientError::Parse(
                XStocksBaselineError::SymbolMismatch,
            ));
        }
        let deployment =
            select_solana_deployment(&asset_document).map_err(XStocksClientError::Parse)?;
        let chain_token_program = self.fetch_solana_token_program(&deployment.address).await?;
        if let Some(provider_token_program) = deployment.solana_token_program.as_deref() {
            if provider_token_program != chain_token_program {
                return Err(XStocksClientError::TokenProgramMismatch {
                    provider: provider_token_program.to_owned(),
                    chain: chain_token_program,
                });
            }
        }
        let price = self
            .fetch_body(&format!("/public/assets/{symbol}/price-data"))
            .await?;
        let multiplier = self
            .fetch_body(&format!(
                "/public/assets/{symbol}/multiplier?network={network}"
            ))
            .await?;
        let corporate_actions = self
            .fetch_body(&format!(
                "/public/corporate-actions/upcoming?page=1&pageSize=100&symbol={symbol}"
            ))
            .await?;
        evaluate_xstocks_baseline_with_verified_token_program(
            &asset,
            &price,
            &multiplier,
            &corporate_actions,
            symbol,
            &chain_token_program,
        )
        .map_err(XStocksClientError::Parse)
    }

    async fn fetch_solana_token_program(&self, mint: &str) -> Result<String, XStocksClientError> {
        #[derive(Serialize)]
        struct RpcRequest<'a> {
            jsonrpc: &'static str,
            id: u8,
            method: &'static str,
            params: (&'a str, RpcAccountOptions),
        }

        #[derive(Serialize)]
        struct RpcAccountOptions {
            encoding: &'static str,
        }

        #[derive(Deserialize)]
        struct RpcResponse {
            result: Option<RpcResult>,
            error: Option<RpcError>,
        }

        #[derive(Deserialize)]
        struct RpcResult {
            value: Option<RpcAccount>,
        }

        #[derive(Deserialize)]
        struct RpcAccount {
            owner: String,
        }

        #[derive(Deserialize)]
        struct RpcError {
            code: i64,
            message: String,
        }

        let response = self
            .client
            .post(&self.solana_rpc_url)
            .json(&RpcRequest {
                jsonrpc: "2.0",
                id: 1,
                method: "getAccountInfo",
                params: (mint, RpcAccountOptions { encoding: "base64" }),
            })
            .send()
            .await
            .map_err(XStocksClientError::Http)?;
        let status = response.status();
        let body = response.text().await.map_err(XStocksClientError::Http)?;
        if !status.is_success() {
            return Err(XStocksClientError::HttpStatus { status, body });
        }
        let rpc_response: RpcResponse = serde_json::from_str(&body)
            .map_err(|error| XStocksClientError::SolanaRpc(error.to_string()))?;
        if let Some(error) = rpc_response.error {
            return Err(XStocksClientError::SolanaRpc(format!(
                "{} ({})",
                error.message, error.code
            )));
        }
        let owner = rpc_response
            .result
            .and_then(|result| result.value)
            .ok_or_else(|| XStocksClientError::SolanaRpc("mint account was not found".to_owned()))?
            .owner;
        match owner.as_str() {
            SPL_TOKEN_PROGRAM_ID => Ok("TokenProgram".to_owned()),
            SPL_TOKEN_2022_PROGRAM_ID => Ok("Token2022Program".to_owned()),
            _ => Err(XStocksClientError::SolanaRpc(format!(
                "mint owner {owner} is not a recognized SPL token program"
            ))),
        }
    }

    async fn fetch_body(&self, path: &str) -> Result<String, XStocksClientError> {
        let response = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .send()
            .await
            .map_err(XStocksClientError::Http)?;
        let status = response.status();
        let body = response.text().await.map_err(XStocksClientError::Http)?;
        if !status.is_success() {
            return Err(XStocksClientError::HttpStatus { status, body });
        }
        Ok(body)
    }
}

fn parse_raw_decimal_q9(value: &RawValue) -> Option<i64> {
    let raw = value.get().trim();
    let decimal = if raw.starts_with('"') {
        serde_json::from_str::<String>(raw).ok()?
    } else {
        raw.to_owned()
    };
    parse_decimal_q9(&decimal).ok()
}

fn parse_pending_multiplier(value: Option<&RawValue>) -> Result<Option<i64>, XStocksBaselineError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if raw_value_is_zero(value) {
        // The provider encodes "no pending change" as either JSON zero or a
        // zero-valued string on different response versions.
        return Ok(None);
    }
    let multiplier = parse_raw_decimal_q9(value).ok_or(XStocksBaselineError::InvalidMultiplier)?;
    if multiplier > 0 {
        Ok(Some(multiplier))
    } else {
        Err(XStocksBaselineError::InvalidMultiplier)
    }
}

fn raw_value_is_zero(value: &RawValue) -> bool {
    let raw = value.get().trim();
    let text = if raw.starts_with('"') {
        serde_json::from_str::<String>(raw).ok()
    } else {
        Some(raw.to_owned())
    };
    text.and_then(|text| text.parse::<f64>().ok()) == Some(0.0)
}

fn parse_optional_activation(
    value: Option<&RawValue>,
) -> Result<Option<String>, XStocksBaselineError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match serde_json::from_str::<serde_json::Value>(value.get())
        .map_err(|_| XStocksBaselineError::InvalidJson)?
    {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(number) if number.as_f64() == Some(0.0) => Ok(None),
        serde_json::Value::Number(number) => Ok(Some(number.to_string())),
        serde_json::Value::String(text)
            if text.trim().is_empty() || text.parse::<f64>().ok() == Some(0.0) =>
        {
            Ok(None)
        }
        serde_json::Value::String(text) => Ok(Some(text)),
        _ => Err(XStocksBaselineError::InvalidMultiplier),
    }
}

fn validate_path_component(value: &str) -> Result<(), XStocksClientError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(XStocksClientError::InvalidRequest(
            "xStocks path components must be non-empty and URL-safe".to_owned(),
        ));
    }
    Ok(())
}

fn truncate_body(body: &str) -> String {
    body.chars().take(512).collect()
}
