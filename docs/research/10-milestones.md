# Milestone log

Orchestration record: who did what, what review found, what was committed.
Claims here are dated; code facts cite commits.

## Commits

| Date | Commit | Milestone | Summary |
| --- | --- | --- | --- |
| 2026-11-15 | `520b60c` | M0 | server skeleton, config, SQLite+migrations, `/health`, Angular scaffold |
| 2026-11-16 | `2b57725` | M0.1 | embedded UI (`rust-embed`), `/openapi.json` via utoipa, `scripts/build.sh` |

## Runs

| Date | Unit | Who | Outcome |
| --- | --- | --- | --- |
| 2026-11-16 | tooling correction (user-driven) | chief-of-staff | `web/` migrated npm → pnpm (81 tests green), `Justfile` added as the single entry point (`just check` gate green), `scripts/*.sh` became `exec just ...` wrappers. D21 recorded; see [11-user-preferences.md](11-user-preferences.md) |
| 2026-11-15 | M0 + M0.1 | chief-of-staff directly | done, all checks green |
| 2026-11-16 | M1 backend (auth, sessions, CSRF, rate limit, audit, bootstrap) | sub-agent `ses_e1ef9f91bffe3v8THBfciRsJdo` | done: 38 tests green, clippy clean; deviations: unique-index instead of duplicate index, lockout `429` + `Retry-After`, disabled account `403`, exclusive `before` cursor, new `error.rs` uniform shape |
| 2026-11-16 | review of M1 backend | sub-agent `ses_e1edd1558ffeOWRmEJWQ4s5QPM` | verdict **needs changes**: 4 major (audit pagination drops rows on timestamp ties; non-UTC cursor mis-normalized; test masked the first; client-IP/proxy trust breaks rate limiting) + 4 minor + 6 nits. Full text: [`docs/reviews/2026-11-16-m1-backend.md`](../reviews/2026-11-16-m1-backend.md) |
| 2026-11-16 | M1 frontend (login, guard, audit view) | sub-agent `ses_e1edd1558ffd1Vh0zFoUSd9DE6` | done: 22 vitest tests green, build green, contract verified against the running backend with curl (incl. CSRF 403, `Retry-After` lockout) |
| 2026-11-16 | M1 fixes (review findings 1-6, 8-13) | sub-agent `ses_e1ed5b6caffexOft1PbrqA2Tx1` | done: 57 tests (19 new), clippy clean. Client-visible: composite audit cursor `next_cursor {before, before_id}`, session rotation on login, uniform `404 route_not_found`/`405 method_not_allowed` for `/api/*`, proxy trust opt-in via `GATEWAY_TRUSTED_PROXY` (rightmost XFF hop only) |
| 2026-11-16 | re-review of M1 fixes | sub-agent `ses_e1ebf2eeeffeptAqZBCpcBQi6L` | verdict **approve with nits**: all 12 fixes confirmed, no masked tests. F1 (chained proxies collapse client IP), F2 (duplicate XFF headers spoofable), N1 (`account_disabled` skips `record_failure`), N2 (test gaps), N3 (sub-ms cursor truncation, accepted). Full text: [`docs/reviews/2026-11-16-m1-fixes-re-review.md`](../reviews/2026-11-16-m1-fixes-re-review.md) |
| 2026-11-16 | close M1 nits (F1, F2, N1, N2 tests, N3 doc) | sub-agent `ses_e1eb61ea4ffe46I8b9xbGpVRuB` | done: 63 tests (6 new), clippy clean, `scripts/check.sh` green. Rightmost-untrusted XFF walk with `get_all`; `account_disabled` counts toward the failure budget; 3-page tie pagination and `/dashboard` fallback tests |
| 2026-11-16 | **M1 closed and committed** | chief-of-staff | `a1f9c33 feat(auth): session auth, csrf, audit log and admin ui` (84 tracked files; verified `scripts/check.sh` myself first) |
| 2026-11-16 | M2 backend (providers, credentials, catalog, prices, OpenRouter sync) | sub-agent `ses_e1ea9a98affeFm0du9HPssp7i3` | done: 117 tests (54 new), `scripts/check.sh` green. Secret redaction proven by scanning db/-wal/-shm bytes; AES-256-GCM `ring` per D16; fixture-based sync (live OpenRouter blocked). Deviations: `last_tested_at` column, `price_sync_runs` table, `display_name` in `capabilities_json`, extra `GET /api/offerings/{id}/price` |
| 2026-11-16 | review of M2 backend (security focus) | sub-agent `ses_e1e6ab877ffeNzzzlsrMGagep6` | verdict **needs changes**: 1 major (header values stored plaintext — resolved as D17: encrypt them), 7 minors (D18/D19 resolve two), 10 nits. Crypto core, redaction, audit and sync semantics verified sound. Full text: [`docs/reviews/2026-11-16-m2-backend.md`](../reviews/2026-11-16-m2-backend.md) |
| 2026-11-16 | M2 alignment pass (UI ↔ live API) | sub-agent `ses_e1e6a6f38ffe6wouwJnOG2X4Sf` | done: 7 of 10 UI assumptions were wrong (bare-array envelopes, header values never on the wire, status/probe shapes, sync outcome shape, no embedded price) — UI fixed and verified against a live gateway; 81 UI tests green. Backend findings B1-B4 (low/info) resolved as D20 + explicit-400 fix |
| 2026-11-16 | close M2 findings (D17 header encryption, minors 1-4/6-7, nits, B3) | sub-agent `ses_e1e54c76bffeocbI2pwwXJ22Af` | done: 129 tests (12 new), `scripts/check.sh` green. Header values sealed in `{"names", "values":"v1:…"}` envelopes (AAD = row id); `UNIQUE(offering_id)` + `ON CONFLICT`; zeroize; run pruning per D19 |
| 2026-11-16 | **M2 closed and committed** | chief-of-staff | `79dc371 feat(catalog): providers, encrypted credentials, models, prices, sync` (127 tracked files; verified `scripts/check.sh` + encryption wiring myself first) |
| 2026-11-16 | M3 backend (chat data plane) | sub-agent `ses_e1e36e9acffeN9GenOzjUnZ3g7` | done: 167 tests (38 new), `scripts/check.sh` green. Bounded-channel streaming, `CancelOnDrop` cancellation (<1s upstream close, stub-proven), pre-byte-only failover, one ledger pair per request (no double-spend), `chars/4` estimation flagged, `/v1` CSRF-exempt |
| 2026-11-16 | review of M3 (deep: ledger, cancellation, timeouts, failover, SSE) | sub-agent `ses_e1daa1888ffe4lvj2W1IWmiMLQ` | verdict **needs changes**: 1 blocker (client `x-request-id` as ledger PK → duplicate id rolls back both rows, a billing-evasion hole), 3 major (estimation counts JSON scaffolding 10-40x over; unbounded SSE line buffer; estimation booked for never-sent attempts), 6 minors, 7 nits. Core design (cancellation, failover boundary, single-tx settlement, budgets) verified sound. Full text: [`docs/reviews/2026-11-16-m3-deep-review.md`](../reviews/2026-11-16-m3-deep-review.md) |
| 2026-11-16 | close M3 findings (B1 ledger key, M1-M3, m1-m6, nits) | sub-agent `ses_e1d98cb32ffeJ9szGN82oFGVax` | done: 181 backend tests (16 new). Server-generated ledger key (`client_request_id` echo preserved, `UNIQUE` on `usage_events.request_id`), content-only token estimation, 1 MiB SSE line cap, `sent`-flag estimation, budget-raced sends, terminal oversized-buffer class |
| 2026-11-16 | **M3 closed and committed** | chief-of-staff | `9efd776 feat(chat): streaming chat data plane with usage ledger` (138 tracked files; verified `just check` + ledger wiring myself first) |
| 2026-11-16 | M4 backend (keys, grants, budgets, reservations, reports, user mgmt) | sub-agent `ses_e1d6f1e0bffeuZz1kPjqgXTAXA` | done: 230 backend tests (49 new), `just check` green. Write-lock-first reservation transaction (barrier-tested), cost from chosen offering prices in `settle`, grant matrix (deny-beats-allow × user∩key), keys shown once + SHA-256 + RPM/TPM buckets, forced rotation, idempotent rollups. 6 schema deviations documented in `0005` header |
| 2026-11-16 | review of M4 (money + access, adversarial) | sub-agent `ses_e1d1bf708ffeHtbRrDrnIha9en` | verdict **needs changes**: 1 blocker (released reservation → actuals never booked; TTL 10min vs total-timeout 600s race, exploitable with larger timeouts), 3 major (period straddle dodges day caps at default config; unvalidated upstream usage can credit budgets; subset-token double-billing — spec defect → D22), 6 minors (incl. admin reset doesn't revoke keys → D23), 4 nits. Authz and key security verified correct. Full text: [`docs/reviews/2026-11-16-m4-money-access.md`](../reviews/2026-11-16-m4-money-access.md) |
| 2026-11-16 | close M4 findings (B1, M1-M3→D22, m1-m6→D23, nits) | sub-agent `ses_e1d09b1f3ffeT2UbPbaazFRjnF` | done: 242 backend tests, `just check` green. Booking keyed on an atomic `status <> 'settled'` marker (released rows still book), `RESERVATION_TTL` derived strictly above `timeout_total`, bucket-scoped `period_start` rows (no cap dodge across midnight), usage clamped ≥ 0 at parse, D22 subset-aware pricing, D23 admin reset revokes keys |
| 2026-11-16 | **M4 backend closed and committed** | chief-of-staff | `30f95ec feat(access): api keys, grants, budgets and usage accounting` (verified `just check` + B1/M1/M2/D22/D23 markers in code myself first) |
| 2026-11-16 | M4 UI (users, keys, grants, limits, usage, logs, forced rotation) | sub-agent `ses_e1cd70a5bffeMI2Y6wO3259rin` | done: 142 UI tests (17 new spec files), build + prettier green. 8 documented assumptions for alignment (flattened report rows, `conflict` code, raw `scopes` string, null-keeps PATCH, 12-char password min, error-message rewrite, key-create omission, raw user id in heading) |
| 2026-11-16 | M4 alignment pass (UI ↔ live API) | sub-agent `ses_e1cb31db1ffe9NmUpLoDp2Mz2R` | done: all 8 assumptions verified (unlike M2); 4 UI fixes (D23 copy contradiction, D9 grant-intersection hint, empty group labels, phantom `from`/`to` filters). D22 pricing verified to the cent against a mock upstream. Backend findings: refused requests inflate `requests`/`priced_requests` (minor), empty price PUT → 200 with all-NULL row (nit) |
| 2026-11-16 | close M4 alignment findings (report counters, price PUT validation) | sub-agent `ses_e1ca18951ffeTRnpAHQaiJuppt` | done: 245 backend tests (3 new), `just check` green. `refused_requests` counter via **0006** migration (0005's checksum untouched — correct call), four-counter partition `refused+priced+unpriced+partial == requests`, empty price PUT now 400 |
| 2026-11-16 | UI: `refused_requests` breakdown | sub-agent `ses_e1c905a9effeiijNl2Sza10xBx` | done: 144 UI tests (2 new), build + prettier green |
| 2026-11-16 | **M4 closed and committed** | chief-of-staff | `b1e58f0 feat(access-ui): users, keys, grants, limits, usage and logs screens` (verified `just check`: 245 backend + 144 UI) |
| 2026-11-16 | M5 backend (telemetry, scoring, combos, benchmarks, breaker) | sub-agent `ses_e1c8a2813ffelP6M1eSRox1NN3` | done: 296 backend tests (51 new), `just check` green. Pure deterministic scoring with explanations for every candidate, `combo:<name>` (ordered/weighted/round_robin + persisted cursor), breaker per (credential, offering), benchmark import with override precedence + CI shrink. Deviation: `combo_models.rr_cursor` column; combo members are offerings only. Deliberate rule: 401/403 fails over *inside* combos only |
| 2026-11-16 | review of M5 (determinism, scoring honesty, breaker, combo semantics) | sub-agent `ses_e1c4c60f3ffeYqDu6532httRvL` | verdict **needs changes**: 6 major (breaker counts client 400s; explanation prints `trips` as "consecutive failures"; presets don't match their contract → D24; mixed quality scale → D25; reliability blind to failover-rescued failures → D26; no warm-up → D27), 6 minors, 5 nits. Determinism, budget/grant non-bypass, combo mechanics verified sound. Full text: [`docs/reviews/2026-11-16-m5-routing.md`](../reviews/2026-11-16-m5-routing.md) |
| 2026-11-16 | close M5 findings (M1-M6→D24-D27, m1-m6, nits) | sub-agent `ses_e1c394ce1ffeQSSEmMUvZ0tb8L` | done: 314 backend tests (18 new), `just check` green. `counts_toward_breaker` classification, pure-order presets (D24), Elo-scale validation (D25), `telemetry_attempts` per-attempt samples via 0008 (D26), `n/(n+10)` shrink (D27), `warm()` wired, `combo:` reserved + advertised, strengthened property test |
| 2026-11-16 | **M5 backend closed and committed** | chief-of-staff | `03c0c3d feat(routing): weighted scoring, combos, telemetry and circuit breaker` (verified `just check` + D24-D27 markers myself first) |
| 2026-11-16 | M5 UI (combos, decision view, benchmarks, overrides) | sub-agent `ses_e1c0682a3ffeiTZsf7SIt56FN4` | done: 171 UI tests (27 new), build + prettier green. Combos page with weights editor (D24 contract), "Why this model" decision panel (imputed/shrunk badges, unclamped total), benchmarks import + override editor. 6 assumptions; two read endpoints (`GET /api/benchmarks`, `GET /api/quality-overrides`) missing in backend (UI degrades gracefully) |
| 2026-11-16 | M5 alignment pass (UI ↔ live API) | sub-agent `ses_e1bed85edffexVjVZu46etvV0m` | done: 4 of 6 assumptions verified; `decision_json` real-vs-typed exact (incl. negative and >1 totals). 2 backend findings (missing `GET /api/benchmarks` and `GET /api/quality-overrides` list endpoints). UI: real-record rendering tests + `invalid_request` code alignment; 175 UI tests green |
| 2026-11-16 | close M5 alignment findings (two list endpoints) | sub-agent `ses_e1bd7701dffeMKObR5nQZ2AczR` | done: 318 backend tests (4 new), `just check` green. `GET /api/benchmarks` + `GET /api/quality-overrides` (newest-first, `model_name` via LEFT JOIN, audited reads, fail-closed) |
| 2026-11-16 | **M5 closed and committed** | chief-of-staff | `d864224 feat(routing-ui): combos, decision view, benchmarks and quality overrides` (verified `just check`: 318 backend + 175 UI) |
| 2026-11-16 | holistic review (whole project, fresh eyes) | sub-agent `ses_e1bc86206ffe3l77GV81u5E5WL` | verdict **ship with follow-ups**: no correctness/security defects. Verified clean end-to-end: money path (exactly-once settlement, D22 pricing, counter partition), authz uniformity, audit coverage, secret redaction, utoipa coverage of all 61 routes, migration chain. Findings: 1 major (M3 acceptance claims unproven: real-provider + 200-stream load), 10 minors (doc drift: README status, 3x-stale size claims, PLAN D1-D12/npm notes, 0004 edited post-commit, web/README fossil, D11 relabel), 5 nits |
| 2026-11-16 | docs reconciliation + code close-out (load test, job supervision, wrappers) | sub-agents `ses_e1bb856daffeSMfbFy4ZQAQQyi` (docs) + `ses_e1bb856daffdakG9OO2DUaKpXZ` (code) | docs half done (next row); code half (load test, job supervision, wrappers) running |
| 2026-11-16 | docs reconciliation (holistic review doc findings 1-10) | sub-agent `ses_e1bb856daffeSMfbFy4ZQAQQyi` | done: README/PLAN/`web/README`/research-README synced to `d864224` (README status → PLAN pointer, closing commits in the PLAN table, env table +`GATEWAY_INSECURE_COOKIES`/`GATEWAY_TRUSTED_PROXY`); sizes re-measured 2026-11-16 at `d864224` (18.85/18.15 MiB, UI −714 KiB; the M0.1 numbers marked superseded); M3 acceptance split into proven vs pending; `0004` post-commit edit + recreate rule recorded; D11 relabeled policy-for-future; all four specs flipped to shipped with closing commits, m5 acceptance #5 = `combo:<name>`, offerings-only members + `rr_cursor` deviation recorded |
| 2026-11-16 | housekeeping | chief-of-staff | killed two leftover `ng serve` processes (06:53/07:08); dev ports free |

**All milestones M0-M5 are implemented and committed** (10 commits,
`520b60c`..`d864224`). Remaining: code close-out (load test, wrappers), the two
M3 evidence items below, final wrap-up.

**M3 pending evidence** (2026-11-16, docs reconciliation): two M3 acceptance
claims are not yet proven — (a) streaming against a **real provider**
(network-blocked here, the same environment limit as the fixture-based
OpenRouter sync) and (b) **flat memory under 200 concurrent streams** (a load
script against the stub upstream is being written in the code close-out run;
it proves this item against the stub, not a live provider). PLAN's M3
acceptance block marks both as pending; close this entry when they land.

**Migration history** (2026-11-16): `0004_usage.sql` was edited after it was
committed — it landed in `7000c88` and was modified again in `9efd776`
(18 insertions, 6 deletions) — the one violation of the forward-only migration
rule (found by the holistic review, 2026-11-16). Dev DBs created before
`9efd776` carry the pre-edit schema and must be recreated; this extends the M4
follow-up below beyond the 0005 rotation flag.

**M4 follow-ups** (from the implementer's report, to keep): token caps are
pre-checked only for the reservation's model (a failover to another model can
overspend a cap by one request); `scopes_json` stored but unenforced (spec
leaves scopes undefined); no admin "reset usage" action; pre-existing dev DBs
need recreating (the 0005 rotation flag and the post-commit `0004` edit — see
"Migration history" above); `last_used_at`/`last_seen_at` are
per-request writes (batch if profiling shows it).

**M3 contract notes for future UI work** (from the M3 report): mid-stream
failures keep HTTP **200** and surface as an in-band `data: {"error":...}` SSE
event before `[DONE]` — never key failure display off the status code for
streams; `stream_options.include_usage: true` is injected and `max_tokens` is
clamped; `/v1` never needs `X-CSRF-Token`; 429 carries `Retry-After` equal to
the recorded credential cooldown.

A final holistic review of the whole project is planned after M5, instead of a
separate UI review for M2 (the UI was already exercised by the alignment pass
against a live backend and has 81 tests).
| 2026-11-16 | M2 admin UI (providers, credentials, models, offerings, prices, sync) | sub-agent `ses_e1ea9368fffe7MUWHymKpO5a04` | done: 74 UI tests (50 new), build + prettier green. 10 documented assumptions where the spec was silent: [`docs/reviews/2026-11-16-m2-frontend-assumptions.md`](../reviews/2026-11-16-m2-frontend-assumptions.md) — alignment pass must reconcile them with the backend |
| 2026-11-16 | frontend adopts composite cursor | sub-agent `ses_e1ebf2eedffebuF0hfBmUoXRvO` | done: 24 vitest tests green, build green, prettier clean; server `next_cursor` used verbatim (never derived), short page ends paging |
| 2026-11-16 | M2 spec | chief-of-staff | [docs/specs/m2-providers-catalog.md](../specs/m2-providers-catalog.md) written (fixture-based OpenRouter sync, `ring` AEAD per D16) |
| 2026-11-16 | M3 spec | chief-of-staff | [docs/specs/m3-chat-dataplane.md](../specs/m3-chat-dataplane.md) written (timeouts, pre-byte-only failover, error taxonomy, fake-upstream tests) |

## Decisions made while supervising (2026-11-16)

- Review finding 7 (forced password rotation) deferred to M4, where a
  password-change surface exists; PLAN amended.
- Review finding 11 (`anyhow` in bootstrap) accepted as startup glue; AGENTS.md
  wording clarified instead of changing code.
- Review finding 14 (`account_disabled` 403) stays: deliberate, documented.

## Review findings worth keeping

- (2026-11-15, own build) `ng build` output path is `browser/`; the build script
  caught it with a clear message. Recorded in
  [09-environment.md](09-environment.md).
- (2026-11-15, own build) fallback handler with `Path<String>` returns 500 on
  unmatched routes.
