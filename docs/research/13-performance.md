# Hot-path performance: SQLite and the chat request

Measured: 2026-11-16. Question: is SQLite (or the request path around it) the
bottleneck for `POST /v1/chat/completions`, and are we near the documented
migration triggers?

## Verdict

**SQLite is not the bottleneck at the expected workload, but per-request write
amplification is the dominant cost once traffic grows.** A plain streaming chat
request runs 22 SQL statements and **three separate write transactions**
(`api_keys.last_used_at`, the reservation, the settlement). On a real NVMe disk
that was ~2.1 ms p50 and ~450 req/s at concurrency 1; switching SQLite from its
default `synchronous=FULL` to the documented `synchronous=NORMAL` (one-line
change, already intended by `06-database-choice.md` §4) cut it to **~0.93 ms
p50 and ~960 req/s**, and raised measured settlement throughput from **883 to
2134 writes/s**. The write path now plateaus near **~0.9–1.0k req/s** in this
in-process harness; the estimated production load is **~0.17 req/s** (30 users ×
500 requests/day).

Against the documented trigger “more than ~100 concurrent writes/sec
sustained”: measured capacity is **~2.1k settlement transactions/s** (NORMAL,
NVMe) — 20× the trigger — but a chat request costs 3 write transactions, so the
trigger maps to roughly **~700 req/s** under this design. We are far below that
at the documented internal-team volume and only approach the ceiling under
synthetic saturation. **No Postgres migration is justified today.** The
highest-value follow-ups are removing redundant writes/queries per request
(section 8), not changing the database.

## 1. Method, hardware, and limits

- **Build**: `cargo test --release --offline` (release profile, thin LTO). The
  probe lives in `server/src/perf_bench.rs`, is `#[ignore]`d, and is run by
  `just bench`.
- **Load shape**: real `Router::oneshot` requests through the full middleware
  stack and the scripted in-process stub upstream (`upstream/stub.rs`), so no
  network or provider latency is included. `Router::oneshot` also skips the TCP
  listener: RPS is an **upper bound** for a socket-served deployment.
- **Database**: one file, one pool of 8 connections, migrations as shipped.
  Two filesystems were used deliberately:
  - **NVMe/ext4** (`/home`, `/dev/nvme0n1p2`) — the headline numbers.
  - **tmpfs** (`/tmp`, which is tmpfs on this machine) — isolates CPU work from
    disk by removing fsync cost.
- **Query counting**: `db::open_logged` sets `log_statements(TRACE)`; a tracing
  layer records `sqlx::query` events (`perf_bench.rs`). One request is counted
  at a time after a warm-up.
- **Hardware**: Intel i5-10400 (6 cores / 12 threads, 2.9–4.3 GHz), 46 GiB RAM,
  Linux 6.18.63, NVMe SSD on ext4. Rust 1.99.1, bundled SQLite via
  `libsqlite3-sys 0.30.1`.
- **Not measured** (marked here once): a real provider over the network, an
  actual PostgreSQL deployment for comparison, multi-hour WAL behaviour,
  power-loss recovery, admin/overview page payloads, body-capture storage,
  weighted combo routing with many members, and the UI half of `just check`
  (the `web/` tree is being edited by others in this workspace).

## 2. Query inventory (measured, not inferred)

Counted per request through the real pipeline, one candidate offering, one
valid API key, admin user, no grants rows, one total budget and one price:

| # | Statement | Purpose |
| --- | --- | --- |
| 1 | `SELECT api_keys JOIN users` | access/auth: key lookup + owner role |
| 2 | `UPDATE api_keys SET last_used_at` | **write** (autocommit txn #1) |
| 3 | `SELECT offerings JOIN models JOIN providers` | resolve candidates |
| 4 | `SELECT provider_credentials` | credential pick (one per candidate) |
| 5 | `SELECT grants` | access grants (one query, evaluated in memory) |
| 6 | `SELECT settings` (`pricing.default_mix_profile`) | default mix resolution |
| 7–8 | `SELECT token_mix_profiles` ×2 | resolve default profile (selector + name) |
| 9 | `SELECT prices` | routing price (one per candidate) |
| 10 | `INSERT reservations` | **write** (txn #2 opens with it) |
| 11 | `SELECT budgets` | reservation cap check |
| 12 | `SELECT SUM(estimated_usd) FROM reservations` | open-reservation headroom |
| 13 | `SELECT token_caps` | reservation cap check |
| 14 | `SELECT prices` | settlement re-reads the price |
| 15 | `INSERT usage_events` | **write** (txn #3) |
| 16 | `INSERT request_logs` | **write** (decision_json included) |
| 17 | `UPDATE reservations` | **write** (settle marker) |
| 18 | `SELECT budgets` | settlement bucket load |
| 19 | `UPDATE budgets` | **write** (book actual spend) |
| 20 | `SELECT token_caps` | settlement cap load |
| 21 | `INSERT ... ON CONFLICT usage_daily` | **write** (rollup upsert) |
| 22 | `INSERT telemetry_attempts` | **write** (one per attempt) |

**Totals: 22 statements, 8 of them writes, in 3 transactions.** Buffered
(`stream: false`) is identical. A failover costs **25** statements: one extra
credential pick, one extra price read, and one extra telemetry sample. Three
candidate offerings cost **26**: one credential pick per candidate and one
price read per candidate. A `429`/`401` failover additionally runs
`UPDATE provider_credentials SET cooldown_until` in a fourth autocommit write
(seen by reading the path, not by this inventory — the counted failover was a
`500`, which does not cool a credential down).

Observations from the inventory:

- **Credential picks and price reads are N+1 by candidate count**, not per
  grant. Single-offering models hide this; a combo with 8 members pays ~16
  extra SELECTs.
- **The price row is read twice**: once to score/reserve (#9) and again at
  settlement (#14). The settle read could reuse the routing row.
- **Default mix resolution costs 3 SELECTs per request** (#6–#8), even for a
  plain model that never uses a combo.
- **`last_used_at` is its own write transaction**, separate from the ledger.
  A request therefore commits to WAL three times.

## 3. Latency and throughput (release, NVMe/ext4)

`POST /v1/chat/completions`, stub upstream, in-process router. p50/p95/p99 are
per-request wall times; RPS is the wave's inclusive rate.

### 3.1 Before: SQLite default `synchronous=FULL`

| mode | conc | p50 ms | p95 ms | p99 ms | req/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| buffered | 1 | 2.09 | 2.85 | 3.47 | 454 |
| buffered | 10 | 13.00 | 66.03 | 86.13 | 155 |
| buffered | 50 | 48.41 | 106.72 | 141.73 | 336 |
| buffered | 200 | 202.75 | 277.19 | 400.15 | 369 |
| stream | 1 | 2.22 | 2.68 | 3.29 | 438 |
| stream | 10 | 12.26 | 59.20 | 82.80 | 162 |
| stream | 50 | 48.07 | 106.92 | 138.77 | 349 |
| stream | 200 | 206.82 | 279.92 | 435.27 | 394 |

### 3.2 After: `synchronous=NORMAL` (the documented setting)

| mode | conc | p50 ms | p95 ms | p99 ms | req/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| buffered | 1 | 0.93 | 1.25 | 3.96 | 959 |
| buffered | 10 | 5.75 | 34.82 | 55.48 | 342 |
| buffered | 50 | 23.72 | 47.27 | 87.20 | 604 |
| buffered | 200 | 112.71 | 189.25 | 206.70 | 928 |
| stream | 1 | 1.01 | 1.34 | 4.03 | 892 |
| stream | 10 | 6.13 | 35.09 | 55.41 | 343 |
| stream | 50 | 21.71 | 46.35 | 111.42 | 547 |
| stream | 200 | 103.26 | 142.36 | 163.23 | 695 |

A second identical run repeated within ~20 % at concurrency 1 and 200 (p50
0.93–1.02 ms buffered at c1; 909–929 req/s at c200; stream c200 was noisier at
476–695 req/s). The c10 row is the least stable: the harness runs fixed-size
batches and waits for the slowest member, and at c10 the per-batch tail
dominates. Read the **shape**, not the c10 point: p50 grows roughly linearly
with concurrency (one serialized writer), while throughput saturates around
~0.9–1.0k req/s.

### 3.3 Reference: same code on tmpfs (no fsync cost)

Before the `NORMAL` change, with the database on `/tmp` (tmpfs): buffered c1
0.84 ms p50 / 1158 req/s; c200 82.8 ms / 1241 req/s; stream c1 1.01 ms /
916 req/s; c200 83.7 ms / 1029 req/s. This is the CPU-only ceiling of the
harness. The before/after delta above is therefore the disk commit cost, not a
code change in the request path.

## 4. Where the time goes (release, NVMe)

Single-threaded microbenchmarks plus the c1 end-to-end p50. “Before/after”
refers to `synchronous=FULL` vs `NORMAL`.

| Stage | Before p50 | After p50 | Share of 0.93 ms e2e |
| --- | ---: | ---: | ---: |
| request parsing + validation | <0.01 ms | <0.01 ms | ~0 % |
| API-key auth (SELECT + `last_used_at` write) | 0.059 ms | 0.053 ms | 6 % |
| session auth (SELECT join + `last_seen_at` write) | 0.059 ms | 0.050 ms | (key path shown) |
| grants load (1 SELECT) | 0.021 ms | 0.018 ms | 2 % |
| credential decrypt + `UpstreamCall::build` | <0.001 ms | <0.001 ms | ~0 % |
| reservation open transaction | 0.459 ms | 0.137 ms | 15 % |
| settlement transaction (10k sequential) | 0.742 ms | 0.321 ms | 34 % |
| upstream stub (open + full stream) | 0.105 ms | 0.101 ms | 11 % |
| one `fsync(4 KiB)` on the same disk | 0.325 ms | 0.327 ms | — |
| telemetry refresh (10k samples, 1 offering) | 11.4 ms | 14.4–19.3 ms | background only |
| `decision_json` parse + serialise (1825 B) | 0.008 ms | 0.008 ms | ~1 % |

The remaining ~0.3 ms is routing/price/mix SELECTs, JSON construction,
task spawn, the mpsc handoff, and response building. At c1 the **settlement
transaction is the single largest stage (about one third)**; the reservation
commit and the auth commit add another ~20 %. The before/after gap at c1
(2.09 ms → 0.93 ms) is disk commit latency: the same code on tmpfs ran at
0.84 ms, and the three commits account for roughly 1.2 ms of the before time.

## 5. SQLite write path

Configured pragmas as measured at runtime:

| Pragma | Value |
| --- | --- |
| `journal_mode` | `wal` |
| `synchronous` | `1` = `NORMAL` (was `2` = `FULL` by SQLite default) |
| `busy_timeout` | 5000 ms |
| `wal_autocheckpoint` | 1000 pages (~4 MiB at 4 KiB pages) |
| `page_size` | 4096 B |
| `cache_size` | −2000 (2 MiB) |
| pool `max_connections` | 8 (one pool for reads and writes) |

Settlement cost, 10 000 sequential real settle transactions (each preceded by
its reservation row, inserted outside the timed region):

| Scenario | Before (FULL) | After (NORMAL) |
| --- | ---: | ---: |
| settle p50 / p95 | 0.742 / 1.070 ms | 0.304–0.321 / 0.41–0.46 ms |
| settle p99 | 2.18 ms | 3.49–3.59 ms (one checkpoint pause) |
| settle with 2 concurrent readers, p50 / p95 | 0.803 / 5.11 ms | 0.386 / 0.474 ms |
| reader queries completed during 2 000 settles | 345 427 | 81 303 |
| 50 concurrent writers × 20 | 796 settlements/s, p50 24.0 ms | 1 740–1 851/s, p50 11.5–12.5 ms |
| 200 concurrent writers × 20 | 883 settlements/s, p50 114 ms | 2 124–2 134/s, p50 54 ms |
| rows written / expected | 4 000 / 4 000 | 4 000 / 4 000 |

No `SQLITE_BUSY` was observed in either configuration: every expected row was
written, and `busy_timeout` never surfaced an error. An earlier harness run
reported missing rows; that was a bug in the probe (reused `request_id`s hit
the `UNIQUE(request_id)` guard), not database contention. It is fixed and the
corrected run writes every row.

**WAL growth.** A dedicated 50 000-settlement run on a fresh database sampled
the WAL file every 5 000 writes: **4099 KiB → 4132 → 4132 → 4132 → 4132 →
4152 → 4152 → 4152 → 4152 → 4152 KiB**. The WAL plateaus at ~4.1 MiB
(autocheckpoint) and does not balloon; `PRAGMA wal_checkpoint(TRUNCATE)`
returns the file to 0 KiB. In the mixed main run, the file's high-water mark
reached 9–25 MiB after the concurrent-writer phases (checkpointing lags
momentarily while writers queue, and the WAL file is reused without shrinking),
then truncates to zero. Monitor the WAL high-water mark in production, but it
is not unbounded.

**Is `synchronous=NORMAL` honest for our durability expectations?** In WAL mode,
`NORMAL` keeps the database consistent across a process crash but does **not**
fsync on every commit, so a **machine power loss can lose the newest
transactions**. For this ledger that means under-billing or a lost usage row,
never corruption or double spend; that is exactly the trade-off
`06-database-choice.md` §4 already accepted. `FULL` buys fsync durability at
~0.33 ms × 3 commits per request; that is the 2.2× latency difference measured
above. If an operator needs power-loss durability for billing, keep `FULL` and
expect ~450 req/s per node; the correct structural fix is fewer commits, not
more fsyncs.

## 6. Suspect-by-suspect findings

| Suspect | Measured answer |
| --- | --- |
| Credential decryption per request | **Re-decrypted on every attempt; no cache.** AES-256-GCM + `ring` costs <1 µs p50, so it is not a bottleneck. A failover decrypts again, but that is still negligible. |
| Grant queries (N+1?) | **One query per request**, evaluated in memory. Not N+1. Credential picks and price reads *are* per candidate (see inventory). |
| Telemetry refresh / cache hits | **No SQL on the request path.** `TelemetryStore::stats` is an in-memory read; routing only consults it for weighted combos. Refresh costs 11–19 ms for 10k samples / 1 offering and runs every 30 s in the background (`GATEWAY_TELEMETRY_REFRESH_MS`). Cold after restart until the first refresh/warm. |
| Rollup upserts | Exactly **one `usage_daily` upsert per request**, inside the settle transaction. |
| `last_used_at` / `last_seen_at` | **One autocommit UPDATE per API-key/session request**, i.e. a third (key) write transaction per chat request. |
| Decision JSON | 1 825 B in the counted 3-candidate request; parse + serialise 0.008 ms p50. Negligible CPU, but it is part of the settlement write. |
| Request parsing | <0.01 ms p50 for the benchmark body. Negligible. |

## 7. Top costs, ranked with evidence

1. **A third of c1 latency is the settlement transaction** (0.32 ms of 0.93 ms,
   10k samples), which writes `usage_events`, `request_logs`,
   `reservations`, `budgets`, `usage_daily`, and `telemetry_attempts`.
2. **Three write transactions per request** (`last_used_at`, reservation,
   settlement). Each commit is an fsync under `FULL`; even under `NORMAL` each
   is a WAL append plus a serialized writer lock. Removing one commit removes
   ~0.1–0.3 ms from every request and directly raises the throughput ceiling.
3. **Per-candidate credential and price SELECTs** (#4, #9, #14): invisible at
   one offering, linear in combo size. Three offerings already cost 26
   statements instead of 22.
4. **Default mix resolution costs 3 SELECTs (#6–#8)** per request for a plain
   model that may never use a mix price.
5. **The price row is read twice** (#9 routing, #14 settlement).

Not a top cost: credential decryption, grants, telemetry cache reads, decision
JSON CPU, and request parsing.

## 8. Recommendations, ranked by impact / effort

1. **Keep `synchronous=NORMAL` (done).** One line in `db.rs`, matches the
   documented decision, measured 2.2× lower p50 at c1 and 2.4× settlement
   throughput. Durability consequence stated in §5. (Also guarded by a
   `db::tests` pragma assertion.)
2. **Remove one commit and one autocommit write per request (medium effort,
   biggest remaining win).** Either fold `last_used_at` into a batched update
   (at most once per key per 30–60 s, or in the settle transaction), or move it
   to a background job. One fewer fsync/commit per request is worth roughly
   0.1–0.3 ms p50 on NVMe and raises the synthetic ceiling.
3. **Cache hot catalog reads with explicit invalidation (medium effort).**
   Cache decrypted credential material, price rows, grants, and the resolved
   default mix profile in `AppState`, invalidated by the existing admin
   mutations (or a short TTL). Removes ~7 SELECTs on the single-offering path
   and the per-candidate N+1; it is the main scaling fix for weighted combos.
   Do **not** cache without invalidation: an operator price or grant change
   must take effect.
4. **Load the default mix profile once at startup (low effort).** It changes
   only through the settings API; resolve it there and store the profile in
   `AppState`. Removes 3 statements/request.
5. **Reuse the routing `PriceRow` at settlement (low effort).** Removes the
   second price SELECT and the possibility of routing and settlement seeing
   different prices within one request.
6. **Operational WAL hygiene (low effort).** Keep autocheckpoint at 1000
   pages, alarm on a WAL high-water mark above ~100 MiB, and run
   `PRAGMA wal_checkpoint(TRUNCATE)` during quiet periods if the file high-water
   matters. Sustained writes plateau at ~4.1 MiB; no action is needed today.
7. **Postgres: not now.** Revisit only for multiple gateway instances, a
   public multi-tenant split, `LISTEN/NOTIFY`, or read replicas — the triggers
   in `06-database-choice.md` §5. Raw SQLite write capacity is ~20× the stated
   `>~100 writes/s` trigger at the expected scale.

## 9. Threshold verdict

| Documented trigger | Measured state |
| --- | --- |
| More than ~100 concurrent writes/sec sustained | Settlement alone sustains **~2 124 writes/s** (`NORMAL`, 200 writers). Three commits/request means ~**700 req/s** before this trigger is reached; the documented production estimate is **~0.17 req/s** (15 000 requests/day). **Far below.** |
| Reports scan months of raw events | Not measured here. Daily rollups exist (`usage_daily`) and are written on the hot path; the raw-event reports path was out of scope. |
| Multiple gateway instances | Not applicable: single process by design. **No Postgres trigger met.** |
| Body capture grows beyond retention | Not measured (body capture not exercised). |

At the measured CPU/disk ceiling the in-process harness reaches ~0.9–1.0k
req/s with `NORMAL`; the same code on tmpfs reaches ~1.2k req/s, so SQLite's
commit path is the leading constraint only above a few hundred requests per
second. The design target is orders of magnitude below that.

## 10. What changed in this investigation

- `server/src/db.rs`: `synchronous=NORMAL` on every connection (aligns the code
  with `06-database-choice.md` §4), plus a pragma assertion in the existing
  database test.
- `server/src/perf_bench.rs` (new, `#[cfg(test)]`): the query-counting layer,
  request waves, stage microbenchmarks, the SQLite write-path probes, and the
  WAL-growth probe. Both tests are `#[ignore]`d.
- `server/src/test_support.rs`: a logging pool constructor and an explicit
  data-directory constructor (for the NVMe vs tmpfs comparison).
- `server/src/chat.rs`: two `#[cfg(test)]` helpers that time the real
  `parse_request` and `settle` paths without instrumenting production code.
- `server/src/db.rs`: `open_logged` for statement counting (test-only).
- `server/Cargo.toml`: `log` as a dev-dependency (SQLx's `log_statements`).
- `Justfile`: `just bench` reproduces the headline tables.

No request semantics changed. `cargo fmt --check`, `cargo clippy --all-targets
-- -D warnings`, `cargo test --offline` (377 passed, 2 ignored), and
`cargo build --offline --no-default-features` all pass. The UI half of
`just check` was not run because `web/` is being edited concurrently.

## 11. Reproduce

```sh
# headline tables on the real filesystem (NVMe/ext4 in this run)
GATEWAY_PERF_DB_DIR="$PWD/server/target/perf-db" just bench

# WAL growth alone (50 000 settlements)
GATEWAY_PERF_DB_DIR="$PWD/server/target/perf-wal" \
  just bench wal_growth_under_sustained_writes

# CPU-only reference (tmpfs; /tmp is tmpfs here)
just bench
```

The probe asserts only that every request completes; it prints numbers and
never fails on a slow machine. Run it with `--test-threads=1` (the `just` recipe
does) because the statement meter is process-global.

## 12. Follow-up: hot-path query and write reduction (implemented)

Measured 2026-11-17 with the same harness. Changes, each tied to the inventory
in §2:

1. **The token-mix profile table is cached per process.** `MixProfileCache`
   (`server/src/mix.rs`) loads all profiles and the
   `pricing.default_mix_profile` setting once (two SELECTs) and resolves the
   default and a combo selector with hash lookups. Every profile mutation and
   the pricing-setting change invalidate it; a selector miss reloads once.
   Removes statements #6–#8 from every request, including plain models that
   never use a mix price.
2. **The routing price row is carried into settlement.** `Attempt` keeps the
   `prices` row it resolved; the chosen offering's row is reused by `settle()`
   when the request is still served by that offering, and a database read
   remains the fallback for an offering outside the resolved set. Removes #14
   and makes routing and settlement see the same price within one request.
3. **`api_keys.last_used_at` is batched.** A bounded in-memory
   `KeyUsageStamps` throttle (60 s per key, 10 000 keys, least-recently-stamped
   eviction) writes the column at most once per key per minute, and only after
   a successful authentication. The list column is therefore accurate to one
   minute instead of exact per request.
4. **Candidate credentials are loaded in one query.** One
   `WHERE provider_id IN (...)` with `ORDER BY provider_id, id` replaces one
   SELECT per candidate; the per-provider preference (not breaker-open, then
   not cooled, id order) is applied in memory exactly as before. A three-member
   request now pays one credential SELECT, not three.
5. **`sessions.last_seen_at` is batched** at one write per session per five
   minutes (`SessionActivity`), while the session lookup and every expiry,
   deactivation, and forced-rotation check still run per request. This affects
   admin/session traffic only, not the API-key chat path.

### 12.1 Query inventory after

| Scenario | Before | After | Removed |
| --- | ---: | ---: | --- |
| single streaming request | 22 | **17** | `last_used_at` write, 3 mix SELECTs, settle price read |
| buffered request | 22 | **17** | same |
| failover request | 25 | **19** | same + one duplicate credential SELECT |
| 3 candidate offerings | 26 | **19** | same + two duplicate credential SELECTs |

The failover/three-candidate runs differ by at most one statement between
runs (a lazily opened pooled connection logs its `PRAGMA journal_mode = WAL`),
so read the deltas (22→17, 25→19, 26→19), not the absolute one-statement
noise.

### 12.2 Latency and throughput after (NVMe/ext4)

Final run on the finished tree, immediately after the baseline run on the same
machine and filesystem. p50 in ms and req/s at the wave's concurrency:

| mode | conc | before p50 | after p50 | before req/s | after req/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| buffered | 1 | 0.94 | 0.75 | 946 | 1174 |
| buffered | 10 | 6.30 | 10.57 | 295 | 121 |
| buffered | 50 | 25.85 | 21.81 | 481 | 397 |
| buffered | 200 | 110.70 | 83.95 | 746 | 1010 |
| stream | 1 | 1.05 | 1.14 | 808 | 761 |
| stream | 10 | 5.74 | 8.50 | 345 | 132 |
| stream | 50 | 25.14 | 19.71 | 526 | 396 |
| stream | 200 | 103.85 | 85.45 | 916 | 657 |

Two post-change runs were taken (the first on an otherwise idle tree, the
second the final table above). Across them, c50/c200 p50 fell by 13–24 % in
both modes, while c1 stayed within run-to-run noise: buffered c1 was 0.97 ms
in one run and 0.75 ms in the other, stream c1 0.91 and 1.14 ms. The c10 row
and the req/s column are dominated by the fixed-batch tail and background
load on this shared machine (the same code measured 695–939 req/s at stream
c200); read p50 at c50/c200 and the statement counts, not single req/s
points. No systematic latency regression was measured.

Stage microbenchmarks (final run):

| Stage | Before p50 | After p50 |
| --- | ---: | ---: |
| API-key auth (SELECT; stamp once/60 s) | 0.056 ms | 0.025 ms |
| session auth (SELECT join; stamp once/5 min) | 0.054 ms | 0.025 ms |
| settle transaction, 10k sequential | 0.311 ms | 0.326 ms (unchanged code path) |
| 200 concurrent writers | 2 172 settlements/s | 1 513 settlements/s (same code, run-to-run load) |

The settlement path is untouched; its 0.311–0.331 ms p50 across runs is the
machine's variance, not a regression. `api-key auth` improves because the
2 000-iteration loop now writes `last_used_at` once instead of 2 000 times.

### 12.3 Verification

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test --offline` (399 passed, 0 failed, 3 ignored) and
`cargo build --offline --release --no-default-features` pass. New tests:

- `throttle::tests::allows_one_write_per_interval`
- `throttle::tests::forget_allows_an_immediate_retry`
- `throttle::tests::evicts_the_least_recently_written_subject`
- `api_keys::tests::last_used_at_is_batched_and_only_stamped_on_success`
- `auth::tests::last_seen_at_is_batched_without_changing_expiry_semantics`
- `routes::mix::tests::profile_edits_take_effect_on_the_next_request`
- `chat::tests::settlement_reuses_the_routing_price_row_and_falls_back`
- `chat::tests::credential_pick_prefers_healthy_then_cooled_then_breaker_open`

The UI half of `just check` was not run because `web/` is being edited
concurrently. No behaviour changed for grants, budgets, the ledger, or auth
decisions.

