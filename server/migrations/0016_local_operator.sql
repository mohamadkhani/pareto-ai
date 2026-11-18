-- 0016_local_operator: the synthetic local operator becomes a real row, and
-- audit rows record insecure-mode actions.
--
-- The unauthenticated "local operator" identity used to exist only in code.
-- Its id was written into foreign-keyed columns (audit_log.actor_user_id and
-- the *_updated_by stamps) and every mutation failed with FOREIGN KEY
-- constraint failed while setup was skipped. It is now a real `users` row
-- (`is_placeholder = 1`) seeded while the gateway runs without a password,
-- and completing setup promotes that same row to the first admin in place.
--
-- `audit_log.insecure` is 1 for rows written while the acting identity was
-- that unauthenticated placeholder: the action was done in insecure mode.

ALTER TABLE users ADD COLUMN is_placeholder INTEGER NOT NULL DEFAULT 0 CHECK (is_placeholder IN (0, 1));

ALTER TABLE audit_log ADD COLUMN insecure INTEGER NOT NULL DEFAULT 0 CHECK (insecure IN (0, 1));
