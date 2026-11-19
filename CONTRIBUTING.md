# Contributing

`pareto-ai` is a self-hosted AI gateway: a Rust server with an embedded Angular
console. It is developed trunk-based: `main` is always releasable, and every
change lands through a short-lived branch and a pull request.

## Workflow

1. Branch from the latest `main`:

   | Change | Branch name |
   | --- | --- |
   | New behaviour | `feat/<short-slug>` |
   | Bug fix | `fix/<short-slug>` |
   | CI, packaging, tooling | `chore/<short-slug>` |
   | Documentation only | direct to `main` is fine |

2. Rebase on `main` before opening the PR (`git rebase main`), never merge
   `main` into the branch.
3. Open a PR and wait for `CI` (Web and Rust) to pass. `Desktop` runs when the
   desktop shell, the backend or the workflows change.
4. Squash-merge and delete the branch. `main` is protected: no force-push, CI
   required.
5. Releases are tags on `main` (`vX.Y.Z`). The release workflows publish the
   server binaries, desktop installers and AUR packages.

Hotfixes for an already released version: branch `fix/<version>-<slug>` from
the tag, PR to `main`, then tag the patch release.

## Checks

```bash
just check     # Rust fmt/clippy/tests, api-only build, UI build/tests/Pretty
just build     # production binary with the console embedded
```

A change that does not pass `just check` is not ready. For UI changes, also
verify in a real browser (DOM assertions; screenshots saved to a file, not
attached to the PR). Servers started for a test must be stopped afterwards.

## Commits and pull requests

- Conventional Commits: `feat(scope): …`, `fix(scope): …`, `refactor(scope): …`,
  `docs: …`, `chore(scope): …`.
- One logical change per PR; keep the diff reviewable.
- The PR description covers: what changed and why, how it was verified
  (commands and results), and any risk or follow-up.
- A design change is recorded as a numbered decision in
  [`docs/research/05-decisions.md`](docs/research/05-decisions.md) in the same
  PR.

## Code standards

The full rules live in [`AGENTS.md`](AGENTS.md) and apply to everyone:

- Rust: `cargo fmt`, clippy with `-D warnings`, no `unwrap` on request paths,
  `thiserror` per module.
- SQL: forward-only numbered migrations, UTC ISO-8601 timestamps, declared
  foreign keys.
- TypeScript: strict, standalone components, signals, no `any`.
- UI: design-system primitives and tokens only, indented Sass, both themes,
  accessible.

## AI assistance

AI coding assistants are used during development. Their output goes through the
same review, the same checks and the same CI gate as any other change.

## Where things live

| Path | Contents |
| --- | --- |
| `PLAN.md` | milestones and their status |
| `docs/research/` | dated research notes and the numbered decisions (D1…) |
| `docs/specs/` | feature specifications |
| `docs/reviews/` | review records, kept as sources |
| `docs/desktop.md` | Tauri packaging, CI matrix, signing |
| `docs/live-verification.md` | how to verify real providers on a networked host |
