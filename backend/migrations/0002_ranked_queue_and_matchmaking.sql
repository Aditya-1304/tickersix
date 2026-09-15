-- Ranked queue and coordinator-admission state.
--
-- League reservations are intentionally only a blocker contract here. Swiss
-- pairing and League lifecycle remain future responsibilities.

CREATE TABLE IF NOT EXISTS ranked_queue (
    market_round_id BIGINT NOT NULL REFERENCES market_rounds(id),
    wallet TEXT NOT NULL REFERENCES users(wallet),
    rating_snapshot INTEGER,
    rating_snapshot_at BIGINT,
    joined_at BIGINT NOT NULL,
    status TEXT NOT NULL,
    PRIMARY KEY (market_round_id, wallet)
);

CREATE INDEX IF NOT EXISTS ranked_queue_cutoff_idx
    ON ranked_queue (market_round_id, status, joined_at, wallet);

CREATE TABLE IF NOT EXISTS league_reservations (
    market_round_id BIGINT NOT NULL REFERENCES market_rounds(id),
    wallet TEXT NOT NULL REFERENCES users(wallet),
    league_id BIGINT NOT NULL,
    status TEXT NOT NULL,
    PRIMARY KEY (market_round_id, wallet)
);

CREATE TABLE IF NOT EXISTS ranked_match_runs (
    id BIGSERIAL PRIMARY KEY,
    market_round_id BIGINT NOT NULL REFERENCES market_rounds(id),
    cutoff_at BIGINT NOT NULL,
    pairing_policy_version INTEGER NOT NULL,
    status TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    UNIQUE (market_round_id, cutoff_at, pairing_policy_version)
);

CREATE TABLE IF NOT EXISTS ranked_pairings (
    id BIGSERIAL PRIMARY KEY,
    match_run_id BIGINT NOT NULL REFERENCES ranked_match_runs(id),
    market_round_id BIGINT NOT NULL REFERENCES market_rounds(id),
    player_a TEXT NOT NULL REFERENCES users(wallet),
    player_b TEXT NOT NULL REFERENCES users(wallet),
    rating_a_snapshot INTEGER NOT NULL,
    rating_b_snapshot INTEGER NOT NULL,
    battle_pubkey TEXT UNIQUE,
    status TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    CHECK (player_a <> player_b),
    UNIQUE (match_run_id, player_a),
    UNIQUE (match_run_id, player_b)
);

CREATE INDEX IF NOT EXISTS ranked_pairings_round_idx
    ON ranked_pairings (market_round_id, status, created_at);

INSERT INTO seasons (name, starts_at, ends_at, status)
SELECT
    'Season 1',
    EXTRACT(EPOCH FROM NOW())::BIGINT,
    EXTRACT(EPOCH FROM NOW())::BIGINT + 31_536_000,
    'ACTIVE'
WHERE NOT EXISTS (SELECT 1 FROM seasons WHERE status = 'ACTIVE');
