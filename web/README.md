# pareto-ai web console

Angular admin console for the pareto-ai gateway (last rewritten 2026-11-16 at
`d864224`). Plain CSS, standalone components, signals, strict TypeScript — no
UI framework. JavaScript tooling is pnpm (D21); from the repo root the `just`
recipes (`just ui-build`, `just ui-test`, `just ui-fmt`, `just dev-ui`) wrap
everything here.

## Routes

| Route | Page |
| --- | --- |
| `/login` | sign in (typed error codes: `invalid_credentials`, `login_locked` with a retry countdown) |
| `/change-password` | change own password (forced rotation for the bootstrap admin) |
| `/providers` | providers and their encrypted credentials (values are write-only) |
| `/models` | model catalog |
| `/offerings` | offerings, prices, per-offering headers |
| `/sync` | OpenRouter catalog/price sync runs |
| `/combos` | routing combos and the weights editor |
| `/benchmarks` | benchmark import and per-model quality overrides |
| `/audit` | audit log (default landing route) |
| `/users` | user administration |
| `/keys` (also `/users/:id/keys`) | API keys: create (plaintext shown once), revoke, rate limits |
| `/grants` | access-control matrix (allow/deny per user and key) |
| `/limits` | budgets and token caps |
| `/usage` | usage and spend reports (own, or all users as admin) |
| `/logs` | redacted request-log viewer with the "why this model" decision panel |

Everything outside `/login` sits behind a route guard and the app shell (user
email, role badge, sign out). Roles are enforced by the server; pages adapt
their detail views via `auth.isAdmin()`.

## Session and CSRF

The session lives in an HttpOnly cookie set by `POST /api/auth/login`. On
startup the app calls `GET /api/auth/me` to restore it. Every state-changing
request sends the session's token in `X-CSRF-Token` (see `src/app/api.ts`).

## Development

From the repository root, `just dev` starts the gateway together with this dev
server, and `just setup` runs `pnpm install` once. To run only the frontend
(against an already running backend):

```bash
pnpm install
pnpm start        # ng serve with proxy.conf.json
```

The dev server runs on `http://localhost:4200/` and proxies `/api`, `/v1`, and
`/health` to `127.0.0.1:8081`. Run a local backend first (see the root
`README.md`; `GATEWAY_MASTER_KEY` is required, and `GATEWAY_INSECURE_COOKIES=1`
lets the session cookie travel over plain HTTP).

## Building and testing

```bash
pnpm run build    # output in dist/web/browser/, embedded into the gateway binary
pnpm test         # unit tests (Vitest, jsdom)
pnpm exec prettier --check src/
```

## Layout

| Path | Purpose |
| --- | --- |
| `src/app/api.ts` | typed `fetch` wrapper, `ApiError` with stable error codes |
| `src/app/auth/` | session state (signals), login/logout/me, route guards |
| `src/app/shell/` | header shell around authenticated routes |
| `src/app/login/`, `src/app/change-password/` | sign-in and password change |
| `src/app/catalog/` | providers, credentials, models, offerings, sync pages |
| `src/app/routing/` | combos, benchmarks, the "why this model" decision panel |
| `src/app/users/`, `src/app/keys/`, `src/app/grants/` | access administration |
| `src/app/money/` | budgets and token caps |
| `src/app/usage/` | usage reports and the request-log viewer |
| `src/app/audit/` | audit log page |
| `src/testing/` | `fetch` double shared by the specs |
