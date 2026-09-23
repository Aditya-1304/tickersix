-- Preserve the frozen settlement source in the Battle projection and prevent
-- invalid projected timestamps from entering the replaceable live-state row.

ALTER TABLE battles
    ADD COLUMN IF NOT EXISTS settlement_source_kind TEXT;

ALTER TABLE battle_live_state
    ADD CONSTRAINT battle_live_state_as_of_check
    CHECK (as_of >= 0);
