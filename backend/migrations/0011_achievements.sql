-- Durable achievement catalog, player unlocks, and rebuildable progress state.
--
-- Competitive achievements are derived from indexed Battle/League/rating
-- facts. The unique unlock key makes a worker retry idempotent and the JSON
-- evidence keeps the exact source facts visible without an on-chain reward or
-- receipt obligation.

CREATE TABLE IF NOT EXISTS achievements (
    id INTEGER PRIMARY KEY,
    code TEXT UNIQUE NOT NULL,
    name TEXT NOT NULL,
    rarity TEXT NOT NULL,
    rule_version INTEGER NOT NULL,
    visible BOOLEAN NOT NULL DEFAULT TRUE,
    enabled BOOLEAN NOT NULL DEFAULT TRUE
);

CREATE TABLE IF NOT EXISTS player_achievements (
    wallet TEXT NOT NULL REFERENCES users(wallet),
    achievement_id INTEGER NOT NULL REFERENCES achievements(id),
    scope_type TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    unlocked_at BIGINT NOT NULL,
    evidence JSONB NOT NULL,
    PRIMARY KEY (wallet, achievement_id, scope_type, scope_id)
);

CREATE INDEX IF NOT EXISTS player_achievements_wallet_idx
    ON player_achievements (wallet, unlocked_at DESC, achievement_id);

CREATE TABLE IF NOT EXISTS achievement_player_state (
    wallet TEXT PRIMARY KEY REFERENCES users(wallet),
    season_id BIGINT REFERENCES seasons(id),
    win_streak INTEGER NOT NULL DEFAULT 0,
    fully_played_rated_battles INTEGER NOT NULL DEFAULT 0,
    fully_played_rated_wins INTEGER NOT NULL DEFAULT 0,
    updated_at BIGINT NOT NULL,
    CHECK (win_streak >= 0),
    CHECK (fully_played_rated_battles >= 0),
    CHECK (fully_played_rated_wins >= 0)
);

INSERT INTO achievements (id, code, name, rarity, rule_version)
VALUES
    (1, 'FIRST_BLOOD', 'First Blood', 'COMMON', 1),
    (2, 'HAT_TRICK', 'Hat Trick', 'UNCOMMON', 1),
    (3, 'UNSTOPPABLE', 'Unstoppable', 'RARE', 1),
    (4, 'GIANT_SLAYER', 'Giant Slayer', 'RARE', 1),
    (5, 'PERFECT_CAPTAIN', 'Perfect Captain', 'UNCOMMON', 1),
    (6, 'GREEN_SIX', 'Green Six', 'RARE', 1),
    (7, 'PHOTO_FINISH', 'Photo Finish', 'UNCOMMON', 1),
    (8, 'LANDSLIDE', 'Landslide', 'UNCOMMON', 1),
    (9, 'LEAGUE_PODIUM', 'Podium', 'RARE', 1),
    (10, 'LEAGUE_CHAMPION', 'League Champion', 'EPIC', 1),
    (11, 'PERFECT_LEAGUE', 'Perfect League', 'LEGENDARY', 1),
    (12, 'VETERAN_25', 'Battle Tested', 'COMMON', 1),
    (13, 'CENTURY_100', 'Centurion', 'EPIC', 1),
    (14, 'DIAMOND', 'Diamond', 'EPIC', 1),
    (15, 'MASTER', 'Master', 'LEGENDARY', 1)
ON CONFLICT (code) DO UPDATE SET
    name = EXCLUDED.name,
    rarity = EXCLUDED.rarity,
    rule_version = EXCLUDED.rule_version,
    visible = EXCLUDED.visible,
    enabled = EXCLUDED.enabled;
