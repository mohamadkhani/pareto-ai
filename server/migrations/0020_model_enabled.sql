-- 0020_model_enabled: the model-level kill switch for routing.
--
-- `offerings.enabled` answers "does this provider sell this model?"; it cannot
-- express "no provider may route this model", because a provider discovered
-- later creates a new offering that would default to enabled. D61 therefore
-- puts the cross-provider switch on the model identity: `models.enabled = 0`
-- excludes every offering of the model from routing and from `/v1/models`,
-- including offerings created after the switch was flipped.
--
-- The two flags are independent: the model flag is the kill switch, the
-- offering flag is the per-provider choice it temporarily overrides. Re-enabling
-- the model restores each provider's own offering setting.
ALTER TABLE models
    ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1));