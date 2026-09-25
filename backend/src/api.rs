//! Axum HTTP surface for the TickerSix backend.
//!
//! Handlers are deliberately thin: authentication and profile invariants live
//! in dedicated modules, while this layer translates HTTP input/output and
//! keeps storage errors away from clients.

use std::{env, error::Error, fmt, path::PathBuf, sync::Arc, time::Duration};

use tower_http::cors::CorsLayer;

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{
        sse::{KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;

use crate::{
    achievements::{self, AchievementError},
    auth::{self, AuthError},
    db,
    leaderboard::{self, LeaderboardError},
    league::{self, LeagueError},
    lineup::{self, LineupError},
    live::{self, LiveError},
    metrics,
    private_markets::{self, PrivateMarketsError},
    profile::{self, ProfileError, ProfileUpdate},
    proof::{self, ProofError},
    ranked::{self, RankedError},
    replay::{self, ReplayError},
};

#[derive(Clone)]
pub struct ApiState {
    pub pool: PgPool,
    pub auth_domain: Arc<str>,
    pub secure_cookie: bool,
    /// Optional web UI origin for credentialed browser requests.
    pub web_origin: Option<Arc<str>>,
    /// Optional local proof snapshot used by the read-only proof API.
    pub proof_snapshot_path: Option<Arc<PathBuf>>,
    pub private_markets: Arc<private_markets::PrivateMarketsService>,
    /// Devnet RPC used only to fetch a recent blockhash for unsigned wallet
    /// transactions. The backend never signs or submits the transaction.
    pub solana_rpc_url: Arc<str>,
}

impl ApiState {
    pub fn new(pool: PgPool, auth_domain: impl Into<Arc<str>>, secure_cookie: bool) -> Self {
        Self {
            pool,
            auth_domain: auth_domain.into(),
            secure_cookie,
            web_origin: None,
            proof_snapshot_path: None,
            private_markets: Arc::new(private_markets::PrivateMarketsService::from_env()),
            solana_rpc_url: Arc::from(
                env::var("TICKERSIX_SOLANA_RPC_URL")
                    .unwrap_or_else(|_| "https://api.devnet.solana.com".to_owned()),
            ),
        }
    }
}

impl ApiState {
    pub fn with_web_origin(mut self, origin: impl Into<Arc<str>>) -> Self {
        self.web_origin = Some(origin.into());
        self
    }

    pub fn with_proof_snapshot_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.proof_snapshot_path = Some(Arc::new(path.into()));
        self
    }

    pub fn with_solana_rpc_url(mut self, url: impl Into<Arc<str>>) -> Self {
        self.solana_rpc_url = url.into();
        self
    }
}

pub fn router(state: ApiState) -> Router {
    let web_origin = state.web_origin.clone();
    let router = Router::new()
        .route("/health", get(health))
        .route("/v1/auth/challenge", post(create_challenge))
        .route("/v1/auth/verify", post(verify_challenge))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/profiles/{wallet}", get(get_profile))
        .route("/v1/profiles/{wallet}/history", get(get_profile_history))
        .route(
            "/v1/profiles/{wallet}/achievements",
            get(get_profile_achievements),
        )
        .route("/v1/profile/me", get(get_my_profile).put(update_my_profile))
        .route("/v1/market-rounds/next", get(get_next_market_round))
        .route(
            "/v1/market-rounds/{id}/assets",
            get(get_market_round_assets),
        )
        .route(
            "/v1/market-rounds/{pubkey}/proof",
            get(get_market_round_proof),
        )
        .route("/v1/battles/{pubkey}/proof", get(get_battle_proof))
        .route("/v1/battles/{pubkey}/replay", get(get_battle_replay))
        .route(
            "/v1/private-markets/assets",
            get(list_private_market_assets),
        )
        .route(
            "/v1/private-markets/assets/{id}",
            get(get_private_market_asset),
        )
        .route(
            "/v1/private-markets/assets/{id}/representations",
            get(get_private_market_representations),
        )
        .route(
            "/v1/private-markets/comparisons/{asset_id}",
            get(get_private_market_comparison),
        )
        .route("/v1/leagues", get(list_leagues))
        .route("/v1/leagues/{id}", get(get_league))
        .route("/v1/leagues/{id}/join", post(join_league))
        .route("/v1/leagues/{id}/leave", post(leave_league))
        .route("/v1/leagues/{id}/rounds", get(get_league_rounds))
        .route("/v1/leagues/{id}/standings", get(get_league_standings))
        .route(
            "/v1/ranked/queue",
            post(join_ranked_queue).delete(leave_ranked_queue),
        )
        .route("/v1/ranked/status", get(get_ranked_status))
        .route("/v1/battles/{pubkey}/lineup/prepare", post(prepare_lineup))
        .route("/v1/leaderboards/global", get(get_global_leaderboard))
        .route("/v1/leaderboards/global/me", get(get_my_leaderboard))
        .route("/v1/stream/battles/{pubkey}", get(stream_battle))
        .route("/metrics", get(get_metrics))
        .with_state(state);

    match web_origin {
        Some(origin) => router.layer(
            CorsLayer::new()
                .allow_origin(
                    origin
                        .parse::<HeaderValue>()
                        .expect("configured web origin is a valid header value"),
                )
                .allow_credentials(true)
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::DELETE,
                    Method::OPTIONS,
                ])
                .allow_headers([header::ACCEPT, header::AUTHORIZATION, header::CONTENT_TYPE]),
        ),
        None => router,
    }
}

/// Starts the HTTP service from runtime configuration.
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
    let web_origin = env::var("TICKERSIX_WEB_ORIGIN").ok();
    let proof_snapshot_path = env::var_os("TICKERSIX_PROOF_SNAPSHOT").map(PathBuf::from);
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&database_url)
        .await?;
    db::run_migrations(&pool).await?;
    let listener = TcpListener::bind(&bind).await?;
    println!("TickerSix API listening on {bind}");
    axum::serve(
        listener,
        router(match proof_snapshot_path {
            Some(path) => {
                let state = ApiState::new(pool.clone(), auth_domain.clone(), secure_cookie)
                    .with_proof_snapshot_path(path);
                match web_origin.clone() {
                    Some(origin) => state.with_web_origin(origin),
                    None => state,
                }
            }
            None => {
                let state = ApiState::new(pool, auth_domain, secure_cookie);
                match web_origin {
                    Some(origin) => state.with_web_origin(origin),
                    None => state,
                }
            }
        }),
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
    Achievements(AchievementError),
    Leaderboard(LeaderboardError),
    League(LeagueError),
    Live(LiveError),
    PrivateMarkets(PrivateMarketsError),
    Profile(ProfileError),
    Proof(ProofError),
    Ranked(RankedError),
    Lineup(LineupError),
    Replay(ReplayError),
    DomainMismatch,
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auth(error) => write!(formatter, "{error}"),
            Self::Achievements(error) => write!(formatter, "{error}"),
            Self::Leaderboard(error) => write!(formatter, "{error}"),
            Self::League(error) => write!(formatter, "{error}"),
            Self::Live(error) => write!(formatter, "{error}"),
            Self::PrivateMarkets(error) => write!(formatter, "{error}"),
            Self::Profile(error) => write!(formatter, "{error}"),
            Self::Proof(error) => write!(formatter, "{error}"),
            Self::Ranked(error) => write!(formatter, "{error}"),
            Self::Lineup(error) => write!(formatter, "{error}"),
            Self::Replay(error) => write!(formatter, "{error}"),
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
            Self::Achievements(AchievementError::InvalidWallet) => StatusCode::BAD_REQUEST,
            Self::Profile(ProfileError::InvalidWallet)
            | Self::Profile(ProfileError::InvalidDisplayName)
            | Self::Profile(ProfileError::InvalidAvatarUrl) => StatusCode::BAD_REQUEST,
            Self::Profile(ProfileError::NotFound) => StatusCode::NOT_FOUND,
            Self::Leaderboard(LeaderboardError::NoActiveSeason | LeaderboardError::NotFound) => {
                StatusCode::NOT_FOUND
            }
            Self::Leaderboard(
                LeaderboardError::InvalidWallet
                | LeaderboardError::InvalidCursor
                | LeaderboardError::InvalidLimit,
            ) => StatusCode::BAD_REQUEST,
            Self::League(LeagueError::NotFound) => StatusCode::NOT_FOUND,
            Self::League(
                LeagueError::RegistrationClosed
                | LeagueError::LeagueFull
                | LeagueError::AlreadyMember
                | LeagueError::NotMember
                | LeagueError::ScheduleUnavailable
                | LeagueError::ScheduleConflict
                | LeagueError::InvalidMembershipState
                | LeagueError::EntropyUnavailable
                | LeagueError::PairingNotReady
                | LeagueError::PairingConflict
                | LeagueError::CoordinatorPlanUnavailable
                | LeagueError::CoordinatorConflict,
            ) => StatusCode::CONFLICT,
            Self::League(
                LeagueError::InvalidLeagueId
                | LeagueError::InvalidWallet
                | LeagueError::InvalidName
                | LeagueError::InvalidLeague
                | LeagueError::InvalidSchedule
                | LeagueError::EmptySchedule
                | LeagueError::DuplicateMarketRound
                | LeagueError::DuplicateLeagueRound
                | LeagueError::ScheduleOverlap
                | LeagueError::ChainIdentityUnavailable
                | LeagueError::InvalidEntropy
                | LeagueError::InvalidPairing,
            ) => StatusCode::BAD_REQUEST,
            Self::Live(LiveError::NotFound) => StatusCode::NOT_FOUND,
            Self::Live(LiveError::InvalidBattle) => StatusCode::BAD_REQUEST,
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
            Self::Proof(ProofError::NotConfigured | ProofError::PathMismatch) => {
                StatusCode::NOT_FOUND
            }
            Self::Proof(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Replay(ReplayError::NotFound | ReplayError::Unavailable) => StatusCode::NOT_FOUND,
            Self::Replay(ReplayError::InvalidBattle) => StatusCode::BAD_REQUEST,
            Self::Replay(ReplayError::Storage(_)) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::PrivateMarkets(PrivateMarketsError::NotFound) => StatusCode::NOT_FOUND,
            Self::PrivateMarkets(PrivateMarketsError::InvalidAssetId) => StatusCode::BAD_REQUEST,
            Self::PrivateMarkets(PrivateMarketsError::Unavailable(_)) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            Self::Auth(AuthError::Storage(_)) | Self::Profile(ProfileError::Storage(_)) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Achievements(AchievementError::Storage(_) | AchievementError::League(_)) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Leaderboard(LeaderboardError::Storage(_)) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::League(LeagueError::InvalidStandings | LeagueError::Storage(_)) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Live(LiveError::Storage(_)) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Ranked(RankedError::Storage(_)) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Lineup(LineupError::BattleNotFound | LineupError::RoundAssetsUnavailable) => {
                StatusCode::NOT_FOUND
            }
            Self::Lineup(LineupError::NotBattleParticipant) => StatusCode::FORBIDDEN,
            Self::Lineup(
                LineupError::InvalidBattle
                | LineupError::InvalidLineupSize
                | LineupError::DuplicateAsset
                | LineupError::AssetNotInFrozenUniverse
                | LineupError::CaptainNotSelected
                | LineupError::InvalidSalt
                | LineupError::RoundMetadataUnavailable,
            ) => StatusCode::BAD_REQUEST,
            Self::Lineup(LineupError::CommitWindowClosed | LineupError::AlreadyCommitted) => {
                StatusCode::CONFLICT
            }
            Self::Lineup(LineupError::RpcUnavailable) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Lineup(LineupError::TransactionBuildFailed | LineupError::Storage(_)) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
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

impl From<AchievementError> for ApiError {
    fn from(error: AchievementError) -> Self {
        Self::Achievements(error)
    }
}

impl From<ProfileError> for ApiError {
    fn from(error: ProfileError) -> Self {
        Self::Profile(error)
    }
}

impl From<LeaderboardError> for ApiError {
    fn from(error: LeaderboardError) -> Self {
        Self::Leaderboard(error)
    }
}

impl From<LeagueError> for ApiError {
    fn from(error: LeagueError) -> Self {
        Self::League(error)
    }
}

impl From<LiveError> for ApiError {
    fn from(error: LiveError) -> Self {
        Self::Live(error)
    }
}

impl From<ProofError> for ApiError {
    fn from(error: ProofError) -> Self {
        Self::Proof(error)
    }
}

impl From<RankedError> for ApiError {
    fn from(error: RankedError) -> Self {
        Self::Ranked(error)
    }
}

impl From<LineupError> for ApiError {
    fn from(error: LineupError) -> Self {
        Self::Lineup(error)
    }
}

impl From<ReplayError> for ApiError {
    fn from(error: ReplayError) -> Self {
        Self::Replay(error)
    }
}

impl From<PrivateMarketsError> for ApiError {
    fn from(error: PrivateMarketsError) -> Self {
        Self::PrivateMarkets(error)
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

async fn prepare_lineup(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(battle_pubkey): Path<String>,
    Json(request): Json<lineup::PrepareLineupRequest>,
) -> Result<Json<lineup::PreparedLineup>, ApiError> {
    let wallet = authenticated_wallet(&state, &headers).await?;
    Ok(Json(
        lineup::prepare_lineup(
            &state.pool,
            &state.solana_rpc_url,
            &battle_pubkey,
            &wallet,
            request,
            auth::unix_now(),
        )
        .await?,
    ))
}

#[derive(Debug, Deserialize, Default)]
struct RankedStatusQuery {
    market_round_id: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
struct LeagueListQuery {
    status: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LeaderboardQuery {
    cursor: Option<String>,
    limit: Option<i64>,
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

async fn get_my_profile(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<profile::ProfileSummary>, ApiError> {
    let wallet = authenticated_wallet(&state, &headers).await?;
    Ok(Json(profile::get_profile(&state.pool, &wallet).await?))
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

async fn get_profile_achievements(
    State(state): State<ApiState>,
    Path(wallet): Path<String>,
) -> Result<Json<Vec<achievements::AchievementView>>, ApiError> {
    Ok(Json(
        achievements::list_for_wallet(&state.pool, &wallet).await?,
    ))
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

async fn get_market_round_assets(
    State(state): State<ApiState>,
    Path(market_round_id): Path<i64>,
) -> Result<Json<ranked::RoundAssetUniverse>, ApiError> {
    Ok(Json(
        ranked::list_round_assets(&state.pool, market_round_id).await?,
    ))
}

fn load_requested_proof(
    state: &ApiState,
    request_path: &str,
) -> Result<proof::PublicProof, ApiError> {
    let snapshot_path = state
        .proof_snapshot_path
        .as_deref()
        .ok_or(ProofError::NotConfigured)?;
    let public_proof = proof::load_public_from_file(snapshot_path)?;
    if !proof::proof_path_matches(request_path, &public_proof) {
        return Err(ProofError::PathMismatch.into());
    }
    Ok(public_proof)
}

async fn get_market_round_proof(
    State(state): State<ApiState>,
    Path(pubkey): Path<String>,
) -> Result<Json<proof::PublicProof>, ApiError> {
    let request_path = format!("/v1/market-rounds/{pubkey}/proof");
    Ok(Json(load_requested_proof(&state, &request_path)?))
}

async fn get_battle_proof(
    State(state): State<ApiState>,
    Path(pubkey): Path<String>,
) -> Result<Json<proof::PublicProof>, ApiError> {
    let request_path = format!("/v1/battles/{pubkey}/proof");
    Ok(Json(load_requested_proof(&state, &request_path)?))
}

async fn get_battle_replay(
    State(state): State<ApiState>,
    Path(pubkey): Path<String>,
) -> Result<Json<replay::ReplayTimeline>, ApiError> {
    Ok(Json(replay::get_replay(&state.pool, &pubkey).await?))
}

async fn list_private_market_assets(
    State(state): State<ApiState>,
) -> Result<Json<private_markets::PrivateMarketCatalogResponse>, ApiError> {
    Ok(Json(state.private_markets.catalog().await?))
}

async fn get_private_market_asset(
    State(state): State<ApiState>,
    Path(asset_id): Path<String>,
) -> Result<Json<private_markets::PrivateMarketAssetDetail>, ApiError> {
    Ok(Json(state.private_markets.asset(&asset_id).await?))
}

async fn get_private_market_representations(
    State(state): State<ApiState>,
    Path(asset_id): Path<String>,
) -> Result<Json<private_markets::PrivateMarketRepresentationsResponse>, ApiError> {
    Ok(Json(
        state.private_markets.representations(&asset_id).await?,
    ))
}

async fn get_private_market_comparison(
    State(state): State<ApiState>,
    Path(asset_id): Path<String>,
) -> Result<Json<private_markets::PrivateMarketComparison>, ApiError> {
    Ok(Json(state.private_markets.comparison(&asset_id).await?))
}

async fn list_leagues(
    State(state): State<ApiState>,
    Query(query): Query<LeagueListQuery>,
) -> Result<Json<Vec<league::LeagueSummary>>, ApiError> {
    Ok(Json(
        league::list_leagues(&state.pool, query.status.as_deref()).await?,
    ))
}

async fn get_league(
    State(state): State<ApiState>,
    Path(league_id): Path<i64>,
) -> Result<Json<league::LeagueDetails>, ApiError> {
    Ok(Json(league::get_league(&state.pool, league_id).await?))
}

async fn join_league(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(league_id): Path<i64>,
) -> Result<Json<league::JoinLeagueResponse>, ApiError> {
    let wallet = authenticated_wallet(&state, &headers).await?;
    Ok(Json(
        league::request_join(&state.pool, league_id, &wallet, auth::unix_now()).await?,
    ))
}

async fn leave_league(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(league_id): Path<i64>,
) -> Result<Json<league::LeaveLeagueResponse>, ApiError> {
    let wallet = authenticated_wallet(&state, &headers).await?;
    Ok(Json(
        league::request_leave(&state.pool, league_id, &wallet, auth::unix_now()).await?,
    ))
}

async fn get_league_rounds(
    State(state): State<ApiState>,
    Path(league_id): Path<i64>,
) -> Result<Json<Vec<league::LeagueRound>>, ApiError> {
    Ok(Json(league::list_rounds(&state.pool, league_id).await?))
}

async fn get_league_standings(
    State(state): State<ApiState>,
    Path(league_id): Path<i64>,
) -> Result<Json<league::LeagueStandings>, ApiError> {
    Ok(Json(
        league::get_league_standings(&state.pool, league_id).await?,
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
) -> Result<Json<ranked::RankedStatus>, ApiError> {
    let wallet = authenticated_wallet(&state, &headers).await?;
    let market_round_id = query.market_round_id.ok_or(RankedError::RoundNotFound)?;
    Ok(Json(
        ranked::ranked_status(&state.pool, &wallet, market_round_id).await?,
    ))
}

async fn get_global_leaderboard(
    State(state): State<ApiState>,
    Query(query): Query<LeaderboardQuery>,
) -> Result<Json<leaderboard::LeaderboardPage>, ApiError> {
    Ok(Json(
        leaderboard::global_leaderboard(
            &state.pool,
            query.cursor.as_deref(),
            query.limit.unwrap_or(leaderboard::DEFAULT_PAGE_SIZE),
        )
        .await?,
    ))
}

async fn get_my_leaderboard(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<leaderboard::LeaderboardMe>, ApiError> {
    let wallet = authenticated_wallet(&state, &headers).await?;
    Ok(Json(
        leaderboard::leaderboard_me(&state.pool, &wallet).await?,
    ))
}

async fn get_metrics() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4"),
        )],
        metrics::global().render(),
    )
        .into_response()
}

async fn stream_battle(
    State(state): State<ApiState>,
    Path(battle_pubkey): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    live::ensure_battle(&state.pool, &battle_pubkey).await?;
    let stream = live::battle_stream(state.pool, battle_pubkey);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
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
        | ApiError::Achievements(AchievementError::InvalidWallet)
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
        ApiError::Achievements(AchievementError::Storage(_) | AchievementError::League(_)) => {
            "INTERNAL_ERROR"
        }
        ApiError::Leaderboard(LeaderboardError::Storage(_)) => "INTERNAL_ERROR",
        ApiError::Leaderboard(LeaderboardError::NoActiveSeason) => "NO_ACTIVE_SEASON",
        ApiError::Leaderboard(LeaderboardError::NotFound) => "PROFILE_NOT_FOUND",
        ApiError::Leaderboard(LeaderboardError::InvalidWallet) => "INVALID_WALLET",
        ApiError::Leaderboard(LeaderboardError::InvalidCursor) => "INVALID_CURSOR",
        ApiError::Leaderboard(LeaderboardError::InvalidLimit) => "INVALID_LIMIT",
        ApiError::League(LeagueError::Storage(_)) => "INTERNAL_ERROR",
        ApiError::League(LeagueError::InvalidLeagueId) => "INVALID_LEAGUE_ID",
        ApiError::League(LeagueError::InvalidWallet) => "INVALID_WALLET",
        ApiError::League(LeagueError::InvalidName) => "INVALID_LEAGUE_NAME",
        ApiError::League(LeagueError::InvalidLeague) => "INVALID_LEAGUE",
        ApiError::League(LeagueError::NotFound) => "LEAGUE_NOT_FOUND",
        ApiError::League(LeagueError::RegistrationClosed) => "LEAGUE_REGISTRATION_CLOSED",
        ApiError::League(LeagueError::LeagueFull) => "LEAGUE_FULL",
        ApiError::League(LeagueError::AlreadyMember) => "ALREADY_LEAGUE_MEMBER",
        ApiError::League(LeagueError::NotMember) => "NOT_LEAGUE_MEMBER",
        ApiError::League(LeagueError::ScheduleUnavailable) => "LEAGUE_SCHEDULE_UNAVAILABLE",
        ApiError::League(LeagueError::InvalidSchedule)
        | ApiError::League(LeagueError::EmptySchedule)
        | ApiError::League(LeagueError::DuplicateMarketRound)
        | ApiError::League(LeagueError::DuplicateLeagueRound)
        | ApiError::League(LeagueError::ScheduleOverlap) => "INVALID_LEAGUE_SCHEDULE",
        ApiError::League(LeagueError::ScheduleConflict) => "LEAGUE_SCHEDULE_CONFLICT",
        ApiError::League(LeagueError::ChainIdentityUnavailable) => "CHAIN_IDENTITY_UNAVAILABLE",
        ApiError::League(LeagueError::InvalidMembershipState) => "INVALID_MEMBERSHIP_STATE",
        ApiError::League(LeagueError::EntropyUnavailable) => "PAIRING_ENTROPY_UNAVAILABLE",
        ApiError::League(LeagueError::InvalidEntropy) => "INVALID_PAIRING_ENTROPY",
        ApiError::League(LeagueError::PairingNotReady) => "PAIRING_NOT_READY",
        ApiError::League(LeagueError::InvalidPairing) => "INVALID_PAIRING",
        ApiError::League(LeagueError::PairingConflict) => "PAIRING_CONFLICT",
        ApiError::League(LeagueError::CoordinatorPlanUnavailable) => "COORDINATOR_PLAN_UNAVAILABLE",
        ApiError::League(LeagueError::CoordinatorConflict) => "COORDINATOR_CONFLICT",
        ApiError::League(LeagueError::InvalidStandings) => "INVALID_LEAGUE_STANDINGS",
        ApiError::Live(LiveError::InvalidBattle) => "INVALID_BATTLE",
        ApiError::Live(LiveError::NotFound) => "BATTLE_NOT_FOUND",
        ApiError::Live(LiveError::Storage(_)) => "INTERNAL_ERROR",
        ApiError::Proof(ProofError::NotConfigured | ProofError::PathMismatch) => "PROOF_NOT_FOUND",
        ApiError::Proof(_) => "PROOF_UNAVAILABLE",
        ApiError::PrivateMarkets(PrivateMarketsError::InvalidAssetId) => {
            "INVALID_PRIVATE_MARKET_ASSET"
        }
        ApiError::PrivateMarkets(PrivateMarketsError::NotFound) => "PRIVATE_MARKET_ASSET_NOT_FOUND",
        ApiError::PrivateMarkets(PrivateMarketsError::Unavailable(_)) => {
            "PRIVATE_MARKETS_UNAVAILABLE"
        }
        ApiError::Replay(ReplayError::InvalidBattle) => "INVALID_BATTLE",
        ApiError::Replay(ReplayError::NotFound) => "BATTLE_NOT_FOUND",
        ApiError::Replay(ReplayError::Unavailable) => "REPLAY_UNAVAILABLE",
        ApiError::Replay(ReplayError::Storage(_)) => "INTERNAL_ERROR",
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
        ApiError::Lineup(LineupError::InvalidBattle) => "INVALID_BATTLE",
        ApiError::Lineup(LineupError::BattleNotFound) => "BATTLE_NOT_FOUND",
        ApiError::Lineup(LineupError::NotBattleParticipant) => "NOT_BATTLE_PARTICIPANT",
        ApiError::Lineup(LineupError::InvalidLineupSize) => "LINEUP_REQUIRES_SIX_ASSETS",
        ApiError::Lineup(LineupError::DuplicateAsset) => "LINEUP_ASSETS_MUST_BE_UNIQUE",
        ApiError::Lineup(LineupError::AssetNotInFrozenUniverse) => {
            "LINEUP_ASSET_NOT_IN_FROZEN_UNIVERSE"
        }
        ApiError::Lineup(LineupError::CaptainNotSelected) => "CAPTAIN_MUST_BE_SELECTED",
        ApiError::Lineup(LineupError::InvalidSalt) => "INVALID_LINEUP_SALT",
        ApiError::Lineup(LineupError::RoundMetadataUnavailable) => "ROUND_METADATA_UNAVAILABLE",
        ApiError::Lineup(LineupError::RoundAssetsUnavailable) => "ROUND_ASSETS_UNAVAILABLE",
        ApiError::Lineup(LineupError::CommitWindowClosed) => "COMMIT_WINDOW_CLOSED",
        ApiError::Lineup(LineupError::AlreadyCommitted) => "LINEUP_ALREADY_COMMITTED",
        ApiError::Lineup(LineupError::RpcUnavailable) => "SOLANA_RPC_UNAVAILABLE",
        ApiError::Lineup(LineupError::TransactionBuildFailed) => "TRANSACTION_BUILD_FAILED",
        ApiError::Lineup(LineupError::Storage(_)) => "INTERNAL_ERROR",
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{header, HeaderMap};

    use super::*;

    #[tokio::test]
    async fn credentialed_web_origin_builds_without_a_wildcard_cors_header() {
        let pool = PgPoolOptions::new()
            .connect_lazy(
                "postgresql://stocklana:stocklana_devnet_local@127.0.0.1:55432/stocklana_devnet",
            )
            .expect("lazy PostgreSQL pool should accept the local test URL");
        let state =
            ApiState::new(pool, "127.0.0.1", false).with_web_origin("http://127.0.0.1:4173");

        let _router = router(state);
    }

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
