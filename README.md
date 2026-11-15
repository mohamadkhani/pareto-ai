# pareto-ai

Self-hosted AI gateway for an internal team. One OpenAI-compatible endpoint in
front of several providers, with price/speed/quality-driven model selection,
fallback, per-user budgets, and admin oversight. The goals are speed, low
resource use, and stability — not feature breadth.

- Backend: Rust (axum + SQLite/WAL), single binary.
- Frontend: Angular, built to static assets and served by the backend.
- Plan: [`PLAN.md`](PLAN.md).
- Research log: [`docs/research/`](docs/research/README.md).

## Requirements

- Rust 1.99+ (stable)
- Node 24+ and npm

## Run

Backend:

```bash
cd server
GATEWAY_DATA_DIR=./data GATEWAY_LISTEN=127.0.0.1:8081 cargo run
curl http://127.0.0.1:8081/health
```

Frontend (development):

```bash
cd web
npm start
```

First run creates `data/gateway.db` (SQLite, WAL) and applies migrations.

## Production build

```bash
scripts/build.sh            # one binary: server/target/release/gateway
```

The Angular build is embedded in the binary, so production is a single
artifact. Size at the time of writing (release, `lto = "thin"`, `strip`):

| Build | Command | Size |
| --- | --- | --- |
| with UI embedded | `scripts/build.sh` | 7.1 MB |
| api-only | `scripts/build.sh --no-default-features` | 6.7 MB |

The UI costs about 455 KB; ship the single binary unless a target really
cannot spare it.

Serving rules: hashed assets (`main-<hash>.js`) are served with
`Cache-Control: immutable`, `index.html` with `no-store`, and any path without a
file extension falls back to `index.html` so Angular routes survive a reload.

## Environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `GATEWAY_LISTEN` | `127.0.0.1:8081` | Bind address |
| `GATEWAY_DATA_DIR` | `./data` | Database directory |
| `GATEWAY_MASTER_KEY` | unset | 32-byte key for credential encryption (milestone 2) |
| `GATEWAY_TIMEZONE` | `UTC` | Timezone for budget periods |
| `RUST_LOG` | `gateway=info` | Log filter |

## Checks

```bash
cd server
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Network note: crates.io

From this network, `index.crates.io` and `static.crates.io` do not respond
(re-checked 2026-11-16: still blocked), so cargo cannot fetch dependencies.
`npm`, `git`, and GitHub are reachable. Two known workarounds:

1. Build offline from the local cargo cache (what we use now): every command
   that touches dependencies carries `--offline`, for example
   `cargo build --offline`. Dependencies were resolved with
   `cargo add --offline ...` against the cached index.
2. If you add a crate that is not in the cache, either reach crates.io from
   another network, or add a sparse mirror in `.cargo/config.toml` and drop
   `--offline`.

`npm` works normally from this network (verified with `npm ping`).

## Status

Milestones 0 and 0.1 of [`PLAN.md`](PLAN.md) are implemented: server skeleton,
config from environment, SQLite with migrations, `/health`, `/openapi.json`, the
Angular scaffold, and the single-binary production build that embeds the UI.
