# Spec: M4 — users, API keys, access, money

Status: shipped 2026-11-16 — closed by `b1e58f0` (backend `30f95ec`). Owner:
chief-of-staff. Conventions:
[`AGENTS.md`](../../AGENTS.md). Decisions: D7 (reservations), D8 (manual
credential balance, advisory in v1), D9 (org limits on top of balance),
D12 (SQLite), review finding 7 (forced password rotation lands here).

## Goal

Users can authenticate with API keys against `/v1/*`, admins control who may
use which models/credentials, and spending is metered and **hard-capped** per
user (daily/weekly/monthly/total USD and per-model token caps).

## Non-goals

Routing strategies (M5), price sync beyond M2, compression, email, SSO,
multi-node.

## Schema (0005_access_money.sql)

```sql
api_keys(id TEXT PK, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
          name TEXT NOT NULL, prefix TEXT NOT NULL, key_hash TEXT NOT NULL UNIQUE,
          scopes_json TEXT, expires_at TEXT, revoked_at TEXT, last_used_at TEXT,
          created_at TEXT NOT NULL, updated_at TEXT NOT NULL)

grants(id TEXT PK, subject_type TEXT NOT NULL CHECK (subject_type IN ('user','api_key')),
          subject_id TEXT NOT NULL, resource_type TEXT NOT NULL
          CHECK (resource_type IN ('model','offering','credential')),
          resource_id TEXT NOT NULL, mode TEXT NOT NULL CHECK (mode IN ('allow','deny')),
          created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
          created_at TEXT NOT NULL, UNIQUE(subject_type, subject_id, resource_type, resource_id))

budgets(id TEXT PK, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
          period TEXT NOT NULL CHECK (period IN ('day','week','month','total')),
          limit_usd REAL NOT NULL, used_usd REAL NOT NULL DEFAULT 0, reset_at TEXT,
          created_at TEXT NOT NULL, updated_at TEXT NOT NULL, UNIQUE(user_id, period))

token_caps(id TEXT PK, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
          model_id TEXT NOT NULL REFERENCES models(id) ON DELETE CASCADE,
          period TEXT NOT NULL CHECK (period IN ('day','week','month','total')),
          limit_tokens INTEGER NOT NULL, used_tokens INTEGER NOT NULL DEFAULT 0,
          reset_at TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
          UNIQUE(user_id, model_id, period))

reservations(id TEXT PK, request_id TEXT NOT NULL, user_id TEXT NOT NULL,
          api_key_id TEXT, offering_id TEXT, estimated_tokens INTEGER NOT NULL,
          estimated_usd REAL NOT NULL, created_at TEXT NOT NULL,
          settled_at TEXT, status TEXT NOT NULL DEFAULT 'open'
          CHECK (status IN ('open','settled','released')))

usage_daily(day TEXT NOT NULL, user_id TEXT NOT NULL, offering_id TEXT NOT NULL,
          input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
          requests INTEGER NOT NULL DEFAULT 0, cost_usd REAL NOT NULL DEFAULT 0,
          PRIMARY KEY (day, user_id, offering_id))
```

- `api_keys.key_hash` = SHA-256 of the key; `prefix` (e.g. `pk_live_ab12`) is
  shown in lists, the full key is returned **once** at creation.
- Indexes: `reservations(status, created_at)`, `api_keys(user_id)`,
  `grants(subject_type, subject_id)`, `budgets(user_id)`.

## Behaviour

### API keys

- `POST /api/users/{id}/keys` → `{key: "pk_..."}` once; `GET`/`DELETE`
  (`revoke`) for lists; expiry and revocation enforced on use.
- `/v1/*` accepts `Authorization: Bearer <key>` (data-plane auth lands here;
  the admin session may keep working for `/v1/models` as today).
- Per-key rate limits: RPM and TPM token buckets (in-memory, per key),
  configurable per key with sane defaults (e.g. 60 RPM), returning `429` with
  `Retry-After`.
- `last_used_at` updated on use (cheap write; batch if needed).

### Access control (D9, D56)

- Effective access = the owner's grant verdict; when the key has grant rows of
  its own, it intersects with the key's verdict (a key may narrow the account,
  never widen it). A key with no grants inherits its owner. `deny` beats
  `allow`; no grants at all means "everything" for admins and "nothing" for
  users? **No:** default is allow for `admin`, deny-unless-granted for `user`.
- Admin screens/API for grants; every change audited.

### Money (D7, D8)

- Cost per request: `input`, `output`, `cache_read`, `cache_write`, `reasoning`
  tokens are billed **subset-aware** (D22): `(input - cache_read) * input_per_m
  + cache_read * cache_read_per_m + (output - reasoning) * output_per_m +
  reasoning * reasoning_per_m`, divided by 1_000_000, plus `request_fee`;
  every term clamped at zero. Missing component prices contribute 0 and the row
  is flagged `priced=0` in the report. Unknown price (`NULL`) → `cost_usd = NULL`
  and the request is reported as "unpriced" (never silently 0).
- **Hard stop**: before forwarding, open a `reservations` row in one
  transaction with the estimate (input estimate from chars/4 + `max_tokens`);
  reject with `402 budget_exceeded` or `429 token_cap_exceeded` when
  `used + open reservations + estimate > limit`. On completion settle:
  `used_usd += actual`, delete/close the reservation. A background task
  releases reservations older than the reservation TTL (`status='released'`).
  - Shipped deviation (2026-11-16, `30f95ec`; review blocker B1): the TTL is
    **derived strictly above `GATEWAY_TIMEOUT_TOTAL_MS`**, not the fixed
    10 minutes this spec first proposed — a fixed 10-minute TTL could release
    a still-running request (default total timeout is 600 s) and lose its
    actuals.
- Periods: `reset_at` computed in `GATEWAY_TIMEZONE` (day/week/month; week
  starts Monday); a request settles into the period it started in. `total`
  never resets.
- Credential balance (D8): displayed in the UI with its `balance_source` and
  age; **warn only** in v1 — no gating.
- Reports: user self-service (`GET /api/usage/me`), admin aggregate
  (`GET /api/usage?user&from&to&group_by=day|user|offering`), backed by
  `usage_daily` rollups rebuilt idempotently from `usage_events`; request log
  viewer stays metadata-only (D11).

### User management (M4 additions)

- `GET/POST/PATCH /api/users` (admin): create users with role and password,
  deactivate, reset password.
- `POST /api/auth/change-password` (self + admin-forced), and bootstrap admin
  gets `must_change_password=1` (review finding 7): `/api/auth/me` exposes it
  and the UI forces the change before other screens work.

## Testing

- Concurrency: two simultaneous requests cannot exceed one budget (proved with
  a barrier-based test on the reservation transaction).
- Reservation lifecycle: open → settle on success, release on failure/timeout,
  reconciler releases stale rows.
- Cost math with every price component present/absent/NULL.
- Grant matrix: user vs key, allow vs deny, admin default, no-grant default.
- Key hashing: the plaintext key is never stored or logged; revoked/expired
  keys rejected; per-key rate limit trips.
- Rollup rebuild is idempotent.

## Acceptance

1. `just check` passes (written as `scripts/check.sh` at the time; the rename
   is D21).
2. Documented `curl` sequence: create user → key → grant → request → budget
   exhausted → `402` → report shows the spend.
3. Budget race test proves no overspend; reconciler test proves no lockout.
4. Reports reconcile with `usage_events` totals (asserted in a test).
5. The bootstrap admin cannot use the UI without changing the password.
