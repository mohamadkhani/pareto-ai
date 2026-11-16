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
| 2026-11-16 | close M2 findings (D17 header encryption, minors 1-4/6-7, nits, B3) | sub-agent `ses_e1e54c76bffeocbI2pwwXJ22Af` | running |
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
