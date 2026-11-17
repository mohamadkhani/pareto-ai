-- 0011_rule_combos: rule-based combo pools and session stickiness (D35).
--
-- A combo may use `pool = 'all'` instead of an explicit member list: the
-- routable pool is every eligible offering that satisfies `min_quality`
-- and/or `max_mix_price_per_m`, ordered by the same weighted score as a
-- `weighted` combo. At least one constraint is required; an unconstrained
-- "everything" combo is almost always a mistake. `pool = 'all'` only allows
-- the `weighted` strategy because `ordered` and `round_robin` need an explicit
-- member list by definition.
--
-- `mix_profile` moves from `weights_json.mix_profile` to its own column (the
-- old key is still accepted on input for compatibility). It names the
-- token-mix profile whose blend supplies the price component and the
-- `max_mix_price_per_m` cap.
--
-- Stickiness pins the chosen offering per session so prompt caching survives
-- across turns (docs/research/12-token-mix.md: per-turn re-picking turns
-- `code-agent` into `code-agent-nocache`). `stickiness_key_source` is `auto`
-- (X-Session-Id when present, else a stable hash of the caller and the
-- conversation opening) or `header` (X-Session-Id only). Pins expire after
-- `stickiness_ttl_seconds` and are pruned opportunistically.
--
-- Timestamps are UTC ISO-8601 with milliseconds and a trailing Z.

ALTER TABLE combo_models
    ADD COLUMN pool TEXT NOT NULL DEFAULT 'members'
    CHECK (pool IN ('members', 'all'));
ALTER TABLE combo_models ADD COLUMN min_quality REAL;
ALTER TABLE combo_models ADD COLUMN max_mix_price_per_m REAL;
ALTER TABLE combo_models ADD COLUMN mix_profile TEXT;
ALTER TABLE combo_models
    ADD COLUMN stickiness_enabled INTEGER NOT NULL DEFAULT 0
    CHECK (stickiness_enabled IN (0, 1));
ALTER TABLE combo_models
    ADD COLUMN stickiness_ttl_seconds INTEGER NOT NULL DEFAULT 1800;
ALTER TABLE combo_models
    ADD COLUMN stickiness_key_source TEXT NOT NULL DEFAULT 'auto'
    CHECK (stickiness_key_source IN ('auto', 'header'));

-- Existing combos stored the profile selector inside weights_json; preserve
-- it in the new authoritative column.
UPDATE combo_models
SET mix_profile = json_extract(weights_json, '$.mix_profile')
WHERE mix_profile IS NULL
  AND weights_json IS NOT NULL
  AND json_valid(weights_json)
  AND json_extract(weights_json, '$.mix_profile') IS NOT NULL;

CREATE TABLE IF NOT EXISTS combo_sticky_choices (
    combo_id    TEXT NOT NULL REFERENCES combo_models (id) ON DELETE CASCADE,
    session_key TEXT NOT NULL,
    offering_id TEXT NOT NULL REFERENCES offerings (id) ON DELETE CASCADE,
    chosen_at   TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    PRIMARY KEY (combo_id, session_key)
);

-- Pruning by expiry and capping per combo are the two background jobs.
CREATE INDEX IF NOT EXISTS idx_combo_sticky_expires
    ON combo_sticky_choices (expires_at);
CREATE INDEX IF NOT EXISTS idx_combo_sticky_combo_chosen
    ON combo_sticky_choices (combo_id, chosen_at DESC);
