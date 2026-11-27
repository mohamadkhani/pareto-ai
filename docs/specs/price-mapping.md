# Spec: provider price mapping

Status: proposed 2026-11-18 (decision **D41**, extended by **D42**, with the
field syntax replaced by **D61** 2026-11-28). Depends on D10/D31/D34.

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

Each field is a **numeric expression** (D61): a lone `$.` path, or arithmetic
over paths and constants. A path is easy to generate from a click in the UI; the
arithmetic exists for the shapes a bare path cannot express.

```json
{
  "entries": "$.data",                      // array of model objects
  "model_id": "$.id",
  "input": "$.pricing_usd_per_million_tokens.balanced.input",
  "output": "$.pricing_usd_per_million_tokens.balanced.output",
  "cache_read": "$.pricing_usd_per_million_tokens.balanced.cached_input",
  "cache_write": null,
  "reasoning": null,
  "unit": {
    "source": "per_million_tokens",         // or "per_token" / "custom"
    "multiplier": 1.0,                      // applied after the base unit
    "currency": "USD",
    "fx_to_usd": null,                      // static; no live FX feed
    "display": { "symbol": "$", "decimals": 4 },
    "note": null                            // shown by the UI
  },
  "modalities_input": "$.input_modalities",
  "modalities_output": null,
  "context_length": null,
  "max_output": null,
  "apply_on_discovery": true
}
```

- Only `entries`, `model_id`, and one of `input`/`output` are required; everything
  else may be `null`.
- **Expressions (D61).** `$` is the root of the document the field reads: one
  catalog entry for every field except `entries` and `model_id`, which read the
  whole response. Grammar: `$.key`, `$.data[0].key`; `+ - * /`; unary minus;
  parentheses; `min`, `max`, `abs`, `round`, `floor`, `ceil`. No comparison,
  logic, or assignment. Examples: `$.pricing.prompt * 1000000` for a
  per-token string, `$.total_credits - $.total_usage` for a remaining balance,
  `max($.balance, 0)`. The five price fields accept expressions; `entries`,
  `model_id`, `modalities_*`, `context_length`, `max_output`, and the balance
  `available` flag accept one path only.
- **Bound and errors.** 512 characters, 32 nested parentheses. A malformed
  expression is refused by `PUT` with the field name and column; the preview
  shows the same message. There is no `default`: a missing or unreadable value
  is an error for a required field and `NULL` + warning for an optional one.
- **Retired syntax.** `/path` JSON Pointers are refused. Migration
  `0020_expression_fields` plus `connectors::migrate_pointer_fields` rewrite
  stored documents (`/a/b` → `$.a.b`), once.
- **Units (D44).** The expression reads a raw number; the unit applies after it.
  `source` sets the base: `per_token` × 1e6,
  `per_million_tokens` × 1, `custom` × `multiplier`. The SailResearch feed is
  per million but quoted in cents, so its template uses `source: "custom"`
  with `multiplier: 0.01` — there is no provider-specific branch. A
  non-positive `multiplier` makes the entry unpriced and raises a preview
  warning; a multiplier on a non-`custom` source is ignored and warned about.
- `currency` is the ISO code of the published numbers. Non-USD values are
  stored with that currency and are **unpriced for routing/mix** unless
  `fx_to_usd` is supplied; with a rate the result is converted to USD and the
  row stores `currency = 'USD'`.
- A negative resolved price marks the component **variable** (D37 rule) and the
  offering unpriced for that component. A non-numeric value is an error, never
  a zero.
- Fields are resolved relative to each entry; a missing or non-numeric
  expression on one entry skips **that entry only** and is counted in the
  preview and run result.
- Preview/test return both `raw` and `converted_per_m` per entry, with
  warnings when the converted input is outside `0.001…1000` USD/M (the usual
  sign of a wrong unit).

## Backend

- `provider_price_mappings(provider_id PK REFERENCES providers ON DELETE
  CASCADE, mapping_json, enabled, updated_by, updated_at)`.
- `GET/PUT /api/providers/{id}/price-mapping` (admin, CSRF, audited).
- `POST /api/providers/{id}/price-mapping/preview` — fetch `/models`, apply the
  mapping, return `{entries_seen, matched, skipped, samples: [{model_id,
  upstream_model_id, raw, converted_per_m, input, output, cache_read,
  warnings, error}], warnings}`; **writes nothing**.
- `POST /api/providers/{id}/prices/refresh` — apply to the catalog:
  upsert `prices` with `source='provider_api'`, `source_url` = the models URL,
  `fetched_at`; **never** overwrite a `manual` row; return
  `{created, updated, unchanged, skipped_manual, skipped_unresolved}` and write
  one run row.
- `apply_on_discovery`: after a successful discovery apply, run the refresh in
  the same flow (counts merged into the discovery result).
- `refresh_seconds`: optional. Balances default to 900 s (min 60, max 86 400)
  and are refreshed by a supervised job with exponential backoff (cap 1 h) and
  jitter. Catalog refresh stays **on demand** unless the connector sets
  `catalog.refresh_seconds`, in which case the same job cadence applies — no
  provider is polled by default.
- Templates: `sailresearch`, `openrouter_per_token`, `generic_pricing_object`
  (best-effort defaults) — selecting a template prefills the mapping; it is
  still fully editable.
- Validation: every field must parse as an expression (and as a lone path where
  the field is not numeric); preview must resolve the model id and one price on
  at least one entry, otherwise `400` naming the failing field and column.

## UI (clean, click-to-pick; arithmetic is typed)

A **Price mapping** dialog reachable from the provider row (and from the
Load-models result: “Prices were not mapped — map them now”):

1. **Sample** — one “Fetch sample” button shows the first model’s JSON as a
   collapsible tree.
2. **Template** — a select (`Detect automatically`, `SailResearch`,
   `OpenRouter (per token)`, `Generic pricing object`, `Custom`). Detection
   tries the templates against the sample and preselects the best match.
3. **Fields** — one row per target field (input, output, cached input, cache
   write, reasoning, currency, units, modalities, context, max output): each has
   an editable box and a **“Pick from sample”** mode where the JSON tree
   highlights the numbers; clicking one inserts that `$.` path at the caret. The
   five price fields accept arithmetic after the path (`* 1000000`); the other
   fields take one path. A shallow check (illegal character, unbalanced
   parenthesis, malformed path) shows under the box; the server is
   authoritative. Clearing a field is one click.
4. **Tier helper** — when picked paths share a tier object (e.g.
   `$.pricing…balanced.input`), show “Tier: asap | balanced | flex” (keys
   discovered in the sample) and switching rewrites the shared segment. A note
   states that the chosen tier becomes this provider's billed price.
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
3. Missing expression → that model stays unpriced and is counted (test).
4. No mapping → refresh refuses with a clear message; discovery leaves prices
   untouched (test).
5. Negative/`-1` values follow the D37 variable rule (test).
6. Preview writes nothing (test) and UI shows the table + errors.
7. Unit conversion follows D44: per-token ×1e6, per-million ×1, custom ×
   `multiplier`, then `fx_to_usd` only for non-USD (test).
8. Non-USD without FX stores its currency, leaves components NULL, and never
   feeds routing (test).
