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
| 2026-11-16 | **M1 closed and committed** | chief-of-staff | verified `scripts/check.sh` myself before committing; commit hash recorded in the next entry |
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
