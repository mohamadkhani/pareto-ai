# pareto-ai web console

Angular admin console for the pareto-ai gateway. Plain CSS, standalone
components, signals, strict TypeScript — no UI framework.

## Pages

- `/login` — sign in with the gateway account. Inline validation, typed error
  handling (`invalid_credentials`, `login_locked` with a retry countdown).
- `/audit` — audit log (admin only). Newest first, "load more" via the
  `before` cursor, expandable rows for `detail_json`, empty/error states.

Everything outside `/login` sits behind a route guard and the app shell (user
email, role badge, sign out).

## Session and CSRF

The session lives in an HttpOnly cookie set by `POST /api/auth/login`. On
startup the app calls `GET /api/auth/me` to restore it. Every state-changing
request sends the session's token in `X-CSRF-Token` (see `src/app/api.ts`).

## Development

```bash
npm start        # ng serve with proxy.conf.json
```

The dev server runs on `http://localhost:4200/` and proxies `/api`, `/v1`, and
`/health` to `127.0.0.1:8081`. Run a local backend first:

```bash
cd ../server
GATEWAY_DATA_DIR=/tmp/gwsmoke GATEWAY_INSECURE_COOKIES=1 \
  GATEWAY_ADMIN_EMAIL=admin@example.com GATEWAY_ADMIN_PASSWORD=change-me-please \
  cargo run --offline
```

## Building

```bash
npm run build   # output in dist/web/browser/, embedded into the gateway binary
```

## Testing

```bash
npm test        # unit tests (Vitest, jsdom)
```

## Layout

| Path | Purpose |
| --- | --- |
| `src/app/api.ts` | typed `fetch` wrapper, `ApiError` with stable error codes |
| `src/app/auth/` | session state (signals), login/logout/me, route guards |
| `src/app/login/` | sign-in page |
| `src/app/shell/` | header shell around authenticated routes |
| `src/app/audit/` | audit log page and its API client |
| `src/testing/` | `fetch` double shared by the specs |
