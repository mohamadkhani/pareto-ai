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

# run both halves for development: gateway on 127.0.0.1:8081 (./data, first-run
# setup wizard, master key generated on first start) plus the Angular dev server
# on http://localhost:4200, which proxies /api, /v1 and /health to the gateway.
# Extra arguments go to `cargo run` (e.g. `just dev --release`). Ctrl-C stops both.
dev *args:
    #!/usr/bin/env bash
    set -euo pipefail

    if [ ! -d web/node_modules ]; then
        echo "error: web/node_modules is missing; run 'just setup' first" >&2
        exit 1
    fi

    # The backend embeds web/dist at compile time, so the crate does not build
    # without it. Build it once on a fresh clone; the :4200 dev server does not
    # need it, only the binary's own copy served on :8081.
    if [ ! -f web/dist/web/browser/index.html ]; then
        echo "==> no UI build found; building it once for the embedded copy"
        ( cd web && pnpm run build )
    fi

    pids=()

    # Deepest child first: `pnpm start` and `cargo run` spawn children that
    # survive a plain kill of the wrapper (leftover `ng serve` has bitten us).
    kill_tree() {
        local pid="$1" child
        if command -v pgrep >/dev/null 2>&1; then
            for child in $(pgrep -P "$pid" 2>/dev/null); do
                kill_tree "$child"
            done
        fi
        kill "$pid" 2>/dev/null || true
    }

    stop() {
        trap - EXIT INT TERM
        for pid in "${pids[@]}"; do
            kill_tree "$pid"
        done
        for pid in "${pids[@]}"; do
            wait "$pid" 2>/dev/null || true
        done
    }

    trap stop EXIT
    trap 'exit 130' INT TERM

    ( cd server && exec cargo run --offline {{args}} ) &
    api=$!
    pids+=("$api")

    ( cd web && exec pnpm start ) &
    ui=$!
    pids+=("$ui")

    echo "==> gateway  http://127.0.0.1:8081  (UI embedded from the last build)"
    echo "==> console  http://localhost:4200   (Vite dev server, proxies /api, /v1, /health)"

    # Exit as soon as either half dies: a crashed backend under a live dev
    # server looks like a broken app, and a dead dev server is easy to miss.
    while kill -0 "$api" 2>/dev/null && kill -0 "$ui" 2>/dev/null; do
        sleep 0.5
    done

    status=0
    if kill -0 "$api" 2>/dev/null; then
        wait "$ui" || status=$?
        echo "==> UI dev server exited (status $status); stopping the gateway" >&2
    else
        wait "$api" || status=$?
        echo "==> gateway exited (status $status); stopping the UI dev server" >&2
    fi
    exit "$status"

# reset a dev password in server/data (pass an email if there are several)
# Prompts twice for the new password: `just reset-password admin@example.com`
# The command embeds no UI, so it also builds on a checkout without web/dist.
reset-password email="":
    cd server && cargo run --offline --no-default-features -- reset-password {{ if email != "" { "--email " + quote(email) } else { "" } }}

# api-only binary, no embedded UI
build-api:
    cd server && cargo build --release --offline --no-default-features

# production binary with the Angular UI embedded (single artifact)
build: ui-build
    cd server && cargo build --release --offline
    upx ./server/target/release/gateway
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

# ── Desktop shell (Tauri v2, D43) ───────────────────────────────────────────
#
# desktop/ bundles the unchanged gateway as a Tauri sidecar. Only the host
# platform can be built here (Linux); Windows and macOS installers come from
# .github/workflows/desktop.yml. The desktop crate has its own Cargo.lock and
# downloads its crates on the first run, so these recipes do not use --offline.

# install the Tauri CLI used by the desktop recipes (pnpm)
desktop-setup:
    cd desktop && pnpm install

# build the gateway and place it where Tauri expects the sidecar
desktop-sidecar: ui-build
    #!/usr/bin/env bash
    set -euo pipefail
    cd server
    cargo build --release --offline
    triple="$(rustc -vV | sed -n 's/^host: //p')"
    ext=""
    case "$triple" in *windows*) ext=".exe" ;; esac
    mkdir -p ../desktop/binaries
    cp "target/release/gateway${ext}" "../desktop/binaries/pareto-gateway-${triple}${ext}"
    echo "sidecar: desktop/binaries/pareto-gateway-${triple}${ext}"

# run the desktop shell against a locally built gateway
desktop-dev: desktop-sidecar
    cd desktop && pnpm tauri dev

# build the desktop installers for this host (deb, rpm, AppImage on Linux)
desktop-build: desktop-sidecar
    cd desktop && pnpm tauri build

# format, lint and test the desktop shell (no bundling, no webview build)
desktop-check:
    cd desktop && cargo fmt --check
    cd desktop && cargo clippy --all-targets -- -D warnings
    cd desktop && cargo test

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
