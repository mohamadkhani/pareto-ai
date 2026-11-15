# Review artifact — M1 fixes re-review (2026-11-16)

Source: review sub-agent `ses_e1ebf2eeeffeptAqZBCpcBQi6L`, verifying the fix
diff for [2026-11-16-m1-backend.md](2026-11-16-m1-backend.md) findings 1-6 and
8-13. Verdict: **approve with nits**. All 12 fixes confirmed implemented; no
test masks a bug; 57 tests pass, api-only 54 pass, clippy clean.

## Findings

- **F1 (minor, security): chained trusted proxies collapse the client IP.**
  `auth.rs:461-476` takes the rightmost XFF hop. With LB → nginx → gateway the
  rightmost hop is the previous proxy, so all clients share one IP and the
  per-account login lockout becomes a shared bucket (the harm finding 4 was
  filed for). Fix: walk XFF right-to-left, skip hops inside `trusted_proxies`,
  return the first non-trusted hop (nginx `real_ip_recursive` semantics), or
  document single-hop-only.
- **F2 (minor, security): only the first `X-Forwarded-For` header line is
  read.** `auth.rs:462-465` uses `headers.get(...)`. Duplicate XFF headers make
  the rightmost hop of the first line client-controlled → spoofed client IP →
  limiter evasion and forged audit IPs. Fix: `get_all` + join with `", "` then
  rightmost-untrusted walk.
- **N1:** `account_disabled` never calls `record_failure`, contradicting the
  comment at `routes/auth.rs:162-163`; a correct-password caller can generate
  unlimited `login_failed` audit rows and 403s.
- **N2 (test gaps):** (a) no test for 3+ pages of all-tied rows (monotonicity
  only read); (b) UI-fallback test misses a deep dot-less route (`/dashboard`);
  (c) `before_id_without_before_is_rejected` asserts only `code`, which a
  generic `QueryRejection` also produces — assert the message too.
- **N3 (accepted limitation):** sub-millisecond `before` is truncated to
  `%.3f`; unreachable from API-generated cursors. Documented, not fixed.

## Verified correct

Composite cursor SQL semantics (bind order, `i64` id compare, all three WHERE
branches tested and failing under the old code), UTC normalization, forced-tie
tests, trusted-CIDR gate before any header read, `record_success` after
`complete_login` with `record_failure` on error, session rotation inside the
login transaction, `/api` fallbacks in both feature configs, single email
normalization, `TS_FMT` single source, generic rejection messages, actor
lookup ordering.
