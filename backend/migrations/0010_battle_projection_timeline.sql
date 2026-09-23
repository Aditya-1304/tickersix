-- Retain the source-aware projected/final timeline required for read-only
-- Battle replay. Replay consumes this table and never refetches market data.

CREATE TABLE IF NOT EXISTS battle_projection_timeline (
    battle_pubkey TEXT NOT NULL REFERENCES battles(chain_pubkey),
    event_id BIGINT NOT NULL,
    state TEXT NOT NULL,
    result TEXT,
    settlement_source_kind TEXT,
    projection_status TEXT NOT NULL,
    player_a_score_q9 BIGINT,
    player_b_score_q9 BIGINT,
    as_of BIGINT NOT NULL,
    recorded_at BIGINT NOT NULL,
    PRIMARY KEY (battle_pubkey, event_id),
    CHECK (projection_status IN ('PROJECTED', 'FINAL')),
    CHECK (as_of >= 0),
    CHECK (recorded_at >= 0)
);

CREATE INDEX IF NOT EXISTS battle_projection_timeline_order_idx
    ON battle_projection_timeline (battle_pubkey, event_id ASC, recorded_at ASC);
