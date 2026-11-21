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
| TLS | Off on the loopback default. A public bind with `GATEWAY_PUBLIC_DOMAIN` obtains a certificate automatically (*HTTPS on a domain* below) |

Authentication modes:

| Mode | Meaning | How it ends |
| --- | --- | --- |
| `setup` | No account exists; only the wizard and login routes answer. | Create the first admin. |
| `unprotected` | Setup was explicitly skipped. `/api/*` and `/v1/*` act as a trusted local operator; sessions stay off. Account creation is blocked in this mode. | Set a password (wizard or Settings) — creates the first admin. |
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

### Locked out as the only admin

Password resets are an admin action (D2), so there is no email flow. If the last
admin has forgotten the password, reset it on the host that holds the database:

```bash
./server/target/release/gateway reset-password --email admin@example.com
# when the database has exactly one real account, the email can be omitted
# (the local-operator placeholder does not count):
./server/target/release/gateway reset-password --data-dir /srv/pareto/data
```

The command prompts for the new password twice (`stdin` provides one line when
it is not a terminal, for containers). It deletes the account's sessions,
revokes its live API keys, and forces a new password at the next sign-in (D56).
The password is never accepted as an argument, so the gateway's own command line
cannot leak it through `ps` or shell history. When stdin is a pipe, read the
value from a file or a secret mount: a literal such as `echo '…' | gateway
reset-password` is recorded by the shell that runs it. Run the binary from the
same release as the server, because opening the database applies migrations. The
desktop build keeps its data under the OS application-data directory listed in
[`docs/desktop.md`](docs/desktop.md); point `--data-dir` there to reset the
sidecar's database. Published Arch packages install the binary as
`pareto-gateway`; use that name in place of the local build path above.

#### When no administrator exists

A database can contain accounts but no administrator (for example, an older
install that created a non-admin account before setting a password). On startup
it counts as `protected`, so the setup wizard is closed. Reset one account's
password, then promote that account directly:

```bash
systemctl stop pareto-gateway   # or stop the local process
pareto-gateway reset-password --email you@example.com --data-dir /srv/pareto/data
sqlite3 /srv/pareto/data/gateway.db \
  "UPDATE users SET role = 'admin', is_active = 1 WHERE email = 'you@example.com';"
systemctl start pareto-gateway
```

Stop the gateway first so this write does not race the server's own connection.
The account then signs in with the password from the reset; forced rotation is
still set, so it chooses a new password at sign-in. Packaged Arch installs keep
the database under `/var/lib/pareto-gateway` (the unit sets
`GATEWAY_DATA_DIR`), so use that directory instead of `/srv/pareto/data`.

### HTTPS on a domain

When the gateway is meant to be reached at a public hostname, it can manage its
own certificate (decision D54). Point the domain's DNS at the host, open ports
80 and 443, and start with:

```bash
GATEWAY_LISTEN=0.0.0.0:8081 \
GATEWAY_PUBLIC_DOMAIN=gateway.example.com \
GATEWAY_ACME_EMAIL=you@example.com \
./server/target/release/gateway
```

What happens, in order:

1. The normal HTTP listener on `GATEWAY_LISTEN` starts immediately; nothing
   waits for the certificate authority.
2. A background task binds port 80, answers the ACME HTTP-01 challenge for the
   domain, and asks Let's Encrypt for a certificate. The account key and the
   certificate live in `<data>/tls/` (`account.json`, `fullchain.pem`,
   `privkey.pem`; the private files are mode `0600`).
3. Once a certificate exists, the gateway serves the same console and API over
   HTTPS on `GATEWAY_HTTPS_PORT` and answers every other port-80 request with a
   `308` to `https://<domain>/…`. HTTPS responses carry
   `Strict-Transport-Security: max-age=31536000`.
4. Renewal starts 30 days before expiry and hot-swaps the TLS configuration;
   no restart is needed and in-flight connections finish on the old one.

The plain-HTTP listener on `GATEWAY_LISTEN` keeps serving. That is deliberate:
a reverse proxy on another port, a local operator, and the desktop app do not
lose access, and a failed renewal never takes the gateway down. `/api/setup/status`
reports `tls: {enabled, domain, reason, error, expires_at}`; `reason` is
`local`, `no_domain`, `off`, or `pending`, and a failure fills `error` while
`enabled` stays `false`. Settings shows the same facts, and the console shows a
banner when a public bind is serving plain HTTP or a renewal is failing.

Requirements and limits:

- Ports 80 and 443 must be reachable from the internet; binding port 80 usually
  needs root (or `CAP_NET_BIND_SERVICE` / a systemd socket).
- A loopback, RFC 1918, or link-local bind is never upgraded: a public CA cannot
  validate a private address. TLS stays off with reason `local`.
- The domain must resolve to this host and answer `/.well-known/acme-challenge/*`
  over port 80. A firewall or proxy that swallows port 80 makes HTTP-01 fail.
- Let's Encrypt rate-limits issuance. For a first try, or while changing DNS,
  use `GATEWAY_ACME_DIRECTORY=https://acme-staging-v02.api.letsencrypt.org/directory`;
  staging certificates are not trusted by browsers.
- Live issuance was **not** verified in this repository's test environment (no
  public domain and no ports 80/443). The decision matrix, the challenge
  endpoint, the redirect, the certificate store, renewal scheduling, and both
  issuance outcomes are covered with unit tests and a fake provider; verify the
  network path on a real host as described above.
- The desktop build stays on plain HTTP over loopback; see
  [`docs/desktop.md`](docs/desktop.md).

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
| `GATEWAY_INSECURE_COOKIES` | unset | Set to `1` to send session cookies without `Secure`, so the gateway works over plain HTTP during development. Without it cookies are `Secure` whenever TLS is active. |
| `GATEWAY_TLS` | `auto` | `auto` obtains a Let's Encrypt certificate when the bind is public and `GATEWAY_PUBLIC_DOMAIN` is set; `off` keeps plain HTTP on every bind. A loopback/private bind never gets TLS, even with a domain. |
| `GATEWAY_PUBLIC_DOMAIN` | unset | Public hostname for the certificate (e.g. `gateway.example.com`). Must point at this host and be reachable on ports 80 and 443. |
| `GATEWAY_ACME_EMAIL` | unset | Contact address registered with the ACME account (Let's Encrypt expiry notices). Optional but recommended. |
| `GATEWAY_HTTPS_PORT` | `443` | Port HTTPS is served on. Port 80 stays reserved for ACME HTTP-01 challenges and the HTTP→HTTPS redirect. |
| `GATEWAY_ACME_DIRECTORY` | Let's Encrypt production | ACME directory URL. Use `https://acme-staging-v02.api.letsencrypt.org/directory` for a staging dry run. |
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

## Contributing

`main` is always releasable and every change lands through a short-lived branch
and a pull request that passes CI. The workflow, branch names, commit format and
release process are in [`CONTRIBUTING.md`](CONTRIBUTING.md); the engineering
rules are in [`AGENTS.md`](AGENTS.md). Current state, open items and the reading
order for a new session are in [`docs/HANDOFF.md`](docs/HANDOFF.md).

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
