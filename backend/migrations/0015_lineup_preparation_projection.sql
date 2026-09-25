-- Persist the frozen round facts required to prepare a wallet-signed lineup.
-- A zero deadline or registry version is intentionally invalid for preparation;
-- it makes incomplete indexer data fail closed instead of producing a hash that
-- the on-chain program would interpret under a different frozen context.
ALTER TABLE market_rounds
    ADD COLUMN IF NOT EXISTS registry_version BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS commit_deadline BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS reveal_deadline BIGINT NOT NULL DEFAULT 0;

-- Battle commit flags are projections of the on-chain Battle sides. Newly
-- materialized Battles start uncommitted; subsequent indexer observations may
-- promote either side to true. The chain remains the final authority when the
-- wallet submits the transaction.
ALTER TABLE battles
    ADD COLUMN IF NOT EXISTS player_a_committed BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS player_b_committed BOOLEAN NOT NULL DEFAULT FALSE;
