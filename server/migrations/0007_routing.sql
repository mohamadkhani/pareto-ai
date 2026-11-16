-- 0007_routing: smart routing (M5) — combos, benchmarks, quality overrides,
-- and telemetry snapshots.
--
-- `combo_models` is the operator-defined routing policy: `members_json` is an
-- ordered list of member offerings, each with an optional per-member weight:
--   [{"offering_id": "off_1", "weight": 1.5}, {"offering_id": "off_2"}]
-- `weights_json` is either NULL (the `balanced` defaults: price 0.25, speed
-- 0.30, quality 0.30, reliability 0.15), a named preset
-- ({"preset": "cheapest" | "fastest" | "balanced" | "best"}), or a partial
-- weight object whose missing keys fall back to the defaults. A combo is
-- addressable as the model id `combo:<name>` in `POST /v1/chat/completions`.
--
-- `benchmarks` holds imported external quality scores with provenance
-- (decision D5: we measure speed ourselves, quality is imported). One row per
-- (model, source, category), replaced by a later import of the same source.
-- `model_quality_overrides` is the operator's manual score for a model: it is
-- never written by an importer and always wins over a synced benchmark at
-- resolution time (same pattern as manual prices, decision D10).
--
-- `telemetry_snapshots` persists the per-offering rolling sample windows the
-- in-memory telemetry cache derives from `usage_events` (spec M5 scope 1:
-- "prefer deriving, keep a hot in-memory cache"). The windows are derived
-- again on every refresh; the snapshot only warms a fresh process and bounds
-- the re-read. Nothing is written here on the request path.
--
-- Deviations from the schema in the M5 deliverables list:
-- * `combo_models.rr_cursor` — the round-robin rotation must be "stable and
--   persisted across requests" (spec M5 scope 2); a column on the combo row
--   avoids a second table and a join on every request. It is bumped once per
--   routed request and modulo-ed over the eligible members.
-- * members reference `offerings` (id) only, not models: an offering is the
--   routable unit (provider + model + credential), and a model-level member
--   would expand to an ambiguous set with ambiguous per-member weights.
--
-- Timestamps are UTC ISO-8601 with milliseconds and a trailing Z.

CREATE TABLE IF NOT EXISTS combo_models (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    strategy     TEXT NOT NULL
                 CHECK (strategy IN ('ordered', 'weighted', 'round_robin')),
    -- NULL = balanced defaults; {"preset": "..."} or a partial weight object.
    weights_json TEXT,
    -- [{"offering_id": "...", "weight": 1.0}, ...], ordered.
    members_json TEXT NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    -- Deviation (see header): the persisted round-robin rotation counter.
    rr_cursor    INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS benchmarks (
    id         TEXT PRIMARY KEY,
    model_id   TEXT NOT NULL REFERENCES models (id) ON DELETE CASCADE,
    source     TEXT NOT NULL,
    category   TEXT NOT NULL CHECK (category IN ('text', 'code')),
    score      REAL NOT NULL,
    -- CI half-width of `score`, in the source's own units (e.g. Elo points).
    ci         REAL,
    rank       INTEGER,
    source_url TEXT,
    fetched_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (model_id, source, category)
);

CREATE TABLE IF NOT EXISTS model_quality_overrides (
    model_id   TEXT PRIMARY KEY REFERENCES models (id) ON DELETE CASCADE,
    score      REAL NOT NULL,
    updated_by TEXT REFERENCES users (id) ON DELETE SET NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS telemetry_snapshots (
    offering_id TEXT PRIMARY KEY REFERENCES offerings (id) ON DELETE CASCADE,
    -- {"samples": [{"latency_ms", "ttft_ms", "output_tokens", "failed"}]},
    -- at most the last 200 samples per offering.
    window_json TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_benchmarks_model_id ON benchmarks (model_id);
