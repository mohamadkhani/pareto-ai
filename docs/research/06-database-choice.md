# Database choice

Question: MongoDB vs SurrealDB vs ClickHouse for the new gateway.
Context: single-node, self-hosted, internal team gateway; Rust backend; needs
both transactional state (budgets, reservations) and analytics (usage reports).

## 1. Split the workload before choosing a database

The gateway has two genuinely different loads. Choosing one engine for both is
where mistakes happen.

| Load | Tables / data | Access pattern | Consistency needs |
| --- | --- | --- | --- |
| OLTP (system of record) | users, sessions, api_keys, providers, credentials, models, offerings, combos, grants, budgets, token caps, reservations, audit log | Point reads/writes, unique lookups, foreign-key integrity | Strong: atomic reserve/settle, no double spend, no lost updates |
| Analytics (append + aggregate) | usage_events, request_logs, price/benchmark snapshots | High-rate append, scan and aggregate by user/model/provider/day | Eventual is acceptable: a report can be minutes stale |
| Bulk payloads (optional) | request/response bodies for debugging | Big blobs, write-once, read rarely | Retention-driven, should not be in the hot path |

Retention decision already made: prompts/bodies kept up to 1 day, later made
configurable. That is a bounded, high-volume stream — the wrong thing to put in
the primary transactional database.

## 2. The three candidates

### ClickHouse

- Strong: hundreds of millions of rows, ad-hoc aggregations, columnar scans,
  cheap retention, real-time dashboards.
- Weak: **no multi-row transactions** (MergeTree is not a ledger). It cannot
  hold the reservation/settlement path that prevents double spend. No
  foreign keys, no uniqueness across inserts, no referential integrity.
- Operational cost: its own daemon, typically 4–16 GB of RAM and real CPU just
  to run. Directly contradicts the low-resource goal.
- Verdict: **not a primary database**. Plausible later as a second store for
  `request_logs`/`usage_events` if volume or query needs outgrow SQL.

### MongoDB

- Strong: flexible documents, horizontal scaling, mature driver, good enough
  aggregation pipeline for reports.
- Weak for this project: multi-document transactions require a replica set
  (a single-node standalone does not have them), so budgets and reservations
  would need an application-level invariant. No foreign keys or constraints —
  every integrity rule lives in code. The schema is genuinely relational (a key
  belongs to a user, an offering belongs to a provider and a model, grants join
  subjects to resources); flattening it into documents means duplicated state
  and harder migrations.
- Operational cost: a second service to back up, monitor, and patch.
- Verdict: workable, but it buys flexibility you do not need and costs the
  guarantees you do need.

### SurrealDB

- Strong: Rust-native, single binary can embed or run as a server, SQL-like
  language, one store for several models, built-in permission rules, Postgres
  wire protocol since 3.3.
- Weak: BSL 1.1 license on core (not open source), two storage-engine changes in
  one year (RocksDB → SurrealKV, plus SurrealMX), a 4.0 on-disk format upgrade,
  snapshot isolation with commit-time write conflicts, and "commit outcome is
  unknown" errors the application must resolve by reading back.
- Verdict: not for v1. Full evidence, corrected claims, and re-evaluation
  triggers: [08-surrealdb-deep-dive.md](08-surrealdb-deep-dive.md).

## 3. The recommendation

**Primary store: SQLite in WAL mode, single file, accessed through SQLx.**

Reasons that fit this project specifically:

1. The deployment target is one binary on one machine. SQLite keeps it that way —
   no second service, no memory footprint beyond page cache, backups are one
   file copy, restore is a tested path.
2. Atomic transactions with `BEGIN IMMEDIATE` give you a correct reservation
   ledger for budgets without distributed-systems work.
3. Foreign keys, constraints, and `UNIQUE` indices keep the relational model
   honest in code instead of in application logic.
4. At internal-team volume it is comfortably enough. Concrete estimate: 30 users
   × 500 requests/day = 15,000 events/day ≈ 450k rows/month, ≈5.5M rows/year.
   With `usage_events` indexed by `(user_id, occurred_at)`, `(offering_id, occurred_at)`
   and daily rollup tables, reports stay in the tens of milliseconds. SQLite
   routinely handles tens of thousands of writes/second in WAL mode; the gateway
   writes one row per request.
5. Reporting is solved by aggregation tables, not by a different engine: a job
   builds `usage_daily(user, offering, day, tokens_in, tokens_out, cost_usd, ...)`
   from `usage_events`, and the dashboard reads the small table.

**When PostgreSQL becomes the right answer** (not now):

- multiple gateway instances sharing one database,
- a public multi-tenant deployment where operators and data are separated,
- need for `LISTEN/NOTIFY`, row-level security, or a read replica.

PostgreSQL is the migration target if that day comes. The schema and SQL are
almost identical, so this is a controlled change rather than a redesign.

**ClickHouse only as a later analytics sink**, when at least one of these is
true: raw request logs past 100M rows, ad-hoc queries over months of data, or a
product feature that would embarrass a row store. The write path
(`usage_events` is append-only) can be dual-written to ClickHouse later without
touching the transactional core.

## 4. Concrete storage layout for v1

```
data/
  gateway.db            (SQLite, WAL, foreign_keys=ON)
  gateway.db-wal
  backups/
  bodies/               optional request bodies, capped, 1-day retention
```

- Busy timeout and WAL mode configured explicitly; `synchronous = NORMAL`
  (documented: survives process crash, not machine power loss; acceptable for
  usage data, and WAL fsync on every commit is the safer setting if power loss
  matters).
- One writer connection pool, several reader connections; keep transactions short.
- Rollups: hourly or daily, idempotent (rebuildable from `usage_events`).
- Aggregations for the admin dashboard read rollups; raw rows only for the
  per-request log view.
- Bodies stored outside the database (compressed files) so the DB never grows
  from megabyte-sized blobs.

## 5. Migration triggers (re-evaluate when)

| Signal | Move to |
| --- | --- |
| Second gateway instance | PostgreSQL for the transactional core |
| Reports scan months of raw events | Pre-built rollups, then optional ClickHouse |
| Body capture grows beyond retention policy | Offload bodies to object storage |
| More than ~100 concurrent writes/sec sustained | PostgreSQL, then consider a queue for events |

## 6. Decision

Use **SQLite (WAL) as the single system of record** for v1, with daily rollup
tables for reporting and optional 1-day body capture stored as files. Defer
ClickHouse; do not use MongoDB or SurrealDB as the primary store.
