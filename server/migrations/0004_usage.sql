-- 0004_usage: per-request usage accounting for the chat data plane (M3).
--
-- `usage_events` is the token/usage ledger: one row per completion (success or
-- failure) of `POST /v1/chat/completions`. When the upstream reports `usage`
-- its numbers are authoritative; when it does not, tokens are estimated as
-- chars/4 and `tokens_estimated` is 1. `cost_usd` is NULL whenever the
-- offering's price is not fully known (unknown is never 0, decision D10).
--
-- `request_logs` is one row per chat request (the `request_id` primary key is
-- the `x-request-id` value). `decision_json` records the routing decision: the
-- candidates considered (id, provider, model, price if known), the chosen one,
-- and a one-line reason — the M5 explanation grows out of this column.
--
-- Bodies are never stored (decision D11); `has_request_body` exists so a later
-- opt-in body capture can be correlated without a schema change.
--
-- Columns follow docs/specs/m3-chat-dataplane.md exactly. Timestamps are UTC
-- ISO-8601 with milliseconds and a trailing Z, written by the application.

CREATE TABLE IF NOT EXISTS usage_events (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id         TEXT NOT NULL,
    user_id            TEXT REFERENCES users (id) ON DELETE SET NULL,
    api_key_id         TEXT,
    offering_id        TEXT REFERENCES offerings (id) ON DELETE SET NULL,
    model_id           TEXT REFERENCES models (id) ON DELETE SET NULL,
    input_tokens       INTEGER,
    output_tokens      INTEGER,
    cache_read_tokens  INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens   INTEGER,
    tokens_estimated   INTEGER NOT NULL DEFAULT 0 CHECK (tokens_estimated IN (0, 1)),
    cost_usd           REAL,
    latency_ms         INTEGER,
    ttft_ms            INTEGER,
    status             INTEGER,
    error_class        TEXT,
    occurred_at        TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS request_logs (
    request_id             TEXT PRIMARY KEY,
    occurred_at            TEXT NOT NULL,
    method                 TEXT,
    path                   TEXT,
    requested_model        TEXT,
    resolved_offering_id   TEXT,
    decision_json          TEXT,
    status                 INTEGER,
    error_class            TEXT,
    bytes_in               INTEGER,
    bytes_out              INTEGER,
    user_id                TEXT,
    api_key_id             TEXT,
    has_request_body       INTEGER NOT NULL DEFAULT 0 CHECK (has_request_body IN (0, 1))
);

CREATE INDEX IF NOT EXISTS idx_usage_events_user_id
    ON usage_events (user_id, occurred_at);
CREATE INDEX IF NOT EXISTS idx_usage_events_offering_id
    ON usage_events (offering_id, occurred_at);
CREATE INDEX IF NOT EXISTS idx_request_logs_occurred_at
    ON request_logs (occurred_at);
