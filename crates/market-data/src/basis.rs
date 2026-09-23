//! Fail-closed basis analytics for comparable tokenized/reference prices.
//!
//! This module is deliberately independent from scoring and settlement. A
//! caller must provide the frozen comparability decision and freshness window;
//! this code only computes a displayable basis when both are valid.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisComparisonStatus {
    Comparable,
    ComparisonUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BasisComparisonInput {
    /// Tokenized representation price in the protocol's exact Q9 scale.
    pub token_price_q9: i64,
    /// Underlying/reference price in the protocol's exact Q9 scale.
    pub reference_price_q9: i64,
    pub token_observed_at_unix_secs: i64,
    pub reference_observed_at_unix_secs: i64,
    pub as_of_unix_secs: i64,
    pub max_age_secs: i64,
    /// Frozen registry comparability, not a caller-invented UI preference.
    pub comparable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BasisComparison {
    pub status: BasisComparisonStatus,
    /// Signed basis in basis points, or `None` when comparison is unavailable.
    pub basis_bps: Option<i64>,
}

/// Computes `(token - reference) / reference` in basis points using integer
/// arithmetic. Any missing comparability, stale observation, future timestamp,
/// non-positive price, or arithmetic overflow returns no basis value.
pub fn compare_tokenized_reference_basis(input: BasisComparisonInput) -> BasisComparison {
    let observations_are_fresh = input.max_age_secs >= 0
        && input.token_observed_at_unix_secs <= input.as_of_unix_secs
        && input.reference_observed_at_unix_secs <= input.as_of_unix_secs
        && input
            .as_of_unix_secs
            .saturating_sub(input.token_observed_at_unix_secs)
            <= input.max_age_secs
        && input
            .as_of_unix_secs
            .saturating_sub(input.reference_observed_at_unix_secs)
            <= input.max_age_secs;
    if !input.comparable
        || input.token_price_q9 <= 0
        || input.reference_price_q9 <= 0
        || !observations_are_fresh
    {
        return unavailable();
    }

    let delta = i128::from(input.token_price_q9) - i128::from(input.reference_price_q9);
    let basis_bps = delta
        .checked_mul(10_000)
        .and_then(|scaled| scaled.checked_div(i128::from(input.reference_price_q9)))
        .and_then(|value| i64::try_from(value).ok());

    match basis_bps {
        Some(basis_bps) => BasisComparison {
            status: BasisComparisonStatus::Comparable,
            basis_bps: Some(basis_bps),
        },
        None => unavailable(),
    }
}

fn unavailable() -> BasisComparison {
    BasisComparison {
        status: BasisComparisonStatus::ComparisonUnavailable,
        basis_bps: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_comparable_prices_produce_q9_basis_in_basis_points() {
        let comparison = compare_tokenized_reference_basis(BasisComparisonInput {
            token_price_q9: 110_000_000_000,
            reference_price_q9: 100_000_000_000,
            token_observed_at_unix_secs: 100,
            reference_observed_at_unix_secs: 102,
            as_of_unix_secs: 105,
            max_age_secs: 10,
            comparable: true,
        });

        assert_eq!(comparison.status, BasisComparisonStatus::Comparable);
        assert_eq!(comparison.basis_bps, Some(1_000));
    }

    #[test]
    fn stale_prices_never_produce_a_basis_value() {
        let comparison = compare_tokenized_reference_basis(BasisComparisonInput {
            token_price_q9: 110_000_000_000,
            reference_price_q9: 100_000_000_000,
            token_observed_at_unix_secs: 80,
            reference_observed_at_unix_secs: 82,
            as_of_unix_secs: 105,
            max_age_secs: 10,
            comparable: true,
        });

        assert_eq!(
            comparison.status,
            BasisComparisonStatus::ComparisonUnavailable
        );
        assert_eq!(comparison.basis_bps, None);
    }

    #[test]
    fn structurally_non_comparable_prices_never_produce_a_basis_value() {
        let comparison = compare_tokenized_reference_basis(BasisComparisonInput {
            token_price_q9: 110_000_000_000,
            reference_price_q9: 100_000_000_000,
            token_observed_at_unix_secs: 100,
            reference_observed_at_unix_secs: 102,
            as_of_unix_secs: 105,
            max_age_secs: 10,
            comparable: false,
        });

        assert_eq!(
            comparison.status,
            BasisComparisonStatus::ComparisonUnavailable
        );
        assert_eq!(comparison.basis_bps, None);
    }
}
