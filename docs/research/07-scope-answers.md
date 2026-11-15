# Credential balance, provider scope, pricing pipeline

Records the decisions given on 2026-11-15 after the first research round.

## 1. Manual balance on a credential (accepted)

An operator can set a manual credit balance on a `provider_credentials` row.

Schema:

```
provider_credentials:
  balance_usd            numeric        -- manual, may be null
  balance_updated_at     timestamptz
  balance_updated_by     user_id        -- who set it
  balance_source         'manual' | 'provider_api' | 'unknown'
  balance_low_threshold  numeric        -- optional warn level
```

Semantics:

- It is an **estimate, not an enforced ledger**. Setting it is enough for v1;
  there is no provider API sync yet.
- At offering selection time, skip an offering when
  `estimated_cost > balance_usd - balance_low_threshold` (fail closed) or warn
  only (fail open). Default: warn for manual balances, skip once a provider API
  sync exists.
- When a request settles and the balance is present and `balance_source` is not
  `manual`, decrement it. A manual balance is only re-read on the next edit.
- Display the balance and its age in the UI; a balance with no refresh date is
  visibly unreliable.

Not in v1: automatic provider credit checks. The hook is `balance_source`, so
adding a sync later does not change the schema.

## 2. Organization policy limits (accepted)

Distinct from the credential balance: limits the organization applies to its
own users.

```
budgets:            user_id, period ('day'|'week'|'month'|'total'), limit_usd
token_caps:         user_id, model_id, period, limit_tokens
org_settings:       global_rpm_per_user, global_tpm_per_user, per_request_max_tokens
```

Rules:

- Both gates apply to every request: the user's limits and the credential's
  balance. The effective constraint is the minimum of the two.
- Also cap "share of remaining credit": a single user may not spend more than a
  configured fraction of a credential's remaining balance, so one employee
  cannot drain the whole pool in one request.
- Reserve before the request, settle after, reconcile stale reservations.

## 3. Provider scope for v1

| Provider | Approach |
| --- | --- |
| OpenRouter | Built in: catalog + pricing sync via `GET /api/v1/models`, 24h TTL, stale-if-error |
| SailResearch | Not built in. Entered manually like any other OpenAI-compatible provider. |
| Any OpenAI-compatible endpoint | Manual entry: base URL + credentials + custom headers + manual price per offering |

Rationale: OpenRouter is special because its price and model catalog are public
and machine readable; it is also a reference price for many other hosts. Other
providers keep the product generic — no hardcoded provider-specific code.

OpenRouter handling details:

- Normalize prices from USD/token strings to USD per 1M tokens, keep raw values.
- Price rows carry `source='openrouter'`, `source_url`, `fetched_at`.
- Two different roles must not be confused: OpenRouter **as a routed provider**
  (you forward traffic to it) and OpenRouter **as a price source** (you read its
  catalog for other providers). Keep them separate in the schema:
  `prices.source` vs `offerings.provider_id`.

## 4. Pricing pipeline

```
sync_openrouter()   -- free, public, rate limited, cached 24h, stale-if-error
sync_models_dev()   -- optional second source, provenance recorded  (UNVERIFIED license)
manual_price()      -- admin override, wins over synced values
custom_price_api()  -- later: user-declared structure of a provider price API
```

Custom price API feature (later): the operator describes the provider's price
endpoint declaratively — method, path, auth header, JSON pointer per field
(input, output, cache, currency) and the model-id field. No new code per
provider; only a description document.

Rules:

1. Manual override always wins until cleared.
2. Never overwrite `balance_source='manual'` rows from a sync.
3. Records older than a staleness limit are excluded from routing scores; the
   component falls back to the pool median.
4. Price for a model on a provider with no source is `null`, not `0` — routing
   must treat "unknown price" differently from "free".

## 5. Body retention (accepted)

- Request/response bodies kept for **1 day**, then purged by a job.
- Captured only when an admin enables it, per user or globally.
- Stored as compressed files outside the database, with a size cap per request
  and a global cap.
- Later: retention becomes a configurable setting (admin, then per key).

## 6. Open questions

1. Should a manual balance gate requests (skip offering) or only warn?
   Default chosen: warn in v1, gate when a provider sync exists.
2. What fraction of a credential's remaining balance may a single user spend?
3. Does OpenRouter need `usage` accounting for cached tokens too, or only
   input/output for v1?
