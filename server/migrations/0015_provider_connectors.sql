-- 0015_provider_connectors: per-provider declarative connectors (decision D42).
--
-- A connector is one operator-authored JSON document per (provider, kind):
--
--   * `catalog`  — fetch a URL (usually `GET {base}/models`) and map its
--                  entries onto model prices and discovery metadata (D41).
--   * `balance`  — fetch a provider-specific usage/balance URL and read the
--                  available flag, balance, currency, spend and burn rate.
--   * `health`   — the endpoint the credential "Test" action calls instead of
--                  `{base}/models`.
--
-- `config_json` is configuration, not a secret store: header values are
-- templates and may reference the credential secret as `{{secret}}`. The
-- secret itself stays in `provider_credentials.secret_cipher` (D16). No
-- response body is ever stored; `connector_runs` keeps counts, layer-named
-- errors, and non-secret detail only.
--
-- `prices.source` gains `provider_api` (D41). SQLite cannot alter a CHECK
-- constraint, so the table is rebuilt: create `prices_new` with the same
-- columns and the widened CHECK, copy every row, drop the old table, and
-- rename. No other table references `prices`, so the drop is safe and the
-- `UNIQUE (offering_id)` invariant survives as the new table's constraint.
--
-- Balance columns on `provider_credentials`:
--   * `balance_available` — 1 when the provider stated the balance is usable,
--     0 when it said unavailable (or the last fetch failed). A 0 never means
--     "balance is zero": the value is NULL in that case, never 0.
--   * `balance_gate` — `warn` (default, advisory: show the number and a
--     low-balance warning, never affect routing) or `block` (opt-in per
--     credential: routing skips the credential once the stored balance is at
--     or below zero).
--
-- `connector_runs` keeps the newest 30 rows per (provider, kind). Pruning is
-- the writer's job (`connectors::record_run`).

CREATE TABLE IF NOT EXISTS provider_connectors (
    provider_id TEXT NOT NULL REFERENCES providers (id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('catalog', 'balance', 'health')),
    config_json TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    updated_by  TEXT REFERENCES users (id) ON DELETE SET NULL,
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (provider_id, kind)
);

-- Rebuild `prices` to allow `source = 'provider_api'` (D41). The column list,
-- defaults, foreign keys, and UNIQUE constraint mirror 0003 + 0012 exactly.
CREATE TABLE prices_new (
    id                TEXT PRIMARY KEY,
    offering_id       TEXT NOT NULL REFERENCES offerings (id) ON DELETE CASCADE,
    input_per_m       REAL,
    output_per_m      REAL,
    cache_read_per_m  REAL,
    cache_write_per_m REAL,
    reasoning_per_m   REAL,
    request_fee       REAL,
    variable_price    INTEGER NOT NULL DEFAULT 0 CHECK (variable_price IN (0, 1)),
    currency          TEXT NOT NULL DEFAULT 'USD',
    source            TEXT NOT NULL
                      CHECK (source IN ('manual', 'openrouter', 'models_dev', 'custom_api', 'provider_api')),
    source_url        TEXT,
    fetched_at        TEXT,
    updated_by        TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (offering_id)
);

INSERT INTO prices_new (
    id, offering_id, input_per_m, output_per_m, cache_read_per_m,
    cache_write_per_m, reasoning_per_m, request_fee, variable_price,
    currency, source, source_url, fetched_at, updated_by, created_at, updated_at
)
SELECT
    id, offering_id, input_per_m, output_per_m, cache_read_per_m,
    cache_write_per_m, reasoning_per_m, request_fee, variable_price,
    currency, source, source_url, fetched_at, updated_by, created_at, updated_at
FROM prices;

DROP TABLE prices;
ALTER TABLE prices_new RENAME TO prices;

ALTER TABLE provider_credentials ADD COLUMN balance_available INTEGER
    CHECK (balance_available IN (0, 1));
ALTER TABLE provider_credentials ADD COLUMN balance_currency TEXT;
ALTER TABLE provider_credentials ADD COLUMN balance_error TEXT;
ALTER TABLE provider_credentials ADD COLUMN balance_fetched_at TEXT;
ALTER TABLE provider_credentials ADD COLUMN balance_gate TEXT NOT NULL DEFAULT 'warn'
    CHECK (balance_gate IN ('warn', 'block'));

CREATE TABLE IF NOT EXISTS connector_runs (
    id            TEXT PRIMARY KEY,
    provider_id   TEXT NOT NULL REFERENCES providers (id) ON DELETE CASCADE,
    credential_id TEXT REFERENCES provider_credentials (id) ON DELETE SET NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('catalog', 'balance', 'health')),
    status        TEXT NOT NULL CHECK (status IN ('success', 'error')),
    started_at    TEXT NOT NULL,
    finished_at   TEXT NOT NULL,
    detail_json   TEXT,
    error         TEXT
);

CREATE INDEX IF NOT EXISTS idx_connector_runs_provider_kind
    ON connector_runs (provider_id, kind, finished_at DESC);
CREATE INDEX IF NOT EXISTS idx_connector_runs_credential
    ON connector_runs (credential_id, kind, finished_at DESC);
