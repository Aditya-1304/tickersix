-- Official League catalog, membership intents, and reserved Market Round windows.
--
-- The on-chain League and LeagueMember accounts remain authoritative. These
-- tables provide searchable catalog data, transactional reservation state, and
-- an audit trail for wallet-signed membership instructions before indexing.

CREATE TABLE IF NOT EXISTS leagues (
    id BIGSERIAL PRIMARY KEY,
    chain_pubkey TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    max_players INTEGER NOT NULL,
    total_rounds INTEGER NOT NULL,
    pairing_policy_version INTEGER NOT NULL,
    rated BOOLEAN NOT NULL DEFAULT TRUE,
    registration_close_at BIGINT NOT NULL,
    current_round INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'REGISTRATION',
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    CHECK (char_length(btrim(name)) BETWEEN 1 AND 80),
    CHECK (max_players BETWEEN 2 AND 100),
    CHECK (total_rounds > 0),
    CHECK (pairing_policy_version > 0),
    CHECK (status IN ('REGISTRATION', 'ACTIVE', 'COMPLETED', 'CANCELLED'))
);

CREATE TABLE IF NOT EXISTS league_memberships (
    league_id BIGINT NOT NULL REFERENCES leagues(id),
    wallet TEXT NOT NULL REFERENCES users(wallet),
    joined_at BIGINT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT FALSE,
    bye_count INTEGER NOT NULL DEFAULT 0,
    membership_state TEXT NOT NULL,
    onchain_member_pubkey TEXT,
    pending_until BIGINT,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (league_id, wallet),
    CHECK (bye_count >= 0),
    CHECK (membership_state IN (
        'PENDING_JOIN', 'ACTIVE', 'PENDING_LEAVE', 'LEFT', 'EXPIRED'
    )),
    CHECK (
        (membership_state = 'ACTIVE' AND active = TRUE)
        OR (membership_state <> 'ACTIVE' AND active = FALSE)
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS league_memberships_member_pubkey_idx
    ON league_memberships (onchain_member_pubkey)
    WHERE onchain_member_pubkey IS NOT NULL;

CREATE INDEX IF NOT EXISTS league_memberships_status_idx
    ON league_memberships (league_id, membership_state, active, wallet);

CREATE INDEX IF NOT EXISTS league_memberships_pending_expiry_idx
    ON league_memberships (pending_until)
    WHERE membership_state IN ('PENDING_JOIN', 'PENDING_LEAVE');

CREATE TABLE IF NOT EXISTS league_round_schedule (
    league_id BIGINT NOT NULL REFERENCES leagues(id),
    league_round_no INTEGER NOT NULL,
    market_round_id BIGINT NOT NULL REFERENCES market_rounds(id),
    assigned_at BIGINT NOT NULL,
    PRIMARY KEY (league_id, league_round_no),
    UNIQUE (league_id, market_round_id),
    CHECK (league_round_no > 0)
);

CREATE INDEX IF NOT EXISTS league_round_schedule_market_round_idx
    ON league_round_schedule (market_round_id, league_id, league_round_no);

ALTER TABLE battles
    ADD COLUMN IF NOT EXISTS league_id BIGINT REFERENCES leagues(id);

ALTER TABLE battles
    ADD COLUMN IF NOT EXISTS league_round_no INTEGER;

CREATE INDEX IF NOT EXISTS battles_league_round_idx
    ON battles (league_id, league_round_no, indexed_at)
    WHERE league_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS league_reservations_wallet_status_idx
    ON league_reservations (wallet, status, market_round_id);

ALTER TABLE league_reservations
    ADD CONSTRAINT league_reservations_league_fk
    FOREIGN KEY (league_id) REFERENCES leagues(id);

ALTER TABLE league_reservations
    ADD CONSTRAINT league_reservations_status_check
    CHECK (status IN ('PENDING', 'ACTIVE', 'PENDING_LEAVE', 'CANCELLED', 'EXPIRED', 'RESOLVED'));
