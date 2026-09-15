-- Phase 3.1 canonical backend schema.
--
-- Solana accounts remain competitive authority. These tables are durable
-- projections, authentication state, and auditable off-chain rating state.

CREATE TABLE IF NOT EXISTS users (
    wallet TEXT PRIMARY KEY,
    display_name TEXT,
    avatar_url TEXT,
    created_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS seasons (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    starts_at BIGINT NOT NULL,
    ends_at BIGINT NOT NULL,
    status TEXT NOT NULL,
    CHECK (ends_at > starts_at)
);

CREATE UNIQUE INDEX IF NOT EXISTS seasons_single_active_idx
    ON seasons (status)
    WHERE status = 'ACTIVE';

CREATE TABLE IF NOT EXISTS ratings (
    season_id BIGINT NOT NULL REFERENCES seasons(id),
    wallet TEXT NOT NULL REFERENCES users(wallet),
    rating INTEGER NOT NULL DEFAULT 1500,
    rated_games INTEGER NOT NULL DEFAULT 0,
    peak_rating INTEGER NOT NULL DEFAULT 1500,
    wins INTEGER NOT NULL DEFAULT 0,
    draws INTEGER NOT NULL DEFAULT 0,
    losses INTEGER NOT NULL DEFAULT 0,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (season_id, wallet),
    CHECK (rating >= 100),
    CHECK (rated_games >= 0),
    CHECK (peak_rating >= rating OR peak_rating >= 100)
);

CREATE INDEX IF NOT EXISTS ratings_leaderboard_idx
    ON ratings (season_id, rating DESC, wallet ASC);

CREATE TABLE IF NOT EXISTS auth_challenges (
    nonce TEXT PRIMARY KEY,
    wallet TEXT NOT NULL,
    domain TEXT NOT NULL,
    message TEXT NOT NULL,
    issued_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    consumed_at BIGINT,
    CHECK (expires_at > issued_at)
);

CREATE INDEX IF NOT EXISTS auth_challenges_expiry_idx
    ON auth_challenges (expires_at);

CREATE TABLE IF NOT EXISTS sessions (
    token_hash BYTEA PRIMARY KEY,
    wallet TEXT NOT NULL REFERENCES users(wallet),
    issued_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    revoked_at BIGINT,
    CHECK (expires_at > issued_at)
);

CREATE INDEX IF NOT EXISTS sessions_wallet_idx
    ON sessions (wallet);

CREATE TABLE IF NOT EXISTS market_rounds (
    id BIGSERIAL PRIMARY KEY,
    chain_pubkey TEXT UNIQUE,
    round_sequence BIGINT NOT NULL UNIQUE,
    state TEXT NOT NULL,
    is_replay BOOLEAN NOT NULL DEFAULT FALSE,
    queue_close_at BIGINT NOT NULL,
    start_target_at BIGINT NOT NULL,
    end_target_at BIGINT NOT NULL,
    indexed_at BIGINT NOT NULL,
    CHECK (start_target_at < end_target_at),
    CHECK (queue_close_at < start_target_at)
);

CREATE TABLE IF NOT EXISTS battles (
    chain_pubkey TEXT PRIMARY KEY,
    market_round_id BIGINT NOT NULL REFERENCES market_rounds(id),
    mode TEXT NOT NULL,
    rated BOOLEAN NOT NULL,
    player_a TEXT NOT NULL REFERENCES users(wallet),
    player_b TEXT NOT NULL REFERENCES users(wallet),
    state TEXT NOT NULL,
    result TEXT,
    indexed_at BIGINT NOT NULL,
    CHECK (player_a <> player_b)
);

CREATE TABLE IF NOT EXISTS rated_exposures (
    market_round_id BIGINT NOT NULL REFERENCES market_rounds(id),
    wallet TEXT NOT NULL REFERENCES users(wallet),
    battle_pubkey TEXT NOT NULL REFERENCES battles(chain_pubkey),
    PRIMARY KEY (market_round_id, wallet)
);

CREATE TABLE IF NOT EXISTS rating_events (
    id BIGSERIAL PRIMARY KEY,
    season_id BIGINT NOT NULL REFERENCES seasons(id),
    market_round_id BIGINT NOT NULL REFERENCES market_rounds(id),
    round_sequence BIGINT NOT NULL,
    battle_pubkey TEXT NOT NULL REFERENCES battles(chain_pubkey),
    wallet TEXT NOT NULL REFERENCES users(wallet),
    opponent TEXT REFERENCES users(wallet),
    event_kind TEXT NOT NULL,
    rating_before INTEGER NOT NULL,
    opponent_rating_snapshot INTEGER,
    expected_score DOUBLE PRECISION,
    actual_score DOUBLE PRECISION,
    k_factor INTEGER,
    delta INTEGER NOT NULL,
    rating_after INTEGER NOT NULL,
    formula_version INTEGER NOT NULL,
    created_at BIGINT NOT NULL,
    UNIQUE (battle_pubkey, wallet),
    CHECK (rating_after >= 100)
);

CREATE UNIQUE INDEX IF NOT EXISTS rating_events_played_exposure_idx
    ON rating_events (market_round_id, wallet)
    WHERE event_kind = 'PLAYED_RATED_BATTLE';

CREATE INDEX IF NOT EXISTS rating_events_order_idx
    ON rating_events (wallet, round_sequence, id);

CREATE TABLE IF NOT EXISTS indexed_accounts (
    account_pubkey TEXT PRIMARY KEY,
    owner_program TEXT NOT NULL,
    account_kind TEXT NOT NULL,
    slot BIGINT NOT NULL,
    data_hash BYTEA NOT NULL,
    indexed_at BIGINT NOT NULL
);
