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

## Open items (tracked as GitHub issues)

| # | Item |
| --- | --- |
| 1 | [Enable macOS desktop builds in CI](https://github.com/mohamadkhani/pareto-ai/issues/1) |
| 2 | [Verify automatic TLS on a public domain](https://github.com/mohamadkhani/pareto-ai/issues/2) |
| 3 | [BenchmarkList: body decode fails from some hosts](https://github.com/mohamadkhani/pareto-ai/issues/3) |
| 4 | [Seal connector header values at rest](https://github.com/mohamadkhani/pareto-ai/issues/4) |
| 5 | [Currency support beyond USD](https://github.com/mohamadkhani/pareto-ai/issues/5) |
| 6 | [Add provider presets: Together, DeepInfra, Fireworks](https://github.com/mohamadkhani/pareto-ai/issues/6) |
| 7 | [Decide on a Sass formatter or stylelint](https://github.com/mohamadkhani/pareto-ai/issues/7) |
| 8 | [Docs drift sweep: PLAN and milestone log](https://github.com/mohamadkhani/pareto-ai/issues/8) |

Details, acceptance notes and constraints are in each issue; this page only
links them so the list cannot drift.

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
