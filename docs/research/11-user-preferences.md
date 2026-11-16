# User preferences

Durable preferences the user has stated, with dates. Instructions that agents
must follow live in [`AGENTS.md`](../../AGENTS.md); this page records where
they came from and what was corrected.

## 2026-11-16 — pnpm for JavaScript/TypeScript

The user pointed out the project must use **pnpm**, not npm (this is also in
their global agent preferences: "JavaScript/TypeScript: pnpm, strict
TypeScript, ESLint, and Prettier"). The Angular scaffold had defaulted to npm
(`packageManager: npm@...` in `web/package.json`) and the project ran with it
until the user corrected it.

**Now:** `web/` uses pnpm 10 (`packageManager: pnpm@10.27.0`,
`pnpm-lock.yaml`, `package-lock.json` removed). Verified 2026-11-16: `pnpm
install`, `pnpm run build`, `pnpm test` (81 tests), prettier clean. If a
scaffold or CLI resets `packageManager` to npm, change it back.

## 2026-11-16 — Justfile as the task runner

The user said they like **Just** (the `just` command runner). The repo now has
a `Justfile` as the single entry point: `just check` is the quality gate,
`just build` the production binary, `just dev`/`just dev-ui` run the halves.
`scripts/check.sh` and `scripts/build.sh` existed briefly as compatibility
wrappers (`exec just ...`) and were removed on 2026-11-16.

## Related

- Technology preferences (Rust/TypeScript/Java choices, container practices)
  come from the user's global `AGENTS.md` and apply project-wide.
- Conventional Commits, Persian conversation, explicit verification reporting —
  also global preferences.