-- Durable finalized score facts and the read path used by League standings.
--
-- Live score updates remain replaceable UI state. These nullable columns are
-- populated only from the indexed terminal Battle account, because forfeits
-- and voided Battles do not require played-score values.

ALTER TABLE battles ADD COLUMN IF NOT EXISTS score_a_q9 BIGINT;
ALTER TABLE battles ADD COLUMN IF NOT EXISTS score_b_q9 BIGINT;

CREATE INDEX IF NOT EXISTS battles_league_standings_idx
    ON battles (league_id, league_round_no, state, indexed_at)
    WHERE league_id IS NOT NULL;
