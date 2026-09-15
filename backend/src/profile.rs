//! Player-profile and rating read models for the Phase 3.1 API.
//!
//! Profiles are intentionally small. Wallet identity and competitive state are
//! kept separate so cosmetic edits cannot mutate rating history or on-chain
//! Battle facts.

use std::fmt;

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use crate::auth::parse_wallet;

pub const MAX_DISPLAY_NAME_BYTES: usize = 32;
pub const MAX_AVATAR_URL_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    InvalidWallet,
    InvalidDisplayName,
    InvalidAvatarUrl,
    NotFound,
    Storage(String),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidWallet => "wallet must be a base58-encoded 32-byte Solana public key",
            Self::InvalidDisplayName => {
                "display name must be 1-32 characters without control characters"
            }
            Self::InvalidAvatarUrl => "avatar URL is too long or contains control characters",
            Self::NotFound => "player profile was not found",
            Self::Storage(_) => "profile storage operation failed",
        })
    }
}

impl std::error::Error for ProfileError {}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfileUpdate {
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileSummary {
    pub wallet: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: i64,
    pub season_id: Option<i64>,
    pub rating: i32,
    pub rated_games: i32,
    pub peak_rating: i32,
    pub wins: i32,
    pub draws: i32,
    pub losses: i32,
    pub placement_complete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RatingHistoryRow {
    pub battle_pubkey: String,
    pub market_round_id: i64,
    pub round_sequence: i64,
    pub event_kind: String,
    pub rating_before: i32,
    pub rating_after: i32,
    pub delta: i32,
    pub created_at: i64,
}

pub fn validate_profile_update(update: &ProfileUpdate) -> Result<(), ProfileError> {
    if let Some(display_name) = &update.display_name {
        let length = display_name.chars().count();
        if !(1..=MAX_DISPLAY_NAME_BYTES).contains(&length)
            || display_name.chars().any(char::is_control)
        {
            return Err(ProfileError::InvalidDisplayName);
        }
    }
    if let Some(avatar_url) = &update.avatar_url {
        if avatar_url.len() > MAX_AVATAR_URL_BYTES || avatar_url.chars().any(char::is_control) {
            return Err(ProfileError::InvalidAvatarUrl);
        }
    }
    Ok(())
}

pub async fn update_profile(
    pool: &PgPool,
    wallet: &str,
    update: &ProfileUpdate,
) -> Result<(), ProfileError> {
    parse_wallet(wallet).map_err(|_| ProfileError::InvalidWallet)?;
    validate_profile_update(update)?;

    let result = sqlx::query(
        "UPDATE users
         SET display_name = COALESCE($1, display_name),
             avatar_url = COALESCE($2, avatar_url)
         WHERE wallet = $3",
    )
    .bind(update.display_name.as_deref())
    .bind(update.avatar_url.as_deref())
    .bind(wallet)
    .execute(pool)
    .await
    .map_err(storage_error)?;

    if result.rows_affected() != 1 {
        return Err(ProfileError::NotFound);
    }
    Ok(())
}

pub async fn get_profile(pool: &PgPool, wallet: &str) -> Result<ProfileSummary, ProfileError> {
    parse_wallet(wallet).map_err(|_| ProfileError::InvalidWallet)?;
    let row = sqlx::query(
        "SELECT u.wallet, u.display_name, u.avatar_url, u.created_at,
                r.season_id, COALESCE(r.rating, 1500) AS rating,
                COALESCE(r.rated_games, 0) AS rated_games,
                COALESCE(r.peak_rating, 1500) AS peak_rating,
                COALESCE(r.wins, 0) AS wins,
                COALESCE(r.draws, 0) AS draws,
                COALESCE(r.losses, 0) AS losses
         FROM users u
         LEFT JOIN seasons s ON s.status = 'ACTIVE'
         LEFT JOIN ratings r ON r.season_id = s.id AND r.wallet = u.wallet
         WHERE u.wallet = $1",
    )
    .bind(wallet)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(ProfileError::NotFound)?;

    let rated_games: i32 = row.try_get("rated_games").map_err(storage_error)?;
    Ok(ProfileSummary {
        wallet: row.try_get("wallet").map_err(storage_error)?,
        display_name: row.try_get("display_name").map_err(storage_error)?,
        avatar_url: row.try_get("avatar_url").map_err(storage_error)?,
        created_at: row.try_get("created_at").map_err(storage_error)?,
        season_id: row.try_get("season_id").map_err(storage_error)?,
        rating: row.try_get("rating").map_err(storage_error)?,
        rated_games,
        peak_rating: row.try_get("peak_rating").map_err(storage_error)?,
        wins: row.try_get("wins").map_err(storage_error)?,
        draws: row.try_get("draws").map_err(storage_error)?,
        losses: row.try_get("losses").map_err(storage_error)?,
        placement_complete: rated_games >= 5,
    })
}

pub async fn get_history(
    pool: &PgPool,
    wallet: &str,
) -> Result<Vec<RatingHistoryRow>, ProfileError> {
    parse_wallet(wallet).map_err(|_| ProfileError::InvalidWallet)?;
    let rows = sqlx::query(
        "SELECT battle_pubkey, market_round_id, round_sequence, event_kind,
                rating_before, rating_after, delta, created_at
         FROM rating_events
         WHERE wallet = $1
         ORDER BY round_sequence ASC, id ASC",
    )
    .bind(wallet)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?;

    rows.into_iter()
        .map(|row| {
            Ok(RatingHistoryRow {
                battle_pubkey: row.try_get("battle_pubkey").map_err(storage_error)?,
                market_round_id: row.try_get("market_round_id").map_err(storage_error)?,
                round_sequence: row.try_get("round_sequence").map_err(storage_error)?,
                event_kind: row.try_get("event_kind").map_err(storage_error)?,
                rating_before: row.try_get("rating_before").map_err(storage_error)?,
                rating_after: row.try_get("rating_after").map_err(storage_error)?,
                delta: row.try_get("delta").map_err(storage_error)?,
                created_at: row.try_get("created_at").map_err(storage_error)?,
            })
        })
        .collect()
}

fn storage_error(error: sqlx::Error) -> ProfileError {
    ProfileError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_text_validation_rejects_control_characters_and_long_names() {
        assert!(validate_profile_update(&ProfileUpdate {
            display_name: Some("valid-name".to_owned()),
            avatar_url: None,
        })
        .is_ok());
        assert_eq!(
            validate_profile_update(&ProfileUpdate {
                display_name: Some("bad\nname".to_owned()),
                avatar_url: None,
            }),
            Err(ProfileError::InvalidDisplayName)
        );
        assert_eq!(
            validate_profile_update(&ProfileUpdate {
                display_name: Some("x".repeat(MAX_DISPLAY_NAME_BYTES + 1)),
                avatar_url: None,
            }),
            Err(ProfileError::InvalidDisplayName)
        );
    }
}
