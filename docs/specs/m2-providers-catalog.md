# Spec: M2 — providers, credentials, catalog, prices

Status: ready for implementation (2026-11-16). Owner: chief-of-staff.
Conventions: [`AGENTS.md`](../../AGENTS.md). Decisions referenced: D2, D4, D8,
D10, D11, D12, D13, D15, D16 in
[05-decisions.md](../research/05-decisions.md).

## Goal

An admin can register OpenAI-compatible providers, attach encrypted credentials
(with manual balance and custom headers), manage models and per-provider
offerings with prices, and import OpenRouter's model catalog and prices.

## Scope

1. Migration `0003_provider_catalog.sql` (tables below).
2. `server/src/secrets.rs` — AEAD box for credential secrets (D16).
3. Admin CRUD API for providers, credentials, models, offerings, prices — all
   mutations audited, CSRF-protected, admin-only.
4. Credential health probe endpoint.
5. OpenRouter catalog/price sync with TTL cache and stale-if-error, tested
   against a fixture (live fetch is blocked in this environment).
6. `GET /v1/models` listing enabled offerings (admin session until M4 keys).

## Non-goals

API keys and grants (M4), budgets (M4), routing (M5), user-facing price
editing, models.dev sync, per-user pricing UI.

## Schema (0003_provider_catalog.sql)

```sql
providers(id TEXT PK, name TEXT NOT NULL UNIQUE, kind TEXT NOT NULL DEFAULT 'openai_compatible'
          CHECK (kind IN ('openai_compatible')), base_url TEXT NOT NULL, enabled INTEGER NOT NULL
          DEFAULT 1 CHECK (enabled IN (0,1)), created_at TEXT NOT NULL, updated_at TEXT NOT NULL)

provider_credentials(id TEXT PK, provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
          label TEXT NOT NULL, secret_cipher TEXT NOT NULL, headers_json TEXT,
          max_concurrency INTEGER, balance_usd REAL, balance_source TEXT NOT NULL DEFAULT 'manual'
          CHECK (balance_source IN ('manual','provider_api','unknown')), balance_updated_at TEXT,
          balance_updated_by TEXT REFERENCES users(id) ON DELETE SET NULL,
          balance_low_threshold REAL, status TEXT NOT NULL DEFAULT 'unknown', cooldown_until TEXT,
          created_at TEXT NOT NULL, updated_at TEXT NOT NULL)

models(id TEXT PK, canonical_name TEXT NOT NULL UNIQUE, family TEXT,
          modalities_input TEXT, modalities_output TEXT, context_length INTEGER,
          max_output_tokens INTEGER, capabilities_json TEXT,
          created_at TEXT NOT NULL, updated_at TEXT NOT NULL)

offerings(id TEXT PK, provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
          model_id TEXT NOT NULL REFERENCES models(id) ON DELETE CASCADE,
          upstream_model_id TEXT NOT NULL, context_override INTEGER, max_output_override INTEGER,
          enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)), headers_json TEXT,
          created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
          UNIQUE(provider_id, upstream_model_id))

prices(id TEXT PK, offering_id TEXT NOT NULL REFERENCES offerings(id) ON DELETE CASCADE,
          input_per_m REAL, output_per_m REAL, cache_read_per_m REAL, cache_write_per_m REAL,
          reasoning_per_m REAL, request_fee REAL, currency TEXT NOT NULL DEFAULT 'USD',
          source TEXT NOT NULL CHECK (source IN ('manual','openrouter','models_dev','custom_api')),
          source_url TEXT, fetched_at TEXT, updated_by TEXT REFERENCES users(id) ON DELETE SET NULL,
          created_at TEXT NOT NULL, updated_at TEXT NOT NULL)
```

- One `prices` row per offering (upsert); a `manual` row is never overwritten by
  a sync (D10). Unknown price stays `NULL`, never `0`.
- Indexes: `offerings(model_id)`, `offerings(provider_id)`,
  `provider_credentials(provider_id)`, `prices(offering_id)`.
- Timestamps: UTC ISO-8601 with `Z`, as in `AGENTS.md`.

## Secret encryption (D16)

`server/src/secrets.rs`:

- Key: `GATEWAY_MASTER_KEY`, exactly 32 bytes (base64url or hex). Startup fails
  with an actionable error when missing or malformed (update `README.md` and the
  config docs accordingly).
- `SecretBox::encrypt(&self, plaintext, aad) -> String` → `v1:<b64url nonce>:<b64url ct>`;
  `decrypt` reverses it and rejects wrong AAD/version.
- AES-256-GCM via `ring::aead` (`LessSafeKey` + `UnboundKey`), 12-byte nonce from
  `ring::rand::SystemRandom`, per-encryption unique. AAD = credential row id.
- Tests: round-trip, tamper detection, wrong-key failure, stable version prefix.

## API (admin session + CSRF + audit on every mutation)

| Method & path | Behaviour |
| --- | --- |
| `GET/POST /api/providers`, `GET/PATCH/DELETE /api/providers/{id}` | CRUD; delete cascades credentials/offers |
| `GET/POST /api/providers/{id}/credentials`, `GET/PATCH/DELETE /api/credentials/{id}` | CRUD; secret write-only: responses return `has_secret` and masked label, never the secret |
| `POST /api/credentials/{id}/test` | probe `GET {base_url}/models` (or a tiny completion) with the credential's headers; record `status`, `last_tested` |
| `GET/POST /api/models`, `GET/PATCH/DELETE /api/models/{id}` | CRUD with all parameters |
| `GET/POST /api/offerings`, `PATCH/DELETE /api/offerings/{id}` | CRUD; manual `upstream_model_id` + overrides |
| `PUT /api/offerings/{id}/price` | manual price override (`source='manual'`, `updated_by`) |
| `POST /api/sync/openrouter`, `GET /api/sync/openrouter/status` | trigger sync / read last run |
| `GET /v1/models` | `{data: [{id, name, context_length, max_output_tokens, modalities, price...}]}` for enabled offerings (admin session until M4) |

Errors use the standard shape. `#[utoipa::path]` on every handler.

## OpenRouter sync

- `server/src/pricing/openrouter.rs`: `GET https://openrouter.ai/api/v1/models`,
  15s timeout, fields `id`, `name`, `context_length`, `pricing.prompt`,
  `pricing.completion`, `pricing.image`, `pricing.request`,
  `top_provider.max_completion_tokens`, `architecture.{modality,input_modalities,output_modalities}`,
  `supported_parameters`.
- Prices arrive as USD-per-token strings; store USD per 1M tokens (`*_per_m`),
  keep the raw value in `source_url`'s record? No: keep raw values only in the
  sync log detail JSON.
- 24h TTL; on failure return the previous snapshot and mark the run
  `stale-if-error` (D10, `openrouterCatalog` pattern in
  [01-omniroute-assessment.md](../research/01-omniroute-assessment.md)).
- Runs recorded in `price_sync_runs(id, source, started_at, finished_at, status,
  model_count, detail_json)`.
- **Live fetch is blocked in this environment** (see
  [09-environment.md](../research/09-environment.md)): tests use
  `server/tests/fixtures/openrouter_models.json` (4–6 realistic entries,
  including edge cases: missing `pricing`, `pricing.request` fee, image
  modality). Live verification is a documented follow-up.

## Manual model/offering entry

- Models carry modalities (`input_modalities`/`output_modalities` JSON arrays),
  `context_length`, `max_output_tokens`, `family`, `capabilities_json`.
- Offerings reference provider + model + `upstream_model_id`, optional context
  and max-output overrides, optional per-offering `headers_json`.
- Custom headers on credentials and offerings merge at request time
  (offering wins on key conflict) — implement the merge as a pure, tested
  function; M3 will use it.

## Acceptance

1. `scripts/check.sh` passes (fmt, clippy `-D warnings`, tests, api-only build).
2. Secrets: the SQLite file contains no plaintext secret; GET responses never
   contain one; decrypt with a wrong key fails cleanly.
3. Creating a provider + credential + model + offering + manual price works
   end-to-end over HTTP (documented `curl` sequence in the PR-style report).
4. OpenRouter sync is idempotent, updates prices only where
   `source != 'manual'`, and records a run row both on success and failure.
5. Every mutation writes exactly one `audit_log` row with enough detail to
   answer "who changed what".
