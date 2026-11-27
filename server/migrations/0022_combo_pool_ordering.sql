-- 0022_combo_pool_ordering: pool sort axis, provider selection, sticky tolerance.
--
-- Decision D64. Three new knobs on a combo:
--
--   * `pool_sort` — the `pool = all` ordering axis: `price` (ascending blended
--     mix price) or `quality` (descending resolved quality). It orders the
--     *models* of the pool first; the weighted score is no longer the primary
--     order for a catalog pool.
--   * `provider_selection` — how to pick among the providers (offerings) of one
--     model once the model is chosen: `mix_price` (cheapest at the combo's mix
--     profile, the default), `speed`, or `reliability`.
--   * `sticky_price_tolerance_pct` — how much pricier a pinned offering may be
--     than the current best before the pin is abandoned and a new one is
--     written (D35 stickiness). Default 5%.
--
-- Existing combos keep the old feel: `price` matches the usual cheapest pool,
-- `mix_price` matches the old provider ordering, and the tolerance only ever
-- *keeps* the session on its pin.
--
-- Timestamps are UTC ISO-8601 with milliseconds and a trailing Z.

ALTER TABLE combo_models
    ADD COLUMN pool_sort TEXT NOT NULL DEFAULT 'price'
    CHECK (pool_sort IN ('price', 'quality'));
ALTER TABLE combo_models
    ADD COLUMN provider_selection TEXT NOT NULL DEFAULT 'mix_price'
    CHECK (provider_selection IN ('mix_price', 'speed', 'reliability'));
ALTER TABLE combo_models
    ADD COLUMN sticky_price_tolerance_pct REAL NOT NULL DEFAULT 5.0;
