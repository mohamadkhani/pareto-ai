# Third-party notices

`pareto-ai` is licensed **MIT** (see `LICENSE`). This file records the
third-party material whose notices must travel with the project.

## Fonts (SIL Open Font License 1.1)

Self-hosted, latin subset only (decision D29):

- **Exo 2** — © The Exo 2 Project Authors. OFL-1.1.
- **Inter** — © The Inter Project Authors. OFL-1.1.
- **JetBrains Mono** — © JetBrains s.r.o. OFL-1.1.

The font files are bundled unmodified. The OFL permits bundling with any
license provided the fonts are not sold on their own and the notice is kept.

## Data

- **LMArena leaderboard dataset** (`lmarena-ai/leaderboard-dataset`, WebDev
  subset) — **CC BY 4.0**. Used for Arena quality scores. Attribution is shown
  in the admin UI and must remain when the data is displayed or exported.
- **BenchmarkList capability index** (`benchmarklist.com/directory-data/models.json`)
  and **BenchLM** (`benchlm.ai/models`) — terms of use are not yet confirmed.
  They are optional sources and stay behind a documented flag until reviewed.
- **OpenRouter model catalog** — public API used for model metadata and prices;
  attribution is shown next to synced prices.

## Inspiration (no code copied)

- **OmniRoute** (`cheaper-inference/OmniRoute`, MIT) — product research,
  provider metadata patterns and routing ideas. Where any file or data was
  translated, the MIT notice applies to that material.
- **llm-leaders** (GPL-3.0, kept as a local reference checkout outside this
  repository) — its README
  documented the public benchmark endpoints and matching approach. **No code
  was copied**; the implementations here are independent.

## Runtime dependencies

- Rust crates: predominantly `MIT OR Apache-2.0` (the Rust convention). A formal
  license audit (`cargo-deny`) is pending because crates.io is unreachable from
  the build network; run it before any public release.
- Angular and its packages: MIT.
