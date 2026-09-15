//! Shared, deterministic protocol primitives.
//!
//! This crate deliberately contains no database, HTTP, Anchor, or wallet code.
//! Its job is to provide the byte-level and integer-level rules that must agree
//! across the on-chain program, backend workers, and client golden vectors.

use sha2::{Digest, Sha256};

pub const PRICE_SCALE_Q9: i128 = 1_000_000_000;
pub const RETURN_SCALE_Q9: i128 = 1_000_000_000;
pub const ATTESTATION_DOMAIN: &[u8] = b"TICKERSIX_PRICE_ATTESTATION_V1\0";
pub const PRICE_SAMPLE_DOMAIN: &[u8] = b"TICKERSIX_PRICE_SAMPLE_V1\0";
pub const PRICE_EVIDENCE_LIST_DOMAIN: &[u8] = b"TICKERSIX_PRICE_EVIDENCE_LIST_V1\0";
pub const LINEUP_SIZE: usize = 6;
pub const CAPTAIN_WEIGHT: i128 = 2;
pub const NORMAL_WEIGHT: i128 = 1;
pub const TOTAL_WEIGHT: i128 = CAPTAIN_WEIGHT + (LINEUP_SIZE as i128 - 1) * NORMAL_WEIGHT;
pub const INITIAL_RATING: i32 = 1_500;
pub const RATING_FLOOR: i32 = 100;

/// Errors returned by deterministic protocol math and serialization helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathError {
    InvalidDecimal,
    NonPositivePrice,
    Overflow,
    EmptyObservationSet,
    InvalidLineup,
    CaptainNotInLineup,
    DuplicateAttestor,
    TooManyAttestors,
    NoCompatibleQuorum,
}

impl std::fmt::Display for MathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidDecimal => "invalid decimal price",
            Self::NonPositivePrice => "price must be strictly positive",
            Self::Overflow => "protocol math overflow",
            Self::EmptyObservationSet => "observation set cannot be empty",
            Self::InvalidLineup => "lineup must contain six unique assets",
            Self::CaptainNotInLineup => "captain must be one of the selected assets",
            Self::DuplicateAttestor => "attestor reports must have unique attestors",
            Self::TooManyAttestors => "attestor quorum supports at most three reports",
            Self::NoCompatibleQuorum => "no compatible attestor quorum exists",
        };

        formatter.write_str(message)
    }
}

impl std::error::Error for MathError {}

/// Parses a base-10 price into positive Q9 units without using binary floats.
///
/// The parser accepts ordinary decimal notation and scientific notation. The
/// resulting value is truncated toward zero exactly as required by settlement.
/// Checked `i128` arithmetic is used for all intermediate operations; values
/// that cannot be represented safely are rejected instead of being rounded or
/// wrapped.
pub fn parse_decimal_q9(input: &str) -> Result<i64, MathError> {
    if input.is_empty() || input.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(MathError::InvalidDecimal);
    }

    let (mantissa, exponent_text) = match input.find(['e', 'E']) {
        Some(index) => {
            if input[index + 1..].contains(['e', 'E']) {
                return Err(MathError::InvalidDecimal);
            }
            (&input[..index], Some(&input[index + 1..]))
        }
        None => (input, None),
    };

    let exponent = exponent_text.map(parse_exponent).transpose()?.unwrap_or(0);

    let (negative, mantissa) = match mantissa.as_bytes().first() {
        Some(b'-') => (true, &mantissa[1..]),
        Some(b'+') => (false, &mantissa[1..]),
        _ => (false, mantissa),
    };

    let mut parts = mantissa.split('.');
    let integer_part = parts.next().ok_or(MathError::InvalidDecimal)?;
    let fractional_part = parts.next().unwrap_or("");
    if parts.next().is_some()
        || integer_part.is_empty() && fractional_part.is_empty()
        || !integer_part.bytes().all(|byte| byte.is_ascii_digit())
        || !fractional_part.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(MathError::InvalidDecimal);
    }

    let digits = format!("{integer_part}{fractional_part}");
    let coefficient = digits.bytes().try_fold(0i128, |value, byte| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i128::from(byte - b'0')))
            .ok_or(MathError::Overflow)
    })?;

    if coefficient == 0 || negative {
        return Err(MathError::NonPositivePrice);
    }

    let fractional_digits =
        i64::try_from(fractional_part.len()).map_err(|_| MathError::Overflow)?;
    let scale = exponent
        .checked_sub(fractional_digits)
        .and_then(|value| value.checked_add(9))
        .ok_or(MathError::Overflow)?;

    let scaled = if scale >= 0 {
        coefficient
            .checked_mul(
                power_of_ten(u64::try_from(scale).map_err(|_| MathError::Overflow)?)
                    .ok_or(MathError::Overflow)?,
            )
            .ok_or(MathError::Overflow)?
    } else {
        let divisor_exponent = u64::try_from(scale.checked_neg().ok_or(MathError::Overflow)?)
            .map_err(|_| MathError::Overflow)?;
        if divisor_exponent > 38 {
            0
        } else {
            coefficient / power_of_ten(divisor_exponent).ok_or(MathError::Overflow)?
        }
    };

    if scaled <= 0 {
        return Err(MathError::NonPositivePrice);
    }

    i64::try_from(scaled).map_err(|_| MathError::Overflow)
}

fn parse_exponent(input: &str) -> Result<i64, MathError> {
    if input.is_empty() {
        return Err(MathError::InvalidDecimal);
    }

    let (negative, digits) = match input.as_bytes().first() {
        Some(b'-') => (true, &input[1..]),
        Some(b'+') => (false, &input[1..]),
        _ => (false, input),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(MathError::InvalidDecimal);
    }

    let magnitude = digits.bytes().try_fold(0i64, |value, byte| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i64::from(byte - b'0')))
            .ok_or(MathError::Overflow)
    })?;

    if negative {
        magnitude.checked_neg().ok_or(MathError::Overflow)
    } else {
        Ok(magnitude)
    }
}

fn power_of_ten(exponent: u64) -> Option<i128> {
    (0..exponent).try_fold(1i128, |value, _| value.checked_mul(10))
}

/// Calculates a signed asset return in Q9 units from two finalized prices.
pub fn return_q9(start_q9: i64, end_q9: i64) -> Result<i64, MathError> {
    if start_q9 <= 0 || end_q9 <= 0 {
        return Err(MathError::NonPositivePrice);
    }

    let start = i128::from(start_q9);
    let end = i128::from(end_q9);
    let numerator = end
        .checked_sub(start)
        .and_then(|value| value.checked_mul(RETURN_SCALE_Q9))
        .ok_or(MathError::Overflow)?;
    let value = numerator.checked_div(start).ok_or(MathError::Overflow)?;

    i64::try_from(value).map_err(|_| MathError::Overflow)
}

/// Calculates a six-asset lineup score using one symmetric captain multiplier.
pub fn lineup_score_q9(
    returns: [i64; LINEUP_SIZE],
    asset_ids: [u16; LINEUP_SIZE],
    captain_asset_id: u16,
) -> Result<i64, MathError> {
    validate_lineup(asset_ids, captain_asset_id)?;

    let total =
        returns
            .into_iter()
            .zip(asset_ids)
            .try_fold(0i128, |total, (asset_return, asset_id)| {
                let weight = if asset_id == captain_asset_id {
                    CAPTAIN_WEIGHT
                } else {
                    NORMAL_WEIGHT
                };
                total
                    .checked_add(
                        i128::from(asset_return)
                            .checked_mul(weight)
                            .ok_or(MathError::Overflow)?,
                    )
                    .ok_or(MathError::Overflow)
            })?;

    let score = total.checked_div(TOTAL_WEIGHT).ok_or(MathError::Overflow)?;
    i64::try_from(score).map_err(|_| MathError::Overflow)
}

/// Verifies the structural rules shared by lineup commitment and scoring.
pub fn validate_lineup(
    asset_ids: [u16; LINEUP_SIZE],
    captain_asset_id: u16,
) -> Result<(), MathError> {
    for left in 0..LINEUP_SIZE {
        if asset_ids[left] == captain_asset_id {
            if asset_ids[left + 1..].contains(&captain_asset_id) {
                return Err(MathError::InvalidLineup);
            }
        } else if asset_ids[left + 1..].contains(&asset_ids[left]) {
            return Err(MathError::InvalidLineup);
        }
    }

    if !asset_ids.contains(&captain_asset_id) {
        return Err(MathError::CaptainNotInLineup);
    }

    Ok(())
}

/// Computes the deterministic median used by an attestor for one phase.
pub fn median_q9(values: &[i64]) -> Result<i64, MathError> {
    if values.is_empty() {
        return Err(MathError::EmptyObservationSet);
    }

    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        Ok(sorted[middle])
    } else {
        let sum = i128::from(sorted[middle - 1])
            .checked_add(i128::from(sorted[middle]))
            .ok_or(MathError::Overflow)?;
        i64::try_from(sum / 2).map_err(|_| MathError::Overflow)
    }
}

/// A signed price summary produced by one registered attestor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttestorReport {
    pub attestor: [u8; 32],
    pub median_price_q9: i64,
}

/// The deterministic report subset selected by on-chain quorum finalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumSelection {
    pub selected_attestors: Vec<[u8; 32]>,
    pub finalized_price_q9: i64,
    pub spread_bps: u64,
}

/// Selects the largest compatible attestor subset, then applies the frozen
/// minimum-spread and lexicographic-attestor tie-breakers from the protocol.
pub fn select_compatible_quorum(
    reports: &[AttestorReport],
    max_spread_bps: u16,
) -> Result<QuorumSelection, MathError> {
    if reports.len() > 3 {
        return Err(MathError::TooManyAttestors);
    }
    if reports.iter().any(|report| report.median_price_q9 <= 0) {
        return Err(MathError::NonPositivePrice);
    }

    for left in 0..reports.len() {
        if reports[left + 1..]
            .iter()
            .any(|report| report.attestor == reports[left].attestor)
        {
            return Err(MathError::DuplicateAttestor);
        }
    }

    let mut best: Option<QuorumSelection> = None;
    let combinations = 1usize << reports.len();
    for mask in 0..combinations {
        let indexes: Vec<usize> = (0..reports.len())
            .filter(|index| mask & (1 << index) != 0)
            .collect();
        if indexes.len() < 2 {
            continue;
        }

        let prices: Vec<i64> = indexes
            .iter()
            .map(|index| reports[*index].median_price_q9)
            .collect();
        let minimum = *prices.iter().min().ok_or(MathError::NoCompatibleQuorum)?;
        let maximum = *prices.iter().max().ok_or(MathError::NoCompatibleQuorum)?;
        let midpoint = (i128::from(minimum) + i128::from(maximum)) / 2;
        let spread = i128::from(maximum)
            .checked_sub(i128::from(minimum))
            .and_then(|value| value.checked_mul(10_000))
            .and_then(|value| value.checked_div(midpoint))
            .ok_or(MathError::Overflow)?;
        let spread_bps = u64::try_from(spread).map_err(|_| MathError::Overflow)?;
        if spread_bps > u64::from(max_spread_bps) {
            continue;
        }

        let mut selected_attestors: Vec<[u8; 32]> = indexes
            .iter()
            .map(|index| reports[*index].attestor)
            .collect();
        selected_attestors.sort_unstable();
        let finalized_price_q9 = if prices.len() == 2 {
            i64::try_from(midpoint).map_err(|_| MathError::Overflow)?
        } else {
            median_q9(&prices)?
        };

        let candidate = QuorumSelection {
            selected_attestors,
            finalized_price_q9,
            spread_bps,
        };
        let is_better = best.as_ref().is_none_or(|current| {
            candidate.selected_attestors.len() > current.selected_attestors.len()
                || (candidate.selected_attestors.len() == current.selected_attestors.len()
                    && (candidate.spread_bps < current.spread_bps
                        || (candidate.spread_bps == current.spread_bps
                            && candidate.selected_attestors < current.selected_attestors)))
        });
        if is_better {
            best = Some(candidate);
        }
    }

    best.ok_or(MathError::NoCompatibleQuorum)
}

/// Result of one player's deterministic Elo update.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EloUpdate {
    pub rating_before: i32,
    pub opponent_rating: i32,
    pub expected_score: f64,
    pub actual_score: f64,
    pub k_factor: i32,
    pub delta: i32,
    pub rating_after: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EloOutcome {
    Win,
    Draw,
    Loss,
}

/// Applies the V2 Elo rule for one player.
///
/// Elo is an off-chain derived read model rather than settlement truth, so
/// this function intentionally uses the documented floating-point expected
/// score formula. The resulting integer delta is rounded once, and the
/// protocol rating floor is applied after the update.
pub fn apply_elo_update(
    rating_before: i32,
    opponent_rating: i32,
    outcome: EloOutcome,
    completed_rated_games: u32,
) -> Result<EloUpdate, MathError> {
    let rating_gap = f64::from(opponent_rating) - f64::from(rating_before);
    let expected_score = 1.0 / (1.0 + 10.0_f64.powf(rating_gap / 400.0));
    let actual_score = match outcome {
        EloOutcome::Win => 1.0,
        EloOutcome::Draw => 0.5,
        EloOutcome::Loss => 0.0,
    };
    let k_factor = match completed_rated_games {
        0..=4 => 64,
        5..=29 => 32,
        _ => 24,
    };
    let raw_delta = (f64::from(k_factor) * (actual_score - expected_score)).round();
    if !raw_delta.is_finite() || raw_delta < f64::from(i32::MIN) || raw_delta > f64::from(i32::MAX)
    {
        return Err(MathError::Overflow);
    }
    let delta = raw_delta as i32;
    let rating_after = i64::from(rating_before)
        .checked_add(i64::from(delta))
        .ok_or(MathError::Overflow)?
        .max(i64::from(RATING_FLOOR));
    let rating_after = i32::try_from(rating_after).map_err(|_| MathError::Overflow)?;
    Ok(EloUpdate {
        rating_before,
        opponent_rating,
        expected_score,
        actual_score,
        k_factor,
        delta,
        rating_after,
    })
}

/// Inputs required by the deterministic Swiss pairing engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingPlayer {
    pub wallet: [u8; 32],
    pub league_points: u32,
    pub rating: i32,
    pub bye_count: u16,
    pub prior_opponents: Vec<[u8; 32]>,
}

/// A reproducible League round pairing result. Pair indices refer to the
/// original input order, so callers can map the result back to wallet IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingResult {
    pub pairs: Vec<(usize, usize)>,
    pub bye: Option<usize>,
}

/// Inputs for the V2 Ranked nearest-rating matcher.
///
/// Ranked pairing has no League score or bye. The backend removes players
/// with chain/database blockers before calling this function, while this
/// pure layer handles deterministic rating distance and recent-rematch
/// avoidance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedPlayer {
    pub wallet: [u8; 32],
    pub rating: i32,
    pub recent_opponents: Vec<[u8; 32]>,
}

/// Deterministic V2 Ranked output. An odd final player remains unmatched and
/// must stay queued or receive an explicit backend status; Ranked never gives
/// a League-style bye.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedPairingResult {
    pub pairs: Vec<(usize, usize)>,
    pub unmatched: Option<usize>,
}

/// Pairs players greedily by nearest rating while treating a recent rematch
/// as a larger penalty than any rating gap. This is the documented V2 policy:
/// deterministic, explainable, and deliberately simpler than global minimum-
/// weight matching. If avoiding every rematch would leave a player without a
/// legal candidate, the matcher permits the least-cost rematch.
pub fn pair_ranked(players: &[RankedPlayer]) -> RankedPairingResult {
    let mut remaining: Vec<usize> = (0..players.len()).collect();
    let mut pairs = Vec::with_capacity(players.len() / 2);

    while remaining.len() >= 2 {
        let first_position = remaining
            .iter()
            .enumerate()
            .min_by_key(|(_, index)| (players[**index].rating, players[**index].wallet))
            .map(|(position, _)| position)
            .expect("remaining contains at least two players");
        let first = remaining.remove(first_position);

        let candidate_position = remaining
            .iter()
            .enumerate()
            .min_by_key(|(_, candidate)| {
                (
                    players[first]
                        .recent_opponents
                        .contains(&players[**candidate].wallet),
                    (i64::from(players[first].rating) - i64::from(players[**candidate].rating))
                        .unsigned_abs(),
                    players[**candidate].rating,
                    players[**candidate].wallet,
                )
            })
            .map(|(position, _)| position)
            .expect("remaining contains a candidate");
        let second = remaining.remove(candidate_position);
        pairs.push(if players[first].wallet <= players[second].wallet {
            (first, second)
        } else {
            (second, first)
        });
    }

    RankedPairingResult {
        pairs,
        unmatched: remaining.first().copied(),
    }
}

/// Pairs a League round while preferring score proximity and avoiding repeats.
///
/// The search first requires non-repeat pairings. It only permits a repeat if
/// no complete pairing exists under that constraint. This makes the hard
/// fairness rule explicit while keeping the fallback auditable.
pub fn pair_swiss(
    players: &[PairingPlayer],
    pairing_seed: [u8; 32],
) -> Result<PairingResult, MathError> {
    if players.is_empty() {
        return Ok(PairingResult {
            pairs: Vec::new(),
            bye: None,
        });
    }

    let mut bye_candidates: Vec<usize> = (0..players.len()).collect();
    if players.len() % 2 == 1 {
        let lowest_points = players
            .iter()
            .map(|player| player.league_points)
            .min()
            .ok_or(MathError::EmptyObservationSet)?;
        let lowest_group_without_bye: Vec<usize> = bye_candidates
            .iter()
            .copied()
            .filter(|index| {
                players[*index].league_points == lowest_points && players[*index].bye_count == 0
            })
            .collect();
        if !lowest_group_without_bye.is_empty() {
            bye_candidates = lowest_group_without_bye;
        } else if bye_candidates
            .iter()
            .any(|index| players[*index].bye_count == 0)
        {
            // If the entire lowest group has already received a bye, prefer a
            // higher-score player over repeating a bye while another eligible
            // player exists.
            bye_candidates.retain(|index| players[*index].bye_count == 0);
        }
        bye_candidates.sort_by_key(|index| {
            (
                players[*index].league_points,
                tie_break_key(pairing_seed, players[*index].wallet),
            )
        });
    } else {
        bye_candidates.clear();
        bye_candidates.push(usize::MAX);
    }

    for &bye in &bye_candidates {
        let remaining: Vec<usize> = (0..players.len()).filter(|index| *index != bye).collect();
        if let Some(pairs) = search_pairing(players, &remaining, pairing_seed, true) {
            return Ok(PairingResult {
                pairs,
                bye: (bye != usize::MAX).then_some(bye),
            });
        }
    }

    let bye = if players.len() % 2 == 1 {
        bye_candidates.first().copied().unwrap_or(usize::MAX)
    } else {
        usize::MAX
    };
    let remaining: Vec<usize> = (0..players.len()).filter(|index| *index != bye).collect();
    let pairs = search_pairing(players, &remaining, pairing_seed, false)
        .ok_or(MathError::NoCompatibleQuorum)?;
    Ok(PairingResult {
        pairs,
        bye: (bye != usize::MAX).then_some(bye),
    })
}

fn search_pairing(
    players: &[PairingPlayer],
    remaining: &[usize],
    pairing_seed: [u8; 32],
    avoid_repeats: bool,
) -> Option<Vec<(usize, usize)>> {
    if remaining.is_empty() {
        return Some(Vec::new());
    }
    if remaining.len() % 2 == 1 {
        return None;
    }

    let first = *remaining.iter().min_by_key(|index| {
        (
            std::cmp::Reverse(players[**index].league_points),
            tie_break_key(pairing_seed, players[**index].wallet),
        )
    })?;
    let mut candidates: Vec<usize> = remaining
        .iter()
        .copied()
        .filter(|index| *index != first)
        .collect();
    candidates.sort_by_key(|candidate| {
        (
            avoid_repeats && has_met(&players[first], &players[*candidate]),
            players[first]
                .league_points
                .abs_diff(players[*candidate].league_points),
            (i64::from(players[first].rating) - i64::from(players[*candidate].rating))
                .unsigned_abs(),
            tie_break_key(
                pairing_seed,
                pair_wallet_key(players[first].wallet, players[*candidate].wallet),
            ),
        )
    });

    for candidate in candidates {
        if avoid_repeats && has_met(&players[first], &players[candidate]) {
            continue;
        }
        let next: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|index| *index != first && *index != candidate)
            .collect();
        if let Some(mut pairs) = search_pairing(players, &next, pairing_seed, avoid_repeats) {
            pairs.push(if first < candidate {
                (first, candidate)
            } else {
                (candidate, first)
            });
            pairs.sort_unstable();
            return Some(pairs);
        }
    }
    None
}

fn has_met(player: &PairingPlayer, opponent: &PairingPlayer) -> bool {
    player.prior_opponents.contains(&opponent.wallet)
        || opponent.prior_opponents.contains(&player.wallet)
}

fn pair_wallet_key(left: [u8; 32], right: [u8; 32]) -> [u8; 64] {
    let mut key = [0u8; 64];
    if left <= right {
        key[..32].copy_from_slice(&left);
        key[32..].copy_from_slice(&right);
    } else {
        key[..32].copy_from_slice(&right);
        key[32..].copy_from_slice(&left);
    }
    key
}

fn tie_break_key<T: AsRef<[u8]>>(seed: [u8; 32], value: T) -> u64 {
    let mut bytes = Vec::with_capacity(64 + value.as_ref().len());
    bytes.extend_from_slice(b"TICKERSIX_PAIRING_V1\0");
    bytes.extend_from_slice(&seed);
    bytes.extend_from_slice(value.as_ref());
    let digest = Sha256::digest(bytes);
    u64::from_le_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 has at least eight bytes"),
    )
}

/// Builds and hashes the canonical lineup commitment preimage.
pub fn commitment(
    program_id: [u8; 32],
    battle: [u8; 32],
    player: [u8; 32],
    registry_version: u32,
    mut asset_ids: [u16; LINEUP_SIZE],
    captain_asset_id: u16,
    salt: [u8; 32],
) -> [u8; 32] {
    asset_ids.sort_unstable();

    let mut bytes = Vec::with_capacity(166);
    bytes.extend_from_slice(b"TICKERSIX_LINEUP_V1\0");
    bytes.extend_from_slice(&program_id);
    bytes.extend_from_slice(&battle);
    bytes.extend_from_slice(&player);
    bytes.extend_from_slice(&registry_version.to_le_bytes());
    for asset_id in asset_ids {
        bytes.extend_from_slice(&asset_id.to_le_bytes());
    }
    bytes.extend_from_slice(&captain_asset_id.to_le_bytes());
    bytes.extend_from_slice(&salt);

    let digest = Sha256::digest(bytes);
    digest.into()
}

/// Builds the exact message signed by a registered price attestor.
///
/// The message includes every value that can affect interpretation of the
/// report, including the frozen mint, policy versions, phase window, evidence
/// root, and report time. The native Ed25519 instruction supplies the signing
/// public key; it is therefore intentionally not duplicated in this payload.
#[allow(clippy::too_many_arguments)]
pub fn attestation_message(
    program_id: [u8; 32],
    market_round: [u8; 32],
    round_asset: [u8; 32],
    asset_id: u16,
    phase: u8,
    scoring_mint: [u8; 32],
    price_policy_version: u16,
    market_quality_policy_version: u16,
    attestor_set_version: u16,
    median_price_q9: i64,
    accepted_observation_count: u16,
    unique_source_block_count: u16,
    first_source_block_id: u64,
    last_source_block_id: u64,
    evidence_root: [u8; 32],
    observation_window_start: i64,
    observation_window_end: i64,
    report_created_at: i64,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(256);
    bytes.extend_from_slice(ATTESTATION_DOMAIN);
    bytes.extend_from_slice(&program_id);
    bytes.extend_from_slice(&market_round);
    bytes.extend_from_slice(&round_asset);
    bytes.extend_from_slice(&asset_id.to_le_bytes());
    bytes.push(phase);
    bytes.extend_from_slice(&scoring_mint);
    bytes.extend_from_slice(&price_policy_version.to_le_bytes());
    bytes.extend_from_slice(&market_quality_policy_version.to_le_bytes());
    bytes.extend_from_slice(&attestor_set_version.to_le_bytes());
    bytes.extend_from_slice(&median_price_q9.to_le_bytes());
    bytes.extend_from_slice(&accepted_observation_count.to_le_bytes());
    bytes.extend_from_slice(&unique_source_block_count.to_le_bytes());
    bytes.extend_from_slice(&first_source_block_id.to_le_bytes());
    bytes.extend_from_slice(&last_source_block_id.to_le_bytes());
    bytes.extend_from_slice(&evidence_root);
    bytes.extend_from_slice(&observation_window_start.to_le_bytes());
    bytes.extend_from_slice(&observation_window_end.to_le_bytes());
    bytes.extend_from_slice(&report_created_at.to_le_bytes());
    bytes
}

/// One accepted raw observation included in an attestor's evidence root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceSample {
    pub market_round: [u8; 32],
    pub asset_id: u16,
    pub phase: u8,
    pub scoring_mint: [u8; 32],
    pub source_block_id: u64,
    pub observed_at_unix_ms: i64,
    pub price_q9: i64,
}

/// Hashes one canonical price-sample leaf using the specification's domain.
pub fn price_sample_leaf(sample: &PriceSample) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(128);
    bytes.extend_from_slice(PRICE_SAMPLE_DOMAIN);
    bytes.extend_from_slice(&sample.market_round);
    bytes.extend_from_slice(&sample.asset_id.to_le_bytes());
    bytes.push(sample.phase);
    bytes.extend_from_slice(&sample.scoring_mint);
    bytes.extend_from_slice(&sample.source_block_id.to_le_bytes());
    bytes.extend_from_slice(&sample.observed_at_unix_ms.to_le_bytes());
    bytes.extend_from_slice(&sample.price_q9.to_le_bytes());
    Sha256::digest(bytes).into()
}

/// Builds a deterministic canonical-list evidence root.
///
/// The source specification permits a canonical-list hash as an alternative
/// to a Merkle tree. Sorting samples before hashing gives the same root even
/// when workers persist accepted polls in different arrival orders.
pub fn evidence_root(samples: &[PriceSample]) -> [u8; 32] {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable_by_key(|sample| {
        (
            sample.asset_id,
            sample.source_block_id,
            sample.observed_at_unix_ms,
            sample.price_q9,
        )
    });

    let mut bytes = Vec::with_capacity(64 + ordered.len() * 32);
    bytes.extend_from_slice(PRICE_EVIDENCE_LIST_DOMAIN);
    bytes.extend_from_slice(&(ordered.len() as u32).to_le_bytes());
    for sample in ordered {
        bytes.extend_from_slice(&price_sample_leaf(&sample));
    }
    Sha256::digest(bytes).into()
}
