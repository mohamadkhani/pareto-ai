# Decisions, open questions, next actions

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

### D10. Only OpenRouter has a built-in price sync

**Decision:** OpenRouter syncs price and catalog through its public API. Any
other provider, including SailResearch, is entered manually with a manually set
price per offering. A declarative "describe your price API" feature comes later.

**Consequence:** unknown price is stored as `null` and scored as unknown, never
as zero.

### D11. Bodies retained for one day, later configurable

**Decision:** request/response bodies captured only when enabled, kept for 24h,
stored as compressed files outside the database.

**Consequence:** the database never holds megabyte blobs; retention becomes a
setting later.

### D12. SQLite as system of record

**Decision:** SQLite (WAL) as the single database for v1, with daily rollup
tables for reporting. PostgreSQL is the migration target for multi-instance
deployments; ClickHouse only later as an analytics sink for raw logs.

**Consequence:** one binary, no second service. Rejected MongoDB (no
multi-document transactions on a single node, no foreign keys) and SurrealDB
(stability risk conflicting with the goal).

Comparison, evidence, and migration triggers: [06-database-choice.md](06-database-choice.md).

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

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Provider prices and model IDs change constantly | Wrong routing, wrong bills | Provenance, staleness limits, manual overrides, sync jobs |
| Benchmarks are noisy and model-level | Misleading "best" | Confidence weighting, show source and date, allow weights |
| Speed telemetry is sparse at first | Bad decisions | Neutral default, minimum sample count, explore/exploit |
| Budget races | Overspend | Reservations + reconciler |
| Streaming failover complexity | Hangs, duplicate output | Strict "no failover after first byte" rule |
| Secret leakage | Compromised upstream accounts | Encryption at rest, hashing, redaction, audit |
| Scope creep toward OmniRoute | Never ships | Fixed P0/P1 list, explicit non-goals |

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
