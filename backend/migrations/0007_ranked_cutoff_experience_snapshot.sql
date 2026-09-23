-- Preserve the canonical played-Battle experience used at Ranked cutoff.
-- Forfeit penalties change rating but never count toward placement or K-factor
-- experience, so the matchmaker must retain the played-game count it froze.

ALTER TABLE ranked_queue
    ADD COLUMN IF NOT EXISTS rated_games_snapshot INTEGER;

ALTER TABLE ranked_queue
    ADD CONSTRAINT ranked_queue_rated_games_snapshot_check
    CHECK (rated_games_snapshot IS NULL OR rated_games_snapshot >= 0);
