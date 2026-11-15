#!/usr/bin/env bash
# Production build: Angular first (its output is embedded in the binary),
# then the Rust binary.
#
#   scripts/build.sh                 # binary with the UI embedded
#   scripts/build.sh --no-default-features   # api-only binary
#
# CARGO_FLAGS overrides the default `--offline`, which is needed on networks
# where crates.io is unreachable (see README).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> building the Angular app"
(cd "$root/web" && npm run build)

echo "==> building the gateway binary"
(cd "$root/server" && cargo build --release "${CARGO_FLAGS:---offline}" "$@")

echo "==> done: $root/server/target/release/gateway"
ls -la "$root/server/target/release/gateway"
