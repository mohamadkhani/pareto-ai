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

With no configuration at all:

```bash
just dev            # backend + Angular dev server; needs `just setup` once
```

That starts two processes:

| Process | Address | Notes |
| --- | --- | --- |
| gateway | `127.0.0.1:8081` | database in `./data`, migrations applied, credential master key generated; serves the UI embedded from the last `just ui-build` |
| Angular dev server | `http://localhost:4200` | hot reload; proxies `/api`, `/v1`, `/health` to the gateway |

Use `http://localhost:4200` while working on the console. Ctrl-C stops both
processes. `just dev --release` and other arguments go to `cargo run`. For a
backend-only run use `cd server && cargo run`.

On the first visit the console opens the **setup wizard**: create the first
administrator, or choose "Skip for now — local only" to run without a password
on this machine. The choice is remembered; Settings can set a password later.

Serving the production binary is the same:

```bash
just build
./server/target/release/gateway
curl http://127.0.0.1:8081/health
```

### Start with no configuration

| What | Where it comes from |
| --- | --- |
| Listen address | `127.0.0.1:8081` (`GATEWAY_LISTEN` overrides) |
| Data | `./data/gateway.db`, created with migrations (`GATEWAY_DATA_DIR` overrides) |
| Credential master key | `GATEWAY_MASTER_KEY` when set; otherwise generated on first start into `./data/master.key` (mode `0600`) and reused. Only the path is logged. |
| First account | The setup wizard when no user exists (`GATEWAY_ADMIN_EMAIL` + `GATEWAY_ADMIN_PASSWORD` still create it non-interactively at startup) |

Authentication modes:

| Mode | Meaning | How it ends |
| --- | --- | --- |
| `setup` | No account exists; only the wizard and login routes answer. | Create the first admin. |
| `unprotected` | Setup was explicitly skipped. `/api/*` and `/v1/*` act as a trusted local operator; sessions stay off. | Set a password (wizard or Settings) — creates the first admin. |
| `protected` | At least one account exists; session auth and CSRF apply as usual. | — |

Exposure rule: binding a **non-loopback** address while in `setup` or
`unprotected` refuses to start, because every route would be reachable without
a password. Set `GATEWAY_ALLOW_PUBLIC_NO_AUTH=1` to start anyway (not
recommended); the console then shows a red banner until a password is set.
Loopback peers and desktop-wrapper origins (`tauri://localhost`,
`https://tauri.localhost`, `wails://...`) count as local for the wizard and the
banner. With the override, setup still has to be finished **locally** (or with
the `GATEWAY_ADMIN_*` shortcut): the wizard's writes are loopback-only, and a
cross-site `Origin` is refused so a random web page cannot claim the first
account.

Frontend (development, proxying the API to `127.0.0.1:8081`):

```bash
cd web
pnpm install
pnpm start
```

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

### Prebuilt server binaries

CI (`.github/workflows/ci.yml`, the `gateway` job) builds and uploads both
server binaries for Linux and Windows. The version in the names comes from
`server/Cargo.toml`:

| Artifact | Contents |
| --- | --- |
| `pareto-gateway-<version>-linux-x86_64` (`.tar.gz`) | `pareto-gateway` (embedded UI), `pareto-gateway-api` (api-only), `LICENSE`, `README.md` |
| `pareto-gateway-<version>-windows-x64` (`.zip`) | `pareto-gateway.exe` (embedded UI), `pareto-gateway-api.exe` (api-only), `LICENSE`, `README.md` |

Download them from the workflow run's **Artifacts** section, or from the GitHub
Release when the commit is tagged `v*`. The embedded-UI binary is a single file:
run it and open `http://127.0.0.1:8081/` (see *Run* above). See
[`docs/desktop.md`](docs/desktop.md) for the desktop installers.

## Environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `GATEWAY_LISTEN` | `127.0.0.1:8081` | Bind address |
| `GATEWAY_DATA_DIR` | `./data` | Database directory |
| `GATEWAY_MASTER_KEY` | generated | 32 bytes (base64url or hex) that encrypt provider credential secrets (AES-256-GCM). When absent, a key is generated on first start into `<data>/master.key` (mode `0600`) and reused; an explicit value wins. To supply one: `openssl rand -base64 32 \| tr '+/' '-_'` |
| `GATEWAY_ADMIN_EMAIL` / `GATEWAY_ADMIN_PASSWORD` | unset | Optional non-interactive shortcut: when **both** are set and no user exists, the first admin is created at startup (flagged for password rotation). Otherwise the setup wizard runs. Half a configuration fails startup. |
| `GATEWAY_ALLOW_PUBLIC_NO_AUTH` | unset | Set to `1` to start on a non-loopback bind while in `setup` or `unprotected` mode. Without it such a start is refused; with it the console keeps a red exposure banner. |
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

The console in `web/` is styled with indented Sass (`.sass`, D53): the global
entry is `web/src/styles.sass` and every component has its own `.sass` file. No
`.css` file may exist under `web/src` — a Vitest guardrail fails if one appears.
Prettier cannot format the indented syntax and is not used for it; the Angular
build is the syntax check. See [`web/README.md`](web/README.md).

## Dependencies and the cargo cache

`crates.io` is reachable again (2026-11-17), so `cargo add` works normally.
Every recipe still passes `--offline` because `server/Cargo.lock` plus the local
cache is enough to build, which keeps the gate reproducible and network-free.
Add a dependency with one online `cargo add`, commit the updated lockfile, and
the offline gate keeps working.

## License

**MIT** — see [`LICENSE`](LICENSE). Third-party notices (fonts under OFL,
LMArena data under CC BY 4.0, dependency licenses) are in
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

## Status

Milestones M0–M5 of [`PLAN.md`](PLAN.md) are implemented and committed (10
commits, `520b60c`..`d864224`, as of 2026-11-16): auth and audit, provider
catalog with encrypted credentials and OpenRouter sync, the streaming
`/v1/chat/completions` data plane with budgets and usage accounting, and
weighted/combo routing — each with its admin UI. The per-milestone status
table with closing commits and the open follow-ups live in the
[`PLAN.md` status table](PLAN.md#status).
