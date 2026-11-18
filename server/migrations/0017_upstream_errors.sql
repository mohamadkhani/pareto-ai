-- 0017_upstream_errors: operator-visible upstream error detail (D49) and
-- breaker trip history (D50).
--
-- `telemetry_attempts` gains the provider's own error fields for every failed
-- attempt; `request_logs` keeps the final failure's detail. Text is bounded and
-- redacted by `upstream::detail` before it reaches these columns, so this is
-- metadata, not request/response body capture (D11 stays in force for bodies).
--
-- `breaker_trips` records each closed -> open transition: the in-memory
-- snapshot resets on restart, these rows do not. Rows older than 30 days are
-- pruned when a new trip is written.

ALTER TABLE telemetry_attempts ADD COLUMN upstream_status INTEGER;

ALTER TABLE telemetry_attempts ADD COLUMN provider_error_code TEXT;

ALTER TABLE telemetry_attempts ADD COLUMN error_detail TEXT;

ALTER TABLE request_logs ADD COLUMN error_detail TEXT;

CREATE TABLE IF NOT EXISTS breaker_trips (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    provider_id          TEXT NOT NULL REFERENCES providers (id) ON DELETE CASCADE,
    credential_id        TEXT NOT NULL REFERENCES provider_credentials (id) ON DELETE CASCADE,
    offering_id          TEXT NOT NULL REFERENCES offerings (id) ON DELETE CASCADE,
    occurred_at          TEXT NOT NULL,
    consecutive_failures INTEGER NOT NULL,
    error_class          TEXT,
    error_detail         TEXT
);

CREATE INDEX IF NOT EXISTS idx_breaker_trips_occurred
    ON breaker_trips (occurred_at DESC);

CREATE INDEX IF NOT EXISTS idx_breaker_trips_pair
    ON breaker_trips (credential_id, offering_id, occurred_at DESC);
