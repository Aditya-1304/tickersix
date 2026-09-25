//! Wallet-signed authentication for the TickerSix backend.
//!
//! The server authenticates a wallet by verifying a canonical challenge with
//! the wallet's Ed25519 public key. A wallet address in an HTTP body is never
//! treated as identity on its own. Challenges and sessions are stored in
//! PostgreSQL by the production handlers, while the pure helpers below keep
//! the signing rules independently testable.

use std::{collections::HashSet, fmt, time::Duration};

use axum::http::HeaderValue;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rand::{rngs::OsRng, RngCore};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

pub const AUTH_CHAIN: &str = "solana";
pub const AUTH_STATEMENT: &str = "TickerSix authentication";
pub const AUTH_CHALLENGE_TTL_SECS: i64 = 5 * 60;
pub const SESSION_TTL_SECS: i64 = 24 * 60 * 60;
pub const SESSION_COOKIE_NAME: &str = "tickersix_session";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    InvalidWallet,
    InvalidSignature,
    InvalidChallenge,
    ChallengeExpired,
    ChallengeConsumed,
    DomainMismatch,
    SessionExpired,
    Storage(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidWallet => "wallet must be a base58-encoded 32-byte Solana public key",
            Self::InvalidSignature => "wallet signature is invalid",
            Self::InvalidChallenge => "authentication challenge was not found",
            Self::ChallengeExpired => "authentication challenge has expired",
            Self::ChallengeConsumed => "authentication challenge was already consumed",
            Self::DomainMismatch => "authentication domain does not match",
            Self::SessionExpired => "session is expired or revoked",
            Self::Storage(_) => "authentication storage operation failed",
        })
    }
}

impl std::error::Error for AuthError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthChallenge {
    pub nonce: String,
    pub wallet: String,
    pub domain: String,
    pub message: String,
    pub issued_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSession {
    pub wallet: String,
    pub raw_token: String,
    pub expires_at: i64,
}

/// Builds the exact text that the wallet signs.
///
/// Field ordering and separators are protocol-facing. Changing this format
/// requires a versioned authentication migration because old signed messages
/// must not be reinterpreted under a new domain.
pub fn canonical_auth_message(
    domain: &str,
    wallet: &str,
    nonce: &str,
    issued_at: i64,
    expires_at: i64,
) -> String {
    format!(
        "{AUTH_STATEMENT}\n\ndomain: {domain}\nwallet: {wallet}\nnonce: {nonce}\nissued_at: {issued_at}\nexpires_at: {expires_at}\nchain: {AUTH_CHAIN}\nstatement: {AUTH_STATEMENT}"
    )
}

pub fn parse_wallet(wallet: &str) -> Result<[u8; 32], AuthError> {
    let bytes = bs58::decode(wallet)
        .into_vec()
        .map_err(|_| AuthError::InvalidWallet)?;
    bytes.try_into().map_err(|_| AuthError::InvalidWallet)
}

/// Verifies a base58-encoded Ed25519 signature over the canonical challenge.
pub fn verify_wallet_signature(
    wallet: &str,
    message: &str,
    signature_base58: &str,
) -> Result<(), AuthError> {
    let public_key =
        VerifyingKey::from_bytes(&parse_wallet(wallet)?).map_err(|_| AuthError::InvalidWallet)?;
    let signature_bytes = bs58::decode(signature_base58)
        .into_vec()
        .map_err(|_| AuthError::InvalidSignature)?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| AuthError::InvalidSignature)?;
    public_key
        .verify(message.as_bytes(), &signature)
        .map_err(|_| AuthError::InvalidSignature)
}

/// Enforces the one-time challenge state transition before signature work.
///
/// Keeping expiry and consumption checks together prevents a caller from
/// accidentally accepting a previously consumed or time-invalid challenge
/// when the database-backed verifier evolves.
pub fn validate_challenge_window(
    consumed_at: Option<i64>,
    expires_at: i64,
    now: i64,
) -> Result<(), AuthError> {
    if consumed_at.is_some() {
        return Err(AuthError::ChallengeConsumed);
    }
    if now >= expires_at {
        return Err(AuthError::ChallengeExpired);
    }
    Ok(())
}

pub fn new_nonce() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn session_token_hash(raw_token: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(raw_token.as_bytes());
    hasher.finalize().into()
}

pub fn session_cookie(raw_token: &str, expires_at: i64, secure: bool) -> HeaderValue {
    let secure_attribute = if secure { "; Secure" } else { "" };
    let cookie = format!(
        "{SESSION_COOKIE_NAME}={raw_token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure_attribute}",
        (expires_at - unix_now()).max(0)
    );
    HeaderValue::from_str(&cookie).expect("session cookie is generated from safe characters")
}

pub fn clear_session_cookie(secure: bool) -> HeaderValue {
    let secure_attribute = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure_attribute}"
    ))
    .expect("session cookie is generated from safe characters")
}

pub fn extract_session_token(cookie_header: Option<&str>) -> Option<&str> {
    cookie_header?.split(';').find_map(|cookie| {
        let (name, value) = cookie.trim().split_once('=')?;
        (name == SESSION_COOKIE_NAME).then_some(value)
    })
}

pub async fn issue_challenge(
    pool: &PgPool,
    wallet: &str,
    domain: &str,
    now: i64,
) -> Result<AuthChallenge, AuthError> {
    parse_wallet(wallet)?;
    if domain.trim().is_empty() {
        return Err(AuthError::DomainMismatch);
    }

    let nonce = new_nonce();
    let expires_at = now
        .checked_add(AUTH_CHALLENGE_TTL_SECS)
        .ok_or_else(|| AuthError::Storage("challenge expiry overflow".to_owned()))?;
    let message = canonical_auth_message(domain, wallet, &nonce, now, expires_at);

    sqlx::query(
        "INSERT INTO auth_challenges
            (nonce, wallet, domain, message, issued_at, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&nonce)
    .bind(wallet)
    .bind(domain)
    .bind(&message)
    .bind(now)
    .bind(expires_at)
    .execute(pool)
    .await
    .map_err(storage_error)?;

    Ok(AuthChallenge {
        nonce,
        wallet: wallet.to_owned(),
        domain: domain.to_owned(),
        message,
        issued_at: now,
        expires_at,
    })
}

pub async fn verify_challenge(
    pool: &PgPool,
    wallet: &str,
    nonce: &str,
    signature_base58: &str,
    domain: &str,
    now: i64,
) -> Result<AuthSession, AuthError> {
    parse_wallet(wallet)?;
    let mut transaction = pool.begin().await.map_err(storage_error)?;
    let row = sqlx::query(
        "SELECT wallet, domain, message, expires_at, consumed_at
         FROM auth_challenges
         WHERE nonce = $1
         FOR UPDATE",
    )
    .bind(nonce)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(storage_error)?
    .ok_or(AuthError::InvalidChallenge)?;

    let stored_wallet: String = row.try_get("wallet").map_err(storage_error)?;
    let stored_domain: String = row.try_get("domain").map_err(storage_error)?;
    let message: String = row.try_get("message").map_err(storage_error)?;
    let expires_at: i64 = row.try_get("expires_at").map_err(storage_error)?;
    let consumed_at: Option<i64> = row.try_get("consumed_at").map_err(storage_error)?;

    if stored_wallet != wallet || stored_domain != domain {
        return Err(AuthError::DomainMismatch);
    }
    validate_challenge_window(consumed_at, expires_at, now)?;
    verify_wallet_signature(wallet, &message, signature_base58)?;

    sqlx::query("UPDATE auth_challenges SET consumed_at = $1 WHERE nonce = $2")
        .bind(now)
        .bind(nonce)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;

    sqlx::query(
        "INSERT INTO users (wallet, created_at)
         VALUES ($1, $2)
         ON CONFLICT (wallet) DO NOTHING",
    )
    .bind(wallet)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;

    let mut token_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut token_bytes);
    let raw_token = hex::encode(token_bytes);
    let token_hash = session_token_hash(&raw_token);
    let session_expires_at = now
        .checked_add(SESSION_TTL_SECS)
        .ok_or_else(|| AuthError::Storage("session expiry overflow".to_owned()))?;

    sqlx::query(
        "INSERT INTO sessions (token_hash, wallet, issued_at, expires_at)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(token_hash.as_slice())
    .bind(wallet)
    .bind(now)
    .bind(session_expires_at)
    .execute(&mut *transaction)
    .await
    .map_err(storage_error)?;

    transaction.commit().await.map_err(storage_error)?;
    Ok(AuthSession {
        wallet: wallet.to_owned(),
        raw_token,
        expires_at: session_expires_at,
    })
}

pub async fn authenticated_wallet(
    pool: &PgPool,
    raw_token: &str,
    now: i64,
) -> Result<String, AuthError> {
    let token_hash = session_token_hash(raw_token);
    let row = sqlx::query(
        "SELECT wallet, expires_at, revoked_at
         FROM sessions
         WHERE token_hash = $1",
    )
    .bind(token_hash.as_slice())
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(AuthError::SessionExpired)?;

    let expires_at: i64 = row.try_get("expires_at").map_err(storage_error)?;
    let revoked_at: Option<i64> = row.try_get("revoked_at").map_err(storage_error)?;
    if revoked_at.is_some() || now >= expires_at {
        return Err(AuthError::SessionExpired);
    }
    row.try_get("wallet").map_err(storage_error)
}

pub async fn revoke_session(pool: &PgPool, raw_token: &str, now: i64) -> Result<(), AuthError> {
    let token_hash = session_token_hash(raw_token);
    sqlx::query("UPDATE sessions SET revoked_at = $1 WHERE token_hash = $2")
        .bind(now)
        .bind(token_hash.as_slice())
        .execute(pool)
        .await
        .map_err(storage_error)?;
    Ok(())
}

/// Small pure state machine used by tests to prove nonce consumption semantics
/// without requiring a live PostgreSQL instance.
#[derive(Default)]
pub struct NonceBook {
    consumed: HashSet<String>,
}

impl NonceBook {
    pub fn consume_once(&mut self, nonce: impl Into<String>) -> bool {
        self.consumed.insert(nonce.into())
    }
}

fn storage_error(error: sqlx::Error) -> AuthError {
    AuthError::Storage(error.to_string())
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    #[test]
    fn canonical_message_is_stable_and_contains_all_binding_fields() {
        let message = canonical_auth_message("localhost:3000", "wallet", "nonce", 10, 20);

        assert_eq!(
            message,
            "TickerSix authentication\n\ndomain: localhost:3000\nwallet: wallet\nnonce: nonce\nissued_at: 10\nexpires_at: 20\nchain: solana\nstatement: TickerSix authentication"
        );
    }

    #[test]
    fn valid_wallet_signature_is_accepted_and_wrong_wallet_is_rejected() {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let wallet = bs58::encode(signing_key.verifying_key().to_bytes()).into_string();
        let message = canonical_auth_message("localhost", &wallet, "nonce", 10, 20);
        let signature = bs58::encode(signing_key.sign(message.as_bytes()).to_bytes()).into_string();

        assert!(verify_wallet_signature(&wallet, &message, &signature).is_ok());
        let other_wallet =
            bs58::encode(SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes()).into_string();
        assert_eq!(
            verify_wallet_signature(&other_wallet, &message, &signature),
            Err(AuthError::InvalidSignature)
        );
    }

    #[test]
    fn expired_or_consumed_challenges_are_rejected_before_signature_work() {
        assert_eq!(
            validate_challenge_window(None, 10, 10),
            Err(AuthError::ChallengeExpired)
        );
        assert_eq!(
            validate_challenge_window(Some(9), 10, 1),
            Err(AuthError::ChallengeConsumed)
        );
        assert!(validate_challenge_window(None, 10, 9).is_ok());
    }

    #[test]
    fn nonce_can_be_consumed_only_once() {
        let mut book = NonceBook::default();

        assert!(book.consume_once("nonce-1"));
        assert!(!book.consume_once("nonce-1"));
        assert!(book.consume_once("nonce-2"));
    }
}
