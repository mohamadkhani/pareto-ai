# Research log — fast, stable, cost-aware LLM gateway

Started: 2026-11-15
Status: discovery / design. No implementation code yet.
Target: internal team gateway (admin-managed users, per-user budgets and API keys).

## Why this log exists

Every finding below is tied to a source and a date so later decisions can be
re-checked instead of re-litigated. When a fact is not verified, it is marked
**UNVERIFIED**.

## Documents

| File | Question it answers |
| --- | --- |
| [01-omniroute-assessment.md](01-omniroute-assessment.md) | What OmniRoute is, how it is built, what is safe to reuse, what to avoid |
| [02-product-scope.md](02-product-scope.md) | Which requested features are core, which are missing, MVP order |
| [03-data-sources.md](03-data-sources.md) | Where price, speed, and benchmark data can come from, and their limits |
| [04-architecture.md](04-architecture.md) | Rust/Angular design, data model, routing math, quota model |
| [05-decisions.md](05-decisions.md) | Decisions taken, open questions, next actions |
| [06-database-choice.md](06-database-choice.md) | MongoDB vs SurrealDB vs ClickHouse vs SQLite |
| [07-scope-answers.md](07-scope-answers.md) | Credential balance, org policy limits, provider scope, pricing pipeline, body retention |
| [08-surrealdb-deep-dive.md](08-surrealdb-deep-dive.md) | SurrealDB: license, storage churn, durability, transactions, verdict |
| [09-environment.md](09-environment.md) | Dated environment knowledge: network, toolchain, cached crates, build quirks |
| [10-milestones.md](10-milestones.md) | Orchestration log: runs, review findings, commits |

Raw review artifacts live in [`../reviews/`](../reviews/) and are cited by the
milestone log; specs live in [`../specs/`](../specs/).

## Method

1. Inspect the local clone under `OmniRoute/` and the upstream repository
   (default branch `release/v3.8.54`, commit `23b8c42`).
2. Use the GitHub REST API for the repository tree, package manifest, license,
   provider registry, pricing sync, benchmark sync, routing modules, and the
   initial database schema.
3. Cross-check product claims against code, not the README alone.
4. Record what was read, what was inferred, and what is still unknown.

## Environment note

- The local clone is 3.4 GB and filled `/home` to 100%. `git checkout` did not
  finish, so the working tree is partial. Source reads for this log were done
  through the GitHub API against the same commit.
- These notes live in `/tmp/opencode/research` until disk space is freed; they
  should be moved into `docs/research/` in the project afterwards.

## Sources used

- Repository metadata: <https://api.github.com/repos/cheaper-inference/OmniRoute>
- Repository tree: `GET /repos/cheaper-inference/OmniRoute/git/trees/release/v3.8.54?recursive=1`
- Package manifest: `package.json` at the same commit
- Provider registry: `open-sse/config/providers/shared.ts`,
  `open-sse/config/providers/registry/*/index.ts`
- Pricing sync: `src/app/api/pricing/sync/route.ts`,
  `src/lib/modelsDevSync/transform.ts`, `src/lib/catalog/openrouterCatalog.ts`
- Benchmark sync: `src/lib/arenaEloSync.ts`, `src/lib/arenaEloHfSource.ts`
- Routing: `open-sse/services/routingStrategies.ts`,
  `open-sse/services/autoCombo/speedRanking.ts`,
  `open-sse/services/combo/*` (≈140 files)
- Storage: `src/lib/db/migrations/001_initial_schema.sql` and the migration list
  (241 migrations at this commit)
- Licensing: `LICENSE`, `THIRD_PARTY_NOTICES.md`
