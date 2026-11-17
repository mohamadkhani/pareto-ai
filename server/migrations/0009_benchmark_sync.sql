-- 0009_benchmark_sync: automatic benchmark download (D31).
--
-- `benchmark_sync_runs` records one row per source per sync attempt: the
-- status, how many published entries were matched, skipped as unknown, or
-- skipped as ambiguous, what the upsert changed, the duration, a secret-free
-- error message, and the source URL the entries came from (the admin UI shows
-- it as the CC BY 4.0 attribution for the Arena data).
--
-- `benchmarks.scale` names the source's native scale. Routing quality is one
-- scale (D25: the Elo-like scale), so only `scale = 'elo'` rows feed
-- [`crate::benchmarks::quality_for_models`]; capability-index and percentage
-- scores are stored with their own scale for display and future calibration,
-- but never mixed into the Elo ranking. Rows written before this migration
-- (the manual LMArena import) default to `elo`, which is what they are.
--
-- Timestamps are UTC ISO-8601 with milliseconds and a trailing Z.

ALTER TABLE benchmarks ADD COLUMN scale TEXT NOT NULL DEFAULT 'elo';

CREATE INDEX IF NOT EXISTS idx_benchmarks_scale ON benchmarks (scale, category);

CREATE TABLE IF NOT EXISTS benchmark_sync_runs (
    id              TEXT PRIMARY KEY,
    source          TEXT NOT NULL,
    status          TEXT NOT NULL
                    CHECK (status IN ('success', 'error', 'skipped')),
    started_at      TEXT NOT NULL,
    finished_at     TEXT NOT NULL,
    duration_ms     INTEGER NOT NULL DEFAULT 0,
    -- Entries the source published and the import could use.
    published       INTEGER NOT NULL DEFAULT 0,
    -- Entries that resolved to exactly one catalog model.
    matched         INTEGER NOT NULL DEFAULT 0,
    -- Entries whose model hint matched no catalog model.
    skipped_unknown INTEGER NOT NULL DEFAULT 0,
    -- Entries whose model hint matched more than one model (never guessed).
    ambiguous       INTEGER NOT NULL DEFAULT 0,
    inserted        INTEGER NOT NULL DEFAULT 0,
    updated         INTEGER NOT NULL DEFAULT 0,
    unchanged       INTEGER NOT NULL DEFAULT 0,
    source_url      TEXT,
    error           TEXT
);

CREATE INDEX IF NOT EXISTS idx_benchmark_sync_runs_source
    ON benchmark_sync_runs (source, finished_at DESC);
