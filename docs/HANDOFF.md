# Handoff — where the project stands

Read this first in a new session, then `AGENTS.md`, then `PLAN.md`. Everything
here is dated or tied to a commit; when it drifts, fix it in the same change
that causes the drift.

Last updated: 2026-11-19, commit `b1d3f12` (+ the commit that adds this file).

## What this is

A self-hosted AI gateway: one Rust binary (with the Angular console embedded)
in front of several OpenAI-compatible providers, with price/speed/quality-driven
routing, fallbacks, per-user budgets, and admin reporting. Trunk-based
development; `main` is protected and CI-gated (see `CONTRIBUTING.md`).

## State

| Area | Status |
| --- | --- |
| Backend | 18 migrations, ~563 tests, WAL + `synchronous=NORMAL` (D36) |
| Console | 15 primitives, indented Sass, light/dark, 394 tests |
| Auth | sessions + CSRF, Argon2id (login ~113 ms), zero-config setup wizard, setup/unprotected/protected modes (D39/D40) |
| Access | users, API keys (hashed, shown once), grants, budgets and token caps with hard stop, usage/logs/audit, dashboard, per-user drawer |
| Routing | weighted scoring with explanations, mix price profiles (D34), rule-based combos + stickiness + preview (D35), breaker panel |
| Providers | presets (`openrouter`, `sailresearch`, `generic`), model discovery, price/balance/health connectors with unit scaling and SSRF guards (D41/D42/D44) |
| Pricing | manual > provider_api > OpenRouter; variable prices (`-1` sentinel) never ranked |
| TLS | automatic Let's Encrypt for a public domain, HTTP→HTTPS, off for local (D54) |
| Packaging | single binary; Tauri desktop (Linux+Windows in CI); AUR packages; server binaries and installers published by GitHub Actions |
| CI | `ci.yml` (web, rust, gateway binaries) and `desktop.yml` (Linux, Windows) on GitHub; `main` requires Web+Rust checks |

Repositories: GitHub `mohamadkhani/pareto-ai` is canonical (CI, releases); the
Gitea remote is a mirror.

## How to verify a change

```bash
just check      # Rust fmt/clippy/tests, api-only build, UI build/tests/Pretty
just build      # production binary with the console embedded
just dev        # gateway + Angular dev server
just bench      # SQLite/request-latency harness (release)
```

## Open items (in rough priority order)

1. **macOS desktop builds** are disabled in CI: a private repo in the current
   plan gets no macOS runner (the job is cancelled in ~2 s). Options in
   `docs/desktop.md`: make the repo public, enable paid macOS minutes, or build
   on a Mac.
2. **Live TLS is unverified** (needs a public domain and ports 80/443). Step by
   step in `README.md` and D54; the failure path (port 80 denied) is tested.
3. **BenchmarkList ingestion** still fails while reading the body from some
   hosts; the error names the layer. Arena and BenchLM work.
4. **Connector header values are stored plaintext** in `config_json` (D42 says
   templates, but a literal key typed there would be stored). Either seal them
   like credential headers (D17) or document the restriction in the UI.
5. **Non-USD balances/prices** need a static `fx_to_usd`; there is no live FX.
6. **Only three provider presets**; adding Together/DeepInfra/Fireworks is one
   registry entry each.
7. **No Sass formatter** (Prettier cannot parse indented syntax): the build and
   `style-extension.spec.ts` are the checks.
8. Documentation drift: `PLAN.md` and the milestone log are close but not
   perfectly current; fix opportunistically.

## Session hygiene

- Commit and push before ending; the next session starts from the repo, not from
  a conversation.
- Update this file and `docs/research/10-milestones.md` when a session changes
  state, and record design choices as the next numbered decision in
  `docs/research/05-decisions.md`.
- Never put secrets, provider keys or passwords in the repo or in this file. The
  local demo database (`./data` or `.data/`) is disposable and machine-local.
- Running services (gateway, `ng serve`) are not part of the repo; start them
  fresh.
