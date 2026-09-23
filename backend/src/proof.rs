//! Read-only proof materialization for finalized TickerSix settlement.
//!
//! The proof layer deliberately accepts a snapshot that has already been
//! indexed and reconciled against chain state. It validates the settlement
//! invariants again before exposing JSON, so an indexer cannot turn partial or
//! stale data into a final-looking public result. Secrets and commitment
//! preimages are not represented by these public types.

use std::{
    error::Error,
    fmt, fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
};

use protocol::return_q9;
use serde::{Deserialize, Serialize};

pub const JUPITER_SETTLEMENT_TRUST_LABEL: &str = "FINAL - ATTESTED SOLANA MARKET SETTLEMENT";
pub const PYTH_SETTLEMENT_TRUST_LABEL: &str = "FINAL - PYTH VERIFIED ON SOLANA DEVNET";
pub const ISSUER_SETTLEMENT_TRUST_LABEL: &str = "FINAL - VERIFIED ISSUER ORACLE SETTLEMENT";
pub const PYTH_DEVNET_VERIFIER_PROGRAM: &str = "pytd2yyk641x7ak7mkaasSJVXh6YYZnC7wTmtgAyxPt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SourceKind {
    JupiterTokenSpotV1,
    PythProVerifiedV1,
    /// Compatibility value for snapshots produced before the Pyth Pro source
    /// name was frozen. New snapshots must use `PythProVerifiedV1`.
    Pyth247IndexV1,
    VerifiedIssuerOracleV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RoundState {
    Finalized,
    Voided,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PhaseState {
    Finalized,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BattleResult {
    Pending,
    PlayerA,
    PlayerB,
    Draw,
    ForfeitA,
    ForfeitB,
    BothForfeit,
    Voided,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VoidReason {
    None,
    PriceUnavailable,
    RegistryFault,
    SystemIncident,
    InvalidRound,
    AdminEmergency,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketRoundProof {
    pub round_id: u64,
    /// Canonical PDA for the frozen market round. Numeric ids are retained for
    /// indexing, while the PDA is the public chain identity used for audit.
    #[serde(default)]
    pub market_round_pubkey: String,
    pub registry_version: u32,
    pub price_policy_version: u16,
    pub quality_policy_version: u16,
    pub attestor_set_version: u16,
    /// Frozen Unix-second boundaries copied from the on-chain Market Round.
    /// Pyth evidence timestamps are checked against the matching boundary.
    #[serde(default)]
    pub start_target_at: i64,
    #[serde(default)]
    pub end_target_at: i64,
    #[serde(default = "default_jupiter_source_kind")]
    pub source_kind: SourceKind,
    /// Signature of the transaction that finalized the market round. The
    /// public proof must bind this to the transaction list below.
    #[serde(default)]
    pub settlement_transaction_signature: String,
    pub state: RoundState,
}

fn default_jupiter_source_kind() -> SourceKind {
    SourceKind::JupiterTokenSpotV1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportProof {
    pub attestor: String,
    pub median_price_q9: i64,
    pub evidence_root: String,
    pub accepted_observation_count: u16,
    pub unique_source_block_count: u16,
    pub first_source_block_id: u64,
    pub last_source_block_id: u64,
}

/// Semantic Pyth fields copied from the verified on-chain evidence account.
/// The raw signed payload remains off-chain and is represented only by its
/// commitment, so public proof material never exposes provider credentials or
/// unverifiable client-supplied price claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythEvidenceProof {
    pub feed_id: u32,
    pub target_timestamp_us: u64,
    pub payload_timestamp_us: u64,
    pub feed_update_timestamp_us: u64,
    pub price_mantissa: i64,
    pub confidence_mantissa: u64,
    pub exponent: i16,
    pub normalized_price_q9: i64,
    pub payload_hash: String,
    pub max_payload_timestamp_delta_us: u64,
    pub max_feed_age_us: u64,
    pub max_confidence_bps: u16,
    pub verifier_program: String,
    pub verifier_transaction_signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseProof {
    pub state: PhaseState,
    pub finalized_price_q9: Option<i64>,
    pub selected_reports: Vec<ReportProof>,
    pub evidence_commitment: Option<String>,
    pub pyth_evidence: Option<PythEvidenceProof>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundAssetProof {
    pub asset_id: u16,
    pub symbol: String,
    pub issuer: String,
    /// Exact issuer-approved scoring mint, never a display or fallback mint.
    pub scoring_mint: String,
    /// Canonical PDA and frozen registry representation/provider metadata.
    #[serde(default)]
    pub round_asset_pubkey: String,
    #[serde(default)]
    pub representation_id: u32,
    #[serde(default)]
    pub provider: String,
    pub source_kind: SourceKind,
    pub price_policy_version: u16,
    pub quality_policy_version: u16,
    pub start: PhaseProof,
    pub end: PhaseProof,
    pub return_q9: Option<i64>,
    pub display_return: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BattleProof {
    pub battle_id: u64,
    pub market_round_id: u64,
    #[serde(default)]
    pub battle_pubkey: String,
    #[serde(default)]
    pub market_round_pubkey: String,
    pub side_a_lineup: Vec<u16>,
    pub side_a_captain: Option<u16>,
    pub side_a_score_q9: Option<i64>,
    pub side_b_lineup: Vec<u16>,
    pub side_b_captain: Option<u16>,
    pub side_b_score_q9: Option<i64>,
    pub side_a_participant: ParticipantProof,
    pub side_b_participant: ParticipantProof,
    pub result: BattleResult,
    pub void_reason: VoidReason,
}

/// Public lifecycle evidence for one Battle participant. Commitment
/// preimages and salts never enter this structure; only the transaction
/// signatures and timestamps needed to audit the commit/reveal sequence do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ParticipantProof {
    pub wallet: String,
    pub commit_transaction_signature: Option<String>,
    pub commit_timestamp_unix_secs: Option<i64>,
    pub reveal_transaction_signature: Option<String>,
    pub reveal_timestamp_unix_secs: Option<i64>,
}

/// Input emitted by a chain indexer after it has fetched the authoritative
/// accounts and compared them with the indexed projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofSnapshot {
    pub market_round: MarketRoundProof,
    pub round_assets: Vec<RoundAssetProof>,
    pub battle: Option<BattleProof>,
    pub transaction_signatures: Vec<String>,
    pub indexed_slot: u64,
    pub chain_slot: u64,
    pub chain_reconciled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublicProof {
    pub market_round: MarketRoundProof,
    pub round_assets: Vec<RoundAssetProof>,
    pub battle: Option<BattleProof>,
    pub source_trust_label: &'static str,
    pub reconciled_slot: u64,
    pub transaction_signatures: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofError {
    NotReconciled,
    ChainSlotMismatch,
    EmptyRoundAssets,
    DuplicateAsset,
    InvalidAssetIdentity,
    PolicyMismatch,
    InvalidPhase,
    InvalidReportSet,
    InvalidPrice,
    ReturnMismatch,
    InvalidLineup,
    ScoreMismatch,
    BattleRoundMismatch,
    BattleNotFinalized,
    MissingBattleTransaction,
    InvalidPythEvidence,
    ChainStateMismatch,
    MissingSettlementTransaction,
    NotConfigured,
    PathMismatch,
    Io(String),
    Json(String),
}

impl fmt::Display for ProofError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::NotReconciled => "proof snapshot was not reconciled against chain state",
            Self::ChainSlotMismatch => "indexed and chain slots do not match",
            Self::EmptyRoundAssets => "proof contains no round assets",
            Self::DuplicateAsset => "proof contains a duplicate asset id",
            Self::InvalidAssetIdentity => "proof contains an incomplete asset identity",
            Self::PolicyMismatch => "proof asset policy does not match the frozen round",
            Self::InvalidPhase => "proof phase is internally inconsistent",
            Self::InvalidReportSet => "proof selected-report set is invalid",
            Self::InvalidPrice => "proof contains a non-positive finalized price",
            Self::ReturnMismatch => "proof return does not match the finalized prices",
            Self::InvalidLineup => "proof contains an invalid battle lineup",
            Self::ScoreMismatch => "proof score does not match the canonical lineup formula",
            Self::BattleRoundMismatch => "proof Battle belongs to another market round",
            Self::BattleNotFinalized => "proof Battle result is not terminal",
            Self::MissingBattleTransaction => {
                "proof Battle lifecycle is missing a referenced transaction"
            }
            Self::InvalidPythEvidence => "proof Pyth evidence is not verifier- or policy-valid",
            Self::ChainStateMismatch => "indexed proof differs from reconciled chain state",
            Self::MissingSettlementTransaction => {
                "proof is missing the finalized settlement transaction"
            }
            Self::NotConfigured => "proof snapshot is not configured",
            Self::PathMismatch => "proof path does not match the requested resource",
            Self::Io(error) | Self::Json(error) => error,
        };
        formatter.write_str(message)
    }
}

impl Error for ProofError {}

impl ProofSnapshot {
    /// Compares an indexed projection with a fresh chain read before any
    /// finality label is emitted. The returned snapshot is the only form the
    /// HTTP layer accepts for public proof generation.
    pub fn reconcile(
        indexed: &ProofSnapshot,
        chain: &ProofSnapshot,
    ) -> Result<ProofSnapshot, ProofError> {
        if indexed != chain {
            return Err(ProofError::ChainStateMismatch);
        }
        let mut reconciled = chain.clone();
        reconciled.chain_reconciled = true;
        reconciled.validate()?;
        Ok(reconciled)
    }

    pub fn validate(&self) -> Result<(), ProofError> {
        if !self.chain_reconciled {
            return Err(ProofError::NotReconciled);
        }
        if self.indexed_slot != self.chain_slot {
            return Err(ProofError::ChainSlotMismatch);
        }
        if self.round_assets.is_empty() {
            return Err(ProofError::EmptyRoundAssets);
        }

        let mut seen_assets = Vec::with_capacity(self.round_assets.len());
        let source_kind = self.round_assets[0].source_kind;
        if self.market_round.market_round_pubkey.trim().is_empty() {
            return Err(ProofError::InvalidAssetIdentity);
        }
        if self.market_round.source_kind != source_kind {
            return Err(ProofError::PolicyMismatch);
        }
        if self
            .market_round
            .settlement_transaction_signature
            .trim()
            .is_empty()
            || !self
                .transaction_signatures
                .iter()
                .any(|signature| signature == &self.market_round.settlement_transaction_signature)
        {
            return Err(ProofError::MissingSettlementTransaction);
        }
        for asset in &self.round_assets {
            if !seen_assets
                .iter()
                .all(|existing| existing != &asset.asset_id)
            {
                return Err(ProofError::DuplicateAsset);
            }
            seen_assets.push(asset.asset_id);
            if asset.symbol.trim().is_empty()
                || asset.issuer.trim().is_empty()
                || asset.scoring_mint.trim().is_empty()
                || asset.round_asset_pubkey.trim().is_empty()
                || asset.provider.trim().is_empty()
            {
                return Err(ProofError::InvalidAssetIdentity);
            }
            if asset.price_policy_version != self.market_round.price_policy_version
                || asset.quality_policy_version != self.market_round.quality_policy_version
                || asset.source_kind != source_kind
            {
                return Err(ProofError::PolicyMismatch);
            }
            validate_phase(
                &asset.start,
                asset.source_kind,
                self.market_round.start_target_at,
                &self.transaction_signatures,
            )?;
            validate_phase(
                &asset.end,
                asset.source_kind,
                self.market_round.end_target_at,
                &self.transaction_signatures,
            )?;
            match (asset.start.state, asset.end.state, asset.return_q9) {
                (PhaseState::Finalized, PhaseState::Finalized, Some(return_q9_value)) => {
                    let start = asset
                        .start
                        .finalized_price_q9
                        .ok_or(ProofError::InvalidPrice)?;
                    let end = asset
                        .end
                        .finalized_price_q9
                        .ok_or(ProofError::InvalidPrice)?;
                    if return_q9(start, end).map_err(|_| ProofError::InvalidPrice)?
                        != return_q9_value
                    {
                        return Err(ProofError::ReturnMismatch);
                    }
                }
                (PhaseState::Unavailable, _, None) | (_, PhaseState::Unavailable, None) => {}
                _ => return Err(ProofError::InvalidPhase),
            }
        }

        if let Some(battle) = &self.battle {
            validate_battle(
                battle,
                self.market_round.round_id,
                &self.market_round.market_round_pubkey,
                &self.round_assets,
                &self.transaction_signatures,
            )?;
        }
        Ok(())
    }

    /// Converts only validated, non-secret settlement facts into the public
    /// proof response. Source-specific labels remain distinct so attested
    /// Jupiter evidence is never presented as cryptographically verified Pyth
    /// evidence, or vice versa.
    pub fn into_public(self) -> Result<PublicProof, ProofError> {
        self.validate()?;
        let source_trust_label = trust_label(self.round_assets[0].source_kind);
        let mut round_assets = self.round_assets;
        for asset in &mut round_assets {
            asset.display_return = asset.return_q9.map(format_return_q9);
        }
        Ok(PublicProof {
            market_round: self.market_round,
            round_assets,
            battle: self.battle,
            source_trust_label,
            reconciled_slot: self.chain_slot,
            transaction_signatures: self.transaction_signatures,
        })
    }
}

fn validate_phase(
    phase: &PhaseProof,
    source_kind: SourceKind,
    target_seconds: i64,
    transaction_signatures: &[String],
) -> Result<(), ProofError> {
    match phase.state {
        PhaseState::Finalized => {
            let price = phase.finalized_price_q9.ok_or(ProofError::InvalidPrice)?;
            if price <= 0 || phase.evidence_commitment.is_none() {
                return Err(ProofError::InvalidPrice);
            }
            if matches!(
                source_kind,
                SourceKind::JupiterTokenSpotV1 | SourceKind::VerifiedIssuerOracleV1
            ) && !(2..=3).contains(&phase.selected_reports.len())
            {
                return Err(ProofError::InvalidReportSet);
            }
            if matches!(
                source_kind,
                SourceKind::PythProVerifiedV1 | SourceKind::Pyth247IndexV1
            ) && (!phase.selected_reports.is_empty() || phase.pyth_evidence.is_none())
            {
                return Err(ProofError::InvalidReportSet);
            }
            if matches!(
                source_kind,
                SourceKind::JupiterTokenSpotV1 | SourceKind::VerifiedIssuerOracleV1
            ) && phase.pyth_evidence.is_some()
            {
                return Err(ProofError::InvalidReportSet);
            }
            let mut attestors = Vec::with_capacity(phase.selected_reports.len());
            for report in &phase.selected_reports {
                if report.attestor.trim().is_empty()
                    || report.median_price_q9 <= 0
                    || report.unique_source_block_count == 0
                    || report.unique_source_block_count > report.accepted_observation_count
                    || report.first_source_block_id > report.last_source_block_id
                {
                    return Err(ProofError::InvalidReportSet);
                }
                if attestors
                    .iter()
                    .any(|existing| existing == &report.attestor)
                {
                    return Err(ProofError::InvalidReportSet);
                }
                attestors.push(report.attestor.as_str());
            }
            if let Some(evidence) = &phase.pyth_evidence {
                let expected_target_timestamp_us = u64::try_from(target_seconds)
                    .ok()
                    .and_then(|seconds| seconds.checked_mul(1_000_000))
                    .unwrap_or_default();
                if evidence.feed_id == 0
                    || evidence.target_timestamp_us == 0
                    || evidence.target_timestamp_us != expected_target_timestamp_us
                    || evidence
                        .payload_timestamp_us
                        .abs_diff(evidence.target_timestamp_us)
                        > evidence.max_payload_timestamp_delta_us
                    || evidence.payload_timestamp_us < evidence.feed_update_timestamp_us
                    || evidence.payload_timestamp_us - evidence.feed_update_timestamp_us
                        > evidence.max_feed_age_us
                    || evidence.price_mantissa <= 0
                    || evidence.verifier_program != PYTH_DEVNET_VERIFIER_PROGRAM
                    || evidence.verifier_transaction_signature.trim().is_empty()
                    || !transaction_signatures
                        .iter()
                        .any(|signature| signature == &evidence.verifier_transaction_signature)
                    || confidence_bps(evidence.price_mantissa, evidence.confidence_mantissa)
                        .is_none_or(|value| value > u64::from(evidence.max_confidence_bps))
                    || evidence.normalized_price_q9 != price
                    || evidence.payload_hash.len() != 64
                    || !evidence
                        .payload_hash
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(ProofError::InvalidPythEvidence);
                }
            }
        }
        PhaseState::Unavailable => {
            if phase.finalized_price_q9.is_some()
                || phase.evidence_commitment.is_some()
                || !phase.selected_reports.is_empty()
                || phase.pyth_evidence.is_some()
            {
                return Err(ProofError::InvalidPhase);
            }
        }
    }
    Ok(())
}

fn confidence_bps(price_mantissa: i64, confidence_mantissa: u64) -> Option<u64> {
    if price_mantissa <= 0 {
        return None;
    }
    // Public proof validation must use the same fail-closed rounding rule as
    // the program, otherwise an invalid on-chain confidence bound could be
    // rendered as an apparently valid proof.
    let denominator = i128::from(price_mantissa);
    let numerator = i128::from(confidence_mantissa).checked_mul(10_000)?;
    u64::try_from(
        numerator
            .checked_add(denominator.checked_sub(1)?)?
            .checked_div(denominator)?,
    )
    .ok()
}

fn validate_battle(
    battle: &BattleProof,
    round_id: u64,
    market_round_pubkey: &str,
    assets: &[RoundAssetProof],
    transaction_signatures: &[String],
) -> Result<(), ProofError> {
    if battle.market_round_id != round_id {
        return Err(ProofError::BattleRoundMismatch);
    }
    if battle.battle_pubkey.trim().is_empty()
        || battle.market_round_pubkey.trim().is_empty()
        || battle.market_round_pubkey != market_round_pubkey
    {
        return Err(ProofError::InvalidAssetIdentity);
    }
    for participant in [&battle.side_a_participant, &battle.side_b_participant] {
        if participant.wallet.trim().is_empty() {
            return Err(ProofError::InvalidAssetIdentity);
        }
    }
    if matches!(
        battle.result,
        BattleResult::PlayerA | BattleResult::PlayerB | BattleResult::Draw
    ) {
        for participant in [&battle.side_a_participant, &battle.side_b_participant] {
            if participant.commit_transaction_signature.is_none()
                || participant.commit_timestamp_unix_secs.is_none()
                || participant.reveal_transaction_signature.is_none()
                || participant.reveal_timestamp_unix_secs.is_none()
                || participant.commit_timestamp_unix_secs > participant.reveal_timestamp_unix_secs
            {
                return Err(ProofError::BattleNotFinalized);
            }
            for signature in [
                participant.commit_transaction_signature.as_ref(),
                participant.reveal_transaction_signature.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                if !transaction_signatures
                    .iter()
                    .any(|known| known == signature)
                {
                    return Err(ProofError::MissingBattleTransaction);
                }
            }
        }
    }
    validate_lineup(&battle.side_a_lineup, battle.side_a_captain, assets)?;
    validate_lineup(&battle.side_b_lineup, battle.side_b_captain, assets)?;
    if battle.result == BattleResult::Pending {
        return Err(ProofError::BattleNotFinalized);
    }

    if battle.result == BattleResult::Voided {
        if battle.void_reason == VoidReason::None {
            return Err(ProofError::BattleNotFinalized);
        }
    } else if battle.void_reason != VoidReason::None {
        return Err(ProofError::BattleNotFinalized);
    }
    if matches!(
        battle.result,
        BattleResult::PlayerA | BattleResult::PlayerB | BattleResult::Draw
    ) && (battle.side_a_score_q9.is_none() || battle.side_b_score_q9.is_none())
    {
        return Err(ProofError::BattleNotFinalized);
    }

    if let (Some(score), Some(captain)) = (battle.side_a_score_q9, battle.side_a_captain) {
        let returns = lineup_returns(&battle.side_a_lineup, assets)?;
        let expected = score_q9(returns, &battle.side_a_lineup, captain)?;
        if expected != score {
            return Err(ProofError::ScoreMismatch);
        }
    }
    if let (Some(score), Some(captain)) = (battle.side_b_score_q9, battle.side_b_captain) {
        let returns = lineup_returns(&battle.side_b_lineup, assets)?;
        let expected = score_q9(returns, &battle.side_b_lineup, captain)?;
        if expected != score {
            return Err(ProofError::ScoreMismatch);
        }
    }
    if let (Some(score_a), Some(score_b)) = (battle.side_a_score_q9, battle.side_b_score_q9) {
        let expected_result = if score_a > score_b {
            BattleResult::PlayerA
        } else if score_a < score_b {
            BattleResult::PlayerB
        } else {
            BattleResult::Draw
        };
        if matches!(
            battle.result,
            BattleResult::PlayerA | BattleResult::PlayerB | BattleResult::Draw
        ) && battle.result != expected_result
        {
            return Err(ProofError::ScoreMismatch);
        }
    }
    Ok(())
}

fn validate_lineup(
    lineup: &[u16],
    captain: Option<u16>,
    assets: &[RoundAssetProof],
) -> Result<(), ProofError> {
    let Some(captain) = captain else {
        return Ok(());
    };
    if lineup.len() != 6
        || lineup
            .iter()
            .any(|asset_id| !assets.iter().any(|asset| asset.asset_id == *asset_id))
        || lineup
            .iter()
            .filter(|asset_id| **asset_id == captain)
            .count()
            != 1
    {
        return Err(ProofError::InvalidLineup);
    }
    let mut sorted = lineup.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() != 6 {
        return Err(ProofError::InvalidLineup);
    }
    Ok(())
}

fn lineup_returns(lineup: &[u16], assets: &[RoundAssetProof]) -> Result<[i64; 6], ProofError> {
    if lineup.len() != 6 {
        return Err(ProofError::InvalidLineup);
    }
    let mut returns = [0i64; 6];
    for (index, asset_id) in lineup.iter().enumerate() {
        let asset = assets
            .iter()
            .find(|asset| asset.asset_id == *asset_id)
            .ok_or(ProofError::InvalidLineup)?;
        returns[index] = asset.return_q9.ok_or(ProofError::InvalidLineup)?;
    }
    Ok(returns)
}

fn score_q9(returns: [i64; 6], lineup: &[u16], captain: u16) -> Result<i64, ProofError> {
    let mut total = 0i128;
    for (asset_return, asset_id) in returns.into_iter().zip(lineup) {
        let weight = if *asset_id == captain { 2i128 } else { 1 };
        total = total
            .checked_add(i128::from(asset_return) * weight)
            .ok_or(ProofError::ScoreMismatch)?;
    }
    i64::try_from(total / 7).map_err(|_| ProofError::ScoreMismatch)
}

fn trust_label(source_kind: SourceKind) -> &'static str {
    match source_kind {
        SourceKind::JupiterTokenSpotV1 => JUPITER_SETTLEMENT_TRUST_LABEL,
        SourceKind::PythProVerifiedV1 => PYTH_SETTLEMENT_TRUST_LABEL,
        SourceKind::Pyth247IndexV1 => PYTH_SETTLEMENT_TRUST_LABEL,
        SourceKind::VerifiedIssuerOracleV1 => ISSUER_SETTLEMENT_TRUST_LABEL,
    }
}

fn format_return_q9(value: i64) -> String {
    let negative = value < 0;
    let magnitude = i128::from(value).abs();
    let hundredths = magnitude / 100_000;
    let whole = hundredths / 100;
    let fraction = hundredths % 100;
    format!(
        "{}{}.{fraction:02}%",
        if negative { "-" } else { "" },
        whole
    )
}

/// Loads and validates a reconciled proof snapshot from disk for the HTTP API.
pub fn load_public_from_file(path: impl AsRef<Path>) -> Result<PublicProof, ProofError> {
    let snapshot: ProofSnapshot = serde_json::from_str(
        &fs::read_to_string(path).map_err(|error| ProofError::Io(error.to_string()))?,
    )
    .map_err(|error| ProofError::Json(error.to_string()))?;
    snapshot.into_public()
}

/// Loads a reconciled proof snapshot from disk and serves read-only proof
/// routes. The `/v1/.../{pubkey}/proof` routes are the public API contract;
/// numeric legacy routes remain available for local fixture compatibility.
pub fn serve_from_file(path: impl AsRef<Path>, bind_address: &str) -> Result<(), ProofError> {
    let proof = load_public_from_file(path)?;
    let body = serde_json::to_vec(&proof).map_err(|error| ProofError::Json(error.to_string()))?;
    let listener =
        TcpListener::bind(bind_address).map_err(|error| ProofError::Io(error.to_string()))?;
    println!("proof endpoint listening on http://{bind_address}");
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                if let Err(error) = respond(&mut stream, &proof, &body) {
                    eprintln!("proof request failed: {error}");
                }
            }
            Err(error) => eprintln!("proof connection failed: {error}"),
        }
    }
    Ok(())
}

fn respond(stream: &mut TcpStream, proof: &PublicProof, body: &[u8]) -> Result<(), ProofError> {
    let mut request = [0u8; 2048];
    let bytes_read = stream
        .read(&mut request)
        .map_err(|error| ProofError::Io(error.to_string()))?;
    let request_line = String::from_utf8_lossy(&request[..bytes_read]);
    let path = request_line.split_whitespace().nth(1).unwrap_or_default();
    let expected_round = format!("/proof/round/{}", proof.market_round.round_id);
    let expected_battle = proof
        .battle
        .as_ref()
        .map(|battle| format!("/proof/battle/{}", battle.battle_id));
    let (status, payload): (&str, &[u8]) = if request_line.starts_with("GET ")
        && (proof_path_matches(path, proof)
            || path == expected_round
            || expected_battle.as_deref() == Some(path))
    {
        ("200 OK", body)
    } else if request_line.starts_with("GET ") {
        ("404 Not Found", br#"{"error":"proof not found"}"#)
    } else {
        (
            "405 Method Not Allowed",
            br#"{"error":"read-only GET endpoint"}"#,
        )
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream
        .write_all(header.as_bytes())
        .and_then(|_| stream.write_all(payload))
        .map_err(|error| ProofError::Io(error.to_string()))
}

pub fn proof_path_matches(path: &str, proof: &PublicProof) -> bool {
    if path
        == format!(
            "/v1/market-rounds/{}/proof",
            proof.market_round.market_round_pubkey
        )
    {
        return true;
    }
    proof
        .battle
        .as_ref()
        .is_some_and(|battle| path == format!("/v1/battles/{}/proof", battle.battle_pubkey))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(attestor: &str, price: i64) -> ReportProof {
        ReportProof {
            attestor: attestor.to_owned(),
            median_price_q9: price,
            evidence_root: "root".to_owned(),
            accepted_observation_count: 3,
            unique_source_block_count: 3,
            first_source_block_id: 10,
            last_source_block_id: 12,
        }
    }

    fn phase(price: i64) -> PhaseProof {
        PhaseProof {
            state: PhaseState::Finalized,
            finalized_price_q9: Some(price),
            selected_reports: vec![report("a", price), report("b", price)],
            evidence_commitment: Some("commitment".to_owned()),
            pyth_evidence: None,
        }
    }

    fn pyth_phase(price: i64) -> PhaseProof {
        pyth_phase_at(price, 1_000_000)
    }

    fn pyth_phase_at(price: i64, target_timestamp_us: u64) -> PhaseProof {
        PhaseProof {
            state: PhaseState::Finalized,
            finalized_price_q9: Some(price),
            selected_reports: Vec::new(),
            evidence_commitment: Some("pyth-commitment".to_owned()),
            pyth_evidence: Some(PythEvidenceProof {
                feed_id: 42,
                target_timestamp_us,
                payload_timestamp_us: target_timestamp_us,
                feed_update_timestamp_us: target_timestamp_us,
                price_mantissa: price,
                confidence_mantissa: 1,
                exponent: -9,
                normalized_price_q9: price,
                payload_hash: "00".repeat(32),
                max_payload_timestamp_delta_us: 1,
                max_feed_age_us: 1,
                max_confidence_bps: 100,
                verifier_program: PYTH_DEVNET_VERIFIER_PROGRAM.to_owned(),
                verifier_transaction_signature: "pyth-verifier-signature".to_owned(),
            }),
        }
    }

    fn snapshot() -> ProofSnapshot {
        ProofSnapshot {
            market_round: MarketRoundProof {
                round_id: 7,
                market_round_pubkey: "MarketRoundPda".to_owned(),
                registry_version: 1,
                price_policy_version: 1,
                quality_policy_version: 1,
                attestor_set_version: 1,
                start_target_at: 1,
                end_target_at: 2,
                source_kind: SourceKind::JupiterTokenSpotV1,
                settlement_transaction_signature: "settlement-signature".to_owned(),
                state: RoundState::Finalized,
            },
            round_assets: vec![RoundAssetProof {
                asset_id: 1,
                symbol: "ASSET".to_owned(),
                issuer: "Issuer".to_owned(),
                scoring_mint: "ExactMint".to_owned(),
                round_asset_pubkey: "RoundAssetPda".to_owned(),
                representation_id: 1,
                provider: "Jupiter".to_owned(),
                source_kind: SourceKind::JupiterTokenSpotV1,
                price_policy_version: 1,
                quality_policy_version: 1,
                start: phase(100_000_000_000),
                end: phase(110_000_000_000),
                return_q9: Some(100_000_000),
                display_return: Some("10.00%".to_owned()),
            }],
            battle: None,
            transaction_signatures: vec![
                "signature".to_owned(),
                "settlement-signature".to_owned(),
                "pyth-verifier-signature".to_owned(),
            ],
            indexed_slot: 42,
            chain_slot: 42,
            chain_reconciled: true,
        }
    }

    #[test]
    fn proof_requires_reconciliation_and_matches_exact_return() {
        let mut pending = snapshot();
        pending.chain_reconciled = false;
        assert_eq!(pending.validate(), Err(ProofError::NotReconciled));
        assert!(snapshot().validate().is_ok());
        assert_eq!(
            snapshot().into_public().unwrap().source_trust_label,
            JUPITER_SETTLEMENT_TRUST_LABEL
        );
    }

    #[test]
    fn jupiter_proof_never_uses_pyth_verification_label() {
        let proof = snapshot().into_public().unwrap();
        assert_ne!(proof.source_trust_label, "PYTH VERIFIED");
        assert_eq!(proof.source_trust_label, JUPITER_SETTLEMENT_TRUST_LABEL);
    }

    #[test]
    fn reconciliation_rejects_indexed_chain_divergence() {
        let indexed = snapshot();
        let mut chain = snapshot();
        chain.round_assets[0].end.finalized_price_q9 = Some(111_000_000_000);
        assert_eq!(
            ProofSnapshot::reconcile(&indexed, &chain),
            Err(ProofError::ChainStateMismatch)
        );
    }

    #[test]
    fn pyth_proof_requires_pyth_evidence_and_uses_the_devnet_label() {
        let mut proof = snapshot();
        proof.market_round.source_kind = SourceKind::PythProVerifiedV1;
        proof.round_assets[0].source_kind = SourceKind::PythProVerifiedV1;
        proof.round_assets[0].start = pyth_phase(100_000_000_000);
        proof.round_assets[0].end = pyth_phase_at(110_000_000_000, 2_000_000);
        assert_eq!(
            proof.into_public().unwrap().source_trust_label,
            PYTH_SETTLEMENT_TRUST_LABEL
        );

        let mut missing = snapshot();
        missing.market_round.source_kind = SourceKind::PythProVerifiedV1;
        missing.round_assets[0].source_kind = SourceKind::PythProVerifiedV1;
        missing.round_assets[0].start = PhaseProof {
            pyth_evidence: None,
            ..phase(100_000_000_000)
        };
        assert_eq!(missing.validate(), Err(ProofError::InvalidReportSet));
    }

    #[test]
    fn pyth_proof_rejects_an_unpinned_verifier_or_missing_verifier_transaction() {
        let mut wrong_program = snapshot();
        wrong_program.market_round.source_kind = SourceKind::PythProVerifiedV1;
        wrong_program.round_assets[0].source_kind = SourceKind::PythProVerifiedV1;
        wrong_program.round_assets[0].start = pyth_phase(100_000_000_000);
        wrong_program.round_assets[0].end = pyth_phase_at(110_000_000_000, 2_000_000);
        wrong_program.round_assets[0]
            .start
            .pyth_evidence
            .as_mut()
            .unwrap()
            .verifier_program = "attacker-program".to_owned();
        assert_eq!(
            wrong_program.validate(),
            Err(ProofError::InvalidPythEvidence)
        );

        let mut missing_transaction = snapshot();
        missing_transaction.market_round.source_kind = SourceKind::PythProVerifiedV1;
        missing_transaction.round_assets[0].source_kind = SourceKind::PythProVerifiedV1;
        missing_transaction.round_assets[0].start = pyth_phase(100_000_000_000);
        missing_transaction.round_assets[0].end = pyth_phase_at(110_000_000_000, 2_000_000);
        missing_transaction
            .transaction_signatures
            .retain(|signature| signature != "pyth-verifier-signature");
        assert_eq!(
            missing_transaction.validate(),
            Err(ProofError::InvalidPythEvidence)
        );
    }

    #[test]
    fn pyth_proof_rejects_confidence_that_exceeds_the_policy_after_rounding_up() {
        let mut proof = snapshot();
        proof.market_round.source_kind = SourceKind::PythProVerifiedV1;
        proof.round_assets[0].source_kind = SourceKind::PythProVerifiedV1;
        proof.round_assets[0].start = pyth_phase(100_000_000_000);
        proof.round_assets[0].end = pyth_phase_at(110_000_000_000, 2_000_000);
        let evidence = proof.round_assets[0].start.pyth_evidence.as_mut().unwrap();
        evidence.price_mantissa = 3;
        evidence.confidence_mantissa = 1;
        evidence.max_confidence_bps = 3_333;
        assert_eq!(proof.validate(), Err(ProofError::InvalidPythEvidence));
    }

    #[test]
    fn pyth_proof_rejects_evidence_not_bound_to_the_frozen_phase_target() {
        let mut proof = snapshot();
        proof.market_round.source_kind = SourceKind::PythProVerifiedV1;
        proof.round_assets[0].source_kind = SourceKind::PythProVerifiedV1;
        proof.round_assets[0].start = pyth_phase(100_000_000_000);
        proof.round_assets[0].end = pyth_phase_at(110_000_000_000, 2_000_000);
        proof.round_assets[0]
            .start
            .pyth_evidence
            .as_mut()
            .unwrap()
            .target_timestamp_us = 2_000_000;
        assert_eq!(proof.validate(), Err(ProofError::InvalidPythEvidence));
    }

    #[test]
    fn jupiter_proof_requires_frozen_source_and_settlement_transaction() {
        let mut missing_transaction = snapshot();
        missing_transaction
            .market_round
            .settlement_transaction_signature
            .clear();
        assert_eq!(
            missing_transaction.validate(),
            Err(ProofError::MissingSettlementTransaction)
        );

        let mut mismatched_source = snapshot();
        mismatched_source.market_round.source_kind = SourceKind::PythProVerifiedV1;
        assert_eq!(
            mismatched_source.validate(),
            Err(ProofError::PolicyMismatch)
        );
    }

    #[test]
    fn proof_route_uses_the_public_battle_and_market_round_paths() {
        let proof = snapshot().into_public().unwrap();

        assert!(proof_path_matches(
            "/v1/market-rounds/MarketRoundPda/proof",
            &proof
        ));
        assert!(!proof_path_matches("/v1/battles/unknown/proof", &proof));
    }

    #[test]
    fn jupiter_proof_validates_the_complete_battle_lifecycle() {
        let mut proof = snapshot();
        proof.round_assets = (1..=6)
            .map(|asset_id| {
                let end_price = if asset_id == 1 {
                    110_000_000_000
                } else {
                    100_000_000_000
                };
                RoundAssetProof {
                    asset_id,
                    symbol: format!("ASSET{asset_id}"),
                    issuer: "Issuer".to_owned(),
                    scoring_mint: format!("Mint{asset_id}"),
                    round_asset_pubkey: format!("RoundAssetPda{asset_id}"),
                    representation_id: u32::from(asset_id),
                    provider: "Jupiter".to_owned(),
                    source_kind: SourceKind::JupiterTokenSpotV1,
                    price_policy_version: 1,
                    quality_policy_version: 1,
                    start: phase(100_000_000_000),
                    end: phase(end_price),
                    return_q9: Some(if asset_id == 1 { 100_000_000 } else { 0 }),
                    display_return: None,
                }
            })
            .collect();
        proof.transaction_signatures.extend([
            "commit-a".to_owned(),
            "reveal-a".to_owned(),
            "commit-b".to_owned(),
            "reveal-b".to_owned(),
        ]);
        proof.battle = Some(BattleProof {
            battle_id: 8,
            market_round_id: 7,
            battle_pubkey: "BattlePda".to_owned(),
            market_round_pubkey: "MarketRoundPda".to_owned(),
            side_a_lineup: vec![1, 2, 3, 4, 5, 6],
            side_a_captain: Some(1),
            side_a_score_q9: Some(28_571_428),
            side_b_lineup: vec![1, 2, 3, 4, 5, 6],
            side_b_captain: Some(2),
            side_b_score_q9: Some(14_285_714),
            side_a_participant: ParticipantProof {
                wallet: "WalletA".to_owned(),
                commit_transaction_signature: Some("commit-a".to_owned()),
                commit_timestamp_unix_secs: Some(120),
                reveal_transaction_signature: Some("reveal-a".to_owned()),
                reveal_timestamp_unix_secs: Some(125),
            },
            side_b_participant: ParticipantProof {
                wallet: "WalletB".to_owned(),
                commit_transaction_signature: Some("commit-b".to_owned()),
                commit_timestamp_unix_secs: Some(120),
                reveal_transaction_signature: Some("reveal-b".to_owned()),
                reveal_timestamp_unix_secs: Some(125),
            },
            result: BattleResult::PlayerA,
            void_reason: VoidReason::None,
        });

        let public = proof.into_public().unwrap();
        assert_eq!(public.source_trust_label, JUPITER_SETTLEMENT_TRUST_LABEL);
        assert_eq!(public.battle.unwrap().result, BattleResult::PlayerA);
    }
}
