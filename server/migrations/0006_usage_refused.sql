-- 0006_usage_refused: the refusal counter (M4 UI-alignment finding 1).
--
-- Pre-send refusals (402 budget, 429 token cap, 403 access) — like every
-- request answered before anything was sent upstream — settled as
-- `usage_events` rows with `cost_usd = 0.0, priced = 1` and NULL tokens: they
-- consumed nothing and were never priced, but the reports counted them in
-- `requests` and `priced_requests` (live, a report showed 7 "requests" of
-- which only 3 consumed anything). Money totals were never wrong; only the
-- request counters were.
--
-- Counter semantics (documented here because `usage_daily` was created in
-- 0005 and an applied migration is never edited):
-- * `usage_daily.requests` keeps its meaning: the total of all rows — every
--   request that reached the chat pipeline, refusals included.
-- * `usage_daily.refused_requests` (new) counts the refusals and takes them
--   out of `priced_requests` (they were never priced). The four counters
--   `refused_requests` + `priced_requests` + `unpriced_requests` +
--   `partial_requests` partition `requests`.
-- * `usage_events.refused` (new, 0/1) is the per-event marker settlement
--   writes and the idempotent rollup rebuild derives `refused_requests`
--   from: 1 for every request that consumed nothing (never sent upstream).
--
-- The UPDATE backfills the marker on rows written before this migration.
-- It recognises exactly the refusal shape the old settlement booked
-- (tokens NULL, cost 0.0, priced = 1): a request that consumed anything
-- always booked token counts, so the shape cannot collide with a billable
-- row.

ALTER TABLE usage_daily ADD COLUMN refused_requests INTEGER NOT NULL DEFAULT 0;
ALTER TABLE usage_events ADD COLUMN refused INTEGER NOT NULL DEFAULT 0;

UPDATE usage_events
   SET refused = 1
 WHERE input_tokens IS NULL
   AND output_tokens IS NULL
   AND cost_usd = 0.0
   AND priced = 1;
