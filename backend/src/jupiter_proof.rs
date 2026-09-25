//! Validation for a retained Jupiter-backed public settlement proof.
//!
//! The generic proof module protects the read-only HTTP surface. This module
//! adds the stronger operator gate required for one replayable Jupiter round:
//! it binds the snapshot to Jupiter, checks the six-asset rated Battle shape,
//! replays the protocol quorum calculation, and verifies deterministic public
//! proof serialization.

use std::{collections::BTreeMap, fmt};

use protocol::{select_compatible_quorum, AttestorReport};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::proof::{
    BattleResult, PhaseProof, PhaseState, ProofError, ProofSnapshot, ReportProof, RoundState,
    SourceKind,
};

pub const JUPITER_PROOF_SCHEMA_VERSION: u16 = 1;
pub const DEVNET_CLUSTER: &str = "devnet";
pub const JUPITER_SOURCE_KIND: &str = "JUPITER_TOKEN_SPOT_V1";
pub const DEFAULT_MAX_ATTESTOR_SPREAD_BPS: u16 = 100;
pub const REQUIRED_ROUND_ASSET_COUNT: usize = 6;
pub const REQUIRED_PHASE_EVIDENCE_COUNT: usize = 12;

/// Operator-owned envelope for one retained Jupiter settlement round.
///
/// The snapshot is the public, reconciled chain projection. The additional
/// quorum records retain all three candidate attestor reports for each asset
/// and price phase, which lets an auditor verify both ordinary three-report
/// agreement and deterministic two-report outlier rejection.
#[derive(Debug, Clone, Deserialize)]
pub struct JupiterProofBundle {
    pub schema_version: u16,
    pub cluster: String,
    pub max_attestor_spread_bps: u16,
    pub snapshot: ProofSnapshot,
    pub quorum_evidence: Vec<QuorumEvidence>,
    #[serde(default)]
    pub expected_public_proof_sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuorumEvidence {
    pub asset_id: u16,
    /// 0 is START and 1 is END, matching the on-chain instruction encoding.
    pub phase: u8,
    pub candidate_reports: Vec<ReportProof>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JupiterProofCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JupiterProofGateReport {
    pub scope: &'static str,
    pub valid: bool,
    pub source_kind: &'static str,
    pub market_round_pubkey: String,
    pub battle_pubkey: String,
    pub indexed_slot: u64,
    pub transaction_count: usize,
    pub replay_sha256: String,
    pub checks: Vec<JupiterProofCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JupiterProofError {
    Snapshot(ProofError),
    InvalidBundle(String),
    InvalidQuorum {
        asset_id: u16,
        phase: u8,
        detail: String,
    },
    FinalizedPriceMismatch {
        asset_id: u16,
        phase: u8,
        expected: i64,
        found: i64,
    },
    ReplayHashMismatch {
        expected: String,
        found: String,
    },
}

impl fmt::Display for JupiterProofError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Snapshot(error) => write!(formatter, "snapshot validation failed: {error}"),
            Self::InvalidBundle(detail) => write!(formatter, "invalid Jupiter proof bundle: {detail}"),
            Self::InvalidQuorum {
                asset_id,
                phase,
                detail,
            } => write!(
                formatter,
                "quorum validation failed for asset {asset_id}, phase {phase}: {detail}"
            ),
            Self::FinalizedPriceMismatch {
                asset_id,
                phase,
                expected,
                found,
            } => write!(
                formatter,
                "quorum finalized price mismatch for asset {asset_id}, phase {phase}: expected {expected}, found {found}"
            ),
            Self::ReplayHashMismatch { expected, found } => write!(
                formatter,
                "retained proof replay hash mismatch: expected {expected}, found {found}"
            ),
        }
    }
}

impl std::error::Error for JupiterProofError {}

impl From<ProofError> for JupiterProofError {
    fn from(error: ProofError) -> Self {
        Self::Snapshot(error)
    }
}

/// Validates a Jupiter snapshot using the deployed quality-policy default.
///
/// This convenience function is useful for callers that already possess only
/// the reconciled public snapshot. Operator evidence should use the bundle
/// validator, because the bundle retains all candidate reports needed to audit
/// outlier rejection.
pub fn validate_jupiter_proof(
    snapshot: &ProofSnapshot,
) -> Result<JupiterProofGateReport, JupiterProofError> {
    validate_snapshot(snapshot, DEFAULT_MAX_ATTESTOR_SPREAD_BPS, None)
}

/// Validates the complete operator-owned Jupiter proof bundle.
pub fn validate_jupiter_proof_bundle(
    bundle: &JupiterProofBundle,
) -> Result<JupiterProofGateReport, JupiterProofError> {
    if bundle.schema_version != JUPITER_PROOF_SCHEMA_VERSION {
        return Err(JupiterProofError::InvalidBundle(format!(
            "expected schema {}, found {}",
            JUPITER_PROOF_SCHEMA_VERSION, bundle.schema_version
        )));
    }
    if bundle.cluster != DEVNET_CLUSTER {
        return Err(JupiterProofError::InvalidBundle(
            "the retained proof must target Solana Devnet".to_owned(),
        ));
    }
    if bundle.quorum_evidence.len() != REQUIRED_PHASE_EVIDENCE_COUNT {
        return Err(JupiterProofError::InvalidBundle(format!(
            "expected {REQUIRED_PHASE_EVIDENCE_COUNT} asset/price-phase quorum records, found {}",
            bundle.quorum_evidence.len()
        )));
    }

    let mut records = BTreeMap::new();
    for evidence in &bundle.quorum_evidence {
        if evidence.phase > 1 {
            return Err(JupiterProofError::InvalidBundle(format!(
                "phase {} is not START (0) or END (1)",
                evidence.phase
            )));
        }
        if records
            .insert(
                (evidence.asset_id, evidence.phase),
                evidence.candidate_reports.clone(),
            )
            .is_some()
        {
            return Err(JupiterProofError::InvalidBundle(format!(
                "duplicate quorum record for asset {}, phase {}",
                evidence.asset_id, evidence.phase
            )));
        }
        if evidence.candidate_reports.len() != 3 {
            return Err(JupiterProofError::InvalidBundle(format!(
                "asset {}, phase {} must retain all three candidate attestor reports",
                evidence.asset_id, evidence.phase
            )));
        }
    }

    let report = validate_snapshot(
        &bundle.snapshot,
        bundle.max_attestor_spread_bps,
        Some(&records),
    )?;
    if let Some(expected) = &bundle.expected_public_proof_sha256 {
        if !is_sha256(expected) {
            return Err(JupiterProofError::InvalidBundle(
                "expected_public_proof_sha256 must be 64 hexadecimal characters".to_owned(),
            ));
        }
        if expected != &report.replay_sha256 {
            return Err(JupiterProofError::ReplayHashMismatch {
                expected: expected.clone(),
                found: report.replay_sha256.clone(),
            });
        }
    }
    Ok(report)
}

fn validate_snapshot(
    snapshot: &ProofSnapshot,
    max_attestor_spread_bps: u16,
    candidate_reports: Option<&BTreeMap<(u16, u8), Vec<ReportProof>>>,
) -> Result<JupiterProofGateReport, JupiterProofError> {
    snapshot.validate()?;
    if snapshot.market_round.source_kind != SourceKind::JupiterTokenSpotV1 {
        return Err(JupiterProofError::InvalidBundle(
            "the retained round is not frozen to Jupiter".to_owned(),
        ));
    }
    if snapshot.market_round.state != RoundState::Finalized {
        return Err(JupiterProofError::InvalidBundle(
            "the retained Market Round is not finalized".to_owned(),
        ));
    }
    if snapshot.round_assets.len() != REQUIRED_ROUND_ASSET_COUNT {
        return Err(JupiterProofError::InvalidBundle(format!(
            "Jupiter rated rounds require exactly {REQUIRED_ROUND_ASSET_COUNT} assets, found {}",
            snapshot.round_assets.len()
        )));
    }
    if snapshot
        .transaction_signatures
        .iter()
        .any(|signature| signature.trim().is_empty())
    {
        return Err(JupiterProofError::InvalidBundle(
            "transaction signatures must be non-empty".to_owned(),
        ));
    }
    let mut signatures = snapshot.transaction_signatures.clone();
    signatures.sort_unstable();
    signatures.dedup();
    if signatures.len() != snapshot.transaction_signatures.len() {
        return Err(JupiterProofError::InvalidBundle(
            "transaction signatures must be unique".to_owned(),
        ));
    }

    let battle = snapshot.battle.as_ref().ok_or_else(|| {
        JupiterProofError::InvalidBundle(
            "the retained proof must contain a rated Battle".to_owned(),
        )
    })?;
    if !matches!(
        battle.result,
        BattleResult::PlayerA
            | BattleResult::PlayerB
            | BattleResult::Draw
            | BattleResult::ForfeitA
            | BattleResult::ForfeitB
    ) {
        return Err(JupiterProofError::InvalidBundle(
            "the retained Battle does not contain a rated terminal result".to_owned(),
        ));
    }
    if snapshot.round_assets.iter().any(|asset| {
        asset.start.state != PhaseState::Finalized || asset.end.state != PhaseState::Finalized
    }) {
        return Err(JupiterProofError::InvalidBundle(
            "every Jupiter asset must have finalized START and END evidence".to_owned(),
        ));
    }

    let mut checks = Vec::new();
    for asset in &snapshot.round_assets {
        validate_phase_quorum(
            asset.asset_id,
            0,
            &asset.start,
            candidate_reports.and_then(|records| records.get(&(asset.asset_id, 0))),
            max_attestor_spread_bps,
        )?;
        validate_phase_quorum(
            asset.asset_id,
            1,
            &asset.end,
            candidate_reports.and_then(|records| records.get(&(asset.asset_id, 1))),
            max_attestor_spread_bps,
        )?;
    }

    let public = snapshot.clone().into_public()?;
    let replay_bytes = serde_json::to_vec(&public)
        .map_err(|error| JupiterProofError::InvalidBundle(error.to_string()))?;
    let replay_again = serde_json::to_vec(&snapshot.clone().into_public()?)
        .map_err(|error| JupiterProofError::InvalidBundle(error.to_string()))?;
    if replay_bytes != replay_again {
        return Err(JupiterProofError::InvalidBundle(
            "replaying the retained proof produced different public bytes".to_owned(),
        ));
    }
    let replay_sha256 = hex::encode(Sha256::digest(&replay_bytes));
    checks.push(JupiterProofCheck {
        name: "protocol_quorum_replay".to_owned(),
        passed: true,
        detail: "every finalized asset phase matches the deterministic attestor quorum".to_owned(),
    });
    checks.push(JupiterProofCheck {
        name: "public_proof_replay".to_owned(),
        passed: true,
        detail: format!("deterministic public proof SHA-256 {replay_sha256}"),
    });

    Ok(JupiterProofGateReport {
        scope: "jupiter_proof_round",
        valid: true,
        source_kind: JUPITER_SOURCE_KIND,
        market_round_pubkey: snapshot.market_round.market_round_pubkey.clone(),
        battle_pubkey: battle.battle_pubkey.clone(),
        indexed_slot: snapshot.indexed_slot,
        transaction_count: snapshot.transaction_signatures.len(),
        replay_sha256,
        checks,
    })
}

fn validate_phase_quorum(
    asset_id: u16,
    phase: u8,
    phase_proof: &PhaseProof,
    candidate_reports: Option<&Vec<ReportProof>>,
    max_attestor_spread_bps: u16,
) -> Result<(), JupiterProofError> {
    let selected = &phase_proof.selected_reports;
    let reports = candidate_reports.unwrap_or(selected);
    if !(2..=3).contains(&reports.len()) || selected.is_empty() {
        return Err(JupiterProofError::InvalidQuorum {
            asset_id,
            phase,
            detail: "a compatible quorum must contain two or three reports".to_owned(),
        });
    }
    let mut protocol_reports = Vec::with_capacity(reports.len());
    for report in reports {
        let attestor = decode_attestor(&report.attestor).map_err(|detail| {
            JupiterProofError::InvalidQuorum {
                asset_id,
                phase,
                detail,
            }
        })?;
        protocol_reports.push(AttestorReport {
            attestor,
            median_price_q9: report.median_price_q9,
        });
        if report.evidence_root.len() != 64 || !is_sha256(&report.evidence_root) {
            return Err(JupiterProofError::InvalidQuorum {
                asset_id,
                phase,
                detail: "evidence roots must be 32-byte hexadecimal commitments".to_owned(),
            });
        }
    }

    let selection =
        select_compatible_quorum(&protocol_reports, max_attestor_spread_bps).map_err(|error| {
            JupiterProofError::InvalidQuorum {
                asset_id,
                phase,
                detail: error.to_string(),
            }
        })?;
    let finalized_price =
        phase_proof
            .finalized_price_q9
            .ok_or_else(|| JupiterProofError::InvalidQuorum {
                asset_id,
                phase,
                detail: "finalized phase has no price".to_owned(),
            })?;
    if selection.finalized_price_q9 != finalized_price {
        return Err(JupiterProofError::FinalizedPriceMismatch {
            asset_id,
            phase,
            expected: selection.finalized_price_q9,
            found: finalized_price,
        });
    }

    let mut expected = selection.selected_attestors;
    expected.sort_unstable();
    let mut actual = Vec::with_capacity(selected.len());
    for report in selected {
        actual.push(decode_attestor(&report.attestor).map_err(|detail| {
            JupiterProofError::InvalidQuorum {
                asset_id,
                phase,
                detail,
            }
        })?);
    }
    actual.sort_unstable();
    if expected != actual {
        return Err(JupiterProofError::InvalidQuorum {
            asset_id,
            phase,
            detail: "snapshot selected reports do not match protocol quorum selection".to_owned(),
        });
    }
    Ok(())
}

fn decode_attestor(value: &str) -> Result<[u8; 32], String> {
    let bytes = bs58::decode(value)
        .into_vec()
        .map_err(|error| format!("attestor is not valid base58: {error}"))?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        format!("attestor decoded to {} bytes instead of 32", bytes.len())
    })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proof::{
        BattleProof, BattleResult, MarketRoundProof, ParticipantProof, PhaseProof, PhaseState,
        ProofSnapshot, ReportProof, RoundAssetProof, RoundState, SourceKind, VoidReason,
    };

    fn attestor(seed: u8) -> String {
        bs58::encode([seed; 32]).into_string()
    }

    fn report(seed: u8, price: i64) -> ReportProof {
        ReportProof {
            attestor: attestor(seed),
            median_price_q9: price,
            evidence_root: "00".repeat(32),
            accepted_observation_count: 3,
            unique_source_block_count: 3,
            first_source_block_id: 10,
            last_source_block_id: 12,
        }
    }

    fn phase(start_price: i64, end: bool) -> PhaseProof {
        let price = if end {
            start_price * 11 / 10
        } else {
            start_price
        };
        PhaseProof {
            state: PhaseState::Finalized,
            finalized_price_q9: Some(price),
            selected_reports: vec![report(1, price), report(2, price)],
            evidence_commitment: Some("00".repeat(32)),
            pyth_evidence: None,
        }
    }

    fn snapshot(mutate_report: bool) -> ProofSnapshot {
        let mut assets = Vec::new();
        for asset_id in 1..=6 {
            let mut start = phase(100_000_000_000, false);
            if mutate_report && asset_id == 1 {
                start.selected_reports[0].median_price_q9 += 1_000_000_000;
            }
            assets.push(RoundAssetProof {
                asset_id,
                symbol: format!("ASSET{asset_id}"),
                issuer: "issuer".to_owned(),
                scoring_mint: format!("Mint{asset_id}"),
                round_asset_pubkey: format!("RoundAsset{asset_id}"),
                representation_id: u32::from(asset_id),
                provider: "Jupiter".to_owned(),
                source_kind: SourceKind::JupiterTokenSpotV1,
                price_policy_version: 1,
                quality_policy_version: 1,
                start,
                end: phase(100_000_000_000, true),
                return_q9: Some(100_000_000),
                display_return: None,
            });
        }
        ProofSnapshot {
            market_round: MarketRoundProof {
                round_id: 1,
                market_round_pubkey: "MarketRound".to_owned(),
                registry_version: 1,
                price_policy_version: 1,
                quality_policy_version: 1,
                attestor_set_version: 1,
                start_target_at: 1,
                end_target_at: 2,
                source_kind: SourceKind::JupiterTokenSpotV1,
                settlement_transaction_signature: "settlement".to_owned(),
                state: RoundState::Finalized,
            },
            round_assets: assets,
            battle: Some(BattleProof {
                battle_id: 1,
                market_round_id: 1,
                battle_pubkey: "Battle".to_owned(),
                market_round_pubkey: "MarketRound".to_owned(),
                side_a_lineup: (1..=6).collect(),
                side_a_captain: Some(1),
                side_a_score_q9: Some(100_000_000),
                side_b_lineup: (1..=6).collect(),
                side_b_captain: Some(2),
                side_b_score_q9: Some(100_000_000),
                side_a_participant: ParticipantProof {
                    wallet: "WalletA".to_owned(),
                    commit_transaction_signature: Some("commit-a".to_owned()),
                    commit_timestamp_unix_secs: Some(1),
                    reveal_transaction_signature: Some("reveal-a".to_owned()),
                    reveal_timestamp_unix_secs: Some(2),
                },
                side_b_participant: ParticipantProof {
                    wallet: "WalletB".to_owned(),
                    commit_transaction_signature: Some("commit-b".to_owned()),
                    commit_timestamp_unix_secs: Some(1),
                    reveal_transaction_signature: Some("reveal-b".to_owned()),
                    reveal_timestamp_unix_secs: Some(2),
                },
                result: BattleResult::Draw,
                void_reason: VoidReason::None,
            }),
            transaction_signatures: vec![
                "settlement".to_owned(),
                "commit-a".to_owned(),
                "reveal-a".to_owned(),
                "commit-b".to_owned(),
                "reveal-b".to_owned(),
            ],
            indexed_slot: 10,
            chain_slot: 10,
            chain_reconciled: true,
        }
    }

    #[test]
    fn mismatched_finalized_price_is_rejected_before_release_evidence() {
        let proof = snapshot(true);

        let error = validate_jupiter_proof(&proof).unwrap_err();

        assert!(error.to_string().contains("quorum"));
    }

    #[test]
    fn valid_snapshot_replays_to_a_stable_public_hash() {
        let report = validate_jupiter_proof(&snapshot(false)).unwrap();

        assert!(report.valid);
        assert_eq!(report.replay_sha256.len(), 64);
        assert_eq!(report.transaction_count, 5);
    }

    #[test]
    fn three_candidate_reports_can_reject_one_outlier() {
        let proof = snapshot(false);
        let mut quorum_evidence = Vec::new();
        for asset in &proof.round_assets {
            for phase in 0..=1 {
                let selected = if phase == 0 { &asset.start } else { &asset.end };
                let selected_price = selected.finalized_price_q9.unwrap();
                quorum_evidence.push(QuorumEvidence {
                    asset_id: asset.asset_id,
                    phase,
                    candidate_reports: vec![
                        report(1, selected_price),
                        report(2, selected_price),
                        report(3, selected_price * 2),
                    ],
                });
            }
        }
        let bundle = JupiterProofBundle {
            schema_version: JUPITER_PROOF_SCHEMA_VERSION,
            cluster: DEVNET_CLUSTER.to_owned(),
            max_attestor_spread_bps: DEFAULT_MAX_ATTESTOR_SPREAD_BPS,
            snapshot: proof,
            quorum_evidence,
            expected_public_proof_sha256: None,
        };

        let report = validate_jupiter_proof_bundle(&bundle).unwrap();

        assert!(report.valid);
    }
}
