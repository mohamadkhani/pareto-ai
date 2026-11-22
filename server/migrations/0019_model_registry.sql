-- 0019_model_registry: external canonical model ids for cross-provider joins.
--
-- Decision D59 resolves an upstream model id to a catalog `models` row by an
-- external registry before falling back to spelling normalization. The table
-- only answers "do these two published ids name the same model?"; it is not a
-- price source and carries no secrets.
--
-- `source` names the publisher of the mapping:
--   * `models_dev` -- `canonical_model_id` from `https://models.dev/api.json`;
--   * `openrouter` -- `hugging_face_id` from
--     `https://openrouter.ai/api/v1/models`, plus the OpenRouter id itself.
--
-- `alias` is the lower-cased id as published (an id may appear under several
-- sources). `canonical` is the source's grouping key: any stable string works
-- for equality, so the two sources do not need to agree on its spelling. It is
-- stored lower-cased, so case never splits a group. Several rows for one
-- `(source, alias)` are meaningful: they mean the source itself is ambiguous,
-- and a write must not guess.
--
-- `fetched_at` is UTC ISO-8601 with milliseconds and a trailing Z. A refresh
-- replaces one source's rows atomically; a failure leaves the previous rows.
CREATE TABLE IF NOT EXISTS model_registry (
    source     TEXT NOT NULL CHECK (source IN ('models_dev', 'openrouter')),
    alias      TEXT NOT NULL,
    canonical  TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    PRIMARY KEY (source, alias, canonical)
);

-- The primary key prefix `(source, alias)` already covers every lookup this
-- table does, so no separate lookup index is needed.

-- The one-time repair repoints usage events to the keeper row; without this
-- index that update scans the whole table on a busy gateway.
CREATE INDEX IF NOT EXISTS idx_usage_events_model_id
    ON usage_events (model_id);
