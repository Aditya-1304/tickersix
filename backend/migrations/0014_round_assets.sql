-- Consumer projection of the frozen on-chain RoundAsset universe.
--
-- The indexer is the only writer. Clients read this table to render the exact
-- MarketRound assets and never substitute a locally invented ranked universe.

CREATE TABLE IF NOT EXISTS round_assets (
    id BIGSERIAL PRIMARY KEY,
    market_round_id BIGINT NOT NULL REFERENCES market_rounds(id) ON DELETE CASCADE,
    round_asset_pubkey TEXT NOT NULL UNIQUE,
    asset_id BIGINT NOT NULL,
    symbol TEXT NOT NULL,
    name TEXT NOT NULL,
    representation TEXT NOT NULL,
    provider TEXT NOT NULL,
    scoring_mint TEXT NOT NULL,
    status TEXT NOT NULL,
    indexed_at BIGINT NOT NULL,
    UNIQUE (market_round_id, asset_id)
);

CREATE INDEX IF NOT EXISTS round_assets_market_round_idx
    ON round_assets (market_round_id, asset_id);
