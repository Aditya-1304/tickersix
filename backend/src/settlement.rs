//! Deterministic, retry-safe settlement orchestration.
//!
//! This module plans the next permissionless or coordinator-authorized
//! operation from an indexed snapshot. It does not hold keys or submit
//! transactions. A caller refreshes the snapshot after each confirmed chain
//! transition and invokes the planner again; resolved phases and Battles then
//! disappear from the plan instead of being replayed with replacement data.

use std::{error::Error, fmt, fs, path::Path};

use serde::{Deserialize, Serialize};

/// Phase 2 Slice 1 only dispatches the permanent Jupiter baseline. Pyth is
/// represented so snapshots can be rejected explicitly until its Gate 0B
/// verification path is enabled in Slice 2.
pub use crate::proof::SourceKind as SettlementSourceKind;

fn default_settlement_source() -> SettlementSourceKind {
    SettlementSourceKind::JupiterTokenSpotV1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Phase {
    Start,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RoundState {
    Ended,
    Settling,
    Finalized,
    Voided,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PhaseStatus {
    Pending,
    Finalized,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetSettlementState {
    pub asset_id: u16,
    pub start: PhaseStatus,
    pub start_deadline: i64,
    pub start_has_compatible_quorum: bool,
    pub end: PhaseStatus,
    pub end_deadline: i64,
    pub end_has_compatible_quorum: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BattleSettlementState {
    pub battle_id: u64,
    pub result_pending: bool,
    pub reveal_deadline: i64,
    pub side_a_revealed: bool,
    pub side_b_revealed: bool,
    pub side_a_score_finalized: bool,
    pub side_b_score_finalized: bool,
    pub selected_asset_ids: Vec<u16>,
    pub selected_asset_unavailable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementSnapshot {
    /// Frozen source selected by the market round. A source-agnostic planner
    /// is unsafe because it can route a valid snapshot to the wrong verifier.
    #[serde(default = "default_settlement_source")]
    pub source_kind: SettlementSourceKind,
    pub now_unix_secs: i64,
    pub round_state: RoundState,
    pub rated_battle_count: u32,
    pub resolved_battle_count: u32,
    pub assets: Vec<AssetSettlementState>,
    pub battles: Vec<BattleSettlementState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SettlementAction {
    AdvanceRoundToSettling,
    FinalizeJupiterPhase { asset_id: u16, phase: Phase },
    MarkJupiterPhaseUnavailable { asset_id: u16, phase: Phase },
    ForfeitBattle { battle_id: u64 },
    VoidBattlePriceUnavailable { battle_id: u64 },
    SettleSideScore { battle_id: u64, side_index: u8 },
    FinalizeBattle { battle_id: u64 },
    FinalizeMarketRound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettlementError {
    UnsupportedSource,
    DuplicateAsset,
    DuplicateBattle,
    InvalidBattleAsset,
    CountMismatch,
    InvalidRoundState,
}

impl fmt::Display for SettlementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsupportedSource => {
                "settlement source is not enabled by the Jupiter Phase 2 slice"
            }
            Self::DuplicateAsset => "settlement snapshot contains a duplicate asset",
            Self::DuplicateBattle => "settlement snapshot contains a duplicate Battle",
            Self::InvalidBattleAsset => "Battle references an invalid or duplicate asset",
            Self::CountMismatch => "settlement Battle count does not match the round",
            Self::InvalidRoundState => "settlement snapshot has an unsupported round state",
        };
        formatter.write_str(message)
    }
}

impl Error for SettlementError {}

/// Computes the next deterministic operation batch. Actions are sorted by
/// asset/Battle identity and no action is produced for already resolved state.
pub fn plan_settlement(
    snapshot: &SettlementSnapshot,
) -> Result<Vec<SettlementAction>, SettlementError> {
    validate_snapshot(snapshot)?;
    let mut actions = Vec::new();

    if snapshot.round_state == RoundState::Ended {
        actions.push(SettlementAction::AdvanceRoundToSettling);
        return Ok(actions);
    }
    if snapshot.round_state != RoundState::Settling {
        return Ok(actions);
    }

    let mut assets = snapshot.assets.clone();
    assets.sort_unstable_by_key(|asset| asset.asset_id);
    for asset in &assets {
        plan_phase(
            &mut actions,
            asset.asset_id,
            Phase::Start,
            asset.start,
            asset.start_deadline,
            asset.start_has_compatible_quorum,
            snapshot.now_unix_secs,
        );
        plan_phase(
            &mut actions,
            asset.asset_id,
            Phase::End,
            asset.end,
            asset.end_deadline,
            asset.end_has_compatible_quorum,
            snapshot.now_unix_secs,
        );
    }

    let mut battles = snapshot.battles.clone();
    battles.sort_unstable_by_key(|battle| battle.battle_id);
    for battle in &battles {
        if !battle.result_pending {
            continue;
        }
        if battle.selected_asset_unavailable && (battle.side_a_revealed || battle.side_b_revealed) {
            actions.push(SettlementAction::VoidBattlePriceUnavailable {
                battle_id: battle.battle_id,
            });
        } else if !battle.side_a_revealed || !battle.side_b_revealed {
            if snapshot.now_unix_secs > battle.reveal_deadline {
                actions.push(SettlementAction::ForfeitBattle {
                    battle_id: battle.battle_id,
                });
            }
        } else if battle.side_a_score_finalized && battle.side_b_score_finalized {
            actions.push(SettlementAction::FinalizeBattle {
                battle_id: battle.battle_id,
            });
        } else if all_assets_available(&battle.selected_asset_ids, &assets) {
            if !battle.side_a_score_finalized {
                actions.push(SettlementAction::SettleSideScore {
                    battle_id: battle.battle_id,
                    side_index: 0,
                });
            }
            if !battle.side_b_score_finalized {
                actions.push(SettlementAction::SettleSideScore {
                    battle_id: battle.battle_id,
                    side_index: 1,
                });
            }
        }
    }

    let all_assets_resolved = assets.iter().all(|asset| {
        matches!(
            asset.start,
            PhaseStatus::Finalized | PhaseStatus::Unavailable
        ) && matches!(asset.end, PhaseStatus::Finalized | PhaseStatus::Unavailable)
    });
    let all_battles_resolved = snapshot.resolved_battle_count == snapshot.rated_battle_count;
    if all_assets_resolved && all_battles_resolved && actions.is_empty() {
        actions.push(SettlementAction::FinalizeMarketRound);
    }
    Ok(actions)
}

fn plan_phase(
    actions: &mut Vec<SettlementAction>,
    asset_id: u16,
    phase: Phase,
    status: PhaseStatus,
    deadline: i64,
    has_compatible_quorum: bool,
    now: i64,
) {
    if status != PhaseStatus::Pending || now <= deadline {
        return;
    }
    if has_compatible_quorum {
        actions.push(SettlementAction::FinalizeJupiterPhase { asset_id, phase });
    } else {
        actions.push(SettlementAction::MarkJupiterPhaseUnavailable { asset_id, phase });
    }
}

fn all_assets_available(asset_ids: &[u16], assets: &[AssetSettlementState]) -> bool {
    asset_ids.iter().all(|asset_id| {
        assets.iter().any(|asset| {
            asset.asset_id == *asset_id
                && asset.start == PhaseStatus::Finalized
                && asset.end == PhaseStatus::Finalized
        })
    })
}

fn validate_snapshot(snapshot: &SettlementSnapshot) -> Result<(), SettlementError> {
    if snapshot.source_kind != SettlementSourceKind::JupiterTokenSpotV1 {
        return Err(SettlementError::UnsupportedSource);
    }
    let mut asset_ids = Vec::with_capacity(snapshot.assets.len());
    for asset in &snapshot.assets {
        if asset_ids.contains(&asset.asset_id) {
            return Err(SettlementError::DuplicateAsset);
        }
        asset_ids.push(asset.asset_id);
    }
    let mut battle_ids = Vec::with_capacity(snapshot.battles.len());
    for battle in &snapshot.battles {
        if battle_ids.contains(&battle.battle_id) {
            return Err(SettlementError::DuplicateBattle);
        }
        battle_ids.push(battle.battle_id);
        let mut selected = battle.selected_asset_ids.clone();
        selected.sort_unstable();
        selected.dedup();
        if selected.len() != 6
            || selected.len() != battle.selected_asset_ids.len()
            || selected
                .iter()
                .any(|asset_id| !asset_ids.contains(asset_id))
        {
            return Err(SettlementError::InvalidBattleAsset);
        }
    }
    if snapshot.rated_battle_count < snapshot.resolved_battle_count
        || usize::try_from(snapshot.rated_battle_count).ok() != Some(snapshot.battles.len())
    {
        return Err(SettlementError::CountMismatch);
    }
    if matches!(
        snapshot.round_state,
        RoundState::Finalized | RoundState::Voided
    ) && snapshot.resolved_battle_count != snapshot.rated_battle_count
    {
        return Err(SettlementError::InvalidRoundState);
    }
    Ok(())
}

/// Reads a planner snapshot from JSON and prints the next action batch. The
/// command is intentionally a planner, not a transaction sender.
pub fn plan_from_file(path: impl AsRef<Path>) -> Result<(), Box<dyn Error>> {
    let snapshot: SettlementSnapshot = serde_json::from_str(&fs::read_to_string(path)?)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&plan_settlement(&snapshot)?)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(asset_id: u16) -> AssetSettlementState {
        AssetSettlementState {
            asset_id,
            start: PhaseStatus::Finalized,
            start_deadline: 10,
            start_has_compatible_quorum: true,
            end: PhaseStatus::Finalized,
            end_deadline: 20,
            end_has_compatible_quorum: true,
        }
    }

    fn battle() -> BattleSettlementState {
        BattleSettlementState {
            battle_id: 1,
            result_pending: true,
            reveal_deadline: 30,
            side_a_revealed: true,
            side_b_revealed: true,
            side_a_score_finalized: true,
            side_b_score_finalized: true,
            selected_asset_ids: vec![1, 2, 3, 4, 5, 6],
            selected_asset_unavailable: false,
        }
    }

    #[test]
    fn planner_is_deterministic_and_emits_no_action_for_resolved_state() {
        let snapshot = SettlementSnapshot {
            source_kind: SettlementSourceKind::JupiterTokenSpotV1,
            now_unix_secs: 40,
            round_state: RoundState::Finalized,
            rated_battle_count: 1,
            resolved_battle_count: 1,
            assets: (1..=6).map(asset).collect(),
            battles: vec![BattleSettlementState {
                result_pending: false,
                ..battle()
            }],
        };
        assert_eq!(plan_settlement(&snapshot).unwrap(), Vec::new());
    }

    #[test]
    fn planner_marks_failed_phase_before_voiding_affected_battle() {
        let mut failed_asset = asset(1);
        failed_asset.end = PhaseStatus::Pending;
        failed_asset.end_deadline = 20;
        failed_asset.end_has_compatible_quorum = false;
        let snapshot = SettlementSnapshot {
            source_kind: SettlementSourceKind::JupiterTokenSpotV1,
            now_unix_secs: 40,
            round_state: RoundState::Settling,
            rated_battle_count: 1,
            resolved_battle_count: 0,
            assets: std::iter::once(failed_asset)
                .chain((2..=6).map(asset))
                .collect(),
            battles: vec![BattleSettlementState {
                selected_asset_unavailable: true,
                ..battle()
            }],
        };
        assert_eq!(
            plan_settlement(&snapshot).unwrap(),
            vec![
                SettlementAction::MarkJupiterPhaseUnavailable {
                    asset_id: 1,
                    phase: Phase::End
                },
                SettlementAction::VoidBattlePriceUnavailable { battle_id: 1 }
            ]
        );
    }

    #[test]
    fn planner_fails_closed_for_sources_not_enabled_in_jupiter_slice() {
        let snapshot = SettlementSnapshot {
            source_kind: SettlementSourceKind::PythProVerifiedV1,
            now_unix_secs: 40,
            round_state: RoundState::Settling,
            rated_battle_count: 1,
            resolved_battle_count: 1,
            assets: (1..=6).map(asset).collect(),
            battles: vec![BattleSettlementState {
                result_pending: false,
                ..battle()
            }],
        };

        assert_eq!(
            plan_settlement(&snapshot),
            Err(SettlementError::UnsupportedSource)
        );
    }
}
