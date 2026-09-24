-- Durable finalized lineup evidence for achievement-ready Battle projections.
--
-- The Battle score is intentionally not enough to reconstruct captain identity
-- or the sign of each asset return. These facts are therefore retained beside
-- the Battle projection and keyed to the immutable chain Battle address.
--
-- JSONB is used for the fixed six-element vectors so the exact lineup order
-- remains visible to reconciliation and future proof/API consumers. The
-- indexer performs the stronger asset/captain/score validation before writing;
-- these checks protect the durable shape if another writer is introduced.

CREATE TABLE IF NOT EXISTS battle_competitive_facts (
    battle_pubkey TEXT PRIMARY KEY REFERENCES battles(chain_pubkey),
    facts_version INTEGER NOT NULL,
    finalized_slot BIGINT NOT NULL,
    side_a_lineup JSONB NOT NULL,
    side_a_captain INTEGER NOT NULL,
    side_a_returns_q9 JSONB NOT NULL,
    side_b_lineup JSONB NOT NULL,
    side_b_captain INTEGER NOT NULL,
    side_b_returns_q9 JSONB NOT NULL,
    indexed_at BIGINT NOT NULL,
    CHECK (facts_version > 0),
    CHECK (finalized_slot >= 0),
    CHECK (side_a_captain BETWEEN 0 AND 65535),
    CHECK (side_b_captain BETWEEN 0 AND 65535),
    CHECK (
        CASE
            WHEN jsonb_typeof(side_a_lineup) = 'array'
            THEN jsonb_array_length(side_a_lineup) = 6
            ELSE FALSE
        END
    ),
    CHECK (
        CASE
            WHEN jsonb_typeof(side_a_returns_q9) = 'array'
            THEN jsonb_array_length(side_a_returns_q9) = 6
            ELSE FALSE
        END
    ),
    CHECK (
        CASE
            WHEN jsonb_typeof(side_b_lineup) = 'array'
            THEN jsonb_array_length(side_b_lineup) = 6
            ELSE FALSE
        END
    ),
    CHECK (
        CASE
            WHEN jsonb_typeof(side_b_returns_q9) = 'array'
            THEN jsonb_array_length(side_b_returns_q9) = 6
            ELSE FALSE
        END
    )
);

CREATE INDEX IF NOT EXISTS battle_competitive_facts_indexed_idx
    ON battle_competitive_facts (indexed_at DESC, battle_pubkey);
