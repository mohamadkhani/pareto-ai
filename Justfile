# pareto-ai — task runner. Run `just` to list recipes.
#
# Dependencies are pinned in server/Cargo.lock and present in the local cargo
# cache, so every cargo command runs with --offline (reproducible, no network).
# crates.io is reachable again, so a new dependency needs one online
# `cargo add`; drop --offline only for that.
# JavaScript tooling is pnpm (user preference, docs/research/11-user-preferences.md).

set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# ── Rust backend ────────────────────────────────────────────────────────────

# format the backend code
fmt:
    cd server && cargo fmt

# check formatting without writing
fmt-check:
    cd server && cargo fmt --check

# lint with clippy; warnings are errors
lint:
    cd server && cargo clippy --offline --all-targets -- -D warnings

# run the backend tests
test:
    cd server && cargo test --offline

# run the backend with no configuration: 127.0.0.1:8081, ./data, first-run
# setup wizard (the master key is generated on first start; see README)
dev *args:
    #!/usr/bin/env bash
    set -euo pipefail
    cd server
    exec cargo run --offline {{args}}

# api-only binary, no embedded UI
build-api:
    cd server && cargo build --release --offline --no-default-features

# production binary with the Angular UI embedded (single artifact)
build: ui-build
    cd server && cargo build --release --offline
    @ls -la server/target/release/gateway

# ── Angular frontend (pnpm) ─────────────────────────────────────────────────

# install web dependencies
setup:
    cd web && pnpm install

# build the UI (web/dist/web/browser is embedded into the binary)
ui-build:
    cd web && pnpm run build

# run the UI tests
ui-test:
    cd web && pnpm test

# check UI formatting
ui-fmt:
    cd web && pnpm exec prettier --check src/

# dev server with the API proxy to 127.0.0.1:8081
dev-ui:
    cd web && pnpm start

# ── Gates ───────────────────────────────────────────────────────────────────

# everything a change must pass before it counts as done
check: fmt-check lint test build-api ui-check
    @echo "==> all checks passed"

# UI gate: build + tests + formatting
ui-check: ui-build ui-test ui-fmt

# M3 acceptance: memory stays flat under 200 concurrent streaming requests.
# Runs the ignored in-process load test (server/src/load_test.rs) against the
# stub upstream: two waves of 200 streams, sampling /proc/self RSS and fds
# before/during/after and asserting bounded growth and no fd leaks. Linux
# only. Runtime ~10 s on this machine (two ~4 s waves; the debug test binary
# is reused from `just test`, a cold compile adds a couple of minutes).
load-test:
    cd server && cargo test --offline load_test -- --ignored --test-threads=1 --nocapture

# what a release would run: the full gate, then the production binary
verify: check build

# live smoke test against real providers (needs OPENROUTER_API_KEY,
# SAIL_API_KEY, ADMIN_PASSWORD; see docs/live-verification.md)
live-smoke:
    scripts/live-smoke.sh

# performance probe: per-request query inventory, latency/RPS at concurrency
# 1/10/50/200, stage microbenchmarks, and the SQLite write path. Prints tables;
# never fails on a slow machine. See docs/research/13-performance.md.
# `just bench` runs the whole probe; `just bench wal_growth_under_sustained_writes`
# runs the WAL-growth probe alone.
bench filter="perf_bench" *args:
    cd server && cargo test --offline --release {{filter}} -- --ignored --nocapture --test-threads=1 {{args}}

# ── Housekeeping ────────────────────────────────────────────────────────────

# remove build outputs and local runtime data
clean:
    rm -rf server/target web/dist web/.angular data
