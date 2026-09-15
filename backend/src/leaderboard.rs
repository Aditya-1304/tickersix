//! Reproducible active-season leaderboard queries.
//!
//! The leaderboard is a read model over canonical `ratings` rows. It never
//! ranks by wallet balance, portfolio value, or any other capital measure, and
//! equal ratings intentionally receive the same displayed rank.

use std::{fmt, num::TryFromIntError};

use serde::Serialize;
use sqlx::{PgPool, Row};

use crate::{auth::parse_wallet, rating::rating_tier};

pub const DEFAULT_PAGE_SIZE: i64 = 25;
pub const MAX_PAGE_SIZE: i64 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaderboardError {
    InvalidWallet,
    InvalidCursor,
    InvalidLimit,
    NoActiveSeason,
    NotFound,
    Storage(String),
}

impl fmt::Display for LeaderboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidWallet => "wallet must be a base58-encoded 32-byte Solana public key",
            Self::InvalidCursor => "leaderboard cursor is invalid",
            Self::InvalidLimit => "leaderboard limit must be between 1 and 100",
            Self::NoActiveSeason => "there is no active season",
            Self::NotFound => "player profile was not found",
            Self::Storage(_) => "leaderboard storage operation failed",
        })
    }
}

impl std::error::Error for LeaderboardError {}

#[derive(Debug, Clone, Serialize)]
pub struct LeaderboardEntry {
    pub rank: i64,
    pub wallet: String,
    pub display_name: Option<String>,
    pub rating: i32,
    pub tier: String,
    pub rated_games: i32,
    pub wins: i32,
    pub draws: i32,
    pub losses: i32,
    pub peak_rating: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct LeaderboardPage {
    pub season_id: i64,
    pub season_name: String,
    pub entries: Vec<LeaderboardEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LeaderboardMe {
    pub season_id: i64,
    pub season_name: String,
    pub wallet: String,
    pub rating: i32,
    pub tier: String,
    pub rated_games: i32,
    pub wins: i32,
    pub draws: i32,
    pub losses: i32,
    pub peak_rating: i32,
    pub placement_complete: bool,
    pub rank: Option<i64>,
    pub neighborhood: Vec<LeaderboardEntry>,
}

pub async fn global_leaderboard(
    pool: &PgPool,
    cursor: Option<&str>,
    limit: i64,
) -> Result<LeaderboardPage, LeaderboardError> {
    let limit = normalize_limit(limit)?;
    let cursor = cursor.map(decode_cursor).transpose()?;
    let (season_id, season_name) = active_season(pool).await?;
    let cursor_rating = cursor.as_ref().map(|value| value.0);
    let cursor_wallet = cursor.as_ref().map(|value| value.1.as_str());
    let rows = sqlx::query(
        "WITH ranked AS (
             SELECT RANK() OVER (ORDER BY r.rating DESC) AS display_rank,
                    u.wallet, u.display_name, r.rating, r.rated_games,
                    r.wins, r.draws, r.losses, r.peak_rating
             FROM ratings r
             JOIN seasons s ON s.id = r.season_id AND s.status = 'ACTIVE'
             JOIN users u ON u.wallet = r.wallet
             WHERE r.rated_games >= $1
         )
         SELECT display_rank, wallet, display_name, rating, rated_games,
                wins, draws, losses, peak_rating
         FROM ranked
         WHERE $2::INTEGER IS NULL
            OR rating < $2
            OR (rating = $2 AND wallet > $3)
         ORDER BY rating DESC, wallet ASC
         LIMIT $4",
    )
    .bind(crate::rating::PLACEMENT_BATTLES)
    .bind(cursor_rating)
    .bind(cursor_wallet)
    .bind(limit + 1)
    .fetch_all(pool)
    .await
    .map_err(storage_error)?;

    let has_more = rows.len() > usize::try_from(limit).map_err(int_error)?;
    let mut entries = rows
        .into_iter()
        .map(entry_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    if has_more {
        entries.pop();
    }
    let next_cursor = has_more
        .then(|| entries.last())
        .flatten()
        .map(|entry| encode_cursor(entry.rating, &entry.wallet));

    Ok(LeaderboardPage {
        season_id,
        season_name,
        entries,
        next_cursor,
    })
}

pub async fn leaderboard_me(
    pool: &PgPool,
    wallet: &str,
) -> Result<LeaderboardMe, LeaderboardError> {
    parse_wallet(wallet).map_err(|_| LeaderboardError::InvalidWallet)?;
    let (season_id, season_name) = active_season(pool).await?;
    let row = sqlx::query(
        "SELECT u.wallet,
                COALESCE(r.rating, 1500) AS rating,
                COALESCE(r.rated_games, 0) AS rated_games,
                COALESCE(r.wins, 0) AS wins,
                COALESCE(r.draws, 0) AS draws,
                COALESCE(r.losses, 0) AS losses,
                COALESCE(r.peak_rating, 1500) AS peak_rating
         FROM users u
         LEFT JOIN ratings r ON r.season_id = $1 AND r.wallet = u.wallet
         WHERE u.wallet = $2",
    )
    .bind(season_id)
    .bind(wallet)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?
    .ok_or(LeaderboardError::NotFound)?;

    let rating: i32 = row.try_get("rating").map_err(storage_error)?;
    let rated_games: i32 = row.try_get("rated_games").map_err(storage_error)?;
    let placement_complete = rated_games >= crate::rating::PLACEMENT_BATTLES;
    let rank: Option<i64> = if placement_complete {
        Some(
            sqlx::query(
                "SELECT 1 + COUNT(*) AS display_rank
                 FROM ratings r
                 WHERE r.season_id = $1
                   AND r.rated_games >= $2
                   AND r.rating > $3",
            )
            .bind(season_id)
            .bind(crate::rating::PLACEMENT_BATTLES)
            .bind(rating)
            .fetch_one(pool)
            .await
            .map_err(storage_error)?
            .try_get::<i64, _>("display_rank")
            .map(displayed_rank)
            .map_err(storage_error)?,
        )
    } else {
        None
    };

    let neighborhood = if let Some(rank) = rank {
        let lower = rank.saturating_sub(2).max(1);
        let upper = rank.saturating_add(2);
        let rows = sqlx::query(
            "WITH ranked AS (
                 SELECT RANK() OVER (ORDER BY r.rating DESC) AS display_rank,
                        u.wallet, u.display_name, r.rating, r.rated_games,
                        r.wins, r.draws, r.losses, r.peak_rating
                 FROM ratings r
                 JOIN users u ON u.wallet = r.wallet
                 WHERE r.season_id = $1
                   AND r.rated_games >= $2
             )
             SELECT display_rank, wallet, display_name, rating, rated_games,
                    wins, draws, losses, peak_rating
             FROM ranked
             WHERE display_rank BETWEEN $3 AND $4
             ORDER BY rating DESC, wallet ASC",
        )
        .bind(season_id)
        .bind(crate::rating::PLACEMENT_BATTLES)
        .bind(lower)
        .bind(upper)
        .fetch_all(pool)
        .await
        .map_err(storage_error)?;
        rows.into_iter()
            .map(entry_from_row)
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    Ok(LeaderboardMe {
        season_id,
        season_name,
        wallet: row.try_get("wallet").map_err(storage_error)?,
        rating,
        tier: rating_tier(rating, rated_games).to_owned(),
        rated_games,
        wins: row.try_get("wins").map_err(storage_error)?,
        draws: row.try_get("draws").map_err(storage_error)?,
        losses: row.try_get("losses").map_err(storage_error)?,
        peak_rating: row.try_get("peak_rating").map_err(storage_error)?,
        placement_complete,
        rank,
        neighborhood,
    })
}

fn normalize_limit(limit: i64) -> Result<i64, LeaderboardError> {
    if (1..=MAX_PAGE_SIZE).contains(&limit) {
        Ok(limit)
    } else {
        Err(LeaderboardError::InvalidLimit)
    }
}

fn encode_cursor(rating: i32, wallet: &str) -> String {
    format!("{rating}:{wallet}")
}

fn displayed_rank(number_of_higher_ratings: i64) -> i64 {
    number_of_higher_ratings.saturating_add(1)
}

fn decode_cursor(cursor: &str) -> Result<(i32, String), LeaderboardError> {
    let (rating, wallet) = cursor
        .split_once(':')
        .ok_or(LeaderboardError::InvalidCursor)?;
    let rating = rating
        .parse::<i32>()
        .map_err(|_| LeaderboardError::InvalidCursor)?;
    parse_wallet(wallet).map_err(|_| LeaderboardError::InvalidCursor)?;
    Ok((rating, wallet.to_owned()))
}

async fn active_season(pool: &PgPool) -> Result<(i64, String), LeaderboardError> {
    sqlx::query("SELECT id, name FROM seasons WHERE status = 'ACTIVE'")
        .fetch_optional(pool)
        .await
        .map_err(storage_error)?
        .map(|row| {
            Ok((
                row.try_get("id").map_err(storage_error)?,
                row.try_get("name").map_err(storage_error)?,
            ))
        })
        .transpose()?
        .ok_or(LeaderboardError::NoActiveSeason)
}

fn entry_from_row(row: sqlx::postgres::PgRow) -> Result<LeaderboardEntry, LeaderboardError> {
    let rating: i32 = row.try_get("rating").map_err(storage_error)?;
    let rated_games: i32 = row.try_get("rated_games").map_err(storage_error)?;
    Ok(LeaderboardEntry {
        rank: row.try_get("display_rank").map_err(storage_error)?,
        wallet: row.try_get("wallet").map_err(storage_error)?,
        display_name: row.try_get("display_name").map_err(storage_error)?,
        rating,
        tier: rating_tier(rating, rated_games).to_owned(),
        rated_games,
        wins: row.try_get("wins").map_err(storage_error)?,
        draws: row.try_get("draws").map_err(storage_error)?,
        losses: row.try_get("losses").map_err(storage_error)?,
        peak_rating: row.try_get("peak_rating").map_err(storage_error)?,
    })
}

fn int_error(_: TryFromIntError) -> LeaderboardError {
    LeaderboardError::InvalidLimit
}

fn storage_error(error: sqlx::Error) -> LeaderboardError {
    LeaderboardError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaderboard_cursor_round_trip_preserves_rating_tie_order() {
        let cursor = encode_cursor(1601, "11111111111111111111111111111111");
        assert_eq!(
            decode_cursor(&cursor).unwrap(),
            (1601, "11111111111111111111111111111111".to_owned())
        );
        assert_eq!(displayed_rank(0), 1);
        assert_eq!(displayed_rank(4), 5);
    }
}
