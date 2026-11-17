# Spec: provider price mapping

Status: proposed 2026-11-18 (decision **D41**, extended by **D42**). Depends on
D10/D31/D34.

## Problem

Some providers expose prices in their `GET /v1/models` payload — but the OpenAI
schema has no price fields, and every provider that adds them uses a different
shape:

| Provider | Shape (observed) |
| --- | --- |
| OpenRouter | `pricing.prompt` / `pricing.completion`, USD **per token**, strings |
| SailResearch | `pricing_usd_per_million_tokens.{asap,balanced,flex}.{input,cached_input,output}` |
| Together / DeepInfra / Fireworks | their own `pricing` objects, mixed units |

Hard-coding each one does not scale and silently misprices every provider we
have not seen yet.

## Decision

A per-provider, user-authored **mapping** from the `/models` payload to our
price fields. **No mapping ⇒ unpriced.** We never guess a shape, and we never
pick a tier the operator did not choose.

Precedence is unchanged: `manual` > `provider_api` > `openrouter`/`models_dev`.

## Mapping format

JSON Pointers (RFC 6901) — unambiguous, easy to validate, and easy to generate
from a click in the UI:

```json
{
  "entries": "/data",                       // array of model objects
  "model_id": "/id",
  "input": "/pricing_usd_per_million_tokens/balanced/input",
  "output": "/pricing_usd_per_million_tokens/balanced/output",
  "cache_read": "/pricing_usd_per_million_tokens/balanced/cached_input",
  "cache_write": null,
  "reasoning": null,
  "currency": "USD",
  "units": "per_million_tokens",            // or "per_token"
  "modalities_input": "/input_modalities",
  "context_length": null,
  "max_output": null,
  "apply_on_discovery": true
}
```

- Only `entries`, `model_id`, and one of `input`/`output` are required; everything
  else may be `null`.
- `units: "per_token"` multiplies by 1e6 on ingestion; strings are accepted.
- A negative or non-numeric resolved price marks the component **variable**
  (D37 rule) and the offering unpriced for that component.
- Pointers are resolved relative to each entry; a missing pointer on one entry
  skips **that entry only** and is counted in the preview and run result.

## Backend

- `provider_price_mappings(provider_id PK REFERENCES providers ON DELETE
  CASCADE, mapping_json, enabled, updated_by, updated_at)`.
- `GET/PUT /api/providers/{id}/price-mapping` (admin, CSRF, audited).
- `POST /api/providers/{id}/price-mapping/preview` — fetch `/models`, apply the
  mapping, return `{entries_seen, matched, skipped, samples: [{model_id,
  upstream_model_id, input, output, cache_read, error}], warnings}`; **writes
  nothing**.
- `POST /api/providers/{id}/prices/refresh` — apply to the catalog:
  upsert `prices` with `source='provider_api'`, `source_url` = the models URL,
  `fetched_at`; **never** overwrite a `manual` row; return
  `{created, updated, unchanged, skipped_manual, skipped_unresolved}` and write
  one run row.
- `apply_on_discovery`: after a successful discovery apply, run the refresh in
  the same flow (counts merged into the discovery result).
- Templates: `sailresearch`, `openrouter_per_token`, `generic_pricing_object`
  (best-effort defaults) — selecting a template prefills the mapping; it is
  still fully editable.
- Validation: pointers must start with `/`; preview must resolve at least the
  model id and one price on at least one entry, otherwise `400` naming the
  failing pointer.

## UI (clean, click-to-pick — no JSONPath typing)

A **Price mapping** dialog reachable from the provider row (and from the
Load-models result: “Prices were not mapped — map them now”):

1. **Sample** — one “Fetch sample” button shows the first model’s JSON as a
   collapsible tree.
2. **Template** — a select (`Detect automatically`, `SailResearch`,
   `OpenRouter (per token)`, `Generic pricing object`, `Custom`). Detection
   tries the templates against the sample and preselects the best match.
3. **Fields** — one row per target field (input, output, cached input, cache
   write, reasoning, currency, units, modalities, context, max output): each has
   a read-only pointer box and a **“Pick from sample”** mode where the JSON tree
   highlights the numbers; clicking one fills the pointer. Clearing a field is
   one click.
4. **Tier helper** — when picked pointers share a tier object (e.g.
   `/…/balanced/input`), show “Tier: asap | balanced | flex” (keys discovered in
   the sample) and switching rewrites the shared segment. A note states that the
   chosen tier becomes this provider's billed price.
5. **Preview table** — first ~10 models with resolved input/output/cache and
   per-entry errors; a summary line `n models · m unpriced · k errors`.
6. **Save** / **Save and refresh prices** with a busy state and result banner;
   footer buttons use the standard spacing.
7. Empty/unpriced state explains: “no mapping configured → this provider's
   models stay unpriced; routing imputes the price instead of guessing”.

## Acceptance

1. SailResearch with the `sailresearch` template resolves all 10 models at the
   chosen tier and refresh writes `source='provider_api'` rows.
2. A manual price is never overwritten (test).
3. Missing pointer → that model stays unpriced and is counted (test).
4. No mapping → refresh refuses with a clear message; discovery leaves prices
   untouched (test).
5. Negative/`-1` values follow the D37 variable rule (test).
6. Preview writes nothing (test) and UI shows the table + errors.
