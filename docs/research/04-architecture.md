# Architecture proposal

Assumptions: internal team gateway, one operator team, a handful to a few dozen
users, self-hosted, single region. Optimize for predictable latency, bounded
memory, and operational simplicity over feature breadth.

## 1. Shape

```
            ┌──────────────────────────────────────────────┐
 clients ──▶│  Rust binary (axum)                          │
            │                                              │
            │  /v1/*        data plane (OpenAI compatible) │
            │  /api/*       admin plane (cookie session)   │
            │  /assets/*    embedded Angular build         │
            │                                              │
            │  router → adapters → upstream (reqwest)      │
            │  metering / budgets / circuit breakers       │
            │  SQLite (WAL) + in-memory caches             │
            └──────────────────────────────────────────────┘
```

One process, one deployment artifact. The UI is static files embedded in the
binary, so there is no Node runtime in production. If the data plane later needs
isolation, the admin plane can be split, but not before it is a measured problem.

## 2. Technology choices

| Concern | Choice | Reason |
| --- | --- | --- |
| HTTP server | `axum` on `tokio` | Mature, tower ecosystem, streaming support |
| Upstream HTTP | `reqwest` (rustls) with `bytes_stream` | Connection pooling, timeouts, streaming |
| Database | SQLite in WAL mode via `sqlx` | Single-node, zero ops, fast enough for this scale |
| Migrations | `sqlx::migrate!` | Compile-time checked, boring |
| Serialization | `serde` / `serde_json` | Standard |
| Auth | `argon2` for passwords, random 256-bit API keys hashed | No plaintext secrets |
| Encryption | AEAD (XChaCha20-Poly1305 or AES-256-GCM) with a master key from env | Credentials encrypted at rest |
| Rate limiting | `governor` or a small token-bucket table | Per key/credential |
| Metrics | `metrics` + Prometheus exporter | Ops visibility |
| Tracing | `tracing` with request IDs | Debuggable |
| Frontend | Angular, standalone components, signals, zoneless | Current Angular direction; no NgModules |
| Packaging | `cargo build --release`, static assets via `rust-embed` | One binary |

**Version note:** pin exact versions at implementation time from official docs
and `cargo add` / `ng new`. This log deliberately does not freeze version
numbers because they were not verified in this environment.

## 3. Module boundaries

- `api/` — HTTP handlers for data plane and admin plane. Thin.
- `auth/` — sessions, passwords, API keys, RBAC checks.
- `catalog/` — providers, models, offerings, prices, benchmarks.
- `routing/` — candidate filtering, scoring, ordering, explanation.
- `upstream/` — provider adapters, request building, SSE parsing, error mapping.
- `metering/` — token accounting, cost calculation, budgets, reservations.
- `storage/` — repositories over SQLite, migrations.
- `jobs/` — price/benchmark sync, aggregation, cleanup.

Rule: `routing` is pure (no I/O) and takes a snapshot of catalog + telemetry.
That makes it testable and keeps the hot path free of database calls.

## 4. Data model

The four-level split is the core decision.

```
providers            id, name, kind ('openai_compatible'), base_url, enabled
provider_credentials id, provider_id, label, encrypted_secret, headers_json,
                     max_concurrency, status, cooldown_until
models               id, canonical_name, family, modalities, context, max_output,
                     capabilities_json
offerings            id, provider_id, model_id, upstream_model_id,
                     context_override, max_output_override, price_id,
                     enabled, headers_json
prices               id, offering_id, input_per_m, output_per_m,
                     cache_read_per_m, cache_write_per_m, reasoning_per_m,
                     request_fee, currency, source, source_url, fetched_at
benchmarks           model_id, source, category, score, ci, rank, fetched_at
combo_models         id, name, strategy, weights_json, members_json, enabled
```

Access and money:

```
users            id, email, password_hash, role ('admin'|'user'), active
sessions         id, user_id, expires_at, csrf_token
api_keys         id, user_id, name, prefix, key_hash, scopes_json,
                 expires_at, last_used_at, revoked_at
grants           subject_type ('user'|'api_key'), subject_id,
                 resource_type ('model'|'offering'|'credential'),
                 resource_id, mode ('allow'|'deny')
token_caps       user_id, model_id, period ('day'|'week'|'month'|'total'),
                 limit_tokens, used_tokens
budgets          user_id, period, limit_usd, used_usd, resets_at
reservations     id, request_id, user_id, api_key_id, offering_id,
                 estimated_tokens, estimated_usd, created_at, settled_at
usage_events     id, request_id, user_id, api_key_id, offering_id, model_id,
                 input_tokens, output_tokens, cache_read, cache_write,
                 reasoning_tokens, cost_usd, latency_ms, ttft_ms,
                 status, error_class, occurred_at
request_logs     request_id, path, requested_model, resolved_offering,
                 decision_json, status, bytes_in, bytes_out, redacted
audit_log        actor_user_id, action, target_type, target_id, diff_json,
                 occurred_at
```

Notes:

- `offerings` is what the router scores and what budgets attach to. Users may be
  granted at the `model` level; the router then picks among allowed offerings.
- Denormalized aggregates (daily usage) are derived tables, rebuildable from
  `usage_events`.
- Store timestamps in UTC as integers (epoch millis) or ISO-8601 with `Z`.
  Choose one and enforce it in the repository layer.

## 5. Request pipeline

1. Authenticate API key → user, scopes, rate-limit bucket.
2. Parse and validate the OpenAI request (size limits, `model`, `stream`).
3. Resolve `model` to a set of offerings (alias or combo expansion).
4. Filter: capability match, context fit, grant, budget, token cap, breaker
   state, concurrency cap.
5. Score and order (see below). Pin if the caller explicitly asked for a
   provider/model.
6. Reserve estimated cost and tokens atomically.
7. Try candidates in order:
   - connect timeout, first-token timeout, idle timeout, total timeout;
   - on failure before first byte, classify and move to the next candidate;
   - on 429, respect `Retry-After` and optionally retry the same candidate;
   - after first byte, never switch; surface a clear error.
8. Stream with backpressure; forward bytes without re-buffering the whole body.
9. On completion, settle the reservation from actual usage; record the event and
   the routing decision.

## 6. Scoring

Filters first, then a weighted score, then a deterministic tie-break.

```
score = w_price   * price_score
      + w_speed   * speed_score
      + w_quality * quality_score
      + w_rel     * reliability_score
      - penalties
```

- Each component is normalized to `[0, 1]` across the eligible pool.
- Missing component → pool median (0.5), matching OmniRoute's policy, and mark it
  as imputed in the explanation.
- Default weights: price 0.25, speed 0.30, quality 0.30, reliability 0.15.
  Expose presets: "cheapest", "fastest", "best", "balanced".
- Penalties: recent failure rate, breaker open, low remaining quota, context
  near limit, large latency standard deviation.
- Tie-break: lower price, then lower p95, then stable ID. Never random, so
  results are reproducible and explainable.

Store per request: candidate list with each score component, the winner, and a
one-line reason per candidate. This is the debugging surface for "why did it
pick that model".

## 7. Budgets and reservations

Concurrency makes check-then-spend wrong. Sequence:

1. Estimate input tokens (tokenizer or `chars/4` heuristic with a safety factor)
   and `max_tokens` for output.
2. In one SQLite transaction: read user budget and caps, add the estimate to
   `reserved`, reject if `used + reserved + estimate > limit`, insert a
   reservation row.
3. On completion: delete the reservation, add actual usage to `used` and
   `usage_events`.
4. On abandon/error: delete the reservation; if upstream reported usage, settle
   it anyway.

A background job reconciles stale reservations (e.g. older than 10 minutes) so a
crashed stream cannot lock a user out forever.

Periods: store `resets_at` explicitly for day/week/month; compute in a configured
timezone, store UTC. A request that starts before the boundary settles into the
period it started in.

## 8. Security model

- Passwords: Argon2id, per-user salt, minimum length, no password in logs.
- Sessions: HttpOnly, Secure, SameSite=Lax cookie, server-side session table,
  CSRF token for state-changing admin calls.
- API keys: 32 random bytes, base64url with a short recognizable prefix
  (`pk_...`). Store `SHA-256(key)` for lookup plus a separate slow hash if the
  threat model requires it. Never store the key itself.
- Provider secrets: AEAD-encrypted with a per-row nonce; master key from
  `GATEWAY_MASTER_KEY`; support key rotation by re-encrypting rows.
- Redaction: default request logging stores metadata only. Body capture is opt-in,
  per-user, with retention and size limits.
- Audit every mutation in the admin plane.

## 9. Failure taxonomy

| Class | Examples | Action |
| --- | --- | --- |
| Auth | 401/403 upstream | Mark credential bad, alert, do not retry |
| Quota | 402, "insufficient credits", provider quota message | Cool down credential, fail over |
| Rate limit | 429 | Honor `Retry-After`, then fail over |
| Transient | connect error, 502/503/504, reset | Retry/fail over before first byte |
| Model | 404 model, unsupported parameter | Disable offering or strip parameter, fail over |
| Content | 400 invalid request | Return to client, no retry |
| Stream | empty stream, truncated stream, stall | Fail over only if nothing was sent |
| Internal | timeout, panic, DB error | 500 with request ID, alert |

## 10. Observability

- Prometheus: requests by status/provider/model, latency histogram, TTFT
  histogram, tokens, cost, breaker state, in-flight streams, queue depth.
- Structured logs with `request_id`, never secrets.
- A `/health` endpoint that checks DB and config only, not upstreams.
- Per-request trace record for the admin log viewer.

## 11. Testing strategy

- Unit: routing math, normalization, cost calculation, budget math.
- Contract: recorded SSE fixtures per provider quirk (usage missing, tool-call
  deltas, reasoning fields, error JSON shapes).
- Integration: fake upstream server with scripted failures (429, 500, stall,
  truncation) to prove failover and settlement.
- Load: many concurrent streams, bounded memory, no descriptor leaks.
- Security: secret redaction, authz matrix, CSRF.

## 12. Deployment

- Docker image (multi-stage, non-root) plus a plain binary and a systemd unit.
- Volumes: database directory and a backup directory.
- Config via env: master key, listen address, data dir, timezone, log level.
- Backup: `sqlite3 .backup` on a schedule; test restore in CI.
