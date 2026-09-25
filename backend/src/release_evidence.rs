//! Beta evidence contracts for release readiness.
//!
//! The beta gate separates reproducible contract validation from evidence
//! collected during real Devnet use. A checked-in template can prove that the
//! release record has the expected shape, but it must never be presented as a
//! completed tester, proof-round, screenshot, or simulation result.

use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};

pub const BETA_EVIDENCE_SCHEMA_VERSION: u16 = 1;
pub const MIN_TESTERS: usize = 8;
pub const MAX_TESTERS: usize = 20;
pub const MIN_BATTLE_WINDOWS: usize = 2;
pub const REQUIRED_LEAGUE_PLAYERS: usize = 100;
pub const DEVNET_CLUSTER: &str = "devnet";
pub const PUBLIC_DEVNET_RPC: &str = "public_devnet";
pub const JUPITER_SOURCE_KIND: &str = "JUPITER_TOKEN_SPOT_V1";
pub const PYTH_SOURCE_KIND: &str = "PYTH_PRO_VERIFIED_V1";
pub const CONTRACT_MODE: &str = "contract";
pub const EVIDENCE_MODE: &str = "evidence";

#[derive(Debug, Clone, Deserialize)]
pub struct BetaEvidenceManifest {
    pub schema_version: u16,
    pub mode: String,
    pub cluster: String,
    pub infrastructure: InfrastructureEvidence,
    pub testers: TesterEvidence,
    pub battle_windows: Vec<BattleWindowEvidence>,
    pub jupiter_proof: ProofRoundEvidence,
    pub pyth_proof: Option<PythProofEvidence>,
    pub private_market_demo: PrivateMarketDemoEvidence,
    pub league_simulation: LeagueSimulationEvidence,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InfrastructureEvidence {
    pub paid_services: bool,
    pub rpc_kind: String,
    pub secrets_in_manifest: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TesterEvidence {
    pub recorded: bool,
    pub count: usize,
    pub participant_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BattleWindowEvidence {
    pub id: String,
    pub opened_at: String,
    pub closed_at: String,
    pub recorded: bool,
    pub battle_count: usize,
    pub source_kind: String,
    pub evidence_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProofRoundEvidence {
    pub recorded: bool,
    pub network: String,
    pub battle_id: String,
    pub evidence_path: String,
    pub transaction_signature: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PythProofEvidence {
    pub enabled: bool,
    pub recorded: bool,
    pub network: String,
    pub battle_id: String,
    pub evidence_path: String,
    pub transaction_signature: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrivateMarketDemoEvidence {
    pub recorded: bool,
    pub screenshot_paths: Vec<String>,
    pub separate_public_ranked_domain: bool,
    pub public_elo_mutated: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeagueSimulationEvidence {
    pub completed: bool,
    pub player_count: usize,
    pub unique_players: usize,
    pub round_count: usize,
    pub one_active_pairing_per_player: bool,
    pub evidence_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EvidenceCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BetaEvidenceReport {
    pub scope: &'static str,
    pub mode: String,
    pub contract_valid: bool,
    pub release_ready: bool,
    pub checks: Vec<EvidenceCheck>,
}

#[derive(Debug, Clone, Copy)]
enum CheckKind {
    Contract,
    ExternalEvidence,
}

/// Validates the beta evidence record without inventing external evidence.
///
/// Contract mode is intentionally useful for CI: it verifies the shape and
/// safety invariants of the record while reporting release readiness as false.
/// Evidence mode is the operator-supplied record used for the actual beta gate.
/// The distinction prevents checked-in templates from being mistaken for real
/// tester sessions, Devnet proofs, screenshots, or League runs.
pub fn validate_beta_evidence(manifest: &BetaEvidenceManifest) -> BetaEvidenceReport {
    let mut checks = Vec::new();
    let mut contract_valid = true;
    let mut external_evidence_valid = true;
    let known_mode = matches!(manifest.mode.as_str(), CONTRACT_MODE | EVIDENCE_MODE);

    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "schema_version",
        manifest.schema_version == BETA_EVIDENCE_SCHEMA_VERSION,
        format!(
            "expected schema {}, found {}",
            BETA_EVIDENCE_SCHEMA_VERSION, manifest.schema_version
        ),
    );
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "mode",
        known_mode,
        "mode must be contract or evidence".to_owned(),
    );
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "cluster",
        manifest.cluster == DEVNET_CLUSTER,
        "Beta evidence must target Solana Devnet".to_owned(),
    );
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "zero_cost_infrastructure",
        !manifest.infrastructure.paid_services
            && manifest.infrastructure.rpc_kind == PUBLIC_DEVNET_RPC
            && !manifest.infrastructure.secrets_in_manifest,
        "paid services, non-public RPCs, and secrets are excluded from the manifest".to_owned(),
    );

    let participant_ids = manifest
        .testers
        .participant_ids
        .iter()
        .map(|id| id.trim())
        .collect::<BTreeSet<_>>();
    let testers_structurally_valid = if manifest.mode == CONTRACT_MODE {
        !manifest.testers.recorded
            && manifest.testers.count == 0
            && manifest.testers.participant_ids.is_empty()
    } else {
        manifest.testers.recorded
            && (MIN_TESTERS..=MAX_TESTERS).contains(&manifest.testers.count)
            && participant_ids.len() == manifest.testers.count
            && manifest
                .testers
                .participant_ids
                .iter()
                .all(|id| !id.trim().is_empty())
    };
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        if manifest.mode == CONTRACT_MODE {
            CheckKind::Contract
        } else {
            CheckKind::ExternalEvidence
        },
        "tester_target",
        testers_structurally_valid,
        format!(
            "record 8-20 distinct testers; found recorded={}, count={}",
            manifest.testers.recorded, manifest.testers.count
        ),
    );

    let window_ids = manifest
        .battle_windows
        .iter()
        .map(|window| window.id.trim())
        .collect::<BTreeSet<_>>();
    let windows_contract_valid = manifest.battle_windows.iter().all(|window| {
        !window.id.trim().is_empty()
            && !window.opened_at.trim().is_empty()
            && !window.closed_at.trim().is_empty()
            && window.opened_at != window.closed_at
            && matches!(
                window.source_kind.as_str(),
                JUPITER_SOURCE_KIND | PYTH_SOURCE_KIND
            )
            && valid_artifact_path(&window.evidence_path)
    }) && window_ids.len() == manifest.battle_windows.len();
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "battle_window_shape",
        windows_contract_valid,
        "Battle windows require unique ids, non-empty bounds, a supported source, and a scoped evidence path".to_owned(),
    );
    let windows_recorded = manifest.battle_windows.len() >= MIN_BATTLE_WINDOWS
        && manifest
            .battle_windows
            .iter()
            .all(|window| window.recorded && window.battle_count > 0);
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::ExternalEvidence,
        "multiple_battle_windows",
        manifest.mode == CONTRACT_MODE || windows_recorded,
        "release evidence requires at least two recorded non-empty Battle windows".to_owned(),
    );

    let jupiter_shape_valid = manifest.jupiter_proof.network == DEVNET_CLUSTER
        && valid_artifact_path(&manifest.jupiter_proof.evidence_path)
        && !manifest.jupiter_proof.battle_id.trim().is_empty();
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "jupiter_proof_shape",
        jupiter_shape_valid,
        "Jupiter proof must bind a Devnet Battle and a scoped evidence path".to_owned(),
    );
    let jupiter_recorded = manifest.jupiter_proof.recorded
        && manifest
            .jupiter_proof
            .transaction_signature
            .as_deref()
            .is_some_and(|signature| !signature.trim().is_empty());
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::ExternalEvidence,
        "jupiter_proof_recorded",
        manifest.mode == CONTRACT_MODE || jupiter_recorded,
        "release evidence requires one recorded Jupiter proof transaction".to_owned(),
    );

    let pyth_shape_valid = manifest.pyth_proof.as_ref().is_none_or(|proof| {
        proof.network == DEVNET_CLUSTER
            && valid_artifact_path(&proof.evidence_path)
            && !proof.battle_id.trim().is_empty()
            && (!proof.enabled || !proof.reason.trim().is_empty())
    });
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "pyth_proof_shape",
        pyth_shape_valid,
        "optional Pyth evidence must use Devnet and a scoped evidence path".to_owned(),
    );
    let pyth_recorded = match manifest.pyth_proof.as_ref() {
        Some(proof) if proof.enabled => {
            proof.recorded
                && proof
                    .transaction_signature
                    .as_deref()
                    .is_some_and(|signature| !signature.trim().is_empty())
        }
        Some(_) | None => true,
    };
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::ExternalEvidence,
        "optional_pyth_proof",
        manifest.mode == CONTRACT_MODE || pyth_recorded,
        "enabled Pyth mode requires its own recorded Devnet proof; disabled mode remains valid"
            .to_owned(),
    );

    let private_demo_shape_valid = manifest.private_market_demo.separate_public_ranked_domain
        && !manifest.private_market_demo.public_elo_mutated
        && !manifest.private_market_demo.screenshot_paths.is_empty()
        && manifest
            .private_market_demo
            .screenshot_paths
            .iter()
            .all(|path| valid_artifact_path(path));
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "private_market_demo_shape",
        private_demo_shape_valid,
        "Private Market evidence must remain separate from Public Elo".to_owned(),
    );
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::ExternalEvidence,
        "private_market_demo_recorded",
        manifest.mode == CONTRACT_MODE
            || (manifest.private_market_demo.recorded && private_demo_shape_valid),
        "release evidence requires at least one recorded Private Market screenshot".to_owned(),
    );

    let league_shape_valid = manifest.league_simulation.player_count == REQUIRED_LEAGUE_PLAYERS
        && manifest.league_simulation.unique_players == REQUIRED_LEAGUE_PLAYERS
        && manifest.league_simulation.round_count > 0
        && manifest.league_simulation.one_active_pairing_per_player
        && valid_artifact_path(&manifest.league_simulation.evidence_path);
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "league_simulation_shape",
        league_shape_valid,
        "League evidence must describe 100 unique players and a scoped report".to_owned(),
    );
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::ExternalEvidence,
        "league_simulation_recorded",
        manifest.mode == CONTRACT_MODE
            || (manifest.league_simulation.completed && league_shape_valid),
        "release evidence requires a completed 100-player League simulation".to_owned(),
    );

    let release_ready = manifest.mode == EVIDENCE_MODE
        && contract_valid
        && external_evidence_valid
        && checks.iter().all(|check| check.passed);
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::ExternalEvidence,
        "external_beta_evidence",
        release_ready,
        if release_ready {
            "all beta evidence is recorded".to_owned()
        } else {
            "checked-in contracts do not claim that external beta evidence exists".to_owned()
        },
    );

    BetaEvidenceReport {
        scope: "beta_evidence",
        mode: manifest.mode.clone(),
        contract_valid,
        release_ready,
        checks,
    }
}

/// Verifies that an evidence-mode manifest points to real, scoped operator
/// artifacts. Contract mode intentionally skips this check so the checked-in
/// fixture remains a schema contract rather than pretending that external
/// sessions or screenshots exist.
pub fn validate_beta_evidence_artifacts(
    manifest: &BetaEvidenceManifest,
    repository_root: impl AsRef<Path>,
) -> Result<(), String> {
    if manifest.mode != EVIDENCE_MODE {
        return Ok(());
    }
    let root = fs::canonicalize(repository_root.as_ref())
        .map_err(|error| format!("evidence root cannot be resolved: {error}"))?;
    let mut paths = BTreeSet::new();
    paths.extend(
        manifest
            .battle_windows
            .iter()
            .map(|window| window.evidence_path.as_str()),
    );
    paths.insert(manifest.jupiter_proof.evidence_path.as_str());
    if let Some(pyth) = manifest.pyth_proof.as_ref().filter(|proof| proof.enabled) {
        paths.insert(pyth.evidence_path.as_str());
    }
    paths.extend(
        manifest
            .private_market_demo
            .screenshot_paths
            .iter()
            .map(String::as_str),
    );
    paths.insert(manifest.league_simulation.evidence_path.as_str());

    for relative in paths {
        if !valid_artifact_path(relative) {
            return Err(format!(
                "evidence path is outside the release artifact scope: {relative}"
            ));
        }
        if relative.split('/').any(|component| {
            let component = component.to_ascii_lowercase();
            component.contains("secret")
                || component.contains("seed")
                || component.contains("private-key")
        }) {
            return Err(format!(
                "evidence path has a sensitive component: {relative}"
            ));
        }
        let candidate = root.join(relative);
        let canonical = fs::canonicalize(&candidate).map_err(|error| {
            format!("required evidence artifact is missing: {relative}: {error}")
        })?;
        if !canonical.starts_with(&root) {
            return Err(format!(
                "evidence artifact escapes the repository root: {relative}"
            ));
        }
        if !canonical.is_file() {
            return Err(format!(
                "required evidence artifact is not a file: {relative}"
            ));
        }
    }
    Ok(())
}

fn record_check(
    checks: &mut Vec<EvidenceCheck>,
    contract_valid: &mut bool,
    external_evidence_valid: &mut bool,
    kind: CheckKind,
    name: &str,
    passed: bool,
    detail: String,
) {
    match kind {
        CheckKind::Contract => *contract_valid &= passed,
        CheckKind::ExternalEvidence => *external_evidence_valid &= passed,
    }
    checks.push(EvidenceCheck {
        name: name.to_owned(),
        passed,
        detail,
    });
}

pub const FEATURE_FREEZE_SCHEMA_VERSION: u16 = 1;
pub const EXPECTED_FEATURE_FREEZE_AT: &str = "2026-09-24T23:59:00+05:30";
pub const FREEZE_MODE: &str = "freeze";
pub const GREEN_STATUS: &str = "green";
pub const DISABLED_STATUS: &str = "disabled";
pub const PLANNED_STATUS: &str = "planned";
pub const ALLOWED_SPONSOR_TRACKS: &[&str] = &[PYTH_SOURCE_KIND, "PRESTOCKS", "TESSERA"];
pub const P0_SCOPE: &[&str] = &[
    "wallet_authentication",
    "reference_asset_registry",
    "dynamic_public_equity_universe",
    "market_round",
    "battle",
    "six_picks",
    "captain",
    "immutable_commit_reveal",
    "jupiter_settlement",
    "q9_score_winner",
    "ranked_queue",
    "public_rating_update",
    "public_leaderboard",
    "official_league_swiss",
    "live_projected_score",
    "source_aware_proof",
    "mobile_usable_ui",
    "replay_demo",
    "zero_cost_devnet",
];
pub const P1_SCOPE: &[&str] = &[
    "pyth_verified_optional",
    "pyth_proof_panel",
    "prestocks_adapter",
    "tessera_representation_cards",
    "private_markets_ui",
    "commit_reveal_recovery",
    "achievements_titles",
    "observability_proof_assets",
];

#[derive(Debug, Clone, Deserialize)]
pub struct FeatureFreezeManifest {
    pub schema_version: u16,
    pub mode: String,
    pub roadmap: String,
    pub cluster: String,
    pub freeze_at: String,
    pub no_new_sponsor_scope: bool,
    pub sponsor_tracks: Vec<String>,
    pub pyth_enabled: bool,
    pub beta_manifest_path: String,
    pub p0: Vec<FreezeItem>,
    pub p1: Vec<FreezeItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FreezeItem {
    pub id: String,
    pub status: String,
    pub evidence_path: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FeatureFreezeReport {
    pub scope: &'static str,
    pub mode: String,
    pub contract_valid: bool,
    pub release_ready: bool,
    pub checks: Vec<EvidenceCheck>,
}

/// Validates the release-freeze record against the documented P0/P1
/// scope and the beta evidence report.
///
/// The contract fixture is suitable for local CI and remains deliberately
/// non-ready. A freeze-mode record can become release-ready only when every P0
/// item is green, every non-disabled P1 item is green, the optional Pyth state
/// is internally consistent, and beta evidence has real evidence-mode readiness.
pub fn validate_feature_freeze(
    manifest: &FeatureFreezeManifest,
    beta_report: &BetaEvidenceReport,
) -> FeatureFreezeReport {
    let mut checks = Vec::new();
    let mut contract_valid = true;
    let mut external_evidence_valid = true;
    let known_mode = matches!(manifest.mode.as_str(), CONTRACT_MODE | FREEZE_MODE);

    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "freeze_schema_version",
        manifest.schema_version == FEATURE_FREEZE_SCHEMA_VERSION,
        format!(
            "expected schema {}, found {}",
            FEATURE_FREEZE_SCHEMA_VERSION, manifest.schema_version
        ),
    );
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "freeze_mode",
        known_mode,
        "mode must be contract or freeze".to_owned(),
    );
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "freeze_source",
        manifest.roadmap == "STOCKLANA_V2.1" && manifest.cluster == DEVNET_CLUSTER,
        "the freeze must bind STOCKLANA_V2.1 and Solana Devnet".to_owned(),
    );
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "freeze_deadline",
        manifest.freeze_at == EXPECTED_FEATURE_FREEZE_AT,
        format!("feature freeze must be {}", EXPECTED_FEATURE_FREEZE_AT),
    );
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "no_new_sponsor_scope",
        manifest.no_new_sponsor_scope,
        "new sponsor scope is forbidden after the release freeze".to_owned(),
    );

    let sponsor_tracks = manifest
        .sponsor_tracks
        .iter()
        .map(|track| track.trim())
        .collect::<BTreeSet<_>>();
    let sponsor_scope_valid = !sponsor_tracks.is_empty()
        && sponsor_tracks.len() == manifest.sponsor_tracks.len()
        && sponsor_tracks
            .iter()
            .all(|track| ALLOWED_SPONSOR_TRACKS.contains(track));
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "sponsor_scope",
        sponsor_scope_valid,
        "only the documented Pyth, PreStocks, and Tessera tracks may be frozen".to_owned(),
    );
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "beta_manifest_path",
        valid_release_evidence_path(&manifest.beta_manifest_path),
        "the freeze must point to a scoped beta manifest".to_owned(),
    );

    let p0_shape_valid = scope_items_have_exact_ids(&manifest.p0, P0_SCOPE)
        && scope_items_have_valid_paths(&manifest.p0);
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "p0_scope_shape",
        p0_shape_valid,
        "P0 must contain each documented submission-blocking item exactly once".to_owned(),
    );
    let p1_shape_valid = scope_items_have_exact_ids(&manifest.p1, P1_SCOPE)
        && scope_items_have_valid_paths(&manifest.p1);
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::Contract,
        "p1_scope_shape",
        p1_shape_valid,
        "P1 must contain each documented sponsor-strengthening item exactly once".to_owned(),
    );
    let p0_status_valid = manifest.p0.iter().all(|item| item.status == GREEN_STATUS);
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::ExternalEvidence,
        "p0_status",
        manifest.mode == CONTRACT_MODE || p0_status_valid,
        "every P0 item must be green before release freeze".to_owned(),
    );
    let p1_status_valid = manifest.p1.iter().all(|item| {
        item.status == GREEN_STATUS
            || (!manifest.pyth_enabled
                && matches!(
                    item.id.as_str(),
                    "pyth_verified_optional" | "pyth_proof_panel"
                )
                && item.status == DISABLED_STATUS)
    });
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::ExternalEvidence,
        "p1_status",
        manifest.mode == CONTRACT_MODE || p1_status_valid,
        "P1 items must be green, with only disabled optional Pyth items allowed when Pyth is off"
            .to_owned(),
    );

    let beta_compatible = if manifest.mode == CONTRACT_MODE {
        beta_report.mode == CONTRACT_MODE && beta_report.contract_valid
    } else {
        beta_report.mode == EVIDENCE_MODE && beta_report.release_ready
    };
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::ExternalEvidence,
        "beta_evidence_compatibility",
        beta_compatible,
        "the freeze must consume a matching beta evidence beta report".to_owned(),
    );

    let release_ready = manifest.mode == FREEZE_MODE
        && contract_valid
        && external_evidence_valid
        && checks.iter().all(|check| check.passed);
    record_check(
        &mut checks,
        &mut contract_valid,
        &mut external_evidence_valid,
        CheckKind::ExternalEvidence,
        "feature_freeze_evidence",
        release_ready,
        if release_ready {
            "P0/P1 scope and beta evidence are ready for feature freeze".to_owned()
        } else {
            "contract mode validates scope only; it does not claim release readiness".to_owned()
        },
    );

    FeatureFreezeReport {
        scope: "release_freeze",
        mode: manifest.mode.clone(),
        contract_valid,
        release_ready,
        checks,
    }
}

fn scope_items_have_exact_ids(items: &[FreezeItem], expected: &[&str]) -> bool {
    let actual = items
        .iter()
        .map(|item| item.id.trim())
        .collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    actual == expected && items.len() == expected.len()
}

fn scope_items_have_valid_paths(items: &[FreezeItem]) -> bool {
    items.iter().all(|item| {
        !item.id.trim().is_empty()
            && !item.note.trim().is_empty()
            && valid_artifact_path(&item.evidence_path)
    })
}

fn valid_release_evidence_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.trim().is_empty()
        && (value.starts_with("artifacts/release-evidence/")
            || value.starts_with("fixtures/release-evidence/"))
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn valid_artifact_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.trim().is_empty()
        && value.starts_with("artifacts/release-evidence/")
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(mode: &str) -> BetaEvidenceManifest {
        let evidence = mode == EVIDENCE_MODE;
        BetaEvidenceManifest {
            schema_version: BETA_EVIDENCE_SCHEMA_VERSION,
            mode: mode.to_owned(),
            cluster: DEVNET_CLUSTER.to_owned(),
            infrastructure: InfrastructureEvidence {
                paid_services: false,
                rpc_kind: PUBLIC_DEVNET_RPC.to_owned(),
                secrets_in_manifest: false,
            },
            testers: TesterEvidence {
                recorded: evidence,
                count: if evidence { MIN_TESTERS } else { 0 },
                participant_ids: if evidence {
                    (0..MIN_TESTERS).map(|id| format!("tester-{id}")).collect()
                } else {
                    Vec::new()
                },
            },
            battle_windows: if evidence {
                (0..MIN_BATTLE_WINDOWS)
                    .map(|index| BattleWindowEvidence {
                        id: format!("window-{index}"),
                        opened_at: format!("2026-09-24T0{index}:00:00Z"),
                        closed_at: format!("2026-09-24T0{index}:30:00Z"),
                        recorded: true,
                        battle_count: 1,
                        source_kind: JUPITER_SOURCE_KIND.to_owned(),
                        evidence_path: format!("artifacts/release-evidence/window-{index}.json"),
                    })
                    .collect()
            } else {
                Vec::new()
            },
            jupiter_proof: ProofRoundEvidence {
                recorded: evidence,
                network: DEVNET_CLUSTER.to_owned(),
                battle_id: if evidence {
                    "battle-jupiter"
                } else {
                    "pending"
                }
                .to_owned(),
                evidence_path: "artifacts/release-evidence/jupiter.json".to_owned(),
                transaction_signature: evidence.then(|| "signature-jupiter".to_owned()),
            },
            pyth_proof: Some(PythProofEvidence {
                enabled: false,
                recorded: false,
                network: DEVNET_CLUSTER.to_owned(),
                battle_id: "pending".to_owned(),
                evidence_path: "artifacts/release-evidence/pyth.json".to_owned(),
                transaction_signature: None,
                reason: "Pyth remains optional and is not enabled in this record.".to_owned(),
            }),
            private_market_demo: PrivateMarketDemoEvidence {
                recorded: evidence,
                screenshot_paths: vec!["artifacts/release-evidence/private-market.png".to_owned()],
                separate_public_ranked_domain: true,
                public_elo_mutated: false,
            },
            league_simulation: LeagueSimulationEvidence {
                completed: evidence,
                player_count: REQUIRED_LEAGUE_PLAYERS,
                unique_players: REQUIRED_LEAGUE_PLAYERS,
                round_count: 5,
                one_active_pairing_per_player: true,
                evidence_path: "artifacts/release-evidence/league-100.json".to_owned(),
            },
        }
    }

    #[test]
    fn contract_fixture_is_valid_without_claiming_external_beta_evidence() {
        let report = validate_beta_evidence(&manifest(CONTRACT_MODE));

        assert!(report.contract_valid);
        assert!(!report.release_ready);
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "external_beta_evidence" && !check.passed));
    }

    #[test]
    fn recorded_evidence_requires_two_windows_and_all_required_proof_surfaces() {
        let report = validate_beta_evidence(&manifest(EVIDENCE_MODE));

        assert!(report.contract_valid);
        assert!(report.release_ready);
        assert!(report.checks.iter().all(|check| check.passed));
    }

    #[test]
    fn evidence_mode_requires_referenced_artifacts_to_exist() {
        let error = validate_beta_evidence_artifacts(&manifest(EVIDENCE_MODE), "/tmp").unwrap_err();

        assert!(error.contains("required evidence artifact is missing"));
    }

    #[test]
    fn enabled_pyth_without_a_recorded_proof_cannot_make_the_beta_ready() {
        let mut evidence = manifest(EVIDENCE_MODE);
        let pyth = evidence
            .pyth_proof
            .as_mut()
            .expect("fixture has optional Pyth");
        pyth.enabled = true;
        pyth.reason.clear();

        let report = validate_beta_evidence(&evidence);

        assert!(!report.release_ready);
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "optional_pyth_proof" && !check.passed));
    }

    #[test]
    fn duplicate_window_ids_are_a_contract_error() {
        let mut evidence = manifest(EVIDENCE_MODE);
        evidence.battle_windows[1].id = evidence.battle_windows[0].id.clone();

        let report = validate_beta_evidence(&evidence);

        assert!(!report.contract_valid);
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "battle_window_shape" && !check.passed));
    }

    fn freeze_manifest(mode: &str) -> FeatureFreezeManifest {
        let release = mode == FREEZE_MODE;
        let item = |id: &str| FreezeItem {
            id: id.to_owned(),
            status: if release {
                GREEN_STATUS
            } else {
                PLANNED_STATUS
            }
            .to_owned(),
            evidence_path: format!("artifacts/release-evidence/freeze/{id}.json"),
            note: "Tracked by the release record.".to_owned(),
        };
        FeatureFreezeManifest {
            schema_version: FEATURE_FREEZE_SCHEMA_VERSION,
            mode: mode.to_owned(),
            roadmap: "STOCKLANA_V2.1".to_owned(),
            cluster: DEVNET_CLUSTER.to_owned(),
            freeze_at: EXPECTED_FEATURE_FREEZE_AT.to_owned(),
            no_new_sponsor_scope: true,
            sponsor_tracks: ALLOWED_SPONSOR_TRACKS
                .iter()
                .map(|track| (*track).to_owned())
                .collect(),
            pyth_enabled: false,
            beta_manifest_path: "fixtures/release-evidence/beta-evidence.contract.json".to_owned(),
            p0: P0_SCOPE.iter().map(|id| item(id)).collect(),
            p1: P1_SCOPE.iter().map(|id| item(id)).collect(),
        }
    }

    #[test]
    fn feature_freeze_contract_accepts_the_documented_scope_without_claiming_readiness() {
        let freeze = freeze_manifest("contract");
        let beta_report = validate_beta_evidence(&manifest(CONTRACT_MODE));

        let report = validate_feature_freeze(&freeze, &beta_report);

        assert!(report.contract_valid);
        assert!(!report.release_ready);
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "feature_freeze_evidence" && !check.passed));
    }

    #[test]
    fn feature_freeze_rejects_a_non_green_p0_even_with_ready_beta_evidence() {
        let mut freeze = freeze_manifest(FREEZE_MODE);
        let beta_report = validate_beta_evidence(&manifest(EVIDENCE_MODE));
        freeze.p0[0].status = PLANNED_STATUS.to_owned();

        let report = validate_feature_freeze(&freeze, &beta_report);

        assert!(!report.release_ready);
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "p0_status" && !check.passed));
    }

    #[test]
    fn feature_freeze_rejects_an_unapproved_sponsor_track() {
        let mut freeze = freeze_manifest(CONTRACT_MODE);
        let beta_report = validate_beta_evidence(&manifest(CONTRACT_MODE));
        freeze.sponsor_tracks.push("CLAWPUMP".to_owned());

        let report = validate_feature_freeze(&freeze, &beta_report);

        assert!(!report.contract_valid);
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "sponsor_scope" && !check.passed));
    }
}
