# SurrealDB deep dive

Research date: 2026-11-15. Search: SearXNG; page reads: SearXNG + surrealdb.com;
source metadata: GitHub API.

## 1. What SurrealDB is today

| Fact | Value | Source |
| --- | --- | --- |
| Repo | `surrealdb/surrealdb`, Rust | GitHub API |
| Stars / forks | 33,309 / 1,407 | GitHub API |
| Issues | 352 open, 2,389 closed | GitHub search API |
| Latest release | v4.0.1 (2026-11-09), v4.0.0 (2026-11-03) | GitHub releases |
| Release volume | 51 releases in 2026, 45 in 2025 (last 100 releases) | GitHub releases |
| Parallel lines | 2.7, 3.2, 3.3 maintained at once | GitHub releases |
| Crate | `surrealdb` 4.0.1, 130 releases, ~190k downloads/month, 245 dependent crates | lib.rs |
| License (core) | **Business Source License 1.1**, change date 2030-01-01 → Apache 2.0 | repo `LICENSE` |
| License (SDKs) | Apache-2.0 / MIT | surrealdb.com licensing page |
| Funding | $38M Series A, Feb 2026, launched 3.0 | firstmark.com |

BSL 1.1 in this repo grants every use except "use the Licensed Work as a
Database Service" (offering database functionality to third parties). For an
internal team gateway that restriction normally does not apply, but it is **not
an OSI open-source license**, and a product decision to embed SurrealDB in a
commercial offering needs a legal read.

## 2. Correcting an earlier claim

My first pass said SurrealDB "has fast-moving APIs and breaking releases". The
evidence both supports and corrects that:

- Supported: major lines every ~12 months (2.x → 3.0 Mar 2026 → 4.0 Nov 2026),
  a documented "2.x to 3.x breaking changes, organised by severity level" guide,
  and 51 releases in 2026.
- Correction: the project also maintains several minor lines at once (2.7, 3.2,
  3.3) and ships patch rollups ("reliability rollup", "backporting fixes").
  Releases are frequent, but they are not unmanaged.

## 3. Storage engine churn (the strongest stability concern)

- RocksDB was the persistent engine until **October 2026**, replaced by
  **SurrealKV** (own engine, synchronous durability, grouped fsync pipeline).
- In-memory work happens in a separate engine, **SurrealMX**.
- Data on disk switches to the 4.0 format on first 4.0 start.
- Their own `surrealkit` documents a "datastore level" concept that changes on
  upgrade; the 2→3 migration guide exists precisely because upgrades are not
  free.

For a product whose headline is *stable*, two storage-engine replacements in one
year is the single biggest risk. On-disk format changes are where databases
lose data.

## 4. Durability controversy (2025–2026)

- Aug 2025: Reddit `r/rust` — "SurrealDB is sacrificing data durability to make
  benchmarks look better". Discussion in HN item 45005071: SurrealDB benchmark
  runs had fsync disabled; SQLite's own default was discussed alongside.
- Their reply: the framework supports sync and async commit modes.
- May 2026: SurrealDB blog "3.x by the numbers" — "We owe a word on durability.
  The previous round of benchmark results ran with fsync disabled for every
  engine."
- Their benchmarks page now states: "Every persistent benchmark runs in full
  durability mode. Each committed transaction is flushed and fsync'd."

Verdict: they fixed the measurement, but the incident is exactly the class of
problem (silent durability default) you must verify yourself rather than trust
in marketing. The same check applies to whatever engine we pick.

## 5. Known reliability reports in the wild

| Report | Date | Note |
| --- | --- | --- |
| Reddit `r/surrealdb` "Disappointed": "transitioning away … horrible memory leaks / general bugs leading to consistent downtime" | 2026-09-04 | Community, single account |
| 3.1 release note: "Fixed a memory leak where DEFINE INDEX would pin …" | 2026-09-01 | Upstream confirmation |
| 3.2 release note: RocksDB memory counted twice, pushing past `SURREAL_MEMORY_THRESHOLD`, "make an instance refuse every request" | 2026-10 | Upstream confirmation |
| Issue #7477: background reclaim task ballooned RSS to cgroup ceiling, OOM-killed (v3.2.4) | 2026-08-18 | Upstream issue, closed |
| Issue #6872: HNSW index creation fails after data corruption on surrealkv (v3.0.0-beta.3), 35 comments | 2026-02-11 | Closed |
| Issue #7621: export writes set literals that corrupt or abort on re-import (3.3.0) | 2026-10-23 | Open at read time |
| Issue #7694: CBOR RPC decoding truncates out-of-range integers, aliases record-id keys | 2026-11-12 | Open at read time |
| RustSec/GHSA (via vuln.management): "SurrealDB header confusion lets one tenant read another's data" | pre-1.5.5 | Multi-tenant isolation bug — relevant if we ever expose it directly |
| NPM/JSR package notes: "So don't use it in production!" (WASM engine context) | 2026 | Package-specific, not the server |

Open issues labelled `GitHub Bug`: 3 open / 179 closed — the team closes bugs
fast, but the absolute volume of open issues (352) is healthy for a project this
active.

## 6. Transaction semantics that matter for our reservation ledger

From their docs (read 2026-11-15):

- `BEGIN` / `COMMIT` / `CANCEL`, one isolation level: **snapshot isolation**,
  write conflicts detected at commit (so a busy reservation write loop needs a
  retry).
- No nested transactions, **no savepoints**; `BEGIN` inside a transaction aborts
  the outer one.
- `BEGIN`/`COMMIT` must live in one query. Multi-request transactions require a
  **client-side transaction over WebSocket or embedded** — not over HTTP.
- Commit can fail after the storage engine started writing: the error says
  "commit outcome is unknown; it may or may not have been applied", and it is
  **not marked retryable**. The application must read back its own writes to
  decide. That is a real implementation burden for budget settlement.
- Their own HA documentation: "Treat backups and a rehearsed restore as the
  recovery path, not in-instance redundancy."

## 7. What it would buy us (honestly)

Useful for this project:

- Embedded in-process mode → still a single binary.
- Built-in row/table/field permissions and record access → could replace parts
  of our authz layer (but we need application-level semantics for API keys and
  scopes anyway).
- Live queries → nice for a streaming dashboard, not required.
- Multi-model features (graph, vector, FTS, time series, geo) → **not needed**.
- `DEFINE API` / GraphQL → would push logic into the database, which fights a
  clean Rust service boundary.
- 3.3 added a **Postgres wire protocol**, so psql/dashboards can connect.

Costs:

- BSL license review.
- Storage-engine and storage-format churn (RocksDB → SurrealKV in 2026).
- Commit-outcome-unknown semantics to handle in settlement code.
- Snapshot-isolation write-conflict retries in the hottest path (budgets).
- Fewer people know SurrealQL than SQL; every debugging session becomes a
  learning session.
- We still must build the same schema, migrations, and aggregates.

## 8. Verdict (updated)

**Still SQLite for v1**, for reasons that survived the deeper look:

1. SurrealDB's strengths (multi-model, in-DB API layer, live queries) map to
   features we do not want.
2. Its weak spots land exactly on what we need: atomic, understandable commit
   semantics in a settlement path.
3. Its churn profile (51 releases in 2026, storage-engine replacement, storage
   format upgrade) contradicts the stability goal.

Re-evaluate SurrealDB when at least two of these hold:

- we need graph/vector search in the product itself,
- we need built-in multi-tenant permission enforcement at the field level,
- we run multiple instances and want one Rust-native store,
- the project has been quiet (no major release in 12 months) and a storage
  format upgrade path is documented and proven.

A time-boxed spike (half a day) is a legitimate way to test the reservation
ledger against real SurrealDB semantics — `BEGIN`, conflict retry, read-back
after unknown commit — before any commitment. Record the result here if we do it.

## 9. Unverified / open

- **UNVERIFIED:** whether SurrealDB has had an independent Jepsen analysis. No
  result found in search; their own messaging mentions "Jepsen-like chaos
  testing" performed internally.
- **UNVERIFIED:** exact semantics of SurrealKV durability defaults in the 4.0
  server (whitepaper claims synchronous durability; verify the shipped default).
- Reddit threads were not read directly (fetch timed out); claims come from
  search result excerpts and are marked as community reports.
