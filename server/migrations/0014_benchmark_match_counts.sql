-- 0014_benchmark_match_counts: split benchmark match counts and record failed
-- pages (D38).
--
-- One published benchmark row can now attach to several catalog models: the
-- same model is published by more than one provider (`deepseek-ai/...` and
-- `deepseek/...`), and the score is model-level. `matched` keeps its meaning
-- (catalog models that received a row), `matched_hints` counts the source
-- entries that resolved to at least one model, so the two numbers together
-- show how far one hint fanned out.
--
-- `failed_pages` counts pages that failed after every HTTP retry before the
-- source aborted. A paged source never truncates silently: the run is
-- `status = 'error'` and this counter is 1.
--
-- Rows written before this migration read 0 for both columns; the old `matched`
-- value was a model count, which is exactly what it still means.

ALTER TABLE benchmark_sync_runs ADD COLUMN matched_hints INTEGER NOT NULL DEFAULT 0;
ALTER TABLE benchmark_sync_runs ADD COLUMN failed_pages INTEGER NOT NULL DEFAULT 0;
