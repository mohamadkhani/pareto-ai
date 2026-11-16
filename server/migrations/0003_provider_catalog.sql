-- 0003_provider_catalog: providers, credentials, catalog, and prices
-- (milestone M2).
--
-- `provider_credentials.secret_cipher` is AES-256-GCM ciphertext in the
-- `v1:<b64url nonce>:<b64url ct>` format (decision D16), with the credential
-- row id as associated data. Custom header values on credentials and
-- offerings are sealed the same way (decision D17): `headers_json` holds the
-- envelope `{"names": ["x-api-key"], "values": "v1:<nonce>:<ct>"}` where the
-- values ciphertext seals the name -> value JSON under the row id as
-- associated data. Header *names* stay plain so lists and audit rows can show
-- them; values are never returned or logged (responses carry header names
-- only).
--
-- `prices` holds at most one row per offering, enforced both here
-- (`UNIQUE (offering_id)`) and by the upserts in `catalog`; `source =
-- 'manual'` rows are never overwritten by a price sync (decision D10). An
-- unknown price stays NULL and is never written as 0.
--
-- This migration was edited in place during the M2 fix round (M2 is
-- unreleased and no production database exists): header values became sealed
-- (D17) and `prices` gained `UNIQUE (offering_id)`. A development database
-- created from an earlier draft of this file must be deleted and recreated;
-- `0001`/`0002` are untouched.
--
-- Deviations from the schema in docs/specs/m2-providers-catalog.md:
-- * `provider_credentials.last_tested_at` — the API contract records the
--   result of `POST /api/credentials/{id}/test` (`status`, `last_tested`) but
--   the spec schema has no column for the test time.
-- * `price_sync_runs` and its index — named in the spec's "OpenRouter sync"
--   section (runs recorded per sync), so it ships in this migration too.
-- * `models.capabilities_json` also carries the sync's `display_name` — the
--   spec schema has no display-name column, and synced models keep their
--   display name there (read by `GET /v1/models` as `name`).
-- * the extra read endpoint `GET /api/offerings/{id}/price` — the spec
--   defines `PUT` on that path only; the `GET` carries `source`/`fetched_at`
--   provenance (see `routes/catalog.rs`).
-- Timestamps are UTC ISO-8601 with milliseconds and a trailing Z.

CREATE TABLE IF NOT EXISTS providers (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    kind       TEXT NOT NULL DEFAULT 'openai_compatible'
               CHECK (kind IN ('openai_compatible')),
    base_url   TEXT NOT NULL,
    enabled    INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS provider_credentials (
    id                    TEXT PRIMARY KEY,
    provider_id           TEXT NOT NULL REFERENCES providers (id) ON DELETE CASCADE,
    label                 TEXT NOT NULL,
    secret_cipher         TEXT NOT NULL,
    -- Sealed header envelope (D17, see header): {"names", "values"}.
    headers_json          TEXT,
    max_concurrency       INTEGER,
    balance_usd           REAL,
    balance_source        TEXT NOT NULL DEFAULT 'manual'
                          CHECK (balance_source IN ('manual', 'provider_api', 'unknown')),
    balance_updated_at    TEXT,
    balance_updated_by    TEXT REFERENCES users (id) ON DELETE SET NULL,
    balance_low_threshold REAL,
    status                TEXT NOT NULL DEFAULT 'unknown',
    cooldown_until        TEXT,
    -- Deviation (see header): when `POST /api/credentials/{id}/test` last ran.
    last_tested_at        TEXT,
    created_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS models (
    id                TEXT PRIMARY KEY,
    canonical_name    TEXT NOT NULL UNIQUE,
    family            TEXT,
    modalities_input  TEXT,
    modalities_output TEXT,
    context_length    INTEGER,
    max_output_tokens INTEGER,
    capabilities_json TEXT,
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS offerings (
    id                  TEXT PRIMARY KEY,
    provider_id         TEXT NOT NULL REFERENCES providers (id) ON DELETE CASCADE,
    model_id            TEXT NOT NULL REFERENCES models (id) ON DELETE CASCADE,
    upstream_model_id   TEXT NOT NULL,
    context_override    INTEGER,
    max_output_override INTEGER,
    enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    -- Sealed header envelope (D17, see header): {"names", "values"}.
    headers_json        TEXT,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (provider_id, upstream_model_id)
);

CREATE TABLE IF NOT EXISTS prices (
    id                TEXT PRIMARY KEY,
    -- At most one price row per offering (D10), enforced here and by the
    -- upserts in `catalog`.
    offering_id       TEXT NOT NULL REFERENCES offerings (id) ON DELETE CASCADE,
    input_per_m       REAL,
    output_per_m      REAL,
    cache_read_per_m  REAL,
    cache_write_per_m REAL,
    reasoning_per_m   REAL,
    request_fee       REAL,
    currency          TEXT NOT NULL DEFAULT 'USD',
    source            TEXT NOT NULL
                      CHECK (source IN ('manual', 'openrouter', 'models_dev', 'custom_api')),
    source_url        TEXT,
    fetched_at        TEXT,
    updated_by        TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (offering_id)
);

-- One row per sync attempt (success or failure), newest last per source.
CREATE TABLE IF NOT EXISTS price_sync_runs (
    id           TEXT PRIMARY KEY,
    source       TEXT NOT NULL,
    started_at   TEXT NOT NULL,
    finished_at  TEXT NOT NULL,
    status       TEXT NOT NULL
                 CHECK (status IN ('success', 'error', 'stale-if-error')),
    model_count  INTEGER NOT NULL DEFAULT 0,
    detail_json  TEXT
);

CREATE INDEX IF NOT EXISTS idx_offerings_model_id ON offerings (model_id);
CREATE INDEX IF NOT EXISTS idx_offerings_provider_id ON offerings (provider_id);
CREATE INDEX IF NOT EXISTS idx_provider_credentials_provider_id
    ON provider_credentials (provider_id);
-- No idx_prices_offering_id: `UNIQUE (offering_id)` already indexes it.
CREATE INDEX IF NOT EXISTS idx_price_sync_runs_source
    ON price_sync_runs (source, finished_at DESC);
