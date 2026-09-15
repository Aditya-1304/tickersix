//! Deterministic League standings derived from finalized Battle facts.
//!
//! This module is intentionally independent of PostgreSQL and HTTP. It owns
//! only the arithmetic and ordering rules that must remain reproducible when
//! a standings response is rebuilt from indexed Solana Battle state.

use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use sha2::{Digest, Sha256};

pub const LEAGUE_SCORE_MARGIN_CAP_Q9: i64 = 50_000_000;

/// Final result values accepted from the indexed Battle projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeagueBattleResult {
    PlayerA,
    PlayerB,
    Draw,
    ForfeitA,
    ForfeitB,
    BothForfeit,
    Voided,
}

/// One finalized League Battle fact used by the standings calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeagueBattleFact {
    pub round_no: i32,
    pub player_a: [u8; 32],
    pub player_b: [u8; 32],
    pub result: LeagueBattleResult,
    pub score_a_q9: Option<i64>,
    pub score_b_q9: Option<i64>,
}

/// One persisted bye fact. A bye is a three-point standings event, not a
/// Battle, and therefore does not contribute to SOS, head-to-head, or margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeagueByeFact {
    pub round_no: i32,
    pub wallet: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StandingsError {
    EmptyMemberSet,
    DuplicateMember,
    InvalidRound,
    UnknownPlayer,
    SamePlayer,
    DuplicateRoundParticipation,
    ByeBattleConflict,
    DuplicateBye,
    MissingScore,
    InvalidScore,
    ScoreResultMismatch,
    Overflow,
}

impl fmt::Display for StandingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyMemberSet => "League standings require at least one member",
            Self::DuplicateMember => "League standings contain a duplicate member",
            Self::InvalidRound => "League standings contain an invalid round number",
            Self::UnknownPlayer => "League Battle references a non-member wallet",
            Self::SamePlayer => "League Battle cannot contain the same player twice",
            Self::DuplicateRoundParticipation => {
                "a League member appears more than once in one round"
            }
            Self::ByeBattleConflict => {
                "a League member cannot receive a bye and a Battle in one round"
            }
            Self::DuplicateBye => "a League member cannot receive two byes in one round",
            Self::MissingScore => "a played League Battle is missing a finalized score",
            Self::InvalidScore => "a finalized League Battle score cannot be negative",
            Self::ScoreResultMismatch => "League Battle result does not match finalized scores",
            Self::Overflow => "League standings arithmetic overflowed",
        })
    }
}

impl std::error::Error for StandingsError {}

/// One fully derived League standing, ordered according to the V2 tie-breaker
/// chain: League Points, Buchholz SOS, head-to-head, capped margin, then the
/// deterministic final seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeagueStanding {
    pub rank: u32,
    pub wallet: [u8; 32],
    pub league_points: u32,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
    pub byes: u32,
    pub buchholz_sos: u32,
    pub head_to_head_points: u32,
    pub cumulative_margin_q9: i64,
}

#[derive(Debug, Clone)]
struct StandingState {
    wallet: [u8; 32],
    league_points: u32,
    wins: u32,
    draws: u32,
    losses: u32,
    byes: u32,
    buchholz_sos: u32,
    head_to_head_points: HashMap<[u8; 32], u32>,
    cumulative_margin_q9: i64,
}

/// Derives deterministic standings from active members, terminal Battle facts,
/// and persisted byes. The caller supplies a final seed recorded for the
/// League; this function never uses wall-clock time or database row order.
pub fn calculate_standings(
    members: &[[u8; 32]],
    battles: &[LeagueBattleFact],
    byes: &[LeagueByeFact],
    final_seed: [u8; 32],
) -> Result<Vec<LeagueStanding>, StandingsError> {
    if members.is_empty() {
        return Err(StandingsError::EmptyMemberSet);
    }

    let mut member_indexes = HashMap::with_capacity(members.len());
    let mut states = Vec::with_capacity(members.len());
    for &wallet in members {
        if member_indexes.insert(wallet, states.len()).is_some() {
            return Err(StandingsError::DuplicateMember);
        }
        states.push(StandingState {
            wallet,
            league_points: 0,
            wins: 0,
            draws: 0,
            losses: 0,
            byes: 0,
            buchholz_sos: 0,
            head_to_head_points: HashMap::new(),
            cumulative_margin_q9: 0,
        });
    }

    let mut round_participants = HashSet::new();
    for battle in battles {
        if battle.round_no <= 0 {
            return Err(StandingsError::InvalidRound);
        }
        let a_index = *member_indexes
            .get(&battle.player_a)
            .ok_or(StandingsError::UnknownPlayer)?;
        let b_index = *member_indexes
            .get(&battle.player_b)
            .ok_or(StandingsError::UnknownPlayer)?;
        if battle.player_a == battle.player_b {
            return Err(StandingsError::SamePlayer);
        }
        if !round_participants.insert((battle.round_no, battle.player_a))
            || !round_participants.insert((battle.round_no, battle.player_b))
        {
            return Err(StandingsError::DuplicateRoundParticipation);
        }

        let (points_a, points_b, win_a, win_b, draw, counts_as_opponent) = match battle.result {
            LeagueBattleResult::PlayerA => (3, 0, true, false, false, true),
            LeagueBattleResult::PlayerB => (0, 3, false, true, false, true),
            LeagueBattleResult::Draw => (1, 1, false, false, true, true),
            LeagueBattleResult::ForfeitA => (0, 3, false, true, false, true),
            LeagueBattleResult::ForfeitB => (3, 0, true, false, false, true),
            LeagueBattleResult::BothForfeit => (0, 0, false, false, false, true),
            LeagueBattleResult::Voided => (0, 0, false, false, false, false),
        };
        add_points(&mut states[a_index].league_points, points_a)?;
        add_points(&mut states[b_index].league_points, points_b)?;

        if win_a {
            states[a_index].wins = states[a_index]
                .wins
                .checked_add(1)
                .ok_or(StandingsError::Overflow)?;
            states[b_index].losses = states[b_index]
                .losses
                .checked_add(1)
                .ok_or(StandingsError::Overflow)?;
        } else if win_b {
            states[b_index].wins = states[b_index]
                .wins
                .checked_add(1)
                .ok_or(StandingsError::Overflow)?;
            states[a_index].losses = states[a_index]
                .losses
                .checked_add(1)
                .ok_or(StandingsError::Overflow)?;
        } else if draw {
            states[a_index].draws = states[a_index]
                .draws
                .checked_add(1)
                .ok_or(StandingsError::Overflow)?;
            states[b_index].draws = states[b_index]
                .draws
                .checked_add(1)
                .ok_or(StandingsError::Overflow)?;
        } else if matches!(battle.result, LeagueBattleResult::BothForfeit) {
            states[a_index].losses = states[a_index]
                .losses
                .checked_add(1)
                .ok_or(StandingsError::Overflow)?;
            states[b_index].losses = states[b_index]
                .losses
                .checked_add(1)
                .ok_or(StandingsError::Overflow)?;
        }

        if counts_as_opponent {
            let current_a = states[a_index]
                .head_to_head_points
                .get(&battle.player_b)
                .copied()
                .unwrap_or_default();
            states[a_index].head_to_head_points.insert(
                battle.player_b,
                current_a
                    .checked_add(points_a)
                    .ok_or(StandingsError::Overflow)?,
            );
            let current_b = states[b_index]
                .head_to_head_points
                .get(&battle.player_a)
                .copied()
                .unwrap_or_default();
            states[b_index].head_to_head_points.insert(
                battle.player_a,
                current_b
                    .checked_add(points_b)
                    .ok_or(StandingsError::Overflow)?,
            );
        }

        if matches!(
            battle.result,
            LeagueBattleResult::PlayerA | LeagueBattleResult::PlayerB | LeagueBattleResult::Draw
        ) {
            let score_a = battle.score_a_q9.ok_or(StandingsError::MissingScore)?;
            let score_b = battle.score_b_q9.ok_or(StandingsError::MissingScore)?;
            if score_a < 0 || score_b < 0 {
                return Err(StandingsError::InvalidScore);
            }
            let score_difference = i128::from(score_a) - i128::from(score_b);
            let expected_result = score_difference.cmp(&0);
            let result_matches = match battle.result {
                LeagueBattleResult::PlayerA => expected_result.is_gt(),
                LeagueBattleResult::PlayerB => expected_result.is_lt(),
                LeagueBattleResult::Draw => expected_result.is_eq(),
                _ => unreachable!(),
            };
            if !result_matches {
                return Err(StandingsError::ScoreResultMismatch);
            }
            let margin = score_difference
                .clamp(
                    -i128::from(LEAGUE_SCORE_MARGIN_CAP_Q9),
                    i128::from(LEAGUE_SCORE_MARGIN_CAP_Q9),
                )
                .try_into()
                .map_err(|_| StandingsError::Overflow)?;
            states[a_index].cumulative_margin_q9 = states[a_index]
                .cumulative_margin_q9
                .checked_add(margin)
                .ok_or(StandingsError::Overflow)?;
            states[b_index].cumulative_margin_q9 = states[b_index]
                .cumulative_margin_q9
                .checked_sub(margin)
                .ok_or(StandingsError::Overflow)?;
        }
    }

    let mut bye_rounds = HashSet::new();
    for bye in byes {
        if bye.round_no <= 0 {
            return Err(StandingsError::InvalidRound);
        }
        let index = *member_indexes
            .get(&bye.wallet)
            .ok_or(StandingsError::UnknownPlayer)?;
        if !bye_rounds.insert((bye.round_no, bye.wallet)) {
            return Err(StandingsError::DuplicateBye);
        }
        if round_participants.contains(&(bye.round_no, bye.wallet)) {
            return Err(StandingsError::ByeBattleConflict);
        }
        states[index].byes = states[index]
            .byes
            .checked_add(1)
            .ok_or(StandingsError::Overflow)?;
        add_points(&mut states[index].league_points, 3)?;
    }

    for index in 0..states.len() {
        let opponents = states[index]
            .head_to_head_points
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for opponent in opponents {
            let opponent_index = *member_indexes
                .get(&opponent)
                .ok_or(StandingsError::UnknownPlayer)?;
            states[index].buchholz_sos = states[index]
                .buchholz_sos
                .checked_add(states[opponent_index].league_points)
                .ok_or(StandingsError::Overflow)?;
        }
    }

    let mut order = (0..states.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        states[*right]
            .league_points
            .cmp(&states[*left].league_points)
            .then_with(|| states[*right].buchholz_sos.cmp(&states[*left].buchholz_sos))
    });

    let mut final_order = Vec::with_capacity(order.len());
    let mut position = 0;
    while position < order.len() {
        let points = states[order[position]].league_points;
        let sos = states[order[position]].buchholz_sos;
        let end = order[position..]
            .iter()
            .position(|index| {
                states[*index].league_points != points || states[*index].buchholz_sos != sos
            })
            .map(|offset| position + offset)
            .unwrap_or(order.len());
        let group = order[position..end].to_vec();
        let head_to_head = group
            .iter()
            .map(|index| {
                (
                    *index,
                    head_to_head_within_group(&states[*index], &group, &states),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut sorted_group = group;
        sorted_group.sort_by(|left, right| {
            head_to_head[right]
                .cmp(&head_to_head[left])
                .then_with(|| {
                    states[*right]
                        .cumulative_margin_q9
                        .cmp(&states[*left].cumulative_margin_q9)
                })
                .then_with(|| {
                    final_tie_break_key(final_seed, states[*left].wallet)
                        .cmp(&final_tie_break_key(final_seed, states[*right].wallet))
                })
                .then_with(|| states[*left].wallet.cmp(&states[*right].wallet))
        });
        final_order.extend(sorted_group);
        position = end;
    }

    let ordered_indices = final_order;
    ordered_indices
        .iter()
        .copied()
        .enumerate()
        .map(|(position, index)| {
            let points = states[index].league_points;
            let sos = states[index].buchholz_sos;
            let tie_group = final_order_for_tie_group(&states, points, sos);
            Ok(LeagueStanding {
                rank: u32::try_from(position + 1).map_err(|_| StandingsError::Overflow)?,
                wallet: states[index].wallet,
                league_points: states[index].league_points,
                wins: states[index].wins,
                draws: states[index].draws,
                losses: states[index].losses,
                byes: states[index].byes,
                buchholz_sos: states[index].buchholz_sos,
                head_to_head_points: head_to_head_within_group(&states[index], &tie_group, &states),
                cumulative_margin_q9: states[index].cumulative_margin_q9,
            })
        })
        .collect()
}

fn add_points(points: &mut u32, amount: u32) -> Result<(), StandingsError> {
    *points = points.checked_add(amount).ok_or(StandingsError::Overflow)?;
    Ok(())
}

fn head_to_head_within_group(
    state: &StandingState,
    group: &[usize],
    states: &[StandingState],
) -> u32 {
    group
        .iter()
        .filter_map(|index| state.head_to_head_points.get(&states[*index].wallet))
        .copied()
        .fold(0, u32::saturating_add)
}

fn final_order_for_tie_group(
    states: &[StandingState],
    league_points: u32,
    buchholz_sos: u32,
) -> Vec<usize> {
    states
        .iter()
        .enumerate()
        .filter_map(|(index, state)| {
            (state.league_points == league_points && state.buchholz_sos == buchholz_sos)
                .then_some(index)
        })
        .collect()
}

fn final_tie_break_key(seed: [u8; 32], wallet: [u8; 32]) -> u64 {
    let mut bytes = Vec::with_capacity(64);
    bytes.extend_from_slice(b"TICKERSIX_LEAGUE_STANDINGS_V1\0");
    bytes.extend_from_slice(&seed);
    bytes.extend_from_slice(&wallet);
    let digest = Sha256::digest(bytes);
    u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 has eight bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(id: u8) -> [u8; 32] {
        [id; 32]
    }

    #[test]
    fn standings_apply_points_byes_sos_and_capped_score_margin() {
        let standings = calculate_standings(
            &[player(1), player(2), player(3), player(4)],
            &[
                LeagueBattleFact {
                    round_no: 1,
                    player_a: player(1),
                    player_b: player(2),
                    result: LeagueBattleResult::PlayerA,
                    score_a_q9: Some(80_000_000),
                    score_b_q9: Some(0),
                },
                LeagueBattleFact {
                    round_no: 1,
                    player_a: player(3),
                    player_b: player(4),
                    result: LeagueBattleResult::PlayerA,
                    score_a_q9: Some(10_000_000),
                    score_b_q9: Some(0),
                },
                LeagueBattleFact {
                    round_no: 2,
                    player_a: player(1),
                    player_b: player(3),
                    result: LeagueBattleResult::Draw,
                    score_a_q9: Some(1_000_000),
                    score_b_q9: Some(1_000_000),
                },
            ],
            &[LeagueByeFact {
                round_no: 2,
                wallet: player(4),
            }],
            [7; 32],
        )
        .unwrap();

        assert_eq!(standings[0].wallet, player(3));
        assert_eq!(standings[0].league_points, 4);
        assert_eq!(standings[0].buchholz_sos, 7);
        assert_eq!(standings[0].cumulative_margin_q9, 10_000_000);

        assert_eq!(standings[1].wallet, player(1));
        assert_eq!(standings[1].league_points, 4);
        assert_eq!(standings[1].buchholz_sos, 4);
        assert_eq!(standings[1].cumulative_margin_q9, 50_000_000);

        assert_eq!(standings[2].wallet, player(4));
        assert_eq!(standings[2].league_points, 3);
        assert_eq!(standings[2].byes, 1);
        assert_eq!(standings[3].wallet, player(2));
    }
}
