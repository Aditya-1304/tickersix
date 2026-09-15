-- Deterministic League pairing runs, byes, and coordinator Battle handoff.
--
-- A pairing run is immutable once created. Battle and RatedSlot projection
-- rows are added later, in one confirmation transaction, after the on-chain
-- coordinator instruction succeeds.

ALTER TABLE leagues
    ADD COLUMN IF NOT EXISTS onchain_joined_players INTEGER NOT NULL DEFAULT 0;

ALTER TABLE leagues
    ADD CONSTRAINT leagues_onchain_joined_players_check
    CHECK (onchain_joined_players BETWEEN 0 AND 100);

CREATE TABLE IF NOT EXISTS league_pairing_runs (
    id BIGSERIAL PRIMARY KEY,
    league_id BIGINT NOT NULL REFERENCES leagues(id),
    league_round_no INTEGER NOT NULL,
    market_round_id BIGINT NOT NULL REFERENCES market_rounds(id),
    pairing_policy_version INTEGER NOT NULL,
    pairing_seed BYTEA NOT NULL,
    standings_input_hash BYTEA NOT NULL,
    seed_source_slot BIGINT NOT NULL,
    seed_source_blockhash TEXT NOT NULL,
    bye_wallet TEXT REFERENCES users(wallet),
    status TEXT NOT NULL DEFAULT 'PLANNED',
    created_at BIGINT NOT NULL,
    UNIQUE (league_id, league_round_no),
    CHECK (league_round_no > 0),
    CHECK (pairing_policy_version > 0),
    CHECK (octet_length(pairing_seed) = 32),
    CHECK (octet_length(standings_input_hash) = 32),
    CHECK (seed_source_slot >= 0),
    CHECK (status IN ('PLANNED', 'IN_PROGRESS', 'COMPLETE', 'CANCELLED'))
);

CREATE INDEX IF NOT EXISTS league_pairing_runs_market_round_idx
    ON league_pairing_runs (market_round_id, status, created_at);

CREATE TABLE IF NOT EXISTS league_pairings (
    id BIGSERIAL PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES league_pairing_runs(id),
    league_id BIGINT NOT NULL REFERENCES leagues(id),
    league_round_no INTEGER NOT NULL,
    market_round_id BIGINT NOT NULL REFERENCES market_rounds(id),
    player_a TEXT NOT NULL REFERENCES users(wallet),
    player_b TEXT NOT NULL REFERENCES users(wallet),
    rating_a_snapshot INTEGER NOT NULL,
    rating_b_snapshot INTEGER NOT NULL,
    pairing_seed BYTEA NOT NULL,
    seed_source_slot BIGINT NOT NULL,
    seed_source_blockhash TEXT NOT NULL,
    repeat_relaxed BOOLEAN NOT NULL DEFAULT FALSE,
    battle_pubkey TEXT UNIQUE REFERENCES battles(chain_pubkey),
    status TEXT NOT NULL DEFAULT 'PENDING_COORDINATOR',
    created_at BIGINT NOT NULL,
    CHECK (league_round_no > 0),
    CHECK (player_a <> player_b),
    CHECK (rating_a_snapshot >= 100),
    CHECK (rating_b_snapshot >= 100),
    CHECK (octet_length(pairing_seed) = 32),
    CHECK (seed_source_slot >= 0),
    CHECK (status IN ('PENDING_COORDINATOR', 'CREATED', 'CANCELLED')),
    UNIQUE (run_id, player_a),
    UNIQUE (run_id, player_b)
);

CREATE INDEX IF NOT EXISTS league_pairings_round_idx
    ON league_pairings (league_id, league_round_no, status, id);

CREATE TABLE IF NOT EXISTS league_pairing_participants (
    run_id BIGINT NOT NULL REFERENCES league_pairing_runs(id),
    league_id BIGINT NOT NULL REFERENCES leagues(id),
    league_round_no INTEGER NOT NULL,
    wallet TEXT NOT NULL REFERENCES users(wallet),
    battle_pubkey TEXT REFERENCES battles(chain_pubkey),
    side TEXT NOT NULL,
    PRIMARY KEY (league_id, league_round_no, wallet),
    CHECK (side IN ('A', 'B')),
    UNIQUE (battle_pubkey, wallet)
);

CREATE INDEX IF NOT EXISTS league_pairing_participants_run_idx
    ON league_pairing_participants (run_id, league_id, league_round_no, side);
