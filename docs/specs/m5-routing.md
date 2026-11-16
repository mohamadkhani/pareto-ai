# Spec: M5 — smart routing

Status: draft (2026-11-16). Owner: chief-of-staff. Conventions:
[`AGENTS.md`](../../AGENTS.md). Decisions: D4 (quality on models, price/speed
on offerings), D5 (speed measured locally, quality imported), D6 (three
strategies), scoring design in
[04-architecture.md](../research/04-architecture.md) §6 and OmniRoute's
`speedRanking`/`routingStrategies` patterns
([01-omniroute-assessment.md](../research/01-omniroute-assessment.md)).

## Goal

Requests are routed to the best offering by configurable weights over price,
speed, quality, and reliability — deterministically, with a stored explanation
of every decision, plus ordered-fallback and round-robin strategies.

## Non-goals

Compression, ML-based routing, cross-region routing, per-task routing
categories (only generic quality for now).

## Scope

1. **Passive telemetry** per offering: TTFT, tokens/sec, end-to-end latency,
   failure rate, p95 — rolling window of the last 200 samples (bounded),
   derived from `telemetry_attempts`, one sample per classified upstream
   attempt written with the settle transaction (per **D26** failures rescued
   by failover still count, and 402/429 are attributed to the credential, not
   the offering). Hot in-memory cache with periodic snapshots to
   `telemetry_snapshots` for warm starts.
2. **Combo models**: `combo_models(id, name, strategy, weights_json,
   members_json, enabled, created_at, updated_at)` where `strategy ∈
   {'ordered','weighted','round_robin'}` and members are offerings/models with
   optional per-member weight and ordering. A combo is addressable as a model
   id in `/v1/chat/completions`.
3. **Scoring** (weighted strategy): filter first (capability, context, grants,
   budget, breaker, enabled), then normalize price/speed/quality/reliability to
   `[0,1]` across the eligible pool, missing components get the neutral value
   0.5 and are marked imputed, penalties for recent failures/breaker/low
   quota; deterministic tie-break (lower price → lower p95 → stable id).
   Presets: `cheapest`, `fastest`, `balanced`, `best` — per **D24** the
   single-component presets apply no penalties (health is handled by the
   filters). Per **D27**, telemetry metrics with fewer than 10 samples shrink
   toward 0.5 by `n/(n+10)`.
   - Deliberate deviation (documented in the module docs): **inside a combo**,
     an `upstream_auth` (401/403) failure advances to the next member (the
     member list is the operator's explicit fallback chain); plain models keep
     M3's "401/403 never fail over" rule.
4. **Explanation**: `request_logs.decision_json` records candidates, each
   component score, the winner, and one-line reasons (why this one, why not the
   others); surfaced in the admin UI ("why this model").
5. **Benchmarks**: `benchmarks(model_id, source, category, score, ci, rank,
   fetched_at, source_url)` with an importer that reads a recorded fixture
   (LMArena-style Elo) — live fetch blocked in this environment
   ([09-environment.md](../research/09-environment.md)). User override per
   model wins over synced scores.
6. **Circuit breaker per (credential, offering)**: after N consecutive
   failures exclude for a cooldown window; recorded in the decision JSON.
   Only failures that reflect the offering's service count — `invalid_request`
   (the caller's request) and `internal` (our configuration) never trip it.

## Acceptance

1. `scripts/check.sh` passes.
2. Same input → same ordering (property test over a fixed pool).
3. Weight change flips the winner in a fixture test; presets match their
   documented intent.
4. A breaker-open offering is excluded and the reason is visible in
   `decision_json`.
5. Combo resolution works end-to-end through `/v1/chat/combos` or the model
   id namespace `combo:<name>` (pick one, document it).
6. Benchmark import is idempotent and user overrides are never overwritten.
