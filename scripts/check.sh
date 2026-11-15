#!/usr/bin/env bash
# Everything a change must pass before it counts as done.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/server"

echo "==> cargo fmt --check"
cargo fmt --check

echo "==> cargo clippy --offline --all-targets -- -D warnings"
cargo clippy --offline --all-targets -- -D warnings

echo "==> cargo test --offline"
cargo test --offline

echo "==> api-only build must still compile"
cargo build --offline --no-default-features

if [ -f "$root/web/dist/web/browser/index.html" ]; then
  echo "==> full build with embedded UI"
  cargo build --offline
else
  echo "==> (skipping embedded-UI build: web/dist/web/browser missing; run scripts/build.sh)"
fi

echo "==> all checks passed"
