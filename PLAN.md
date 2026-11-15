# PLAN — pareto-ai

Internal, self-hosted AI gateway: one OpenAI-compatible endpoint in front of
several providers, optimized for speed, low resource use, and stability. Price,
speed, and benchmark data drive a weighted, explainable model/provider choice
with fallback, per-user budgets, and admin oversight.

- Stack: Rust (axum) backend, Angular frontend, SQLite (WAL) storage.
- Research and decisions: [`docs/research/`](docs/research/README.md).
- Decisions D1–D12 are locked; see [docs/research/05-decisions.md](docs/research/05-decisions.md).
- Budget enforcement is **hard stop** (decided 2026-11-15).
- Execution: agent writes, human reviews each milestone.

## Status

| Milestone | State | Date |
| --- | --- | --- |
| M0 repository skeleton | **Done** (fmt, clippy `-D warnings`, tests, `/health`, Angular build all pass) | 2026-11-15 |
| M1 auth and audit | Not started | |
| M2 providers, credentials, catalog | Not started | |
| M3 chat data plane | Not started | |
| M4 users, keys, money | Not started | |
| M5 routing | Not started | |

Environment note: crates.io is unreachable from this network, so dependency
commands run with `--offline` against the local cargo cache; `npm` works
normally. Details in [`README.md`](README.md).

## Non-goals (v1)

Public signup, email flows, SSO, non-OpenAI wire protocols, MCP/A2A, Electron,
MITM, compression, embeddings/images, i18n, multi-node deployment.

## Repository layout

```
pareto-ai/
  PLAN.md
  README.md
  docs/research/          research log (dated, sourced)
  server/                 Rust binary
    src/
    migrations/
    Cargo.toml
  web/                    Angular app (static build embedded in the binary)
```

## Conventions

- Rust: `cargo fmt --check`, `cargo clippy -- -D warnings`, no `unwrap()` in
  request paths, explicit `Result`, typed errors per module.
- TypeScript: strict mode, ESLint, Prettier, standalone components + signals.
- Commits: Conventional Commits, e.g. `feat(routing): weighted scoring`.
- Every decision that changes design is appended to `docs/research/`.
- Secrets never in code, logs, or `.env` committed to git.

---

## Milestone 0 — repository skeleton

Tasks:

1. `server/` cargo crate with axum, tokio, tower-http, tracing, sqlx (SQLite).
2. Config from env: `GATEWAY_LISTEN`, `GATEWAY_DATA_DIR`, `GATEWAY_MASTER_KEY`,
   `GATEWAY_TIMEZONE`, `RUST_LOG`.
3. Migrations directory wired through `sqlx::migrate!`; migration 001 creates
   `db_meta` and `settings`.
4. `GET /health` returning build info and DB status.
5. `web/` Angular app scaffolded (standalone, strict TS, signals), dev proxy to
   the backend, production build output target for embedding.
6. README with run instructions.

Acceptance:

- `cargo fmt --check && cargo clippy -- -D warnings && cargo test` pass.
- `curl /health` returns 200 with `db: ok`.
- `npm run build` produces `web/dist/`.
- Startup creates `data/gateway.db` on first run.

## Milestone 1 — auth and audit (P0)

Tasks:

1. `users` (admin role, Argon2id password hash), `sessions` (server-side,
   HttpOnly cookie, CSRF token), `audit_log`.
2. Login/logout routes, session middleware, `require_admin` guard.
3. Bootstrap: first run creates the admin from `GATEWAY_ADMIN_EMAIL` /
   `GATEWAY_ADMIN_PASSWORD` if no user exists; password must be rotated.
4. Angular: login page, guarded admin shell, audit log view.

Acceptance:

- Wrong password is rate-limited and logged.
- Unauthenticated admin API calls return 401; CSRF-less mutations return 403.
- Every mutation writes one `audit_log` row.

## Milestone 2 — providers, credentials, catalog (P0)

Tasks:

1. Tables: `providers`, `provider_credentials` (AEAD-encrypted secret and
   custom headers, `balance_usd`, `balance_source='manual'`),
   `models`, `offerings`, `prices`.
2. Admin CRUD for provider + credential + manual model/offering with all
   parameters (modalities, context, max output) and custom headers.
3. OpenRouter import: `GET https://openrouter.ai/api/v1/models`, 24h cache,
   stale-if-error, provenance columns.
4. `GET /v1/models` listing offerings the caller may use.
5. Health check per credential (an OpenAI-compatible `/models` probe or a
   minimal completion), recorded on the credential.

Acceptance:

- Secrets are unreadable in the DB file and never returned by any API.
- OpenRouter import is idempotent and shows `source`/`fetched_at` in the UI.
- A manual offering with a manual price can be selected and priced.

## Milestone 3 — chat data plane (P0)

Tasks:

1. `POST /v1/chat/completions`: resolve model → offering(s), validate context
   and `max_tokens`, forward with per-offering headers, stream SSE back
   unbuffered.
2. Timeouts: connect, first byte, idle between chunks, total. Client disconnect
   aborts upstream.
3. Error taxonomy with mapped status codes and `request_id`.
4. `usage_events` written on completion (tokens, latency, TTFT, status,
   error class); `request_logs` metadata-only by default.
5. Deterministic single-candidate choice (no scoring yet).

Acceptance:

- Streaming works against a real provider and against a scripted fake upstream
  that stalls, truncates, and returns 429/500.
- A stalled stream is aborted at the configured timeout with a clean 504.
- Disconnecting the client frees the upstream connection within one second.
- Memory stays flat under 200 concurrent streams (test with a load script).

## Milestone 4 — users, keys, money (P1)

Tasks:

1. `api_keys` (prefix + SHA-256 hash, scopes, expiry, revocation), key auth on
   `/v1/*`, per-key RPM/TPM buckets.
2. `grants` model/offering/credential allow+deny for users and keys; admin
   screens for access control.
3. Pricing: input/output/cache/reasoning rates per offering; cost computed from
   reported usage, `estimated` vs `reported` delta recorded.
4. Budgets (day/week/month/total, USD) and token caps (per user per model) with
   **hard stop**: reservation at request start in one SQLite transaction,
   settlement after, reconciler for stale reservations.
5. Credential balance displayed; warn in v1, gate when `balance_source` changes.
6. Usage report for users; admin report for all users + redacted request log
   viewer.

Acceptance:

- Two concurrent requests cannot exceed a budget (proved by a test).
- After the cap is reached, the next request returns 402 with a clear body.
- Usage report reconciles with `usage_events` totals.

## Milestone 5 — routing (P2)

Tasks:

1. Passive telemetry per offering: TTFT, tokens/sec, p95, failure rate, EWMA
   windows (bounded, 200 samples).
2. Ordered-fallback combo with recorded decision JSON (candidates, scores,
   reason each lost).
3. Weighted scoring: price/speed/quality/reliability normalized per pool, user
   weights, presets (cheapest/fastest/balanced/best), deterministic tie-break.
4. Benchmark import (LMArena-style Elo) with `source`, `fetched_at`, `ci`.
5. Routing explanation view in the admin UI.

Acceptance:

- Same input always produces the same ordering.
- A breaker-open offering is excluded and the reason is visible.
- Weights changed by a user change the winner in a fixture-based test.

## Later (explicitly out of scope now)

User-defined price API extraction, prompt-cache affinity, request dedup,
compression with savings accounting, non-OpenAI protocols, multi-instance.

---

## Working order

M0 → M1 → M2 → M3 are sequential; M4 and M5 may interleave after M3.
No milestone is "done" until its acceptance block passes and the research log
records anything that changed a decision.
