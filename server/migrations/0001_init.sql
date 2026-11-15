-- 0001_init: metadata and settings storage.
-- Everything else is added by later migrations; this one only proves that
-- migrations run and foreign keys/PRAGMAs behave.

CREATE TABLE IF NOT EXISTS db_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Global, non-secret configuration. Secrets never live in this table: they are
-- encrypted credential columns (milestone 2) or environment variables.
CREATE TABLE IF NOT EXISTS settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_by  TEXT
);

INSERT INTO db_meta (key, value)
VALUES ('schema_initialized_at', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
ON CONFLICT (key) DO NOTHING;
