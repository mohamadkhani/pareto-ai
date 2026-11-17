-- 0012_variable_price: record OpenRouter's "-1" variable-price sentinel.
--
-- OpenRouter publishes `pricing.prompt` / `pricing.completion` (and the cache
-- components) as the string "-1" for models whose price is not fixed, for
-- example `openrouter/auto` and `nvidia/switchyard`. "-1" is a sentinel, not
-- a price: it means "this offering has a variable price" (the provider picks
-- the model and therefore the price per request).
--
-- The price sync now maps every negative component to NULL at parse time and
-- sets `prices.variable_price = 1` for that offering. NULL means unknown and
-- is never written as 0 (D10), so a variable price can never enter
-- `price_min`/`price_avg`, the mix blend, or the routing price score. An
-- offering whose components are all variable behaves exactly like an unpriced
-- offering: routing imputes the neutral price score, and a combo price cap
-- excludes it (`unpriced`), never ranks it cheapest.
--
-- The UPDATE below repairs rows written before this migration: every negative
-- component was the sentinel, so it is nulled and the offering is flagged.
-- A negative `request_fee` is treated the same way (it is not a fee).
-- `source = 'manual'` rows never hold negative values (`set_manual_price`
-- rejects them), so the UPDATE only touches synced rows in practice.

ALTER TABLE prices ADD COLUMN variable_price INTEGER NOT NULL DEFAULT 0
    CHECK (variable_price IN (0, 1));

UPDATE prices
SET variable_price = 1,
    input_per_m = CASE WHEN input_per_m < 0 THEN NULL ELSE input_per_m END,
    output_per_m = CASE WHEN output_per_m < 0 THEN NULL ELSE output_per_m END,
    cache_read_per_m = CASE WHEN cache_read_per_m < 0 THEN NULL ELSE cache_read_per_m END,
    cache_write_per_m = CASE WHEN cache_write_per_m < 0 THEN NULL ELSE cache_write_per_m END,
    reasoning_per_m = CASE WHEN reasoning_per_m < 0 THEN NULL ELSE reasoning_per_m END,
    request_fee = CASE WHEN request_fee < 0 THEN NULL ELSE request_fee END
WHERE input_per_m < 0
   OR output_per_m < 0
   OR cache_read_per_m < 0
   OR cache_write_per_m < 0
   OR reasoning_per_m < 0
   OR request_fee < 0;
