# Review artifact — M4 money and access (2026-11-16)

Source: review sub-agent `ses_e1d1bf708ffeHtbRrDrnIha9en`, adversarial review
of the uncommitted M4 work on top of `9efd776`. Verdict: **needs changes** —
1 blocker, 3 major, 6 minors, 4 nits. Authorization and key security verified
correct (grant matrix matches D9, key hashing/rotation guard sound, write-lock
reservation open is sound and barrier-tested).

## Blocker

- **B1. A released reservation means actuals are never booked.**
  `money.rs:883-890` early-returns `NoReservation` when the row is no longer
  `open`; the reconciler releases rows older than `RESERVATION_TTL` (10 min,
  `money.rs:82`) while `DEFAULT_TIMEOUT_TOTAL` is 600 s (`config.rs:27`).
  After release: cost lands in `usage_events`/`usage_daily` but `used_usd`/
  `used_tokens` are untouched and headroom is freed mid-request. With
  `GATEWAY_TIMEOUT_TOTAL_MS > 600000` this is an open hole: stream past 10 min,
  nudge the clock, repeat → unbounded spend past the total cap. **Fix:** book
  actuals for any billable request whose reservation is not `settled`
  (idempotence keyed on the settle marker, not on `status='open'`), and make
  `RESERVATION_TTL` strictly greater than `timeout_total`.

## Major

- **M1. Period straddling dodges day/week/month caps.** `money.rs:920-927`
  skips a bucket that "opened after the request started" (warn + `total`
  only). Reachable at default config: slow stream at 23:59, one tiny request at
  00:00:01 resets the day row, the big cost never hits the day cap. **Fix:**
  key bookings to the bucket derived from `started_at` (e.g.
  `UNIQUE(user_id, period, period_start)` rows) so a reset cannot hide earlier
  spend; or record + bound the deviation.
- **M2. Unvalidated upstream usage can credit budgets.** `upstream/sse.rs:39-60`
  accepts any `as_i64` (negatives included); `money.rs:227-240` multiplies
  unclamped; `money.rs:248-254` unchecked `+`. `prompt_tokens: -5000000` or an
  i64 overflow books negative cost/tokens, *raising* remaining budget.
  **Fix:** clamp components ≥ 0 at parse (negatives → unknown/0), saturating
  adds.
- **M3. Five-component formula double-bills OpenAI-style usage (spec defect).**
  In OpenAI-compatible `usage`, reasoning is a subset of `completion_tokens`
  and cached tokens a subset of `prompt_tokens`; the spec's sum bills them
  twice when both prices are set, and falsely flags `priced=0` when
  `reasoning_per_m` is NULL. Test `cost_math_covers_every_price_component`
  encodes the defect. **Fix (chief-of-staff, D22):** subset-aware pricing —
  `(input - cache_read)*input_per_m + cache_read*cache_read_per_m` and
  `(output - reasoning)*output_per_m + reasoning*reasoning_per_m`, clamped at
  0; spec amended.

## Minor

- **m1.** Rollup rebuild reads `usage_events` outside its delete/rewrite
  transaction (`reports.rs:185-191` vs `225-248`) — a settlement can vanish
  for up to 60 s. Fix: read inside the transaction.
- **m2.** Partial-priced rows (`priced=0` with understated cost) appear in
  neither `priced_requests` nor `unpriced_requests` (`reports.rs:148-150,
  220-222`). Fix: explicit `partial_requests` counter + test.
- **m3.** Admin CRUD is read-then-write on a deferred `BEGIN`
  (`money.rs:344-374` etc.) → spurious `SQLITE_BUSY_SNAPSHOT` 500s under load.
  Fix: write-first transactions like the reservation open.
- **m4.** USD reservation can be 0.0 against a priced request (estimate priced
  on the *first* candidate, `unwrap_or(0.0)`, output estimate capped at 4096):
  the cap is soft at this edge. Fix: price the estimate on the first *priced*
  candidate or reserve a floor; record the residual.
- **m5.** Forced rotation does not touch API keys (`auth.rs` bearer path is
  exempt by design; `users.rs:361-365` drops sessions only).
  **Fix (chief-of-staff, D23):** self-changed password keeps keys; **admin
  password reset revokes the user's API keys** (suspected compromise).
- **m6.** TPM is charged before the budget check (`chat.rs` ~750-770) — a 402'd
  request burns quota. Fix: reorder or refund.

## Nits

- `CreatedKey` derives `Debug` while holding the plaintext key
  (`api_keys.rs:76-81`) — redact.
- `release_stale` stamps `settled_at` on `released` rows (`money.rs:980-990`).
- `last_used_at` write per key request (`api_keys.rs:470`) — batch later.
- `access.rs:301-338` "allow-by-any, deny-by-any" reading of an underspecified
  spec — record in `05-decisions.md` (chief-of-staff: appended to D9).

## Missing tests (reviewer)

Concurrent settlements; settle-after-release (B1); period-straddle end-to-end
(M1); negative usage (M2); partial-price rollup counters (m2).
