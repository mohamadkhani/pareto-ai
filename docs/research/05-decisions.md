# Decisions, open questions, next actions

## How decisions are recorded

- One numbered entry per **hard-to-reverse** choice (schema, protocol, security,
  licensing, product shape). Ordinary implementation details do not need one.
- Numbering is append-only: **never renumber and never rewrite an accepted
  entry**. To change course, add a new entry and mark the old one
  `Superseded by Dnn` in its heading.
- Every entry has: **Decision**, **Why**, **Consequence** (what becomes harder
  or impossible), and links to the spec, review, or commit it came from. Add a
  date for anything that can age.
- The PR that implements a decision links its number; the code comment or spec
  that depends on it cites the number too.
- Research and evidence live beside it in dated pages under `docs/research/`;
  the decision only summarises and points.
- Open work is **not** a decision: it belongs in GitHub Issues, linked from
  [`docs/HANDOFF.md`](../HANDOFF.md).

## Decisions taken

### D1. Build a new service; do not fork OmniRoute

**Decision:** new Rust backend, new Angular UI, OmniRoute used as reference and
as a source of provider/pricing metadata only.

**Why:** the stated goals (speed, low resources, stability) conflict with
OmniRoute's 29k-file Next.js/Electron/MITM/compression surface. MIT allows a
fork, but a fork inherits the shape and the maintenance burden.

**Consequence:** slower start on provider breadth, faster start on correctness.

### D2. Internal team gateway first

**Decision:** admin-created accounts, no public signup, no email flow.

**Consequence:** password reset is an admin action; no email infrastructure,
verification tokens, or anti-abuse signup machinery in v1.

### D3. OpenAI-compatible providers only in v1

**Decision:** one wire format in, one wire format out. Provider differences are
declared as data (base URL, auth, headers, quirks flags).

**Consequence:** Anthropic/Gemini native protocols are out of scope until an
offering can only be reached natively.

### D4. Price and quality are separate tables

**Decision:** `models` carry quality/benchmarks; `offerings` carry price, speed,
context, and reliability.

**Consequence:** the router can compare the same model across providers, and
reports can attribute spend per offering.

### D5. Measure speed ourselves; import quality

**Decision:** TTFT/TPS/failure rate from passive telemetry; Elo/benchmarks from
external sources with provenance.

**Consequence:** no dependency on a speed API, but no useful speed scores until
real traffic exists. Seed with a neutral score and a short warm-up.

### D6. Three combo strategies in v1

**Decision:** ordered fallback, weighted score, round-robin. Add cost-first as a
preset of weighted, not a separate engine.

**Consequence:** a small, testable routing core instead of a strategy zoo.

### D7. Reservations for budgets

**Decision:** estimate → reserve → settle, with a reconciler for stale rows.

**Consequence:** no check-then-spend race; slightly conservative enforcement.

### D8. Credential balance is a manual value in v1

**Decision:** operators set a manual balance on a credential; no provider API
sync yet. `balance_source` marks it as manual so a sync can be added later
without a schema change.

**Consequence:** balance is advisory (warn) in v1, becomes a hard gate when a
sync exists. See [07-scope-answers.md](07-scope-answers.md).

### D9. Organization limits apply on top of credential balance

**Decision:** per-user daily/weekly/monthly/total USD, per-user token caps, and a
cap on a single user's share of a credential's remaining credit.

**Consequence:** one employee cannot drain the shared pool; both gates are
checked at reservation time.

**Access-matrix note (2026-11-16, review nit):** `grants` are evaluated as
"allow-by-any, deny-by-any" across the model/offering/credential rows matching
the request; the subject verdict is `deny` if any matching row denies, `allow`
if any allows, else the owner-role default (admin allow, user deny). Effective
access = user verdict AND key verdict.

### D10. Only OpenRouter has a built-in price sync

**Decision:** OpenRouter syncs price and catalog through its public API. Any
other provider, including SailResearch, is entered manually with a manually set
price per offering. A declarative "describe your price API" feature comes later.

**Consequence:** unknown price is stored as `null` and scored as unknown, never
as zero.

### D11. Body capture policy — for a not-yet-implemented opt-in feature

**Status (2026-11-16, `d864224`): policy only, not implemented.** Body capture
does not exist: `request_logs` is metadata-only (`0004_usage.sql` reserves
`has_request_body` for correlation). This entry is deliberately kept: it is the
retention decision that applies when the opt-in capture feature lands.

**Decision (when capture lands):** request/response bodies are captured only
when explicitly enabled, kept for 24h, stored as compressed files outside the
database.

**Consequence (when capture lands):** the database never holds megabyte blobs;
retention becomes a setting later.

### D12. SQLite as system of record

**Decision:** SQLite (WAL) as the single database for v1, with daily rollup
tables for reporting. PostgreSQL is the migration target for multi-instance
deployments; ClickHouse only later as an analytics sink for raw logs.

**Consequence:** one binary, no second service. Rejected MongoDB (no
multi-document transactions on a single node, no foreign keys) and SurrealDB
(stability risk conflicting with the goal).

Comparison, evidence, and migration triggers: [06-database-choice.md](06-database-choice.md).

### D13. sqlx over sea-orm; utoipa for the API surface

**Decision:** data access through `sqlx` with hand-written SQL and checked-in
migrations. `utoipa` documents the HTTP surface and serves `/openapi.json`.

**Why:** the workload is a reservation ledger and time-series rollups, where
the SQL is the design. An ORM adds a code-generation step and an abstraction
over exactly the transactions we must control. The local cache has both crates,
but `utoipa-axum` is not cached, so the OpenAPI document is assembled manually.

**Consequence:** schema changes are plain SQL in `server/migrations/`. Every
handler keeps a `#[utoipa::path]` attribute in sync with the route table.

### D14. One binary that serves the UI

**Decision:** the Angular build is embedded (`rust-embed`) and served by the
gateway; a `--no-default-features` build produces an api-only binary.

**Measured (2026-11-16, `d864224`, release with `lto = "thin"`, `strip`):**
18.85 MiB (19,765,096 B) with the UI, 18.15 MiB (19,033,768 B) without — the
embedded UI costs 714 KiB. The UI is cheap, so the single binary is the default
artifact. Superseded: the first measurement here (7.1 MB / 6.7 MB / −455 KB)
was true 2026-11-16 at `2b57725` (M0.1); the M1–M5 dependencies grew the binary
about 3x, not the UI.

**Consequence:** `just build` builds the UI first (pnpm), then the binary. The
build fails with a clear message when the Angular output is missing.

### D15. PBKDF2-HMAC-SHA256 for passwords until Argon2 is available

**Decision:** password hashing uses PBKDF2-HMAC-SHA256, 600k iterations,
16-byte random salt, encoded as `pbkdf2$sha256$600000$<salt>$<hash>`.

**Why:** `argon2`, `password-hash`, `bcrypt`, and `scrypt` are not in the local
cargo cache and crates.io is unreachable (see
[09-environment.md](09-environment.md)). PBKDF2 from the cached `pbkdf2`,
`hmac`, and `sha2` crates is the standard-compliant option available.

**Consequence:** revisit when crates.io is reachable and migrate to Argon2id;
the stored format carries the algorithm id, so a re-hash on next login is a
clean migration.

### D16. AES-256-GCM via `ring` for credential encryption

**Decision:** provider secrets are encrypted with AES-256-GCM from the cached
`ring` crate, key from `GATEWAY_MASTER_KEY` (32 bytes, base64url or hex),
stored as `v1:<nonce>:<ciphertext>` with the row id as associated data.

**Why:** `aes-gcm`, `chacha20poly1305`, and `aead` are not in the local cache
([09-environment.md](09-environment.md)); `ring` is cached and battle-tested.

**Consequence:** startup fails when `GATEWAY_MASTER_KEY` is missing, so
credentials can never be written unencrypted.

### D17. Credential header values are encrypted like the secret

**Decision:** custom header values (credential and offering `headers_json`) are
sealed with the same AES-256-GCM `SecretBox`, AAD = row id. Header _names_ stay
plain so lists and audit rows can show them.

**Why:** review finding M1 (2026-11-16,
[../reviews/2026-11-16-m2-backend.md](../reviews/2026-11-16-m2-backend.md)):
`X-API-Key`-style headers are credential-equivalent material; storing them in
plaintext next to an AEAD-sealed bearer secret contradicted `AGENTS.md` and
D16's threat model (leaked DB file or backup).

**Consequence:** `headers_json` becomes a sealed envelope; the UI already treats
values as write-only (names only in responses), so no API change.

### D18. Sync overwrites synced fields; admins correct prices, not parameters

**Decision:** OpenRouter sync refreshes `context_length`, `max_output_tokens`,
modalities and capabilities on every run; only prices have a `manual` override
that sync never touches. Correcting a synced _parameter_ by hand is not
supported in M2 and is an explicit follow-up (needs a "manually edited" marker).

**Consequence:** recorded per review minor 5 (2026-11-16); operators must not
rely on hand-edited synced parameters surviving a sync.

### D19. Price sync run rows are pruned

**Decision:** `price_sync_runs.detail_json` keeps raw prices only for changed or
skipped models; rows older than 30 runs are deleted. (Review minor 6: unpruned
detail grows ~25-35 MB/year at daily syncs.)

### D20. PATCH semantics: null means "keep"

**Decision:** PATCH requests treat `null`/omitted fields as "leave unchanged";
clearing a nullable column back to "unknown" is not supported in M2 (an
all-null patch is `400`). Blank `secret` on a credential PATCH is `400`, not
"keep" (alignment finding B2, 2026-11-16) — the UI omits the field instead.

**Consequence:** recorded as a known API limitation (alignment finding B1);
supporting explicit clearing needs `Option<Option<T>>` inputs and is a
follow-up. Silent dropping of `provider_id`/`model_id` on offering PATCH
becomes an explicit `400` (finding B3).

### D21. pnpm for JavaScript tooling; `just` as the task runner

**Decision:** `web/` is managed with pnpm (lockfile `pnpm-lock.yaml`), and the
`Justfile` is the single entry point for builds and checks.

**Why:** user preference (2026-11-16) — the Angular scaffold had defaulted to
npm and was corrected. Also recorded in
[11-user-preferences.md](11-user-preferences.md).

**Consequence:** `scripts/check.sh`/`scripts/build.sh` were kept briefly as
thin `exec just ...` wrappers for in-flight callers and **removed on
2026-11-16** once none remained (the milestone log's historical entries still
name the old path).

### D22. Subset-aware pricing for OpenAI-style usage

**Decision:** in OpenAI-compatible `usage` payloads, `reasoning_tokens` is a
subset of `completion_tokens` and `cache_read_tokens` a subset of
`prompt_tokens`. Cost therefore bills the **non-overlapping** parts:

```
(input - cache_read) * input_per_m  +  cache_read * cache_read_per_m
(output - reasoning) * output_per_m +  reasoning * reasoning_per_m
```

clamped at zero per component; missing price components contribute 0 and flag
the row `priced=0` as before.

**Why:** review M3 finding (2026-11-16,
[../reviews/2026-11-16-m4-money-access.md](../reviews/2026-11-16-m4-money-access.md)):
the spec's five-term sum double-billed overlapping tokens and falsely flagged
rows when `reasoning_per_m` was NULL.

**Consequence:** `m4-access-money.md`'s cost formula is amended; providers that
report components **separately** (e.g. Anthropic cache creation) need a
normalization step when their wire format is supported (M-later).

### D23. Admin password reset revokes API keys

**Decision:** a user changing their own password keeps their API keys; an
**admin password reset** revokes all of that user's API keys and sessions.

**Why:** review M5 finding (2026-11-16): forced rotation existed only for
sessions, so an account rotated for suspected compromise kept full `/v1`
access through old keys.

### D24. Presets are pure single-component orders

**Decision:** `cheapest` = price order, `fastest` = speed order, `best` =
quality order — with **no penalties applied**; penalties exist only in
`weighted`/`balanced`. Health is enforced by the _filters_ (breaker, cooldown,
capability, budget) that run before any ordering.

**Why:** review M3 (2026-11-16,
[../reviews/2026-11-16-m5-routing.md](../reviews/2026-11-16-m5-routing.md)):
penalties were applied under every preset, so `cheapest` could pick a more
expensive offering — the documented contract was not guaranteed. A user who
explicitly asks for "cheapest" should get price order among _eligible_
candidates; health concerns are what the filters and the breaker are for.

**Consequence:** tests pin the pure orders and the filter boundary.

### D25. Quality lives on the benchmark scale; overrides are scale-checked

**Decision:** quality scores (benchmarks and `model_quality_overrides`) live on
one scale — the source's native Elo-like scale. `set_override` validates the
value against the imported benchmark range when benchmarks exist (a likely
scale error is rejected with a clear message), and tests use Elo-scale
fixtures consistently.

**Why:** review M4: an override entered as `0.99` against Elo values
(1351-1433) silently ranked last after min-max normalization.

### D26. Per-attempt telemetry; quota classes belong to the credential

**Decision:** every classified attempt persists a telemetry sample (attempt
outcome, latency, tokens) — a failed attempt rescued by failover must still
teach the router about the failing offering. `upstream_quota` (402) and
`upstream_rate_limited` (429) are attributed to the **credential**, not the
offering.

**Why:** review M5: the reliability component was blind to failover-recovered
failures and punished offerings behind frequently-limited credentials.

### D27. Telemetry warm-up: shrink toward 0.5 by sample count

**Decision:** a metric with fewer than 10 samples in its window is shrunk
toward the neutral 0.5 by `n / (n + 10)`; fully trusted at 10+ samples. This
implements the "short warm-up" D5 promised and stops one lucky sample from
defining an offering's speed and reliability.

**Consequence:** explanation text marks shrunk metrics like imputed ones.

### D28. Own UI primitives instead of a component library

**Decision:** the admin console uses a small in-repo design system
(`web/src/app/ui/`): design tokens plus ~12 primitives (button, icon-button,
field, input, textarea, select, switch, badge, card, table wrapper, dialog,
banner, tabs, empty-state), documented on a `/styleguide` route. No Angular
Material, no CDK, no third-party component library.

**Why:** the UI adds ~714 KiB to the single binary; a library adds its own
runtime and an opinionated theme. The console is form/table shaped, so a small
primitive set covers almost everything, and the design stays under our control.

**Consequence:** pages may not restyle primitives — only tokens and variants.
Every new primitive or state appears on `/styleguide` first. Use the native
`<dialog>` element for modals (focus trap and ESC for free).

### D29. Fonts: Exo 2 for display, Inter for body, JetBrains Mono for data

**Decision:** self-hosted woff2 via `@fontsource-variable` packages, latin
subset only. Exo 2 → brand/headings/nav/buttons; Inter → body and tables;
JetBrains Mono → identifiers, keys, JSON, logs. All behind tokens
(`--font-display`, `--font-body`, `--font-mono`) so a future Persian locale can
swap the body font (Exo 2 and Inter have no Arabic script; Vazirmatn is the
likely replacement, as OmniRoute used).

**Why:** user preference (2026-11-16). Exo 2's wide geometric letterforms are
hard to read in dense 13-14px tables; Inter is built for UI text.

### D30. Light and dark themes from one token set

**Decision:** one semantic token set with `html[data-theme="light|dark"]`
values, defaulting to the system preference, persisted in `localStorage`, with
a toggle in the shell. Components only reference tokens.

**Why:** admin consoles are used for long sessions; the cost is low once tokens
exist, and it forces the token discipline D28 needs.

### D31. Models is one page; providers load their own models

**Decision (user, 2026-11-16):**

- The admin console merges models, prices and quality into a single **Models**
  page. The Offerings and Benchmarks pages disappear; price sync and benchmark
  sync become toolbar actions with last-run status.
- The main table shows only important fields (model, provider, context, input/
  output/cache price, quality, status). Everything else opens in a detail
  dialog/expansion.
- A provider filter switches between per-provider rows and, with no filter, one
  row per model with **average and minimum price across providers** plus the
  model-level quality score.
- **Providers** get a "Load models" action: discover `GET {base_url}/models`
  with a credential (dry-run preview, idempotent upsert, never overwriting
  manual overrides).

**Benchmark sources** (network-tested 2026-11-16 from this host):

| Source                       | Endpoint                                                                   | Reachable        | License                          |
| ---------------------------- | -------------------------------------------------------------------------- | ---------------- | -------------------------------- |
| Arena (WebDev)               | `datasets-server.huggingface.co/rows?...lmarena-ai/leaderboard-dataset...` | yes (slow, ~20s) | CC BY 4.0 — attribution required |
| BenchmarkList capability     | `benchmarklist.com/directory-data/models.json`                             | yes (~11 MB)     | check before shipping; attribute |
| BenchLM                      | `benchlm.ai/_next/data/{build_id}/models.json`                             | yes              | check; brittle (build id)        |
| OpenRouter prices/benchmarks | `openrouter.ai/api/...`                                                    | **blocked**      | stays fixture-tested             |

`llm-leaders` (a local reference checkout outside this repository) is
**GPL-3.0**: use its README for the API contracts and matching ideas only —
reimplement independently, never copy its code into this project.

### D32. License: MIT

**Decision (user, 2026-11-16):** the project is licensed **MIT** — `LICENSE`
with the standard text, `license = "MIT"` in `server/Cargo.toml` and
`web/package.json`, third-party notices in `THIRD_PARTY_NOTICES.md`.

**Why:** after comparing MIT and Apache-2.0 directly (patent grant, NOTICE and
state-change duties, GPLv2 compatibility, simplicity), the user chose MIT:
simplest, most widely accepted, compatible with GPLv2 and GPLv3, and the
project can be relicensed later while a single copyright holder controls it.
Apache-2.0's explicit patent grant was not needed for an internal tool.

**Consequences:** fonts (OFL) and LMArena data (CC BY 4.0) keep their own
notices; a `cargo-deny` audit is still pending because crates.io is
unreachable.

### D33. Only Elo-scale quality drives routing; other scales are display-only

**Decision:** benchmark rows carry a `scale` (`elo`, `capability`, `percent`).
Only `scale = 'elo'` rows feed `quality_for_models` and override validation;
capability/percentage rows are stored with provenance, shown in the UI, and
explicitly labelled display-only until a calibration decision is made.

**Why:** mixing scales silently ranks a "0.87 capability" against an Elo 1400
(the M5 review's M4 finding). Calibrating one onto the other is a modelling
decision, not an implementation detail.

**Consequences:** with OpenRouter benchmarks blocked and BenchmarkList currently
publishing no capability scores, routing quality comes from **Arena (Elo)** and
manual overrides; BenchLM percentages are informational. Follow-up: confirm
BenchmarkList's current shape (it changed on 2026-11-16) or drop the source.

### D34. Mix price is the routing price metric; billing stays component-based

**Decision (user, 2026-11-16):** add token-mix profiles and a derived
**mix price** = blended $/M at the profile's token shares. Used as the price
component in routing and shown in the Models page; **never** used for billing
(`usage_events.cost_usd` keeps component pricing per D22).

Built-in presets (research: [12-token-mix.md](12-token-mix.md)):

| Preset                 | input | output | cache read | Use                                     |
| ---------------------- | ----: | -----: | ---------: | --------------------------------------- |
| `code-agent` (default) |   8.1 |    0.4 |       91.5 | long agent sessions with warm cache     |
| `code-agent-nocache`   |  99.6 |    0.4 |          0 | same work when prefixes do not survive  |
| `chat`                 |  74.0 |   26.0 |          0 | short assistants, one-shot tasks        |
| `long-context`         |  97.0 |    3.0 |          0 | document analysis / batch summarisation |

Rules:

- A provider without cache pricing bills the cached share at the **input**
  price (never drops it) — otherwise it looks ~5x cheaper than its bill.
- `cache_write` and `reasoning` shares are part of the profile schema but 0 by
  default; when a price exists for them they are blended in (they fall outside
  the research's three-component model).
- The mix is a **client property**: re-picking an offering every turn silently
  turns `code-agent` into `code-agent-nocache` (5x price difference on the
  reference model). Session/cache affinity therefore matters more once mix
  pricing drives routing.
- Calibration follow-up: derive the deployment's measured mix from our own
  `usage_events` and offer it as a preset.

### D35. Rule-based combos: constraints over the whole catalog

**Decision (user, 2026-11-16):** a combo may use **pool = all models** instead of
an explicit member list. The pool is every eligible offering that satisfies the
combo's constraints — `min_quality` (score) and/or `max_mix_price_per_m`
(mix price under the combo's profile) — then ordered by the **same weighted
score** as `weighted` combos (price/speed/quality/reliability + penalties).

Rules:

- At least one constraint is required (an unconstrained "everything" combo is
  almost always a mistake); a constraint set to null is "not constrained".
- An offering with an **unknown** mix price is excluded when
  `max_mix_price_per_m` is set — an unverifiable price must not pass a price
  cap. It is shown in the preview as `excluded: unpriced`.
- Boundaries are inclusive (`>= min_quality`, `<= max price`); the score used is
  the same resolved quality as routing (override > Elo benchmark > default).
- Strategy for pool combos is `weighted` (ordered/round-robin need an explicit
  list by definition); document the restriction in the API error.
- The pool is resolved per request (after grants/budget/breaker filters), so
  access control and budgets still apply.
- A **preview** endpoint returns the ranked candidates and the exclusions with
  one-line reasons, computed by the _same_ engine the runtime uses — the UI's
  "which models this combo prioritizes" view must never drift from reality.

**Why:** the user wants a combo like "best value coding models, quality >= X,
mix price <= $Y/M", ordered by their weights, without maintaining a list. The
preview makes the rule auditable before the operator saves it.

### Stickiness (same decision, added 2026-11-16)

A combo may pin its chosen offering per **session** so prompt caching survives
across turns (research: per-turn re-picking turns `code-agent` into
`code-agent-nocache`, up to ~5x cost).

- Config: `stickiness {enabled, ttl_seconds (default 1800), key_source:
"header"|"auto"}`; when enabled the combo keeps the choice keyed by
  `X-Session-Id` when present, otherwise by a stable hash of
  `(user or api key, system prompt + first user message)`.
- State: `combo_sticky_choices(combo_id, session_key, offering_id, chosen_at,
expires_at)` with `(combo_id, session_key)` as the key and TTL pruning.
- On each request: a valid pin is used only if the offering is still eligible
  (enabled, access, budget, breaker); otherwise the pool is re-scored and the
  pin is replaced. A failed pinned attempt may fail over, and the pin then moves
  to the offering that served the request.
- Visible in `decision_json` (`sticky: {enabled, key_source, pinned, age_s,
replaced_why}`) and in the combo preview (the current pin per example key).
- Applies to both combo pools; it is a no-op for `ordered` when the pinned
  member is still first.

### D36. SQLite runs WAL + `synchronous=NORMAL`

**Decision:** the pool sets `journal_mode=WAL` and `synchronous=NORMAL` (it had
silently defaulted to `FULL`). Measured on 2026-11-16
([13-performance.md](13-performance.md)): p50 2.09 ms → 0.93 ms and ~454 → ~959
req/s at concurrency 1; settle p50 0.742 → 0.304 ms; 200 concurrent writers
~2.1k settlements/s; WAL plateaus ~4.1 MiB over 50k settlements.

**Trade-off:** NORMAL survives a process crash; a **machine power loss** can
lose the newest usage commits (billing under-counts), never corruption and never
a double charge (settlement is idempotent). Accepted for an internal gateway;
flip the one line back to `FULL` (~2.2x slower) if that changes.

**Why SQLite is not our bottleneck:** 22 statements / 8 writes / 3 transactions
per request, and the practical ceiling is ~700 req/s against a documented load
of ~0.17 req/s — far from the Postgres triggers. The remaining cost is the
three write transactions, not the engine. OmniRoute's slowness is not evidence
against SQLite here: it runs the data plane inside Next.js with synchronous
SQLite on the hot path.

**Follow-ups (measured, ranked):** cache mix-profile resolution (3 SELECTs per
request), stop reading the price twice, batch `last_used_at`, resolve candidate
credentials in one query.

### D37. A published variable price is unknown, never a number

**Decision (2026-11-17, live finding):** OpenRouter publishes
`pricing.prompt`/`pricing.completion` as the string `"-1"` for models whose
price it cannot state (`openrouter/auto`, `nvidia/switchyard`, six more live
ids). `-1` is a **sentinel**, not USD per token.

- The sync maps every negative component to `NULL` and sets
  `prices.variable_price = 1`; `-1` never reaches a price column, an
  aggregate, the mix blend, or the routing price score. `0012_variable_price`
  repairs rows written before the fix (the live DB held `-$1,000,000/M` and
  ranked those models cheapest).
- An all-variable offering is unpriced: routing imputes the neutral 0.5 price
  score and marks it `imputed`; a combo price cap excludes it (`unpriced`),
  never passes it. A partial sentinel keeps the components that are real
  numbers.
- The UI shows a `variable` badge with a tooltip instead of a number, and
  sortable columns keep missing prices last in both directions, so a variable
  price can never appear at the cheap end.
- A manual price clears the flag: the operator has stated a number.

**Rejected:** storing `-1` as a very small price (it ranked cheapest); storing
0 (unknown is never 0, D10); dropping the model (it is routable, just not
price-comparable).

### D38. A benchmark row attaches to every model of its normalized identity

**Decision (2026-11-18, live matching finding):** a published benchmark score
is **model-level**, so one source entry must attach to every catalog model that
denotes the same model. Live data proves the need: BenchLM/Arena publish
`deepseek-v4.1-flash`, the catalog holds both
`deepseek-ai/DeepSeek-V4.1-Flash-1017` (SailResearch) and
`deepseek/deepseek-v4.1-flash` (OpenRouter), and the old exact-first matcher
could not bridge them: a four-digit build suffix (`-1017`) was not stripped,
and even when two spellings normalized alike the old matcher reported
`ambiguous` instead of attaching the score to both.

- Matching normalizes to a **tail id** (lower case, punctuation collapsed, path
  vendor dropped), then to an **identity**: trailing tier markers
  (`-latest`, `-preview`, `-thinking`, `-nothinking`, `-xhigh|high|medium|low`),
  date/build stamps (`-1017`, `-0731`, `-20250807`, `-2025-08-07`) and a leading
  hyphenated vendor token are removed. Vendor spellings are aligned for
  matching only (`deepseek-ai`↔`deepseek`, `z-ai`↔`zai-org`, `x-ai`↔`xai`,
  `moonshotai`↔`moonshot`, `google-ai`↔`google`); stored names are never
  rewritten. `:free`/`:batch` remain distinct offerings.
- A hint resolves, in order, to: the exact full id; every model sharing its
  identity; every model sharing its vendor-stripped identity; or — only when
  exactly one distinct identity is a hyphen-boundary suffix — that identity.
  Several distinct candidates are `ambiguous` and are never written.
- Consequences, accepted deliberately: two providers of one model both get the
  row (`benchmarks` keeps `UNIQUE(model_id, source, category)`, so this is one
  row per model); two date-stamped variants of one name share the row; the
  counts distinguish `matched_models` from `matched_hints`.
- A generic tail (`flash`, `pro`, `auto`, `mini`, …) is skipped unless a full id
  matches exactly, so a benchmark row named `flash` cannot land on whichever
  model happens to carry that word.
- Rejected: preferring the OpenRouter variant (the score is not provider
  specific); picking the "best" of several candidates (a guess); matching
  `:free` rows onto the paid model (they are separate offerings).

### D39. Argon2id passwords, hashed off the async runtime

**Decision (2026-11-17):** new password hashes are Argon2id with `m = 64 MiB`,
`t = 3`, `p = 4`, stored as the standard PHC string
(`$argon2id$v=19$m=65536,t=3,p=4$...`). PBKDF2 rows from D15 are still
verified, and a successful login re-hashes them with Argon2id
(`password::needs_rehash`). Every derivation on a request path runs in
`tokio::task::spawn_blocking`.

**Why:** D15's revisit clause fired — crates.io is reachable, and Argon2id is
the memory-hard standard. Measured on this machine (release): Argon2id hash
111.9 ms vs PBKDF2 600k at 317.9 ms, and end-to-end login 112.7 ms vs 328 ms
([13-performance.md](13-performance.md#13-password-hashing-and-login-latency-d39-2026-11-17)).
Unoptimized, Argon2id is ~2 s per hash, which is why the runtime must not be
blocked; a test pins that a slow hash leaves a concurrent request responsive.

**Consequence:** the stored format carries the algorithm id, so both formats
coexist without a migration; the one-time PBKDF2 upgrade costs ~428 ms on the
next login. Parameters live in `password::Argon2Cost`; changing them flags
every row for transparent rehash on login. `GATEWAY_PASSWORD_ITERATIONS` was
not added: the Argon2 path is already below the 150 ms target.

### D40. Zero-configuration start; setup / unprotected / protected

**Decision (2026-11-17):** with no environment at all the gateway listens on
`127.0.0.1:8081`, uses `./data`, generates a credential master key on first
start (`<data>/master.key`, mode `0600`, reused; `GATEWAY_MASTER_KEY` still
wins), and starts in **setup** mode instead of failing. The mode machine:

| Mode          | Condition                            | Behaviour                                                               |
| ------------- | ------------------------------------ | ----------------------------------------------------------------------- |
| `setup`       | no user, no recorded skip            | only `/health`, `/openapi.json`, `/api/setup/*`, and `/api/auth/*`      |
| `unprotected` | no user, skip recorded in `settings` | `/api/*` and `/v1/*` act as a trusted local operator; sessions stay off |
| `protected`   | at least one user                    | the normal session + CSRF rules                                         |

`GATEWAY_ADMIN_EMAIL` + `GATEWAY_ADMIN_PASSWORD` remain a non-interactive
shortcut; half a configuration fails startup. The wizard's first admin is
role admin with `must_change_password = false` (the operator chose it).

**Exposure rule:** a non-loopback bind while in `setup` or `unprotected`
refuses to start, naming `GATEWAY_ALLOW_PUBLIC_NO_AUTH=1` as the override; with
the flag it starts and the console shows a persistent red banner. Loopback
peers and desktop-wrapper origins (`tauri://localhost`,
`https://tauri.localhost`, `wails://...`) count as local for the wizard and the
banner. Setup mutations (`/api/setup/admin`, `/api/setup/skip`) are loopback
(or wrapper) only; `/api/setup/status` is readable from anywhere because the
console needs it to render the warning.

**Why:** the user asked that `just dev` work with no preparation, and that an
accidental network exposure be loud rather than silent. Refusing the start is
the only way to guarantee no route is reachable without a password before the
operator has made a decision.

**Consequence:** skipping is recorded in `settings` (`setup.skipped`) so a
restart does not silently re-open the wizard. With the override flag the
wizard is still loopback-only, so a remote operator finishes setup locally or
with the `GATEWAY_ADMIN_*` shortcut. Setup mutations also require a same-origin
(or absent) `Origin`, because a browser can reach a loopback gateway from any
page; they are not CSRF-guarded (no session exists yet). TLS was a separate
task at the time; D54 now adds automatic certificates for a public domain, and
the status reports the full `tls` shape.

### D41. Provider price mapping is user-authored; no mapping means unpriced

**Decision (user, 2026-11-18):** prices found in a provider's `GET /models`
payload are ingested only through an explicit, per-provider **mapping** the
operator writes (or a template they pick). Without a mapping the provider's
models stay **unpriced** — we never guess a shape or silently choose one tier
out of `asap/balanced/flex`.

**Why:** the OpenAI `/models` schema has no price fields; OpenRouter,
SailResearch, Together, DeepInfra and others each use a different shape. Hard
coding them does not scale and mispricing is worse than "unknown".

**Consequence:** `source='provider_api'` rows exist only for mapped providers;
manual always wins. Tiered payloads require choosing the tier in the mapping,
which becomes the billed price for that provider. Full spec:
[../specs/price-mapping.md](../specs/price-mapping.md).

### D42. Provider connectors: one declarative profile for prices, balance and health

**Decision (user, 2026-11-18):** the price mapping (D41) is the first of a
small family of per-provider **connectors** configured by the operator — same
engine, same click-to-pick UI:

- **catalog/prices** — `GET {base}/models` → prices (D41).
- **balance** — a provider-specific URL (e.g. SailResearch
  `/v2/usage/summary?range=30d`) + headers + pointers (`/balance`,
  `/available`, `/period_spend`, `/burn_rate`, `/days_remaining`), stored **per
  credential** in `balance_usd`/`balance_source='provider_api'`, refreshed on a
  schedule (default 15 min) with backoff.
- **health** — the endpoint used by the credential "Test" action.

Rules: URL + headers are editable per provider; headers may reference the
credential secret and are encrypted like other header values (D17); only
`http(s)`; block link-local/metadata IPs unless explicitly allowed; timeouts and
a small response cap; never log or store response bodies. A connector failure
never affects prices or routing. Balance is **advisory by default (warn)**;
hard gating is an opt-in per credential, because a provider balance can include
other products (SailResearch shares it with sailboxes) and can be stale.
`balance_unavailable`/`available: false` shows "unavailable", never `0`.

**Balance policy (user, 2026-11-18):** warn by default — show the balance and a
low-balance / “runs out in N days” warning, but keep routing unchanged. A hard
gate is **opt-in per credential** (`balance_gate: warn|block`), for operators
who trust the number; otherwise a stale or product-shared balance would refuse
requests for no good reason.

**Why:** every provider differs in path, parameters, auth headers and payload
shape; hard-coding them does not scale and silent fallbacks misprice.

### D43. Desktop builds via Tauri v2, with the gateway as a sidecar

**Decision (user, 2026-11-18):** ship Linux, Windows and macOS desktop builds as
a Tauri v2 app (stable `2.x`, not the 3 alpha). The app is a thin shell: it
picks a free loopback port, starts the **unchanged gateway binary as a
sidecar** with a per-app data directory, waits for `/health`, then opens a
window at `http://127.0.0.1:<port>`; it kills the sidecar on exit. No custom
webview assets and no duplicated UI build — the gateway keeps serving its
embedded console.

**Consequences:**

- Desktop data lives in the OS app-data dir; the gateway's zero-config start
  (D40) generates the master key there, so the first run shows the setup
  wizard. Tauri origins count as local, so no public-exposure banner.
- macOS and Windows artifacts must be built on their own runners (signing and
  notarization included); only Linux can be built on this host. CI provides the
  three-way matrix and uploads artifacts.
- Signing keys stay out of the repo: unsigned builds work, signed builds read
  secrets from CI.

### D44. Price units are part of the connector, with a live conversion preview

**Decision (user, 2026-11-18):** the operator declares how a provider's numbers
are expressed, instead of us assuming USD-per-million:

- `unit`: `per_million_tokens` | `per_token` | `custom`
- `multiplier`: applied to the extracted number (e.g. `0.01` for a provider that
  quotes cents)
- `currency`: ISO code; non-USD values are displayed as-is and only feed
  routing when an optional static `fx_to_usd` is supplied
- `display`: symbol/decimals for the table (e.g. `¢` with 4 decimals)

The mapping **preview** must show both the raw extracted value and the converted
USD-per-million value for the first entries, and **warn** when a converted input
price is implausibly low (<$0.001/M) or high (>$1000/M) — that is how a wrong
unit gets caught before saving.

**Why:** providers quote prices in different units (USD/token strings, USD per
million, cents), and guessing wrong misprices by 100x. Only the operator can
confirm the invoice.

### D45. The desktop service lives in the system tray

**Decision (user, 2026-11-18):** the desktop shell keeps the gateway running
in the background after the window is closed. Closing the window hides it and
leaves the tray icon active; the only user-facing way to stop the service is
"Quit Pareto AI" in the tray menu, which kills the sidecar before exiting.
The tray menu also offers "Open Pareto AI" to restore the window; a second
launch keeps focusing the running instance.

**Why:** the gateway is a long-running local service (routing, budgets,
telemetry); killing it because the user closed the window would break API
clients and scheduled work. An explicit quit keeps "stop the service"
deliberate.

### D46. The desktop gateway uses a fixed port, never a random one

**Decision (user, 2026-11-18):** the desktop shell must not open a random
port. The gateway listens on the fixed loopback address `127.0.0.1:8081` — the
same default as the headless server — so API clients and the operator always
know where the service is. Amends D43's startup sequence, which picked a free
port per launch.

**Consequences:** the desktop's gateway and the `pareto-gateway.service` daemon
share the port, so only one of them may run at a time (documented in
`docs/desktop.md`). If the port is taken by anything else, startup fails with
the log-tail error dialog instead of silently moving.

### D47. AUR package names: pareto-ai for the server, pareto-ai-desktop for the desktop

**Decision (user, 2026-11-18):** the server family is `pareto-ai` /
`pareto-ai-bin` and the desktop family is `pareto-ai-desktop` /
`pareto-ai-desktop-bin`; the release tarballs follow the package names. The
first naming (`pareto-gateway*` for the server, `pareto-ai*` for the desktop)
was dropped within the day; the old AUR packages are deleted rather than
migrated. Binary names (`pareto-gateway`, `pareto-desktop`) and the daemon
unit (`pareto-gateway.service`) are unchanged.

### D48. Drawers for sub-resource work, bottom sheets on phones

**Decision (user, 2026-11-18):** content that belongs to one selected record —
provider credentials, model discovery, connector editing — opens in a
right-edge drawer on desktop instead of expanding inline in a list. A drawer is
a modal task surface: the list stays visible as context, the task gets the full
height, and the panel slides in from the right. Below 40rem the same
`variant="drawer"` becomes a bottom sheet
(full width, about 92dvh, rounded top edge, safe-area padding). One variant
keeps pages from branching on the viewport.

Drawers are not the default for every dialog. Short standalone forms (new
provider, new key) and confirmations stay centred modals; confirmations sit
above a drawer when they belong to it. Content that is too wide for a drawer
(e.g. mix profiles and combo previews) keeps its wide centred modal. A
`size="lg"` drawer (56rem) exists for editors that show a sample or preview.

Creation and editing stay inside the drawer that owns the list (the
credentials drawer swaps between list and form) instead of stacking a second
dialog. Compact inline controls use `size="sm"` so selects and inputs match
`ui-button size="sm"` at 32px.

**Consequences:** `UiDialog` gains the sheet behavior and the `size` input, and
both appear on `/styleguide` (D28). The user drawer inherits the 34rem width
and the phone sheet. Inline expandable panels are no longer used for provider
credentials.

### D49. Upstream errors are captured for the operator, never for the caller

**Decision (user, 2026-11-19):** every failed attempt records an **error
detail** — the provider's `error.message`, its own `type`/`code`, its request
id, and the HTTP status — for all three failure paths: a non-2xx response, an
error event inside a stream, and a transport/timeout failure (which has no
provider text and records the transport message instead). The detail is stored
for **every attempt**, including attempts that failover later recovered.

Detail is bounded and redacted: message ≤512 characters (2 KiB is not used),
`type`/`code` ≤64, provider request id ≤128, and a non-JSON body falls back to
its first 512 characters. The credential secret, every custom header value, and
common key shapes (`sk-…`, `Bearer …`) are removed before storage; a value that
still looks like a secret is dropped entirely. No request or response body and
no headers are stored. Capture is always on, with no setting.

Storage: `telemetry_attempts` gains `upstream_status`, `provider_error_code`
and `error_detail`; `request_logs` gains `error_detail` for the final failure of
a request. `telemetry_attempts` keeps its existing 200-row-per-offering
retention, so per-attempt detail is a recent window; `request_logs` is the
durable record. `GET /api/logs` returns the final error class and detail and
gains `status`, `error_class`, `provider` and `offering` filters; a new
`GET /api/logs/{request_id}/attempts` returns every attempt with its detail.
The console shows this in `/logs` (row expansion) and in a 24-hour
provider-grouped error card on the dashboard.

The caller-facing message is unchanged: `/v1` keeps the fixed, secret-free
strings from `failure_from_class`. This entry is an explicit exception to D11:
a bounded error excerpt is metadata, not body capture.

**Why:** today the provider's response body is dropped at the source
(`server/src/upstream/mod.rs:356-363`) and only an error class survives, so an
operator cannot tell a bad model id from an exhausted account or a provider
outage. Exposing the same text to `/v1` callers would leak account and model
detail, which `error.rs` and the M3 contract deliberately avoid.

**Consequence:** a provider that echoes the API key in its error message cannot
leak it into the database; a very long provider error is truncated. Error text
in `telemetry_attempts` disappears after the 200-row window, which is accepted.

### D50. Breaker state, trips and manual reset are operator-visible

**Decision (user, 2026-11-19):** the in-memory breaker snapshot is exposed in
the console under `/providers`, grouped by credential with its offerings
nested, listing **every** pair that has state (closed and open) with its
consecutive-failure count, state, and the error class plus bounded, redacted
error detail of its last failure. Open pairs show `open_until`. A
`breaker_trips` table records each trip with provider, credential, offering,
time, consecutive-failure count, last error class and the same bounded detail;
rows older than 30 days are pruned on insert. The snapshot comes from memory and
resets on restart; the trip history survives it. An admin endpoint resets one
pair to closed, writes an audit entry, and is session-guarded with CSRF like
other admin mutations.

**Why:** after a trip, `decision_json` is the only trace
(`skipped_breaker_open`); an operator cannot see which pair is open, why, or
since when, and a restart erases the evidence. Trip rows are rare (one per five
consecutive failures), so persistence is cheap.

**Consequence:** a trip is recorded even when the process later restarts;
resetting a pair is an audited admin action, not a hidden operator trick.

### D51. Dashboard cards: one primary number, opt-in hover, small real charts

**Decision (user, 2026-11-19):** each dashboard card follows one skeleton —
icon, linked title, one primary number, supporting rows, and a pinned footer
status line — in a deterministic grid (4 columns at 1440px and above, 2 at
800–1439px, 1 below 800px) with equal heights. Cards opt into
`[interactive]="true"`; hover and focus-within strengthen the border with a new
`--border-hover` token and `--shadow-sm`, while only the title link navigates
(no whole-card click, so nested buttons stay honest).

Spend gets a seven-day priced-spend sparkline and Requests a stacked outcome
bar (priced, partial, unpriced, refused), both with text alternatives and no
per-point tooltip. A non-dismissible attention strip above the cards lists
credential problems and stale price/benchmark syncs when they exist. Empty
states inside cards use the new compact `ui-empty` variant; loading uses
skeletons. Recent activity keeps the dense table on desktop, capped at eight
rows, and a two-line list on phones.

**Consequences:** `UiCard`, `UiEmpty`, and the new `UiSparkline` gain variants
shown on `/styleguide` (D28). Unpriced spend is never drawn as zero. The
initial bundle warning budget rises to 850 kB for the richer dashboard; the
page stays a lazy chunk.

### D52. Balance units and provider presets

**Decision (user, 2026-11-19):** the connector unit rule (D44) applies to the
**balance** too, and providers are created from a **preset** instead of a
free-form "OpenAI-compatible" entry.

- `BalanceConfig.unit` mirrors the catalog unit without a token base:
  `multiplier` (default 1), `currency`, `fx_to_usd`, `display`, `note`. The
  multiplier applies to `balance`, `period_spend`, and `burn_rate`;
  `days_remaining` is a count and is never scaled.
- `BalanceConfig.subtract` (optional pointer) makes
  `balance = balance_pointer − subtract_pointer` **before** the multiplier.
  OpenRouter needs it: `/api/v1/credits` publishes total credits and lifetime
  usage, so remaining = `total_credits − total_usage`.
- Preview and refresh return `raw` (after subtract), `converted` (after the
  multiplier), and warnings for a non-positive multiplier, a negative or
  absurdly large converted balance, and a non-USD currency without FX.
- A non-USD balance without `fx_to_usd` is stored with its currency and is
  **ignored by routing and the balance gate**, exactly like a non-USD price.
- `providers.preset` is an additive nullable column (`openrouter`,
  `sailresearch`, `generic`); `kind` stays the wire protocol. The registry is
  code, and it reuses the connector templates so pointers and units have one
  source of truth.
- Creating with a preset seeds its catalog/balance/health connectors in the
  same transaction, enabled, with `updated_by = NULL`. An explicit
  base URL or connector document wins over the preset.
- `POST /api/providers/{id}/apply-preset` (admin, CSRF, audited) re-applies the
  defaults. A connector the operator saved (`updated_by` set) is kept unless
  `force: true`; the provider's base URL is never changed, so a mock/proxy
  override survives. The view reports one action per kind
  (`created`, `updated`, `overwritten`, `unchanged`, `skipped`).

SailResearch's balance template uses multiplier `0.01` with the note "quoted
in cents — verify against an invoice", matching its catalog prices.
OpenRouter's uses multiplier 1 and the subtract pointer.

**Why:** a balance quoted in cents was being read as dollars (100× too small),
while the same provider's prices were converted correctly; and configuring a
known provider by hand was error-prone. Presets are code so they stay in sync
with the templates and need no migration to tune a pointer.

**Consequences:** `providers` gains one indexed nullable column; connector
rows seeded by a preset carry `updated_by = NULL` as the "unmodified" marker,
so operator edits are never silently replaced. The UI calls the surface
**Provider setup** (tabs Prices / Balance / Health) and links to it with
`/providers?setup=<id>&tab=prices|balance|health`; API and DB names stay
`connectors/*`.

### D53. The console is styled with indented Sass, without a formatter

**Decision (user, 2026-11-19):** every stylesheet under `web/src` is indented
Sass (`.sass`), not plain CSS and not SCSS. `web/src/styles.sass` is the global
entry and `@use`s six partials in load order: `_fonts.sass`, `_tokens.sass`,
`_themes.sass`, `_base.sass`, `_utilities.sass`, `_components.sass`. Each
component points its `styleUrl`/`styleUrls` at its own `.sass` file, primitives
included. `sass` is a devDependency; Angular compiles the indented syntax once
it is installed.

**Why:** the user prefers the indentation-based syntax over CSS/SCSS braces,
and `@use` replaces the old `@import` entry list with an explicit, ordered
module graph. The converter output was checked against the previous production
CSS: the emitted global bundle is semantically identical (329 rules and
declarations, same order; only Sass's harmless `@charset "UTF-8"`, single- to
double-quote normalisation, and the removal of `calc()` inside `min()` differ).

**Consequences:** Prettier cannot parse indented Sass, so `just ui-fmt`
(`prettier --check src/`) skips these files and no auto-formatter exists; the
Angular build is the syntax check, and `src/app/style-extension.spec.ts` fails
if any `.css` file reappears under `web/src` (the allowlist is empty). Declaration
order is load order, so the `@use` list must stay fonts → tokens → themes →
base → utilities → components. Note: the requested decision number "D50" was
already taken by breaker visibility, so the styles decision is recorded as D53.

### D54. Automatic TLS for a public domain, off everywhere else

**Decision (user, 2026-11-19):** when the gateway is bound to a public interface
and `GATEWAY_PUBLIC_DOMAIN` is set, it obtains and renews a Let's Encrypt
certificate itself. The mode is chosen at startup by a pure decision:

| Bind | `GATEWAY_TLS` | Domain | Result | `tls.reason` |
| --- | --- | --- | --- | --- |
| loopback, RFC 1918, or link-local | any | any | plain HTTP | `local` |
| public | `off` | any | plain HTTP | `off` |
| public | `auto` | unset | plain HTTP | `no_domain` |
| public | `auto` | set | HTTPS on `GATEWAY_HTTPS_PORT`, HTTP→HTTPS on port 80 | `null` (`pending` until issued) |

Configuration: `GATEWAY_TLS=auto|off` (default `auto`), `GATEWAY_PUBLIC_DOMAIN`,
`GATEWAY_ACME_EMAIL`, `GATEWAY_HTTPS_PORT` (default 443), and
`GATEWAY_ACME_DIRECTORY` (default Let's Encrypt production; staging is
documented). The certificate store is `<data>/tls/` with `account.json`,
`fullchain.pem`, and `privkey.pem`; the two private files are mode `0600` and
writes are atomic renames.

Issuance uses **HTTP-01 on port 80** through a small listener that answers
`/.well-known/acme-challenge/*` and, once a certificate is active, `308`-redirects
every other request to HTTPS. It runs in a background task and never blocks
startup: the plain-HTTP listener on `GATEWAY_LISTEN` starts first and keeps
serving. A failure only sets `tls.error` and retries hourly. Renewal is scheduled
30 days before expiry; the new certificate is hot-swapped into the rustls
configuration that each new connection reads, so nothing restarts. HTTPS
responses add `Strict-Transport-Security: max-age=31536000`, and session cookies
are `Secure` by default (unchanged `GATEWAY_INSECURE_COOKIES=1` escape hatch).

The decision sits behind a `CertificateProvider` trait, so unit tests exercise
issuance success and failure with a fake and do not touch the network.

**Why:** a public gateway carrying API keys and admin sessions must not run on
plain HTTP, and asking the operator to terminate TLS in a separate proxy is a
setup step that gets skipped or misconfigured. Let's Encrypt over HTTP-01 needs
nothing but DNS and ports 80/443, which a gateway host already has.

**Consequences:** ports 80/443 are now used when TLS is active; binding 80 may
need privilege, and a firewall that blocks port 80 makes issuance fail (the
gateway still runs). No TLS is attempted on private/loopback binds, so the
desktop sidecar and `just dev` are unchanged (D43). A reverse proxy in front of
the gateway is still supported: it should talk to `GATEWAY_LISTEN`, and the
operator can set `GATEWAY_TLS=off` if the proxy owns certificates. Live issuance
cannot be verified in this repository's test environment (no public domain and
no ports 80/443); the decision matrix, challenge endpoint, redirect handler,
store, renewal math, status shape, and fake-provider outcomes are covered by
tests, and the README documents the live procedure and the staging directory.

### D55. Trunk-based development with short-lived branches and PRs

**Decision (user, 2026-11-19):** `main` is always releasable. Every change lands
through a short-lived branch (`feat/…`, `fix/…`, `chore/…`), a pull request, and
a green `CI` run before a squash merge. Documentation-only changes may go
straight to `main`. Releases are tags (`vX.Y.Z`) on `main`; hotfixes branch from
the tag. `main` is protected: no force-push, CI required.

**Why:** the repository is public, CI already runs on pull requests, and desktop
/ AUR / release workflows trigger from tags. Direct pushes to `main` bypass the
only check that exists.

**Consequence:** process rules live in [`CONTRIBUTING.md`](../../CONTRIBUTING.md)
for humans; [`AGENTS.md`](../../AGENTS.md) keeps the engineering rules and points
there, without assistant-specific phrasing. GitHub is the canonical remote (it
hosts CI and releases); Gitea remains a mirror.

### D56. A forgotten admin password is reset by a local operator command

**Decision (2026-11-20):** `gateway reset-password [--email <address>]
[--data-dir <path>]` resets one account's password directly against the SQLite
file. It reads the new password from a hidden terminal prompt (or one line on
`stdin` when it is not a terminal), never from an argument or environment
variable. In one transaction it deletes the account's sessions, revokes its
live API keys, flags forced rotation, and writes an `auth.password_reset_cli`
audit row with a `NULL` actor.

**Why:** with no public signup and no email delivery (D2), the last admin
forgetting the password is the remaining hard lockout. Filesystem access to the
data directory already implies control of `gateway.db` and `master.key`, so a
local command adds no trust boundary, while any remote recovery path would.
Reusing the admin-reset rules (D23) keeps revocation consistent. Alternatives
rejected: a loopback HTTP reset endpoint (any local process or web page could
reach it), extending `GATEWAY_ADMIN_PASSWORD` to update existing accounts
(persists in service configuration and would reset on restart), a
recovery-token file (same trust as the CLI with more moving parts), and deleting
the user row (loses audit attribution and account data).

**Consequence:** recovery requires filesystem access on the host and the binary
from the same release, because opening the database applies migrations. The
command resolves only `GATEWAY_DATA_DIR` (or `--data-dir`) and runs before the
rest of the configuration is parsed, so a malformed timezone, timeout, or master
key cannot block recovery. The desktop app's data directory must be passed
explicitly. The command does not reactivate a deactivated account; that still
needs an admin. Its audit row has a `NULL` actor and `insecure = 0`, because the
target is always a real account (so the database resolves to `protected`) and the
offline process cannot know the live mode; `detail.source = "cli"` identifies the
row.

**Source:** [PR #11](https://github.com/mohamadkhani/pareto-ai/pull/11),
introduced by [`4a1782f`](https://github.com/mohamadkhani/pareto-ai/commit/4a1782f6ea0d3e77d54e84b5a03b15ecead118aa).

### D57. Setup closes at the first active administrator, and the last one cannot be removed

**Decision (2026-11-20):** while the process is not already `protected`, the
first-admin/setup path is refused when an **active real administrator** exists
(`is_placeholder = 0`, `role = 'admin'`, `is_active = 1`), not when any account
exists; a process already in `protected` still refuses every setup mutation.
The administrator guard is repeated inside the promoting and inserting
statements, so two concurrent wizard requests cannot both create an
administrator. The promotion forces `role = 'admin'` and `is_active = 1` and
revokes the local operator's live API keys, so the first password always yields
exactly one active administrator and no credential minted without a password
survives it. `PATCH /api/users/{id}` refuses the placeholder with
`409 local_operator`, and refuses to deactivate or demote the last active real
administrator with `409 last_active_admin`.

**Why:** skipping setup leaves the gateway as a trusted local operator, and the
console lets that operator create accounts before setting a password. With the
old "any account" gate, creating a normal user closed the first-admin path in
the same process; after the next restart the database was `protected` with no
administrator, and neither Settings nor the CLI could restore one. Checking the
same invariant from the other side exposed a second route to zero
administrators: an admin demoting or deactivating themselves.

**Consequence:** a non-admin account no longer closes setup, so Settings can
still promote the local-operator placeholder and set the first password.
Removing the last active administrator is a `409`; promote or activate another
administrator first. The placeholder is also filtered out of `GET /api/users`,
so the console never offers Edit or Deactivate for the one row the API refuses
to patch. A process already in `protected` mode still refuses setup mutations,
so this does not reopen a finished installation. The promotion repairs a
placeholder that an older build (or direct database access) demoted or
deactivated: role and status are forced. API keys minted while the gateway had no
password are revoked in the same transaction — after promotion they would act
with the administrator's `/v1` access (D23), so create new keys after setting the
password. Databases that already contain accounts but no administrator are not
repaired automatically: their mode is `protected`, and they need a manual role
change to recover.

**Source:** [PR #11](https://github.com/mohamadkhani/pareto-ai/pull/11),
introduced by [`4a1782f`](https://github.com/mohamadkhani/pareto-ai/commit/4a1782f6ea0d3e77d54e84b5a03b15ecead118aa)
and tightened by [`eb2ae47`](https://github.com/mohamadkhani/pareto-ai/commit/eb2ae473f4facf451dde5da7e68fb8542f7416ee).

### D58. Accounts cannot be created while the gateway is unprotected

**Decision (2026-11-21):** `POST /api/users` is refused with
`409 setup_incomplete` while the gateway is in `unprotected` mode. The rule
lives in `users::create_user`, so every caller inherits it. The only way out of
that mode is the first admin/password (`/api/setup/admin`, exposed as Settings),
which promotes the local operator.

**Why:** skipping setup lets the local operator use the console without a
password. Creating any real account in that state flips `resolve_mode` to
`protected` on the next restart (`real_user_count > 0`), and without an
administrator the instance has no way in: setup mutations are closed in
`protected`, the local operator has no password, and the CLI only resets
existing passwords. Blocking creation is the smallest rule that removes the
trap without weakening the mode: `protected` still cannot silently fall back to
`unprotected` when an administrator disappears, and no normal user is promoted
to administrator against their will.

**Consequence:** the order is fixed: skip setup (optional) → set the first
password → create accounts. While unprotected, the Users page disables
**New user** and points at Settings; the API answers `409 setup_incomplete` if
it is called anyway. The check uses the in-memory mode, not an administrator
count, so it cannot race with a concurrent first-admin creation: an account can
never commit while the process still considers itself unprotected. Databases
that already contain accounts but no administrator predate this rule and are not
repaired by it.

**Source:** [PR #11](https://github.com/mohamadkhani/pareto-ai/pull/11),
introduced by [`4a1782f`](https://github.com/mohamadkhani/pareto-ai/commit/4a1782f6ea0d3e77d54e84b5a03b15ecead118aa);
the service-level gate is [`4d0a14f`](https://github.com/mohamadkhani/pareto-ai/commit/4d0a14fcbbbe3fe1cb1f826200129f0e148f5d02).

## Open questions

1. **Where does the money actually come from?** Team-paid central account, or
   per-user keys with individual billing? This changes whether budgets are soft
   alarms or hard gates.
2. **Should prompts be stored?** Default no. If yes: retention, encryption, and
   who may read them.
3. **Which providers are needed on day one?** Pick the five that cover real
   usage; provider breadth is the main cost driver.
4. **Is reselling access a goal?** If yes, provider terms and invoicing change
   the design early.
5. **How strict is "cheapest"?** Price can be 10x different for the same model
   with different quantization or uptime. Is degradation acceptable?
6. **Budget boundary timezone** and week start.
7. **Quality source:** is a paid Artificial Analysis subscription acceptable, or
   only free LMArena data?
8. **Deployment target:** one VM, a NAS, or Kubernetes? Affects packaging and
   metrics.
9. **Do users bring their own provider keys?** If yes, credential ownership and
   spend attribution change.
10. **SSO later?** If the team already has an identity provider, OIDC may beat
    local passwords even for v1.

## Risks

| Risk                                            | Impact                        | Mitigation                                                |
| ----------------------------------------------- | ----------------------------- | --------------------------------------------------------- |
| Provider prices and model IDs change constantly | Wrong routing, wrong bills    | Provenance, staleness limits, manual overrides, sync jobs |
| Benchmarks are noisy and model-level            | Misleading "best"             | Confidence weighting, show source and date, allow weights |
| Speed telemetry is sparse at first              | Bad decisions                 | Neutral default, minimum sample count, explore/exploit    |
| Budget races                                    | Overspend                     | Reservations + reconciler                                 |
| Streaming failover complexity                   | Hangs, duplicate output       | Strict "no failover after first byte" rule                |
| Secret leakage                                  | Compromised upstream accounts | Encryption at rest, hashing, redaction, audit             |
| Scope creep toward OmniRoute                    | Never ships                   | Fixed P0/P1 list, explicit non-goals                      |

## Next actions

1. Free disk space and move this research log into `docs/research/`.
2. Answer the open questions above, starting with money flow and day-one
   providers.
3. Write a one-page API contract for `/v1/chat/completions` behaviour, including
   streaming and error shapes.
4. Sketch the SQLite schema and the routing formula in a design doc, then build
   the P0 skeleton.
5. Pick five providers and convert their OmniRoute registry entries into the new
   declarative format with attribution.
