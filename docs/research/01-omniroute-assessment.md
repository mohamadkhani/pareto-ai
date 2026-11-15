# OmniRoute assessment

Read on 2026-11-15 against `release/v3.8.54` (commit `23b8c42`).

## 1. What it is

A self-hosted AI gateway that exposes one OpenAI-compatible endpoint in front of
hundreds of upstream providers, with free-tier rotation, auto-fallback, token
compression, MCP/A2A support, a web dashboard, a CLI, and an Electron desktop
build. MIT licensed.

Metadata:

| Field | Value |
| --- | --- |
| Repository | `cheaper-inference/OmniRoute` |
| License | MIT (`license.spdx_id = MIT`) |
| Default branch | `release/v3.8.54` |
| Created | 2026-02-13 |
| Stars / forks | 87,683 / 13,445 |
| Open issues | 560 |
| Reported repo size | 3,522,851 KB (≈3.5 GB) |
| Primary language | TypeScript |
| Runtime | Node ≥22.22.2, Next.js 16, React 19 |
| Storage | SQLite (`better-sqlite3` / `sql.js`), file-backed caches |

Repository shape at this commit: **29,542 files**

| Area | Files |
| --- | --- |
| `docs/` (mostly i18n copies) | 10,610 |
| `tests/` | 9,837 |
| `src/` | 4,596 |
| `open-sse/` (gateway core) | 2,429 |
| `scripts/` | 506 |
| `changelog.d/` | 476 |
| `bin/`, `public/`, `packages/`, `@omniroute/`, `config/`, `deploy/`, `electron/` | ≈800 |

Provider registry: **287 provider directories**, 321 registry files. Database:
**241 migrations**. The combo/routing area alone is ≈140 modules with names like
`noReplayLatch`, `safetyStopLatch`, `protectedPriorityStopStatus`,
`quotaShareInflight`, `agenticStallFailover`.

That shape is the single most important finding: this is a breadth-first
product built by many contributors at high velocity, not a small stable core.
The parts that matter for a speed-and-stability product are buried inside it.

## 2. Architecture, as observable from code

- `open-sse/` is the gateway engine: provider registry, executors, handlers,
  translators, services (combo, usage, compression), and an MCP server.
- `src/` is a Next.js app that serves both the dashboard UI and the admin/API
  routes. The same process handles management and data-plane traffic.
- Provider behavior is a mix of declarative registry entries and per-provider
  executor/translator code. The registry entry itself is small and
  data-like (base URL, auth style, default headers, model list, quirks).
- Routing: `routingStrategies.ts` implements a deterministic engine with
  blacklist/whitelist, cooldown/circuit breaker, cost priority, latency-aware
  ordering, and an explicit fallback chain, with a one-line `explain` per
  candidate. This is a good design worth borrowing.
- Speed scoring: `autoCombo/speedRanking.ts` ranks provider×model candidates by
  TTFT, tokens/sec, end-to-end latency, p95, breaker health, failure rate, and
  latency standard deviation, with tunable weights and a reason string. Missing
  telemetry is treated as the pool median (0.5), which avoids crushing new
  entries but also avoids giving them a free pass.
- Pricing: `models.dev` sync plus an OpenRouter catalog cache
  (`https://openrouter.ai/api/v1/models`, public, 24h TTL, stale-if-error).
- Benchmarks: Arena Elo sync from a third-party leaderboard API
  (`api.wulong.dev/arena-ai-leaderboards`) with a Hugging Face dataset fallback,
  normalized into a `model_intelligence` table with text/code categories.
- Usage: `usage_history` and `call_logs` record tokens (input, output, cache
  read/write, reasoning), latency, TTFT, status, provider, model, key, combo,
  and error summary.

## 3. Security and data-model observations

These are the parts to explicitly **not** copy:

1. `001_initial_schema.sql` stores provider credentials (`api_key`,
   `access_token`, `refresh_token`) and client API keys (`api_keys.key`) as
   plaintext text columns. For an internal gateway this is still a problem:
   database backups and support dumps leak every upstream key.
2. There is no first-class `users`/`tenant` table in the migration list. The
   product appears to be single-admin plus API keys, with permissions attached to
   keys. Your requirement (users with their own passwords, per-user budgets,
   admin reports) is a real extension, not a port.
3. 241 migrations over nine months indicate schema churn; a new project should
   design the model once and keep migrations boring.
4. A single process serving UI, management API, and data plane makes
   blast-radius and resource limits harder, which conflicts with your
   low-resource and stability goal.

**UNVERIFIED:** whether any later migration adds a users table under a
different name, and whether secrets are encrypted by a wrapper before insert.
The migration list contains no file named for users/tenants/roles; the initial
schema is plaintext. A deeper read would be needed before making a stronger
claim.

## 4. Reuse assessment

MIT permits copying and modification with the copyright notice preserved. The
practical limit is language and architecture: TypeScript/Next.js code does not
port to Rust line by line, and the valuable parts are mostly data and formulas.

| Area | Reuse | How |
| --- | --- | --- |
| Provider registry metadata (base URL, auth style, headers, model IDs, limits) | **High** | Convert the relevant entries to declarative TOML/JSON. Keep attribution. |
| OpenRouter pricing/catalog fetch (URL, fields, TTL, stale-if-error) | **High** | Re-implement in Rust; the schema and failure policy are the value. |
| models.dev transform and provider-ID mapping | **High** | Port the mapping table and field mapping; it encodes real-world naming drift. |
| Arena Elo ingestion and task-category mapping | **Medium-High** | Re-implement; reuse the category mapping and fallback-source idea. |
| Deterministic routing strategy engine | **Medium** | Port the rule set and `explain` contract, not the code. |
| Speed ranking formula and default weights | **Medium** | Port the formula; validate weights against your own traffic. |
| Combo execution/failover logic | **Low-Medium** | Read for failure taxonomy (429, quota, empty stream, truncated stream, stall). Do not port the 140-module structure. |
| Protocol translation (OpenAI↔Anthropic/Gemini) | **Low** | Only needed if you accept non-OpenAI providers; you scoped to OpenAI-compatible. |
| React/Next.js dashboard | **None** | You are using Angular. |
| Electron, MITM, compression engines, browser automation, MCP/A2A | **None for MVP** | Explicitly out of scope. |
| Tests and fixtures | **Medium** | Recorded upstream error/stream shapes are useful as test vectors. |

Practical estimate: **little or no direct code reuse for a Rust backend**;
roughly **60–70% of the provider/pricing metadata work can be shortcut** by
converting registry entries and mapping tables; **zero UI reuse**. The larger
benefit is a catalog of upstream failure modes and protocol quirks that would
otherwise take weeks to rediscover.

Attribution: keep `LICENSE` and a `THIRD_PARTY_NOTICES.md` entry that names
OmniRoute, the copied files, and the MIT notice, even for translated data.

## 5. Product lesson

OmniRoute's README sells "375 providers, 150+ free, 1,400+ models, compression,
MCP/A2A, desktop/PWA". Your stated differentiator is the inverse: few features,
fast, small, stable. That means:

- Do not inherit the provider count. Support OpenAI-compatible providers first;
  add bespoke adapters only when a paying use case demands it.
- Do not inherit the process model. One binary, one data plane, one admin plane,
  bounded memory, explicit timeouts.
- Do not inherit the schema churn. Model provider, credential, model, and
  provider-model offering as separate entities from day one.
- Do inherit the explainability idea: every routing decision should record why
  the chosen candidate won and why the others lost.

## 6. Open items

- Confirm whether OmniRoute encrypts secrets before persistence.
- Confirm the license of `models.dev` data and the Arena/HF dataset before
  shipping derived data.
- Decide whether any OmniRoute file will be copied verbatim or only translated;
  the notice differs.
