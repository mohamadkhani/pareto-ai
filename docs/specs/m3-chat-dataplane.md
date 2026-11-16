# Spec: M3 — chat data plane

Status: draft (2026-11-16). Owner: chief-of-staff. Conventions:
[`AGENTS.md`](../../AGENTS.md). Decisions: D3 (OpenAI-compatible only), D11
(bodies retained 1 day, metadata only by default), D12 (SQLite), error taxonomy
in [04-architecture.md](../research/04-architecture.md) §9.

## Goal

`POST /v1/chat/completions` works against a real OpenAI-compatible provider,
streams SSE back unbuffered, survives upstream failures without hanging or
double-spending, and records usage and a routing decision for every request.

## Scope

1. Migration `0004_usage.sql`: `usage_events`, `request_logs` (columns below).
2. `server/src/upstream/` — OpenAI-compatible client: request build (merging
   credential headers + offering headers, offering wins on conflict), SSE
   passthrough, usage extraction from the final chunk, error mapping.
3. `server/src/chat.rs` — request pipeline: validate → resolve offering →
   forward → stream → settle usage. Deterministic single-candidate choice
   (scoring comes in M5); if the chosen offering fails before the first byte,
   try the next enabled offering for the model in a fixed order.
4. Timeouts and cancellation, request limits, error taxonomy, request ids.
5. `GET /v1/chat/completions` never buffers the whole response body.

## Non-goals

API keys and per-key quotas (M4), budgets (M4), scoring/telemetry (M5),
compression, non-OpenAI wire formats, embeddings/images.

## Schema (0004_usage.sql)

```sql
usage_events(id INTEGER PK AUTOINCREMENT, request_id TEXT NOT NULL, user_id TEXT
          REFERENCES users(id) ON DELETE SET NULL, api_key_id TEXT, offering_id
          TEXT REFERENCES offerings(id) ON DELETE SET NULL, model_id TEXT
          REFERENCES models(id) ON DELETE SET NULL, input_tokens INTEGER,
          output_tokens INTEGER, cache_read_tokens INTEGER, cache_write_tokens
          INTEGER, reasoning_tokens INTEGER, tokens_estimated INTEGER NOT NULL
          DEFAULT 0 CHECK (tokens_estimated IN (0,1)), cost_usd REAL, latency_ms
          INTEGER, ttft_ms INTEGER, status INTEGER, error_class TEXT,
          occurred_at TEXT NOT NULL)

request_logs(request_id TEXT PK, client_request_id TEXT, occurred_at TEXT NOT
          NULL, method TEXT, path TEXT, requested_model TEXT,
          resolved_offering_id TEXT, decision_json TEXT, status INTEGER,
          error_class TEXT, bytes_in INTEGER, bytes_out INTEGER, user_id TEXT,
          api_key_id TEXT, has_request_body INTEGER NOT NULL DEFAULT 0
          CHECK (has_request_body IN (0,1)))
```

- `request_id` is **server-generated** and is the ledger key; the client's
  optional `x-request-id` is preserved in `client_request_id` (amended after
  review blocker B1, 2026-11-16: a client-controlled PK let a duplicate id
  roll back both ledger rows).
- Indexes: `usage_events(user_id, occurred_at)`, `usage_events(offering_id,
  occurred_at)`, `usage_events(request_id)`, `request_logs(occurred_at)`,
  `request_logs(client_request_id)`.
- `decision_json` records the candidates considered (id, provider, model,
  price if known), the chosen one, and a one-line reason — the M5 explanation
  grows out of this column.
- Bodies are never stored (D11); `has_request_body` exists for later opt-in.

## Timeouts (env-configurable, defaults in parentheses)

- connect (10s), first byte (30s), idle between chunks (60s), total request
  (600s). Client disconnect must abort the upstream request within 1s.
- Config keys: `GATEWAY_TIMEOUT_CONNECT_MS`, `_FIRST_BYTE_MS`, `_IDLE_MS`,
  `_TOTAL_MS`. Documented in `README.md`.

## Request limits

- Max request body (4 MB default, `GATEWAY_MAX_BODY_BYTES`), max `messages`
  count (200), `max_tokens` clamped to the offering's `max_output_tokens`/
  `max_output_override`; input longer than the offering's context minus
  `max_tokens` → `400 context_length_exceeded` before any upstream call.

## Auth and CSRF

- `/v1/*` is bearer-authenticated (API key from M4; admin session until then)
  and **exempt from the CSRF guard** — OpenAI-SDK clients never send
  `X-CSRF-Token` (M2 review nit, 2026-11-16). The CSRF guard stays on
  cookie-authenticated `/api/*` only.

## Streaming and failure rules

- SSE passthrough: forward bytes as they arrive (`Body::from_stream`), never
  re-buffer the whole body; forward `[DONE]` and `data:` lines untouched.
- Failover is allowed **only before the first byte reaches the client**.
  After that, surface the error and end the stream (documented in the handler).
- Upstream `usage` in the final chunk is authoritative; when missing, estimate
  tokens from **content text only** (`choices[].message.content` /
  `delta.content`, plus `reasoning` when present; input from the message text),
  `chars/4`, and set `tokens_estimated=1` — never from JSON scaffolding or raw
  body bytes (review M1, 2026-11-16). Estimation applies only when a request
  was actually sent upstream (review M3).
- Never retry a `400`/`401`/`403` from upstream; `429` is retried once on the
  next candidate honouring `Retry-After` as a cooldown on that credential.

## Error taxonomy (status + `error.code`)

| Class | HTTP | `error.code` | Action |
| --- | --- | --- | --- |
| bad request / validation | 400 | `invalid_request` / `context_length_exceeded` | no upstream call |
| auth (caller) | 401/403 | `unauthorized` | — |
| no eligible offering | 404 | `model_not_found` | — |
| upstream auth/quota | 502 | `upstream_auth` / `upstream_quota` | cooldown, failover |
| upstream rate limit | 429 | `upstream_rate_limited` | honour `Retry-After`, failover |
| upstream transient | 502 | `upstream_error` | failover |
| upstream timeout | 504 | `upstream_timeout` | failover |
| stream died mid-flight | 502 | `upstream_stream_failed` | no failover |
| internal | 500 | `internal` | alert |

All with the standard `{error: {code, message, request_id}}` shape.

## Testing (fake upstream, no network)

`server/src/upstream/tests.rs` or `server/tests/` with a scripted local
OpenAI-compatible stub (tokio TCP or axum on a random port) covering: normal
stream with usage, stream without usage (estimation), slow first byte (timeout),
stall mid-stream (idle timeout), truncated stream, `429` with `Retry-After`,
`500`, malformed SSE, and a client that disconnects mid-stream (assert the
upstream connection is closed within 1s). Live provider verification is blocked
in this environment ([09-environment.md](../research/09-environment.md)) and is
a documented follow-up.

## Acceptance

1. `scripts/check.sh` passes.
2. A `curl` sequence (documented in the report) streams a completion from the
   stub end-to-end, and `usage_events`/`request_logs` rows are written for it.
3. Failover happens only before the first byte; after it, the client sees a
   clean error and no second upstream call is made (tested).
4. Timeout tests prove no request can hang longer than its configured budget.
5. Disconnect test proves the upstream call is aborted within 1s.
