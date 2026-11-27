-- 0021_offering_routes: per-provider (route) offerings for OpenRouter.
--
-- Decision D64. OpenRouter fronts many upstream providers (Azure, OpenAI,
-- Together, ...) and publishes a *different price per provider* for the same
-- model through `GET /api/v1/models/{author}/{slug}/endpoints`. Until now the
-- sync stored one offering per OpenRouter model at the aggregated price and
-- never told OpenRouter which provider to use.
--
-- Each upstream provider becomes its own offering under the single managed
-- `openrouter` provider row (one credential, many offerings — the offering is
-- already "the routable unit"). Two columns are added:
--
--   * `route`        — the provider's OpenRouter slug (`tag`, e.g. "azure"),
--                       the identity that makes two providers of one model two
--                       offerings. Empty for every non-openrouter offering and
--                       for a model with no per-provider endpoints.
--   * `routing_json` — the request-body fragment the upstream call merges in,
--                       e.g. {"provider":{"order":["azure"],"allow_fallbacks":
--                       false}}. Stored here so the request path stays generic
--                       and never hard-codes OpenRouter semantics.
--
-- The old `UNIQUE (provider_id, upstream_model_id)` cannot express this and
-- SQLite cannot drop a table-level constraint, so the table is rebuilt with
-- `UNIQUE (provider_id, upstream_model_id, route)`. `NULL` is distinct in a
-- SQLite unique index, so `route` is `TEXT NOT NULL DEFAULT ''` instead.
--
-- The rebuild fills a brand-new table and then renames *it* into place; the
-- old table is dropped first. The child tables that reference `offerings`
-- (prices, usage_events, combo_sticky_choices, ...) keep their
-- `REFERENCES offerings` clauses untouched, because the renamed table is the
-- new one (child clauses never mention its temporary name). `DROP TABLE`
-- runs an implicit DELETE, so dropping a referenced table with foreign keys
-- ON would cascade the child rows away. Migrations therefore run with foreign
-- keys off (see `db::open`); `PRAGMA foreign_keys` cannot be toggled inside
-- the transaction the migrator always opens, so the pragma is not set here.

CREATE TABLE offerings_new (
    id                  TEXT PRIMARY KEY,
    provider_id         TEXT NOT NULL REFERENCES providers (id) ON DELETE CASCADE,
    model_id            TEXT NOT NULL REFERENCES models (id) ON DELETE CASCADE,
    upstream_model_id   TEXT NOT NULL,
    -- OpenRouter (or another router) upstream provider slug; '' = no route.
    route               TEXT NOT NULL DEFAULT '',
    -- Body fragment merged into the upstream request (routing, not secrets).
    routing_json        TEXT,
    context_override    INTEGER,
    max_output_override INTEGER,
    enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    -- Sealed header envelope (D17, see 0003): {"names", "values"}.
    headers_json        TEXT,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (provider_id, upstream_model_id, route)
);

INSERT INTO offerings_new
    (id, provider_id, model_id, upstream_model_id, route, routing_json,
     context_override, max_output_override, enabled, headers_json,
     created_at, updated_at)
SELECT
    id, provider_id, model_id, upstream_model_id, '', NULL,
    context_override, max_output_override, enabled, headers_json,
    created_at, updated_at
FROM offerings;

DROP TABLE offerings;

ALTER TABLE offerings_new RENAME TO offerings;

CREATE INDEX IF NOT EXISTS idx_offerings_model_id ON offerings (model_id);
CREATE INDEX IF NOT EXISTS idx_offerings_provider_id ON offerings (provider_id);

PRAGMA foreign_key_check;
