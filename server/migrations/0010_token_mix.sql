-- 0010_token_mix: token-mix profiles and their derived mix price (D34).
--
-- A profile describes the *client's* token shape as disjoint shares (input,
-- output, cache read, cache write, reasoning). The shares are stored as
-- published percentages (8.1 / 0.4 / 91.5), normalised to sum to 1 when a
-- price is blended; a sum > 0 is the only hard requirement, so a percentage
-- profile and a fractional one describe the same mix.
--
-- `kind` is `preset` for the four research presets below, `custom` for an
-- operator-defined profile, and `measured` for a future profile calibrated
-- from this deployment's own `usage_events`. The overview API reports
-- `measured` profiles with `basis = "measured"` and everything else as
-- `"blended"`.
--
-- Built-ins can be cloned but never edited or deleted: a routing number that
-- silently changed meaning after an edit would make old decisions
-- irreproducible. `is_builtin` marks them, and the four rows are seeded
-- idempotently (`ON CONFLICT DO NOTHING`) so re-running migrations never
-- duplicates or rewrites them.
--
-- The mix price is a **routing and display** metric only; billing keeps the
-- D22 component formula on `usage_events.cost_usd`.
--
-- Timestamps are UTC ISO-8601 with milliseconds and a trailing Z.

CREATE TABLE IF NOT EXISTS token_mix_profiles (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL UNIQUE,
    kind              TEXT NOT NULL DEFAULT 'custom'
                      CHECK (kind IN ('preset', 'custom', 'measured')),
    input_share       REAL NOT NULL CHECK (input_share >= 0),
    output_share      REAL NOT NULL CHECK (output_share >= 0),
    cache_read_share  REAL NOT NULL CHECK (cache_read_share >= 0),
    cache_write_share REAL NOT NULL DEFAULT 0 CHECK (cache_write_share >= 0),
    reasoning_share   REAL NOT NULL DEFAULT 0 CHECK (reasoning_share >= 0),
    is_builtin        INTEGER NOT NULL DEFAULT 0 CHECK (is_builtin IN (0, 1)),
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (input_share + output_share + cache_read_share
           + cache_write_share + reasoning_share > 0)
);

-- The four built-in presets from D34 / docs/research/12-token-mix.md.
INSERT INTO token_mix_profiles
    (id, name, kind, input_share, output_share, cache_read_share,
     cache_write_share, reasoning_share, is_builtin)
VALUES
    ('mix-code-agent', 'code-agent', 'preset', 8.1, 0.4, 91.5, 0, 0, 1),
    ('mix-code-agent-nocache', 'code-agent-nocache', 'preset', 99.6, 0.4, 0, 0, 0, 1),
    ('mix-chat', 'chat', 'preset', 74.0, 26.0, 0, 0, 0, 1),
    ('mix-long-context', 'long-context', 'preset', 97.0, 3.0, 0, 0, 0, 1)
ON CONFLICT DO NOTHING;
