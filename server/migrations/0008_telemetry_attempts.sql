-- 0008_telemetry_attempts: per-attempt telemetry samples (M5, decision D26).
--
-- One row per classified upstream attempt, written with the settle
-- transaction. The M5 review found that deriving telemetry from
-- `usage_events` (one row per *request*) is blind to failures rescued by
-- failover and attributes credential-side 402/429 limits to the offering.
-- This table fixes both: every attempt teaches the router, and quota/rate
-- rows carry the credential they belong to while never counting against the
-- offering's reliability window (see `telemetry.rs` sample semantics).
--
-- Columns: the offering and credential the attempt used, the outcome class
-- (`NULL` = the attempt completed; otherwise the failure taxonomy code such
-- as `upstream_auth`), latency/ttft/output tokens of the attempt, and its
-- timestamp.
--
-- Deviations from the house style:
-- * `id` is an INTEGER rowid, not a TEXT uuid: the rolling window breaks
--   ties on insertion order (`ORDER BY occurred_at DESC, id DESC`) and these
--   rows are never used as external ids. Attempts of one request insert in
--   attempt order, so the tie-break preserves it.
-- * retention is enforced by the background telemetry refresh, which prunes
--   rows beyond the newest 200 per (offering, outcome-class) group — the
--   window only ever reads the newest 200, so the table stays small.

CREATE TABLE IF NOT EXISTS telemetry_attempts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id    TEXT NOT NULL,
    offering_id   TEXT NOT NULL REFERENCES offerings (id) ON DELETE CASCADE,
    credential_id TEXT REFERENCES provider_credentials (id) ON DELETE SET NULL,
    -- NULL = the attempt completed; else the failure taxonomy code.
    outcome       TEXT,
    latency_ms    INTEGER,
    ttft_ms       INTEGER,
    output_tokens INTEGER,
    occurred_at   TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_telemetry_attempts_offering
    ON telemetry_attempts (offering_id, occurred_at);
