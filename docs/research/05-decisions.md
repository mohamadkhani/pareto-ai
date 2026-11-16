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

**Measured:** 7.1 MB with the UI, 6.7 MB without (−455 KB). The UI is cheap, so
the single binary is the default artifact.

**Consequence:** `scripts/build.sh` builds the UI first, then the binary. The
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
sealed with the same AES-256-GCM `SecretBox`, AAD = row id. Header *names* stay
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
that sync never touches. Correcting a synced *parameter* by hand is not
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

**Consequence:** `scripts/check.sh`/`scripts/build.sh` are thin `exec just ...`
wrappers kept only for in-flight callers and will be removed.

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
`weighted`/`balanced`. Health is enforced by the *filters* (breaker, cooldown,
capability, budget) that run before any ordering.

**Why:** review M3 (2026-11-16,
[../reviews/2026-11-16-m5-routing.md](../reviews/2026-11-16-m5-routing.md)):
penalties were applied under every preset, so `cheapest` could pick a more
expensive offering — the documented contract was not guaranteed. A user who
explicitly asks for "cheapest" should get price order among *eligible*
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
