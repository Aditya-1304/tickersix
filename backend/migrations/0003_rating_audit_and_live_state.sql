-- Rating audit metadata and ephemeral projected Battle state.
--
-- The rating columns are copied from the coordinator's on-chain Battle
-- metadata when available. They are never used as canonical rating input.
ALTER TABLE battles ADD COLUMN IF NOT EXISTS rating_a_before INTEGER;
ALTER TABLE battles ADD COLUMN IF NOT EXISTS rating_b_before INTEGER;
ALTER TABLE battles ADD COLUMN IF NOT EXISTS rating_formula_version INTEGER;

-- Projected scores are replaceable UI state. Final scores and results still
-- come from indexed/finalized on-chain Battle state and are not written here.
CREATE TABLE IF NOT EXISTS battle_live_state (
    battle_pubkey TEXT PRIMARY KEY REFERENCES battles(chain_pubkey),
    player_a_score_q9 BIGINT,
    player_b_score_q9 BIGINT,
    as_of BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS battle_live_state_updated_idx
    ON battle_live_state (updated_at, battle_pubkey);
