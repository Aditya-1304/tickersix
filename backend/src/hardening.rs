//! Failure classification and incident evidence retention.
//!
//! Provider and infrastructure failures are never converted into player
//! penalties or silent source switches. The decision matrix is pure and is
//! persisted separately from Battle projections so recovery can audit it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;

use crate::metrics;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FailureClass {
    PlayerNoShow,
    BothNoShow,
    PriceUnavailable,
    ProviderOutage,
    PythVerifierFailure,
    DatabaseUnavailable,
    RevealWorkerOutage,
    SystemVoid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FailureDecision {
    pub terminal_action: &'static str,
    pub player_penalty: bool,
    pub opponent_rating_gain: bool,
    pub competitive_achievement: bool,
    pub source_switch_allowed: bool,
    pub evidence_retained: bool,
}

pub fn failure_decision(class: FailureClass) -> FailureDecision {
    match class {
        FailureClass::PlayerNoShow => FailureDecision {
            terminal_action: "FORFEIT",
            player_penalty: true,
            opponent_rating_gain: false,
            competitive_achievement: false,
            source_switch_allowed: false,
            evidence_retained: true,
        },
        FailureClass::BothNoShow => FailureDecision {
            terminal_action: "BOTH_FORFEIT",
            player_penalty: true,
            opponent_rating_gain: false,
            competitive_achievement: false,
            source_switch_allowed: false,
            evidence_retained: true,
        },
        FailureClass::PriceUnavailable => FailureDecision {
            terminal_action: "VOIDED_PRICE_UNAVAILABLE",
            player_penalty: false,
            opponent_rating_gain: false,
            competitive_achievement: false,
            source_switch_allowed: false,
            evidence_retained: true,
        },
        FailureClass::ProviderOutage | FailureClass::PythVerifierFailure => FailureDecision {
            terminal_action: "SOURCE_UNAVAILABLE",
            player_penalty: false,
            opponent_rating_gain: false,
            competitive_achievement: false,
            source_switch_allowed: false,
            evidence_retained: true,
        },
        FailureClass::DatabaseUnavailable => FailureDecision {
            terminal_action: "RECONCILE_AFTER_RECOVERY",
            player_penalty: false,
            opponent_rating_gain: false,
            competitive_achievement: false,
            source_switch_allowed: false,
            evidence_retained: true,
        },
        FailureClass::RevealWorkerOutage | FailureClass::SystemVoid => FailureDecision {
            terminal_action: "SYSTEM_VOID",
            player_penalty: false,
            opponent_rating_gain: false,
            competitive_achievement: false,
            source_switch_allowed: false,
            evidence_retained: true,
        },
    }
}

#[derive(Debug)]
pub struct IncidentInput<'a> {
    pub event_key: &'a str,
    pub class: FailureClass,
    pub competition_domain: Option<&'a str>,
    pub market_round_id: Option<i64>,
    pub battle_pubkey: Option<&'a str>,
    pub settlement_source_kind: Option<&'a str>,
    pub evidence: Value,
    pub occurred_at: i64,
}

/// Persists an incident with only structured, non-secret evidence. Replayed
/// indexer updates use event_key idempotency and do not create duplicates.
pub async fn record_incident(pool: &PgPool, input: IncidentInput<'_>) -> Result<bool, sqlx::Error> {
    let decision = failure_decision(input.class);
    let result = sqlx::query(
        "INSERT INTO operational_incidents
            (event_key, failure_class, competition_domain, market_round_id,
             battle_pubkey, settlement_source_kind, terminal_action,
             player_penalty, opponent_rating_gain, competitive_achievement,
             source_switch_allowed, evidence, occurred_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         ON CONFLICT (event_key) DO NOTHING",
    )
    .bind(input.event_key)
    .bind(serde_json::to_string(&input.class).expect("failure class serializes"))
    .bind(input.competition_domain)
    .bind(input.market_round_id)
    .bind(input.battle_pubkey)
    .bind(input.settlement_source_kind)
    .bind(decision.terminal_action)
    .bind(decision.player_penalty)
    .bind(decision.opponent_rating_gain)
    .bind(decision.competitive_achievement)
    .bind(decision.source_switch_allowed)
    .bind(input.evidence)
    .bind(input.occurred_at)
    .execute(pool)
    .await?;
    if result.rows_affected() == 1 {
        metrics::increment("operational_incidents_total", 1);
        Ok(true)
    } else {
        Ok(false)
    }
}

pub async fn count_incidents(pool: &PgPool, class: FailureClass) -> Result<i64, sqlx::Error> {
    let value: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM operational_incidents WHERE failure_class = $1")
            .bind(serde_json::to_string(&class).expect("failure class serializes"))
            .fetch_one(pool)
            .await?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_and_verifier_failures_fail_closed_without_player_penalty_or_fallback() {
        for class in [
            FailureClass::ProviderOutage,
            FailureClass::PythVerifierFailure,
        ] {
            let decision = failure_decision(class);
            assert_eq!(decision.terminal_action, "SOURCE_UNAVAILABLE");
            assert!(!decision.player_penalty);
            assert!(!decision.opponent_rating_gain);
            assert!(!decision.competitive_achievement);
            assert!(!decision.source_switch_allowed);
            assert!(decision.evidence_retained);
        }
    }

    #[test]
    fn price_unavailable_voids_without_scoring_a_partial_lineup() {
        let decision = failure_decision(FailureClass::PriceUnavailable);
        assert_eq!(decision.terminal_action, "VOIDED_PRICE_UNAVAILABLE");
        assert!(!decision.player_penalty);
        assert!(!decision.competitive_achievement);
        assert!(decision.evidence_retained);
    }

    #[test]
    fn no_show_penalty_does_not_create_a_played_opponent_win() {
        let decision = failure_decision(FailureClass::PlayerNoShow);
        assert_eq!(decision.terminal_action, "FORFEIT");
        assert!(decision.player_penalty);
        assert!(!decision.opponent_rating_gain);
        assert!(!decision.competitive_achievement);
    }
}
