# Product scope audit

The requested feature list, checked against what an internal team gateway
actually needs to be correct and stable. "Core" means the first usable release
is not safe or useful without it.

## 1. Requested features

| # | Request | Verdict | Notes |
| --- | --- | --- | --- |
| 1 | Multiple OpenAI-compatible providers | Core | Keep the adapter declarative. |
| 2 | Multiple credentials per provider | Core | Credential is the unit of quota, cooldown, and spend. |
| 3 | Load models and parameters (modalities, context, max output) | Core | Needs a per-provider catalog + manual overrides. |
| 4 | Load ranks and benchmarks | Should-have | Model-level quality, not provider-level. |
| 5 | Load price from OpenRouter, extensible later | Core | Price belongs to the provider×model offering. |
| 6 | Manual model entry with all parameters | Core | Escape hatch for private/self-hosted models. |
| 7 | Custom headers per credential and model | Core | Required by many gateways and Azure-style deployments. |
| 8 | Users with their own password | Core (team) | Admin-created accounts, no public signup. |
| 9 | API keys for users | Core | Hash at rest, show once, support expiry/rotation. |
| 10 | Admin access to models and credentials per user | Core | Role + explicit grants. |
| 11 | API-key access to models and credentials | Core | Scopes on the key, not only on the user. |
| 12 | User login and self-service usage view | Core | Read-only. |
| 13 | Admin usage reports for all users | Core | Aggregations by day/week/month/user/model/provider. |
| 14 | Admin request log viewer | Core | Redact prompts by default. |
| 15 | Combo models with multiple algorithms incl. weighted | Core | Start with 3 strategies, not 15. |
| 16 | Per-user, per-model token caps | Core | Reservation model, see architecture. |
| 17 | Per-user spend caps: daily/weekly/monthly/total | Core | Define timezone and reset boundaries. |
| 18 | English only, i18n later | Decision | Do not add an i18n framework in v1. |
| 19 | Compression later (RTK etc.) with savings report | Later | Keep a token-accounting seam so savings are measurable. |

## 2. Missing essentials

These are not in the request and will cause incorrect billing, leaked keys, or
hangs if omitted.

### Correctness and safety

1. **Model vs provider-offering separation.** `gpt-4o` is a model; "gpt-4o on
   Provider A at $2.50/M with 128k context" is an offering. Price, speed,
   context, and availability differ per offering. Mixing them makes routing and
   reporting wrong.
2. **Streaming contract.** SSE passthrough, `[DONE]`, tool-call deltas, usage in
   the final chunk (many providers omit it), and `finish_reason` handling.
3. **Cancellation and timeouts.** Client disconnect must abort the upstream
   request. Separate connect, first-token, idle-between-chunks, and total
   timeouts. Without these, a dead upstream pins a task and memory forever.
4. **Mid-stream failover policy.** Before the first byte you can retry another
   candidate. After bytes are flushed you cannot. Decide and document this.
5. **Request limits.** Max body size, max images, max tool schema size, max
   concurrent streams per key and per credential.
6. **Idempotency and retry.** Retry only safe failures (connect error, 429 with
   `Retry-After`, 5xx before first byte). Never retry a 400 or an auth error.

### Money and quotas

7. **Price components.** Input, output, cached input read, cache write, reasoning
   tokens, image/audio units, per-request fees. A single "per 1M tokens" number
   is not enough.
8. **Unknown-price policy.** Fail closed, use a configured default, or block the
   model. Pick one per provider.
9. **Budget atomicity.** Check-then-spend races under concurrency. Use a
   reservation ledger settled from the final usage numbers.
10. **Budget calendar.** Daily/weekly/monthly boundaries need an explicit
    timezone (store UTC, evaluate in a configured zone) and a defined week start.
11. **Token counting.** Local estimate before the request (to clamp
    `max_tokens` and reject oversized input), authoritative upstream usage after.
12. **Reconciliation.** Upstream usage can be missing or wrong; record estimated
    vs reported and expose the delta.

### Security

13. **Credential encryption at rest.** Upstream API keys and custom header
    values must be encrypted with a key outside the database (env/KMS), not
    stored as plaintext like OmniRoute's initial schema.
14. **API-key hashing.** Store a hash (Argon2id or SHA-256 for high-entropy
    keys) plus a lookup prefix. Display the key once.
15. **Log redaction.** Never persist `Authorization`, provider keys, or full
    prompts by default. If bodies are stored, make it opt-in with retention.
16. **Audit log.** Who created/changed a provider, credential, user, key, grant,
    or budget, and when.
17. **Login protection.** Rate limit, lockout/backoff, secure cookie flags,
    CSRF for cookie-authenticated admin calls.

### Operations

18. **Health model.** Circuit breaker per (provider, credential, model), not per
    provider. One dead model must not poison the others (OmniRoute has an
    explicit comment about this exact OpenRouter failure mode).
19. **Metrics and tracing.** Request count, latency, TTFT, tokens, cost, error
    class, queue depth, in-flight streams, per-candidate outcome.
20. **Backup, restore, migrations.** A single SQLite file with WAL; a documented
    backup command and a tested restore.
21. **Config import/export.** Providers, models, combos, and grants as a file so
    environments can be reproduced.
22. **Abuse and loop protection.** Per-user RPM, max tokens per request, and a
    guard against a client retry loop draining budget.

### Product surface

23. **API compatibility scope.** At minimum `POST /v1/chat/completions`
    (stream and non-stream), `GET /v1/models`, and `GET /v1/models/{id}`.
    Decide early on `/v1/embeddings`, `/v1/responses`, and `/v1/images`.
24. **Model aliasing.** Combo IDs and aliases must be resolvable and must not
    collide with real model IDs.
25. **Explainability.** Store the routing decision: eligible set, scores, chosen
    candidate, and the reason each other candidate lost.

## 3. Scope cuts for v1

- No public signup, no email delivery, no password reset by email. Admin resets
  passwords.
- No OAuth provider logins, no MCP, no A2A, no Electron, no MITM proxy, no
  browser automation, no compression.
- No non-OpenAI wire protocols. Anthropic/Gemini native support waits; their
  OpenAI-compatible endpoints cover most needs.
- No embeddings/images/audio until chat is solid.
- No automatic provider onboarding by URL probing beyond a health check.

## 4. Suggested delivery order

**P0 — trustworthy core**

- Single binary, SQLite, migrations, config.
- Admin login, one admin user, audit log skeleton.
- Provider + credential CRUD with encrypted secrets.
- Model catalog: manual entry + OpenRouter import.
- `GET /v1/models`, `POST /v1/chat/completions` (stream + non-stream).
- Usage log with tokens, latency, TTFT, status.
- Timeouts, cancellation, body limits.

**P1 — money and access**

- Users, roles, API keys (hashed), model/credential grants.
- Price table with provenance; cost calculation per request.
- Per-user, per-model token caps; daily/weekly/monthly/total USD budgets.
- Reservation + settlement.
- Fallback combo (ordered).
- Admin usage report and request log viewer (redacted).

**P2 — smart routing**

- Passive speed telemetry (TTFT, TPS, p95, failure rate).
- Weighted scoring combo with user-defined weights and preview.
- Benchmark import with source and timestamp.
- Routing explanation view.
- Weighted/round-robin/cost-first strategies.

**P3 — extension**

- User-defined pricing adapters (declarative extraction rules + JSONPath).
- Prompt-cache affinity and request deduplication.
- Compression with savings accounting.
