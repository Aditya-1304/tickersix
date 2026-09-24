//! Small dependency-free Prometheus text registry for the hackathon backend.

use std::{
    collections::BTreeMap,
    fmt::Write,
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone, Default)]
pub struct Metrics {
    values: Arc<Mutex<BTreeMap<String, i64>>>,
}

impl Metrics {
    pub fn increment(&self, name: &str, amount: i64) {
        let mut values = self
            .values
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let value = values.entry(name.to_owned()).or_default();
        *value = value.saturating_add(amount);
    }

    pub fn set(&self, name: &str, value: i64) {
        let mut values = self
            .values
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        values.insert(name.to_owned(), value);
    }

    fn get(&self, name: &str) -> i64 {
        self.values
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(name)
            .copied()
            .unwrap_or_default()
    }

    /// Records one Ranked rating-gap observation in fixed, bounded buckets.
    pub fn observe_rating_gap(&self, gap: i64) {
        let bucket = match gap {
            0..=25 => "25",
            26..=50 => "50",
            51..=100 => "100",
            101..=200 => "200",
            _ => "+Inf",
        };
        self.increment(&format!("match_rating_gap_bucket{{le=\"{bucket}\"}}"), 1);
        self.increment("match_rating_gap_count", 1);
        self.increment("match_rating_gap_sum", gap.max(0));
    }

    pub fn render(&self) -> String {
        let mut output = String::new();
        for (name, help, metric_type) in KNOWN_METRICS {
            let _ = writeln!(output, "# HELP {name} {help}");
            let _ = writeln!(output, "# TYPE {name} {metric_type}");
            let _ = writeln!(output, "{name} {}", self.get(name));
        }
        let commit_attempts = self.get("battle_commit_attempts_total");
        let commit_successes = self.get("battle_commit_success_total");
        let reveal_attempts = self.get("battle_reveal_attempts_total");
        let reveal_successes = self.get("battle_reveal_success_total");
        let values = self
            .values
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (name, value) in values.iter() {
            if !KNOWN_METRICS.iter().any(|(known, _, _)| known == name) {
                let _ = writeln!(output, "{name} {value}");
            }
        }
        let commit_rate = if commit_attempts == 0 {
            0
        } else {
            commit_successes.saturating_mul(1_000) / commit_attempts
        };
        let reveal_rate = if reveal_attempts == 0 {
            0
        } else {
            reveal_successes.saturating_mul(1_000) / reveal_attempts
        };
        let _ = writeln!(
            output,
            "# HELP battle_commit_success_rate Coordinator confirmation success rate in per mille"
        );
        let _ = writeln!(output, "# TYPE battle_commit_success_rate gauge");
        let _ = writeln!(output, "battle_commit_success_rate {commit_rate}");
        let _ = writeln!(
            output,
            "# HELP battle_reveal_success_rate Battle reveal success rate in per mille"
        );
        let _ = writeln!(output, "# TYPE battle_reveal_success_rate gauge");
        let _ = writeln!(output, "battle_reveal_success_rate {reveal_rate}");
        output
    }
}

static GLOBAL: OnceLock<Metrics> = OnceLock::new();

pub fn global() -> &'static Metrics {
    GLOBAL.get_or_init(Metrics::default)
}
pub fn increment(name: &str, amount: i64) {
    global().increment(name, amount);
}
pub fn decrement(name: &str, amount: i64) {
    global().increment(name, -amount);
}
pub fn set(name: &str, value: i64) {
    global().set(name, value);
}

pub fn observe_rating_gap(gap: i64) {
    global().observe_rating_gap(gap);
}

pub fn observe_league_pairing_duration(milliseconds: i64) {
    set("league_pairing_duration_ms", milliseconds.max(0));
}

const KNOWN_METRICS: &[(&str, &str, &str)] = &[
    (
        "ranked_queue_depth",
        "Current queued Ranked players",
        "gauge",
    ),
    (
        "ranked_matches_created_total",
        "Ranked pairings created",
        "counter",
    ),
    (
        "ranked_unmatched_total",
        "Ranked players left unmatched",
        "counter",
    ),
    (
        "match_rating_gap_count",
        "Observed Ranked rating gaps",
        "counter",
    ),
    (
        "match_rating_gap_sum",
        "Sum of Ranked rating gaps",
        "counter",
    ),
    (
        "league_pairing_duration_ms",
        "Last League pairing duration",
        "gauge",
    ),
    (
        "league_repeat_pairings_total",
        "League pairings requiring a repeat",
        "counter",
    ),
    (
        "battle_commit_attempts_total",
        "Coordinator Battle confirmation attempts",
        "counter",
    ),
    (
        "battle_commit_success_total",
        "Coordinator Battle confirmations",
        "counter",
    ),
    (
        "battle_reveal_attempts_total",
        "Battle reveal attempts",
        "counter",
    ),
    (
        "battle_reveal_success_total",
        "Successful Battle reveals",
        "counter",
    ),
    (
        "market_quality_rejections_total",
        "Market observations rejected by quality policy",
        "counter",
    ),
    (
        "representation_lifecycle_rejections_total",
        "Representation lifecycle rejections",
        "counter",
    ),
    (
        "settlement_round_total",
        "Settlement rounds planned",
        "counter",
    ),
    ("settlement_failure_total", "Settlement failures", "counter"),
    (
        "battle_settlement_delay_seconds",
        "Latest Battle settlement delay",
        "gauge",
    ),
    (
        "rating_event_failures_total",
        "Rating worker failures",
        "counter",
    ),
    (
        "rating_reconciliation_mismatch_total",
        "Rating reconciliation mismatches",
        "counter",
    ),
    (
        "achievement_unlocks_total",
        "Achievement unlocks materialized from authoritative history",
        "counter",
    ),
    (
        "achievement_evidence_missing_total",
        "Settled eligible Battles missing valid finalized lineup evidence",
        "counter",
    ),
    (
        "achievement_reconciliation_failures_total",
        "Achievement reconciliation failures",
        "counter",
    ),
    (
        "operational_incidents_total",
        "Retained fail-closed operational incidents",
        "counter",
    ),
    ("forfeit_total", "Player-attributable forfeits", "counter"),
    ("battle_void_total", "Voided Battles", "counter"),
    (
        "sse_connected_clients",
        "Active Battle SSE streams",
        "gauge",
    ),
    ("sse_publish_lag_ms", "Latest SSE projection lag", "gauge"),
    ("sse_stream_errors_total", "SSE stream errors", "counter"),
    (
        "scheduler_ticks_total",
        "Completed scheduler ticks",
        "counter",
    ),
    (
        "sse_unavailable_total",
        "SSE Battle-unavailable terminal events",
        "counter",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_render_known_and_derived_values_without_network_dependencies() {
        let metrics = Metrics::default();
        metrics.increment("battle_commit_attempts_total", 4);
        metrics.increment("battle_commit_success_total", 3);
        metrics.observe_rating_gap(51);
        let rendered = metrics.render();
        assert!(rendered.contains("ranked_queue_depth 0"));
        assert!(rendered.contains("battle_commit_success_rate 750"));
        assert!(rendered.contains("match_rating_gap_count 1"));
        assert!(rendered.contains("match_rating_gap_bucket{le=\"100\"} 1"));
    }
}
