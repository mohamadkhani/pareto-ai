# Environment knowledge

Living notes about the build environment, dated. Instructions for agents are in
[`AGENTS.md`](../../AGENTS.md); this page records what was observed and when.

## 2026-11-16 — external AI/price APIs unreachable

- `openrouter.ai/api/v1/models` and `models.dev/api.json` both time out
  (`curl` → `000`) from this network, like crates.io.
- Consequence: price/catalog sync code is implemented and tested against
  recorded fixtures; live verification is pending a network that can reach
  them. Recorded in [10-milestones.md](10-milestones.md).

## 2026-11-16 — crypto availability in the cache

- Only `ring 0.17.14` and `openssl 0.10.81` are cached; `aes-gcm`,
  `chacha20poly1305`, `aead`, `orion`, `sodiumoxide` are not.
- Consequence: credential encryption uses `ring::aead` AES-256-GCM (D16).

## 2026-11-15 — network

- `index.crates.io` and `static.crates.io` do not respond. `cargo` cannot fetch
  dependencies, even from mirrors (`rsproxy.cn`, `mirrors.ustc.edu.cn` answer
  `curl` with 200, but cargo's own HTTP gets "transferred 0 bytes").
- `npm`, `git`/GitHub, and `api.github.com` work normally.
- Consequence: all cargo commands run with `--offline` against
  `~/.cargo/registry` (2150 cached crates at that date, index cache from
  2026-11-13).

## 2026-11-15 — toolchain

- `cargo 1.99.1`, `rustc 1.99.1`, `node v26.13.1`, `npm 12.2.2`, `rustfmt 1.10.0`.
- Angular CLI is not installed globally; `npx @angular/cli` works (Angular
  22.2.0 scaffolded `web/` on 2026-11-15).

## 2026-11-15 — dependency availability in the cache

Present: `axum 0.8.9`, `tokio 1.54.1`, `sqlx 0.8.6`, `utoipa 5.5.0`,
`rust-embed 8.14.0`, `mime_guess 2.0.5`, `pbkdf2 0.12.2`, `hmac 0.12/0.13`,
`sha2 0.10/0.11`, `rand 0.8/0.9/0.10`, `subtle 2.6`, `base64 0.22/0.23`,
`cookie 0.18`, `reqwest 0.13.5`, `tower-http 0.6.11`, `tracing-subscriber 0.3.23`.

Absent (checked 2026-11-15): `argon2`, `password-hash`, `bcrypt`, `scrypt`,
`utoipa-axum`, `sea-orm-migration`, `include_dir`, `headers`, `zip`.

Consequences:

- Password hashing uses **PBKDF2-HMAC-SHA256** (cached `pbkdf2`+`hmac`+`sha2`),
  not Argon2id. Revisit if crates.io becomes reachable: see
  [05-decisions.md](05-decisions.md) D15.
- The OpenAPI document is assembled without `utoipa-axum` (manual wiring).

## 2026-11-15 — Angular build output

`ng build` writes to `web/dist/web/browser/` (not `web/dist/web/`), with
content-hashed, **alphanumeric-but-not-hex** names like `main-GFV6MIKA.js`.
Hash detection must not assume hex; the first version of `cache_control` did
and cached everything for one hour.

## 2026-11-15 — axum fallback handlers

A fallback handler cannot use `Path<String>`: unmatched requests carry no path
parameters and axum returns 500 "Wrong number of path arguments". Read the
path from `request.uri().path()` instead (`server/src/static_files.rs`).
