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

- Rust 1.99+ (stable), `just`
- Node 24+ and `pnpm`

## Run

Backend:

```bash
cd server
GATEWAY_MASTER_KEY=$(openssl rand -base64 32 | tr '+/' '-_' | tr -d '=')
GATEWAY_DATA_DIR=./data GATEWAY_LISTEN=127.0.0.1:8081 cargo run
curl http://127.0.0.1:8081/health
```

`GATEWAY_MASTER_KEY` is required: it encrypts provider credentials at rest
(decision D16), and startup fails without it.

Frontend (development):

```bash
cd web
pnpm install
pnpm start
```

First run creates `data/gateway.db` (SQLite, WAL) and applies migrations.

## Production build

```bash
just build             # one binary: server/target/release/gateway
just build-api         # api-only binary (no embedded UI)
```

`just build` runs the Angular build first (pnpm) and embeds it in the binary,
so production is a single artifact. The task runner is `just` (see
`Justfile`); `just check` is the full quality gate and `just` lists every
recipe. Size (release, `lto = "thin"`, `strip`; measured 2026-11-16 at
`d864224`):

| Build | Command | Size |
| --- | --- | --- |
| with UI embedded | `just build` | 18.85 MiB (19,765,096 B) |
| api-only | `just build-api` | 18.15 MiB (19,033,768 B) |

The embedded UI adds 714 KiB (731,328 B); ship the single binary unless a
target really cannot spare it. Superseded: the earlier numbers here (7.1 MB /
6.7 MB / −455 KB) were true 2026-11-16 at `2b57725` (M0.1), before the M1–M5
dependencies grew the binary about 3x.

Serving rules: hashed assets (`main-<hash>.js`) are served with
`Cache-Control: immutable`, `index.html` with `no-store`, and any path without a
file extension falls back to `index.html` so Angular routes survive a reload.

## Environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `GATEWAY_LISTEN` | `127.0.0.1:8081` | Bind address |
| `GATEWAY_DATA_DIR` | `./data` | Database directory |
| `GATEWAY_MASTER_KEY` | **required** | 32 bytes (base64url or hex) that encrypt provider credential secrets (AES-256-GCM). Startup fails without it. Generate: `openssl rand -base64 32 \| tr '+/' '-_'` |
| `GATEWAY_TIMEZONE` | `UTC` | IANA timezone (e.g. `Asia/Tehran`) for budget-period boundaries and usage-rollup days. Startup fails on an unknown zone. |
| `GATEWAY_INSECURE_COOKIES` | unset | Set to `1` to send session cookies without `Secure`, so the gateway works over plain HTTP during development |
| `GATEWAY_TRUSTED_PROXY` | unset | Comma-separated IPv4/IPv6 CIDRs of reverse proxies allowed to vouch for the client address via `X-Forwarded-For`/`X-Real-IP` (e.g. `10.0.0.0/8,127.0.0.1/32`). This decides the client IP used for login rate limiting and audit rows: when unset, the peer address is the client and those headers are ignored ([review finding 4](docs/reviews/2026-11-16-m1-backend.md)). Startup warns when listening on a non-loopback address without it. |
| `GATEWAY_MAX_BODY_BYTES` | `4194304` (4 MiB) | Largest `POST /v1/chat/completions` request body |
| `GATEWAY_TIMEOUT_CONNECT_MS` | `10000` | Upstream TCP connect timeout |
| `GATEWAY_TIMEOUT_FIRST_BYTE_MS` | `30000` | Budget from upstream request start to the first response byte |
| `GATEWAY_TIMEOUT_IDLE_MS` | `60000` | Maximum gap between upstream body chunks |
| `GATEWAY_TIMEOUT_TOTAL_MS` | `600000` | Total budget for one client request, failover attempts included |
| `GATEWAY_TELEMETRY_REFRESH_MS` | `30000` | How often the per-offering routing telemetry (TTFT, tokens/sec, latency, failure rate) rebuilds from `usage_events` |
| `GATEWAY_BREAKER_THRESHOLD` | `5` | Consecutive failures before a (credential, offering) circuit breaker excludes the pair from routing |
| `GATEWAY_BREAKER_COOLDOWN_MS` | `60000` | How long an open circuit breaker excludes the pair before a probe |
| `RUST_LOG` | `gateway=info` | Log filter |

## Checks

```bash
just check     # Rust gate + api-only build + UI build/tests/formatting
just verify    # just check, then the production binary
```

`just` lists every recipe (`just build`, `just test`, `just dev`, `just dev-ui`,
`just ui-test`, ...).

## Network note: crates.io

From this network, `index.crates.io` and `static.crates.io` do not respond
(re-checked 2026-11-16: still blocked), so cargo cannot fetch dependencies.
`pnpm`, `git`, and GitHub are reachable. Two known workarounds:

1. Build offline from the local cargo cache (what we use now): every command
   that touches dependencies carries `--offline`, for example
   `cargo build --offline`. Dependencies were resolved with
   `cargo add --offline ...` against the cached index.
2. If you add a crate that is not in the cache, either reach crates.io from
   another network, or add a sparse mirror in `.cargo/config.toml` and drop
   `--offline`.

## Status

Milestones M0–M5 of [`PLAN.md`](PLAN.md) are implemented and committed (10
commits, `520b60c`..`d864224`, as of 2026-11-16): auth and audit, provider
catalog with encrypted credentials and OpenRouter sync, the streaming
`/v1/chat/completions` data plane with budgets and usage accounting, and
weighted/combo routing — each with its admin UI. The per-milestone status
table with closing commits and the open follow-ups live in the
[`PLAN.md` status table](PLAN.md#status).
