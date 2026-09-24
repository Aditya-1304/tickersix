//! Canonical finalized Battle lineup evidence used by indexer projections.
///
/// This module keeps the fixed-point score calculation and structural evidence
/// validation in one place. The chain reader supplies the facts; this module
/// only proves that the supplied facts are internally consistent with the
/// finalized Battle scores before they are persisted.
use std::fmt;

use protocol::{lineup_score_q9, MathError};

/// Version of the durable finalized Battle evidence shape understood by this
/// backend. Any future shape change must be introduced as a new version.
pub const CURRENT_FACTS_VERSION: i32 = 1;

/// Finalized per-side lineup facts decoded from authoritative chain state.
///
/// Returns are stored in the same order as the six asset identifiers. Keeping
/// that ordering explicit is required for captain-specific achievement rules
/// and prevents a score-only projection from reconstructing missing evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedBattleFacts {
    pub facts_version: i32,
    pub finalized_slot: i64,
    pub side_a_lineup: Vec<u16>,
    pub side_a_captain: u16,
    pub side_a_returns_q9: [i64; 6],
    pub side_b_lineup: Vec<u16>,
    pub side_b_captain: u16,
    pub side_b_returns_q9: [i64; 6],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleFactsError {
    UnsupportedVersion,
    InvalidFinalizedSlot,
    InvalidLineup,
    CaptainNotInLineup,
    ScoreMismatch,
    ArithmeticOverflow,
}

impl fmt::Display for BattleFactsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => "unsupported finalized Battle facts version",
            Self::InvalidFinalizedSlot => "finalized Battle facts have an invalid slot",
            Self::InvalidLineup => "finalized Battle lineup must contain six unique assets",
            Self::CaptainNotInLineup => "finalized Battle captain is not in the lineup",
            Self::ScoreMismatch => "finalized Battle facts do not reproduce the stored score",
            Self::ArithmeticOverflow => "finalized Battle score arithmetic overflowed",
        })
    }
}

impl std::error::Error for BattleFactsError {}

/// Validates both sides and reproduces the exact integer score recorded on the
/// finalized Battle account. The division deliberately matches the authority
/// engine's fixed-point truncation semantics.
pub fn validate(
    facts: &IndexedBattleFacts,
    score_a_q9: i64,
    score_b_q9: i64,
) -> Result<(), BattleFactsError> {
    if facts.facts_version != CURRENT_FACTS_VERSION {
        return Err(BattleFactsError::UnsupportedVersion);
    }
    if facts.finalized_slot < 0 {
        return Err(BattleFactsError::InvalidFinalizedSlot);
    }

    validate_side(
        &facts.side_a_lineup,
        facts.side_a_captain,
        facts.side_a_returns_q9,
        score_a_q9,
    )?;
    validate_side(
        &facts.side_b_lineup,
        facts.side_b_captain,
        facts.side_b_returns_q9,
        score_b_q9,
    )?;
    Ok(())
}

/// Calculates the authority-engine score from six lineup returns and captain
/// selection. This narrow helper is exported for the proof and reconciliation
/// layers so they cannot drift into separate scoring implementations.
pub fn score_q9(
    returns_q9: [i64; 6],
    lineup: &[u16],
    captain: u16,
) -> Result<i64, BattleFactsError> {
    if lineup.len() != 6 {
        return Err(BattleFactsError::InvalidLineup);
    }

    let asset_ids: [u16; 6] = lineup
        .try_into()
        .map_err(|_| BattleFactsError::InvalidLineup)?;
    lineup_score_q9(returns_q9, asset_ids, captain).map_err(|error| match error {
        MathError::InvalidLineup => BattleFactsError::InvalidLineup,
        MathError::CaptainNotInLineup => BattleFactsError::CaptainNotInLineup,
        MathError::Overflow => BattleFactsError::ArithmeticOverflow,
        _ => BattleFactsError::ArithmeticOverflow,
    })
}

fn validate_side(
    lineup: &[u16],
    captain: u16,
    returns_q9: [i64; 6],
    expected_score_q9: i64,
) -> Result<(), BattleFactsError> {
    if lineup.len() != 6 {
        return Err(BattleFactsError::InvalidLineup);
    }

    if score_q9(returns_q9, lineup, captain)? != expected_score_q9 {
        return Err(BattleFactsError::ScoreMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> IndexedBattleFacts {
        IndexedBattleFacts {
            facts_version: CURRENT_FACTS_VERSION,
            finalized_slot: 900,
            side_a_lineup: vec![1, 2, 3, 4, 5, 6],
            side_a_captain: 1,
            side_a_returns_q9: [70, 60, 50, 40, 30, 20],
            side_b_lineup: vec![7, 8, 9, 10, 11, 12],
            side_b_captain: 7,
            side_b_returns_q9: [10, 10, 10, 10, 10, 10],
        }
    }

    #[test]
    fn validates_lineups_and_exact_fixed_point_scores() {
        assert!(validate(&facts(), 48, 10).is_ok());
    }

    #[test]
    fn rejects_a_score_that_cannot_be_reproduced_from_finalized_facts() {
        assert_eq!(
            validate(&facts(), 49, 10),
            Err(BattleFactsError::ScoreMismatch)
        );
    }

    #[test]
    fn rejects_duplicate_assets_and_missing_captains() {
        let mut invalid = facts();
        invalid.side_a_lineup[1] = 1;
        assert_eq!(
            validate(&invalid, 48, 10),
            Err(BattleFactsError::InvalidLineup)
        );

        let mut invalid = facts();
        invalid.side_b_captain = 99;
        assert_eq!(
            validate(&invalid, 48, 10),
            Err(BattleFactsError::CaptainNotInLineup)
        );
    }
}
