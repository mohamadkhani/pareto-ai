-- 0005_access_money: API keys, access grants, budgets, and the reservation
-- ledger (milestone M4).
--
-- `api_keys.key_hash` is SHA-256 of the full key string (`pk_live_...`); the
-- plaintext key is returned exactly once at creation and is never stored,
-- logged, or returned again. `prefix` (the first characters of the key) is
-- the only part lists and screens ever show. `scopes_json` holds an optional
-- JSON array of scope names; v1 stores it verbatim and does not enforce it.
--
-- `grants` is the access matrix (decision D9): one row binds a subject
-- (`user` or `api_key`) to a resource (`model`, `offering`, or `credential`)
-- with `allow` or `deny`. `deny` beats `allow`; a subject with no matching
-- row falls back to its role default (allow for `admin`, deny for `user`).
--
-- `budgets` and `token_caps` are the hard spend caps, **bucket-scoped**
-- (review M1): one row per (user, period, period_start) and (user, model,
-- period, period_start), where `period_start` is the UTC start of the
-- `GATEWAY_TIMEZONE` period bucket the spend belongs to (`total` rows use the
-- `1970-01-01T00:00:00.000Z` sentinel — `total` never resets). A period reset
-- therefore *creates a new row* (carrying the newest `limit_usd` /
-- `limit_tokens` forward) instead of zeroing the old one, so a request that
-- started before the reset still books its spend into its own bucket and a
-- reset can never hide earlier spend. `reset_at` is the end of the row's own
-- bucket (`NULL` for `total`). A request is booked into the bucket derived
-- from the period it started in (decision D7).
--
-- `reservations` is the reserve -> settle/release ledger (decision D7): one
-- `open` row per in-flight chat request with its estimate; settlement marks
-- it `settled` and books the actuals (exactly once — the `open`/`released` ->
-- `settled` transition is the settle marker), failures mark it `released`
-- (`released_at`, never `settled_at`), and a reconciler releases rows that
-- stayed open past the request's total timeout plus a cushion (so it can
-- never race a live request). The columns are plain TEXT on
-- purpose (like `usage_events`): these are event rows and must survive
-- user/offering deletion, so no foreign keys here.
--
-- `usage_daily` is the report rollup keyed by local day (in
-- `GATEWAY_TIMEZONE`), user, and offering. It is updated in the settlement
-- transaction and can be rebuilt idempotently from `usage_events`.
--
-- Deviations from the schema in docs/specs/m4-access-money.md:
-- * `budgets`/`token_caps` `period_start` and the bucket-scoped unique keys
--   (`UNIQUE (user_id, period, period_start)` / `UNIQUE (user_id, model_id,
--   period, period_start)`): the spec's `UNIQUE (user_id, period)` rows are
--   reset in place, which lets a period reset hide spend from a request that
--   started before it (review M1). Bucket rows make the reset create a new
--   row while the old bucket still receives straddling settlements.
-- * `api_keys.rate_limit_rpm` / `rate_limit_tpm` — the behaviour section
--   requires configurable per-key RPM/TPM limits "with sane defaults", but
--   the spec schema has no columns for them. NULL means the defaults
--   (60 RPM / 100k TPM) applied at check time.
-- * `usage_events.priced` — the cost rule flags rows whose computed cost is
--   incomplete ("the row is flagged priced=0 in the report") but the spec
--   schema has no flag column. 1 = every used price component was known,
--   0 = the cost is missing (NULL) or understates a used component whose
--   price is unknown (that component contributes 0, per the spec).
-- * `usage_events.started_at` — "a request settles into the period it
--   started in" needs the request's start time, but `usage_events` only
--   records `occurred_at` (settlement time). The rollup day key and the
--   budget period therefore use `started_at` (falling back to `occurred_at`
--   for pre-M4 rows) so an idempotent rebuild reproduces the incremental
--   rows exactly.
-- * `usage_daily.unpriced_requests` / `priced_requests` /
--   `partial_requests` — the reports must distinguish complete costs, costs
--   that exclude an unpriced component (`priced = 0` with a non-NULL
--   understated cost), and costs that could not be computed at all; three
--   counters carry that without losing the spec's columns.
-- * `users.must_change_password` — forced password rotation (M4 deliverable,
--   review finding 7 deferred from M1).
-- * `reservations.released_at` — release and settlement are different
--   events; stamping `settled_at` on released rows (review nit) conflated
--   them, so releases stamp `released_at` and `settled_at` stays reserved
--   for the settle marker.
-- * `reservations.request_id` gets an index and `reservations(user_id,
--   status)` is indexed: settlement looks up by request id and the budget
--   check sums open reservations per user on every request.
--
-- Timestamps are UTC ISO-8601 with milliseconds and a trailing Z, written
-- by the application. Money is REAL USD. Tokens are INTEGER.

CREATE TABLE IF NOT EXISTS api_keys (
    id              TEXT PRIMARY KEY,
    user_id         TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    -- Visible head of the key (e.g. `pk_live_ab12`); lists show only this.
    prefix          TEXT NOT NULL,
    -- SHA-256 hex of the full key string; the key itself is never stored.
    key_hash        TEXT NOT NULL UNIQUE,
    scopes_json     TEXT,
    expires_at      TEXT,
    revoked_at      TEXT,
    last_used_at    TEXT,
    -- NULL = defaults (60 RPM / 100k TPM); see the header.
    rate_limit_rpm  INTEGER,
    rate_limit_tpm  INTEGER,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS grants (
    id            TEXT PRIMARY KEY,
    subject_type  TEXT NOT NULL CHECK (subject_type IN ('user', 'api_key')),
    subject_id    TEXT NOT NULL,
    resource_type TEXT NOT NULL
                  CHECK (resource_type IN ('model', 'offering', 'credential')),
    resource_id   TEXT NOT NULL,
    mode          TEXT NOT NULL CHECK (mode IN ('allow', 'deny')),
    created_by    TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at    TEXT NOT NULL,
    UNIQUE (subject_type, subject_id, resource_type, resource_id)
);

CREATE TABLE IF NOT EXISTS budgets (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    period       TEXT NOT NULL CHECK (period IN ('day', 'week', 'month', 'total')),
    -- Start (UTC) of this row's period bucket; `total` rows use the
    -- 1970-01-01 sentinel. The reset creates a new row (see the header).
    period_start TEXT NOT NULL,
    limit_usd    REAL NOT NULL,
    used_usd     REAL NOT NULL DEFAULT 0,
    reset_at     TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    UNIQUE (user_id, period, period_start)
);

CREATE TABLE IF NOT EXISTS token_caps (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    model_id     TEXT NOT NULL REFERENCES models (id) ON DELETE CASCADE,
    period       TEXT NOT NULL CHECK (period IN ('day', 'week', 'month', 'total')),
    -- Start (UTC) of this row's period bucket; see `budgets.period_start`.
    period_start TEXT NOT NULL,
    limit_tokens INTEGER NOT NULL,
    used_tokens  INTEGER NOT NULL DEFAULT 0,
    reset_at     TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    UNIQUE (user_id, model_id, period, period_start)
);

CREATE TABLE IF NOT EXISTS reservations (
    id                TEXT PRIMARY KEY,
    request_id        TEXT NOT NULL,
    user_id           TEXT NOT NULL,
    api_key_id        TEXT,
    offering_id       TEXT,
    estimated_tokens  INTEGER NOT NULL,
    estimated_usd     REAL NOT NULL,
    created_at        TEXT NOT NULL,
    -- The settle marker: only an actual settlement stamps `settled_at`.
    settled_at        TEXT,
    -- Release stamp (reconciler or a request that consumed nothing).
    released_at       TEXT,
    status            TEXT NOT NULL DEFAULT 'open'
                      CHECK (status IN ('open', 'settled', 'released'))
);

CREATE TABLE IF NOT EXISTS usage_daily (
    day                TEXT NOT NULL,
    user_id            TEXT NOT NULL,
    offering_id        TEXT NOT NULL,
    input_tokens       INTEGER NOT NULL DEFAULT 0,
    output_tokens      INTEGER NOT NULL DEFAULT 0,
    requests           INTEGER NOT NULL DEFAULT 0,
    cost_usd           REAL NOT NULL DEFAULT 0,
    -- Deviation (see header): requests whose cost could not be computed at
    -- all (cost_usd NULL on the event), requests whose cost is complete, and
    -- requests whose cost is understated (priced = 0 with a non-NULL cost).
    unpriced_requests  INTEGER NOT NULL DEFAULT 0,
    priced_requests    INTEGER NOT NULL DEFAULT 0,
    partial_requests   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, user_id, offering_id)
);

-- Forced password rotation (review finding 7); 0 for every existing row.
ALTER TABLE users ADD COLUMN must_change_password INTEGER NOT NULL DEFAULT 0;

-- Cost of one completion, priced in M4's settlement (decision D7).
ALTER TABLE usage_events ADD COLUMN priced INTEGER NOT NULL DEFAULT 0;
ALTER TABLE usage_events ADD COLUMN started_at TEXT;

CREATE INDEX IF NOT EXISTS idx_api_keys_user_id ON api_keys (user_id);
CREATE INDEX IF NOT EXISTS idx_grants_subject ON grants (subject_type, subject_id);
CREATE INDEX IF NOT EXISTS idx_budgets_user_id ON budgets (user_id);
CREATE INDEX IF NOT EXISTS idx_reservations_status_created
    ON reservations (status, created_at);
CREATE INDEX IF NOT EXISTS idx_reservations_request_id
    ON reservations (request_id);
CREATE INDEX IF NOT EXISTS idx_reservations_user_status
    ON reservations (user_id, status);
