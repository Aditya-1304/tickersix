-- Retained, non-secret operational evidence for deterministic fail-closed paths.
--
-- This table deliberately has no foreign keys to competitive projections: an
-- incident must remain available while a repaired indexer rebuilds those rows.

CREATE TABLE IF NOT EXISTS operational_incidents (
    id BIGSERIAL PRIMARY KEY,
    event_key TEXT NOT NULL UNIQUE,
    failure_class TEXT NOT NULL,
    competition_domain TEXT,
    market_round_id BIGINT,
    battle_pubkey TEXT,
    settlement_source_kind TEXT,
    terminal_action TEXT NOT NULL,
    player_penalty BOOLEAN NOT NULL,
    opponent_rating_gain BOOLEAN NOT NULL,
    competitive_achievement BOOLEAN NOT NULL,
    source_switch_allowed BOOLEAN NOT NULL,
    evidence JSONB NOT NULL,
    occurred_at BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS operational_incidents_lookup_idx
    ON operational_incidents (occurred_at DESC, failure_class, event_key);
