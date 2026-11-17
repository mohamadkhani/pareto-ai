# Project conventions — pareto-ai

Rules for every agent working in this repository. They are enforced by
`just check` (see `Justfile`); a change that does not pass it is not done.

## Environment (read this first)

- **Network access is intermittent.** crates.io and provider hosts
  (openrouter.ai, benchmarklist.com, huggingface, …) are sometimes reachable and
  sometimes blocked. Try online first; if the index or a host is unreachable,
  fall back to `--offline` (cached crates) or the fixture-based tests. Never
  hard-code a "network is blocked" assumption into code or docs without a date.
- **npm is not used here; JavaScript tooling is `pnpm`** (user preference —
  see `docs/research/11-user-preferences.md`). `cd web && pnpm install`,
  `pnpm run build`, `pnpm test`. If Angular scaffolding ever resets
  `packageManager` to npm, change it back to `pnpm` in `web/package.json`.
- Angular output lands in `web/dist/web/browser/` and is embedded into the
  binary (`server/src/static_files.rs`). The build fails without it; use
  `--no-default-features` for an api-only binary.
- **The task runner is `just`** (see `Justfile`): `just check` is the gate
  (Rust fmt/clippy/tests + api-only build + UI build/tests/formatting),
  `just build` produces the single production binary, `just build-api` the
  api-only one, `just dev`/`just dev-ui` run the two halves. A change that
  does not pass `just check` is not done.
- **Never `git commit` or `git push`.** The supervising agent commits at
  milestone boundaries with Conventional Commits.

## Code

### Rust (`server/`)

- `cargo fmt` before finishing; `cargo clippy --all-targets -- -D warnings`
  must be clean; every module that holds state or logic has tests.
- No `unwrap`/`expect` on request paths or in library code (tests and process
  startup where failure is fatal are fine). Errors: `thiserror` per module,
  `anyhow` only at the top level — startup/bootstrap glue may use `anyhow`,
  since its failures are fatal and user-facing.
- Public functions document behaviour and failure modes in one line.
- Handlers are thin: parse → call a service → map the result. Business logic
  lives in modules that are testable without HTTP.
- Secrets: never log them, never return them in a response, never store them
  plaintext. Credentials are encrypted (AEAD) or hashed (PBKDF2 for passwords,
  SHA-256 for session/API-key tokens).

### SQL (`server/migrations/`)

- Numbered `000N_name.sql`, forward-only, no edits to an applied migration —
  add a new one instead.
- Timestamps: `TEXT` in UTC, ISO-8601 with `Z` (`strftime('%Y-%m-%dT%H:%M:%fZ','now')`).
- Money: `REAL` in USD. Tokens: `INTEGER`. Booleans: `INTEGER` 0/1 with
  `CHECK`.
- Every foreign key is declared and every table that is queried by a known
  pattern has an index.

### TypeScript (`web/`)

- Strict mode, standalone components, signals, no NgModules, no `any`.
- Services call the backend through `fetch` with `credentials: 'include'`;
  state-changing calls send the `X-CSRF-Token` header.

### UI (admin console)

- **No component library.** Primitives live in `web/src/app/ui/` (see D28);
  use them instead of raw markup for buttons, fields, dialogs, badges, tables.
- **Tokens only.** Colors, spacing, radius, type, shadows come from
  `web/src/styles/tokens.css`; no hard-coded colors or sizes in components.
- **Every primitive and state appears on `/styleguide`** before it is used.
  When you add a primitive or a variant, add it there in the same change.
- **Pages do not restyle primitives** — only tokens and variants. Light and
  dark themes must both be checked (D30).
- Accessibility is part of done: keyboard-operable, visible focus, labelled
  controls, correct `aria-*`.
- **Browser verification is required for UI changes.** Use the Firefox tools:
  assert through `take_snapshot` and `evaluate_script` (text, no images), and
  save screenshots with `screenshot_page({saveTo})` so only a file path comes
  back. Never attach more than 2 inline images per turn — that limit is what
  broke an earlier agent. Check light and dark themes and one mobile width
  (~390px) for the shell.

## API

- `/v1/*` is the OpenAI-compatible data plane (API-key auth).
- `/api/*` is the admin plane (session auth + CSRF for mutations).
- `/health`, `/openapi.json` are public. Every handler keeps its
  `#[utoipa::path]` attribute in sync with the route table.
- Errors follow one shape: `{ "error": { "code", "message", "request_id" } }`
  with a sensible HTTP status. Never leak internals or secrets in a message.

## Decisions

Design decisions live in `docs/research/05-decisions.md` (D1–D14 and growing).
If your work contradicts one, stop and record the conflict instead of working
around it.
