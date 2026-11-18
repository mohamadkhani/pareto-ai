-- 0018_provider_presets: an additive provider `preset` (D52).
--
-- `kind` stays the wire protocol (`openai_compatible`); a preset is a
-- code-side template that names a known upstream (OpenRouter, SailResearch) so
-- creating it seeds the price, balance, and health connectors instead of
-- asking the operator to fill twelve pointers by hand.
--
-- The column is nullable: a provider created without a preset (or created
-- before this migration) keeps working exactly as before. The CHECK names the
-- shipped presets; adding a preset later means a new forward migration.
-- Nothing references this column with a foreign key.

ALTER TABLE providers ADD COLUMN preset TEXT
    CHECK (preset IS NULL OR preset IN ('openrouter', 'sailresearch', 'generic'));

CREATE INDEX IF NOT EXISTS idx_providers_preset ON providers (preset);
