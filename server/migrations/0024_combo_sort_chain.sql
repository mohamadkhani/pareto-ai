-- 0024_combo_sort_chain: a three-axis priority chain for catalog pools (D65).
--
-- D65 gave `pool = all` one ordering axis (`pool_sort`). When two models tie
-- on it — two offers at the same blended price — the order fell back to the
-- weighted score's internal tie-break (latency then id), which is neither
-- documented nor what an operator expects. This adds two tie-break axes:
--
--   sort     — primary axis   (default 'price')
--   sort_2   — first tie-break (default 'quality')
--   sort_3   — second tie-break (default 'reliability')
--
-- Each is one of 'price', 'quality', 'speed', or 'reliability'; the three must
-- be distinct (validation rejects a chain with a repeated axis). Lower sorts
-- first on every axis, so quality/speed/reliability are negated (their scores
-- are higher-is-better) while price is the blended mix price.
--
-- Existing combos get the documented defaults, which preserve the previous
-- first-axis behaviour and only add deterministic tie-breaking.

ALTER TABLE combo_models
    ADD COLUMN sort_2 TEXT NOT NULL DEFAULT 'quality'
    CHECK (sort_2 IN ('price', 'quality', 'speed', 'reliability'));
ALTER TABLE combo_models
    ADD COLUMN sort_3 TEXT NOT NULL DEFAULT 'reliability'
    CHECK (sort_3 IN ('price', 'quality', 'speed', 'reliability'));