# Review artifact — M3 deep review (2026-11-16)

Source: review sub-agent `ses_e1daa1888ffe4lvj2W1IWmiMLQ`, deep review of the
uncommitted M3 chat data plane on top of `7000c88`. Verdict: **needs changes** —
1 blocker, 3 major, 6 minor, 7 nits. Core design verified sound: runner/oneshot
cancellation (<1s upstream close), pre-byte failover boundary, single-transaction
settlement, absolute total budget, bounded backpressure, secret redaction (one
gap: m4).

## Blocker

- **B1. Client-supplied `x-request-id` is the `request_logs` PK.**
  `routes/chat.rs:97` → `chat.rs:1466`; `SetRequestIdLayer` keeps a client
  value. A duplicate id makes the second settle's `request_logs` INSERT hit the
  PK conflict → whole transaction rolls back → **zero ledger rows** while the
  response is a full 200. Deterministic, client-driven, and once M4 bills on
  `usage_events`, any key holder can erase their own usage. **Fix:** server-
  generated ledger key unconditionally; keep the incoming id in a nullable
  `client_request_id` column; index `usage_events(request_id)`. The spec line
  naming `x-request-id` as the PK has the same hole and is amended.

## Major

- **M1. `chars/4` estimation counts JSON scaffolding and bytes, not content
  chars** (`upstream/sse.rs:107, 127-132`, `chat.rs:1401-1408`): streaming
  counts full `data:` JSON (~38 chars per output char), non-streaming counts
  the whole response body, input counts raw body bytes (UTF-8 inflation).
  Over-bills 10-40x when M4 prices these rows. **Fix:** count
  `choices[].message.content`/`delta.content`(+`reasoning`) chars; input from
  message text chars.
- **M2. `SseInspector.partial` is unbounded** (`sse.rs:51, 70-72`): a huge
  single line or CR-only endings accumulate per request with no cap.
  **Fix:** cap at 1 MiB, in-band error on overflow.
- **M3. Estimation booked for attempts that never reached upstream**
  (`chat.rs:1401-1402`): decrypt failure or connect timeout still books
  `bytes_in/4` tokens. **Fix:** estimate only when a request was actually sent
  (`sent` flag after `open()`).

## Minor

- **m1.** Sends (`tx.send` at `chat.rs:844`, pump sends) are not raced with the
  total budget; a stalled reader pins the runner past 600s with unsettled rows.
- **m2.** In-band error can concatenate onto a partial SSE line
  (`chat.rs:1321-1330`) — prefix `\n\n`.
- **m3.** Upstream failures are never logged (`chat.rs:1265` claims "details go
  to the log"). Add `tracing::warn!` at classification.
- **m4.** Overridden `authorization` headers not `set_sensitive`
  (`upstream/mod.rs` open()); also mark `x-api-key`.
- **m5.** Buffered 32 MiB overflow is classified retryable
  (`upstream/mod.rs:135-139`) → N candidates can each buffer 32 MiB. Make it
  terminal; cap overshoots one chunk.
- **m6.** Untested failover edges: malformed first chunk, empty body, total
  budget spanning attempts, buffered cap.

## Nits

`cost_usd` NULL vs migration comment; `bytes_in` records the limit for
oversized bodies; cooldown recorded in attempt 1 not applied to attempt 2 of
the same request (spec-consistent — document); `resolve_candidates` N+1
queries (M5); multi-`data:`-line SSE events flagged `protocol_error` while the
doc claims full SSE semantics; no schema guard on exactly-once (follows B1's
fix).

## Suggested order (reviewer)

B1 → M1 → M2/M3 + minors as one pass → the four missing tests (m6) plus a
duplicate-`x-request-id` regression.
