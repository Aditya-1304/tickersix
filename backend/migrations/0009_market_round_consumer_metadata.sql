-- Freeze the consumer-facing domain and settlement-source metadata with the
-- indexed Market Round so Ranked UI labels cannot infer either value.

ALTER TABLE market_rounds
    ADD COLUMN IF NOT EXISTS competition_domain TEXT NOT NULL DEFAULT 'PUBLIC_EQUITY',
    ADD COLUMN IF NOT EXISTS settlement_source_kind TEXT NOT NULL DEFAULT 'JUPITER_TOKEN_SPOT_V1';

ALTER TABLE market_rounds
    ADD CONSTRAINT market_rounds_competition_domain_check
    CHECK (competition_domain IN ('PUBLIC_EQUITY', 'PRIVATE_MARKET')),
    ADD CONSTRAINT market_rounds_settlement_source_check
    CHECK (settlement_source_kind IN ('JUPITER_TOKEN_SPOT_V1', 'PYTH_PRO_VERIFIED_V1', 'PYTH_247_INDEX_V1'));
