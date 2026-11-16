# pareto-ai — task runner. Run `just` to list recipes.
#
# crates.io is unreachable from this network: every cargo command runs with
# --offline against the local cache (docs/research/09-environment.md).
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

# run the backend (needs GATEWAY_MASTER_KEY; see README)
dev *args:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${GATEWAY_MASTER_KEY:?set GATEWAY_MASTER_KEY (32 bytes, base64url or hex) — see README}"
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

# what a release would run: the full gate, then the production binary
verify: check build

# ── Housekeeping ────────────────────────────────────────────────────────────

# remove build outputs and local runtime data
clean:
    rm -rf server/target web/dist web/.angular data
