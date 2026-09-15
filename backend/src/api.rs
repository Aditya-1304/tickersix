//! Axum HTTP surface for Phase 3.1.
//!
//! Handlers are deliberately thin: authentication and profile invariants live
//! in dedicated modules, while this layer translates HTTP input/output and
//! keeps storage errors away from clients.

use std::{env, error::Error, fmt, sync::Arc};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;

use crate::{
    auth::{self, AuthError},
    db,
    profile::{self, ProfileError, ProfileUpdate},
    ranked::{self, RankedError},
};

#[derive(Clone)]
pub struct ApiState {
    pub pool: PgPool,
    pub auth_domain: Arc<str>,
    pub secure_cookie: bool,
}

impl ApiState {
    pub fn new(pool: PgPool, auth_domain: impl Into<Arc<str>>, secure_cookie: bool) -> Self {
        Self {
            pool,
            auth_domain: auth_domain.into(),
            secure_cookie,
        }
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/auth/challenge", post(create_challenge))
        .route("/v1/auth/verify", post(verify_challenge))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/profiles/:wallet", get(get_profile))
        .route("/v1/profiles/:wallet/history", get(get_profile_history))
        .route("/v1/profile/me", put(update_my_profile))
        .route("/v1/market-rounds/next", get(get_next_market_round))
        .route(
            "/v1/ranked/queue",
            post(join_ranked_queue).delete(leave_ranked_queue),
        )
        .route("/v1/ranked/status", get(get_ranked_status))
        .with_state(state)
}

/// Starts the Phase 3.1 HTTP service from runtime configuration.
///
/// Database credentials, authentication domain, and cookie security are
/// intentionally environment-owned. No key material or database secret is
/// compiled into the binary or committed to the repository.
pub async fn serve_from_env() -> Result<(), Box<dyn Error>> {
    let database_url = env::var("TICKERSIX_DATABASE_URL")
        .map_err(|_| "TICKERSIX_DATABASE_URL must point to PostgreSQL")?;
    let bind = env::var("TICKERSIX_API_BIND").unwrap_or_else(|_| "127.0.0.1:8788".to_owned());
    let auth_domain = env::var("TICKERSIX_AUTH_DOMAIN").unwrap_or_else(|_| "localhost".to_owned());
    let secure_cookie = env::var("TICKERSIX_SECURE_COOKIE")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&database_url)
        .await?;
    db::run_migrations(&pool).await?;
    let listener = TcpListener::bind(&bind).await?;
    println!("TickerSix API listening on {bind}");
    axum::serve(
        listener,
        router(ApiState::new(pool, auth_domain, secure_cookie)),
    )
    .await?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: &'static str,
}

#[derive(Debug)]
pub enum ApiError {
    Auth(AuthError),
    Profile(ProfileError),
    Ranked(RankedError),
    DomainMismatch,
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auth(error) => write!(formatter, "{error}"),
            Self::Profile(error) => write!(formatter, "{error}"),
            Self::Ranked(error) => write!(formatter, "{error}"),
            Self::DomainMismatch => {
                formatter.write_str("authentication domain does not match server configuration")
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::Auth(AuthError::InvalidSignature)
            | Self::Auth(AuthError::InvalidWallet)
            | Self::Auth(AuthError::InvalidChallenge)
            | Self::Auth(AuthError::ChallengeExpired)
            | Self::Auth(AuthError::ChallengeConsumed)
            | Self::Auth(AuthError::DomainMismatch)
            | Self::Auth(AuthError::SessionExpired)
            | Self::DomainMismatch => StatusCode::UNAUTHORIZED,
            Self::Profile(ProfileError::InvalidWallet)
            | Self::Profile(ProfileError::InvalidDisplayName)
            | Self::Profile(ProfileError::InvalidAvatarUrl) => StatusCode::BAD_REQUEST,
            Self::Profile(ProfileError::NotFound) => StatusCode::NOT_FOUND,
            Self::Ranked(RankedError::RoundNotFound | RankedError::QueueNotFound) => {
                StatusCode::NOT_FOUND
            }
            Self::Ranked(
                RankedError::QueueClosed
                | RankedError::AlreadyPaired
                | RankedError::AlreadyExposed
                | RankedError::LeagueReserved
                | RankedError::UnresolvedPreviousBattle
                | RankedError::CoordinatorConflict,
            ) => StatusCode::CONFLICT,
            Self::Ranked(
                RankedError::InvalidWallet
                | RankedError::InvalidRound
                | RankedError::RoundNotEligible
                | RankedError::CoordinatorPlanUnavailable,
            ) => StatusCode::BAD_REQUEST,
            Self::Auth(AuthError::Storage(_)) | Self::Profile(ProfileError::Storage(_)) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Ranked(RankedError::Storage(_)) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            Json(ErrorBody {
                error: error_code(&self),
            }),
        )
            .into_response()
    }
}

impl From<AuthError> for ApiError {
    fn from(error: AuthError) -> Self {
        Self::Auth(error)
    }
}

impl From<ProfileError> for ApiError {
    fn from(error: ProfileError) -> Self {
        Self::Profile(error)
    }
}

impl From<RankedError> for ApiError {
    fn from(error: RankedError) -> Self {
        Self::Ranked(error)
    }
}

#[derive(Debug, Deserialize)]
struct ChallengeRequest {
    wallet: String,
    domain: String,
}

#[derive(Debug, Deserialize)]
struct VerifyRequest {
    wallet: String,
    nonce: String,
    signature: String,
    domain: String,
}

#[derive(Debug, Serialize)]
struct VerifyResponse {
    wallet: String,
    expires_at: i64,
}

#[derive(Debug, Deserialize)]
struct RankedQueueRequest {
    market_round_id: i64,
}

#[derive(Debug, Deserialize, Default)]
struct RankedStatusQuery {
    market_round_id: Option<i64>,
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn create_challenge(
    State(state): State<ApiState>,
    Json(request): Json<ChallengeRequest>,
) -> Result<Json<auth::AuthChallenge>, ApiError> {
    if request.domain != state.auth_domain.as_ref() {
        return Err(ApiError::DomainMismatch);
    }
    Ok(Json(
        auth::issue_challenge(
            &state.pool,
            &request.wallet,
            &request.domain,
            auth::unix_now(),
        )
        .await?,
    ))
}

async fn verify_challenge(
    State(state): State<ApiState>,
    Json(request): Json<VerifyRequest>,
) -> Result<(HeaderMap, Json<VerifyResponse>), ApiError> {
    if request.domain != state.auth_domain.as_ref() {
        return Err(ApiError::DomainMismatch);
    }
    let session = auth::verify_challenge(
        &state.pool,
        &request.wallet,
        &request.nonce,
        &request.signature,
        &request.domain,
        auth::unix_now(),
    )
    .await?;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        auth::session_cookie(&session.raw_token, session.expires_at, state.secure_cookie),
    );
    // Preserve an explicit content type even when an upstream middleware later
    // adds additional response headers.
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((
        response_headers,
        Json(VerifyResponse {
            wallet: session.wallet,
            expires_at: session.expires_at,
        }),
    ))
}

async fn logout(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<(HeaderMap, StatusCode), ApiError> {
    if let Some(token) = session_from_headers(&headers) {
        auth::revoke_session(&state.pool, token, auth::unix_now()).await?;
    }
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        auth::clear_session_cookie(state.secure_cookie),
    );
    Ok((response_headers, StatusCode::NO_CONTENT))
}

async fn get_profile(
    State(state): State<ApiState>,
    Path(wallet): Path<String>,
) -> Result<Json<profile::ProfileSummary>, ApiError> {
    Ok(Json(profile::get_profile(&state.pool, &wallet).await?))
}

async fn get_profile_history(
    State(state): State<ApiState>,
    Path(wallet): Path<String>,
) -> Result<Json<Vec<profile::RatingHistoryRow>>, ApiError> {
    Ok(Json(profile::get_history(&state.pool, &wallet).await?))
}

async fn update_my_profile(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(update): Json<ProfileUpdate>,
) -> Result<Json<profile::ProfileSummary>, ApiError> {
    let wallet = auth::authenticated_wallet(
        &state.pool,
        session_from_headers(&headers).ok_or(AuthError::SessionExpired)?,
        auth::unix_now(),
    )
    .await?;
    profile::update_profile(&state.pool, &wallet, &update).await?;
    Ok(Json(profile::get_profile(&state.pool, &wallet).await?))
}

async fn get_next_market_round(
    State(state): State<ApiState>,
) -> Result<Json<Option<ranked::NextMarketRound>>, ApiError> {
    Ok(Json(
        ranked::next_market_round(&state.pool, auth::unix_now()).await?,
    ))
}

async fn join_ranked_queue(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<RankedQueueRequest>,
) -> Result<Json<ranked::QueueEntry>, ApiError> {
    let wallet = authenticated_wallet(&state, &headers).await?;
    Ok(Json(
        ranked::join_queue(
            &state.pool,
            &wallet,
            request.market_round_id,
            auth::unix_now(),
        )
        .await?,
    ))
}

async fn leave_ranked_queue(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<RankedStatusQuery>,
) -> Result<StatusCode, ApiError> {
    let wallet = authenticated_wallet(&state, &headers).await?;
    let market_round_id = query.market_round_id.ok_or(RankedError::RoundNotFound)?;
    ranked::leave_queue(&state.pool, &wallet, market_round_id, auth::unix_now()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_ranked_status(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<RankedStatusQuery>,
) -> Result<Json<ranked::QueueEntry>, ApiError> {
    let wallet = authenticated_wallet(&state, &headers).await?;
    let market_round_id = query.market_round_id.ok_or(RankedError::RoundNotFound)?;
    Ok(Json(
        ranked::queue_status(&state.pool, &wallet, market_round_id).await?,
    ))
}

async fn authenticated_wallet(state: &ApiState, headers: &HeaderMap) -> Result<String, ApiError> {
    let token = session_from_headers(headers).ok_or(AuthError::SessionExpired)?;
    Ok(auth::authenticated_wallet(&state.pool, token, auth::unix_now()).await?)
}

fn session_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| auth::extract_session_token(Some(value)))
}

fn error_code(error: &ApiError) -> &'static str {
    match error {
        ApiError::Auth(AuthError::InvalidWallet)
        | ApiError::Profile(ProfileError::InvalidWallet) => "INVALID_WALLET",
        ApiError::Auth(AuthError::InvalidSignature) => "INVALID_SIGNATURE",
        ApiError::Auth(AuthError::InvalidChallenge) => "INVALID_CHALLENGE",
        ApiError::Auth(AuthError::ChallengeExpired) => "CHALLENGE_EXPIRED",
        ApiError::Auth(AuthError::ChallengeConsumed) => "CHALLENGE_CONSUMED",
        ApiError::Auth(AuthError::DomainMismatch) | ApiError::DomainMismatch => "DOMAIN_MISMATCH",
        ApiError::Auth(AuthError::SessionExpired) => "SESSION_EXPIRED",
        ApiError::Profile(ProfileError::InvalidDisplayName) => "INVALID_DISPLAY_NAME",
        ApiError::Profile(ProfileError::InvalidAvatarUrl) => "INVALID_AVATAR_URL",
        ApiError::Profile(ProfileError::NotFound) => "PROFILE_NOT_FOUND",
        ApiError::Auth(AuthError::Storage(_)) | ApiError::Profile(ProfileError::Storage(_)) => {
            "INTERNAL_ERROR"
        }
        ApiError::Ranked(RankedError::Storage(_)) => "INTERNAL_ERROR",
        ApiError::Ranked(RankedError::RoundNotFound) => "ROUND_NOT_FOUND",
        ApiError::Ranked(RankedError::QueueNotFound) => "QUEUE_NOT_FOUND",
        ApiError::Ranked(RankedError::InvalidWallet) => "INVALID_WALLET",
        ApiError::Ranked(RankedError::InvalidRound) => "INVALID_ROUND",
        ApiError::Ranked(RankedError::RoundNotEligible) => "ROUND_NOT_ELIGIBLE",
        ApiError::Ranked(RankedError::QueueClosed) => "QUEUE_CLOSED",
        ApiError::Ranked(RankedError::AlreadyPaired) => "ALREADY_PAIRED",
        ApiError::Ranked(RankedError::AlreadyExposed) => "ALREADY_EXPOSED",
        ApiError::Ranked(RankedError::LeagueReserved) => "LEAGUE_RESERVED",
        ApiError::Ranked(RankedError::UnresolvedPreviousBattle) => "UNRESOLVED_PREVIOUS_BATTLE",
        ApiError::Ranked(RankedError::CoordinatorPlanUnavailable) => "COORDINATOR_PLAN_UNAVAILABLE",
        ApiError::Ranked(RankedError::CoordinatorConflict) => "COORDINATOR_CONFLICT",
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{header, HeaderMap};

    use super::*;

    #[test]
    fn session_cookie_parser_does_not_confuse_similar_cookie_names() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("other=wrong; tickersix_session=expected"),
        );

        assert_eq!(session_from_headers(&headers), Some("expected"));
    }
}
