# Engineering conventions — pareto-ai

Rules for everyone working in this repository, human or automated assistant.
`just check` (see `Justfile`) enforces the mechanical parts; a change that does
not pass it is not done. Branching, PRs and releases are described in
[`CONTRIBUTING.md`](CONTRIBUTING.md).

## Environment

- **Tests must not depend on the network.** Provider, price and benchmark
  behaviour is covered by fixtures and a scripted upstream; live checks are a
  separate, documented procedure in `docs/live-verification.md`. Local network
  conditions are recorded — dated — in `docs/research/09-environment.md`, never
  as a project rule.
- **JavaScript tooling is `pnpm`**, not npm. `cd web && pnpm install`,
  `pnpm run build`, `pnpm test`. If a scaffold resets `packageManager` to npm,
  change it back in `web/package.json`.
- The Angular build lands in `web/dist/web/browser/` and is embedded into the
  binary (`server/src/static_files.rs`). The build fails without it; use
  `--no-default-features` for an api-only binary.
- **The task runner is `just`** (see `Justfile`): `just check` is the gate
  (Rust fmt/clippy/tests, api-only build, UI build/tests/formatting),
  `just build` produces the single production binary, `just build-api` the
  api-only one, `just dev` runs both halves together (gateway plus the Angular
  dev server), and `just dev-ui` runs only the frontend half.
- Do not rewrite published history or force-push `main`; the branch is
  protected and every change lands through a pull request.

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
  plaintext. Credentials are encrypted (AEAD) or hashed (Argon2id for passwords,
  SHA-256 for session/API-key tokens).

### SQL (`server/migrations/`)

- Numbered `000N_name.sql`, forward-only, no edits to an applied migration —
  add a new one instead.
- Timestamps: `TEXT` in UTC, ISO-8601 with `Z`
  (`strftime('%Y-%m-%dT%H:%M:%fZ','now')`).
- Money: `REAL` in USD. Tokens: `INTEGER`. Booleans: `INTEGER` 0/1 with
  `CHECK`.
- Every foreign key is declared and every table that is queried by a known
  pattern has an index.

### TypeScript (`web/`)

- Strict mode, standalone components, signals, no NgModules, no `any`.
- Services call the backend through `fetch` with `credentials: 'include'`;
  state-changing calls send the `X-CSRF-Token` header.

### UI (admin console)

- **No component library.** Primitives live in `web/src/app/ui/`; use them
  instead of raw markup for buttons, fields, dialogs, badges, tables.
- **Tokens only.** Colors, spacing, radius, type, shadows come from
  `web/src/styles/_tokens.sass`; no hard-coded colors or sizes in components.
- **Styles are indented Sass (`.sass`), never plain CSS.** The global entry is
  `web/src/styles.sass`, which `@use`s the partials in load order; components
  point `styleUrl` at their `.sass` file. No `.css` file may exist under
  `web/src` — `style-extension.spec.ts` fails if one appears. Prettier cannot
  format the indented syntax, so `just ui-fmt` skips it; the Angular build and
  that guardrail are the checks.
- **Every primitive and state appears on `/styleguide`** before it is used.
  When you add a primitive or a variant, add it there in the same change.
- **Pages do not restyle primitives** — only tokens and variants. Light and
  dark themes must both be checked.
- Accessibility is part of done: keyboard-operable, visible focus, labelled
  controls, correct `aria-*`.
- **Browser verification is required for UI changes.** Assert through DOM
  snapshots and script evaluation; save screenshots to a file instead of
  attaching them, and prefer DOM assertions to images. Check light and dark
  themes and one mobile width (~390px) for the shell.

## API

- `/v1/*` is the OpenAI-compatible data plane (API-key auth).
- `/api/*` is the admin plane (session auth + CSRF for mutations).
- `/health`, `/openapi.json` are public. Every handler keeps its
  `#[utoipa::path]` attribute in sync with the route table.
- Errors follow one shape: `{ "error": { "code", "message", "request_id" } }`
  with a sensible HTTP status. Never leak internals or secrets in a message.

## Decisions

Design decisions live as numbered entries (D1, D2, …) in
[`docs/research/05-decisions.md`](docs/research/05-decisions.md). If your work
contradicts one, stop and record the conflict — or the new decision — instead of
working around it.
