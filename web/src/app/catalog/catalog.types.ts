/**
 * Wire types for the M2 admin catalog API: providers, credentials, models,
 * offerings, prices, and the OpenRouter price sync.
 *
 * Every request/response shape lives here so that aligning the UI with the
 * server is a one-file change. Conventions inherited from M1: failures use the
 * `{ error: { code, message, request_id } }` envelope (parsed by `api.ts`),
 * timestamps are RFC-3339 UTC strings ending in `Z`, and mutations require the
 * CSRF token. Aligned against the running M2 backend on 2026-11-16; where the
 * spec was silent, the wire behaviour is noted on the field.
 */

import type { ConnectorSummary } from './connectors.types';

/** Provider flavours; the schema only allows `openai_compatible` today. */
export type ProviderKind = 'openai_compatible';

/** One registered upstream provider. */
export interface Provider {
  id: string;
  name: string;
  kind: ProviderKind;
  base_url: string;
  /** DB stores 0/1; travels as a JSON boolean. */
  enabled: boolean;
  created_at: string;
  updated_at: string;
  /**
   * The last **applied** model discovery (a dry run never writes these), or
   * null before the first one. Persisted server-side so "Last load" survives
   * a reload.
   */
  last_discovery_at: string | null;
  /** `models` rows the last applied discovery inserted. */
  last_discovery_created: number | null;
  /** Offerings the last applied discovery found already matching. */
  last_discovery_unchanged: number | null;
  /** Configured connectors, one entry per saved kind (D42). */
  connectors: ConnectorSummary[];
}

/** `POST /api/providers`. */
export interface ProviderInput {
  name: string;
  base_url: string;
  /** Defaults to `openai_compatible` server-side when omitted. */
  kind?: ProviderKind;
}

/**
 * `PATCH /api/providers/{id}`: only the fields the admin changed. The wire
 * patch cannot change `kind` (only `name`, `base_url`, `enabled`).
 */
export interface ProviderPatch {
  name?: string;
  base_url?: string;
  enabled?: boolean;
}

/** How the recorded balance was obtained. */
export type BalanceSource = 'manual' | 'provider_api' | 'unknown';

/** Whether an exhausted balance only warns or also blocks routing (D42). */
export type BalanceGate = 'warn' | 'block';

/**
 * Credential health. The backend records `ok` / `error` from the probe and
 * starts rows at `unknown`; `cooldown` is reserved for the future (a cooldown
 * is currently tracked in `cooldown_until`, never as `status`).
 */
export type CredentialStatus = 'unknown' | 'ok' | 'error' | 'cooldown';

/**
 * One credential for a provider. The secret is write-only: responses carry
 * `has_secret` and `secret_masked` instead of the value, and custom header
 * values are write-only too — responses carry `header_names` only.
 */
export interface Credential {
  id: string;
  provider_id: string;
  label: string;
  /** Whether a secret is stored; the secret itself is never returned. */
  has_secret: boolean;
  /** Fixed mask shown instead of the secret; reveals no secret bytes. */
  secret_masked: string;
  /** Header names only; values are write-only. */
  header_names: string[];
  max_concurrency: number | null;
  balance_usd: number | null;
  balance_source: BalanceSource;
  balance_updated_at: string | null;
  balance_updated_by: string | null;
  balance_low_threshold: number | null;
  /** Last provider-reported availability: false is "unavailable", never 0. */
  balance_available: boolean | null;
  /** Currency of the last provider-reported balance. */
  balance_currency: string | null;
  /** Why the last balance fetch failed; cleared on success. */
  balance_error: string | null;
  balance_fetched_at: string | null;
  /** `warn` is advisory; `block` skips an exhausted credential (D42). */
  balance_gate: BalanceGate;
  status: CredentialStatus;
  /** When `POST /api/credentials/{id}/test` last completed. */
  last_tested_at: string | null;
  cooldown_until: string | null;
  created_at: string;
  updated_at: string;
}

/** `POST /api/providers/{id}/credentials`. */
export interface CredentialInput {
  label: string;
  /** Write-only: stored encrypted, never returned again. */
  secret: string;
  headers: Record<string, string> | null;
  max_concurrency: number | null;
  /** Manual balance; the server records `balance_source='manual'`. */
  balance_usd: number | null;
  balance_low_threshold: number | null;
  /** `warn` (default) or `block`; see D42. */
  balance_gate?: BalanceGate;
}

/**
 * `PATCH /api/credentials/{id}`: omit `secret` to keep the stored one (the
 * wire rejects an empty `secret` with 400, so the UI omits it instead).
 * `headers` is a full replace and is sent only when the admin edited the rows.
 * The wire cannot clear `max_concurrency` / `balance_usd` back to null: a
 * null there means "keep" and an all-null patch is a 400, so those fields are
 * only sent as concrete values.
 */
export interface CredentialPatch {
  label?: string;
  /** Omitted keeps the stored secret (the field is write-only). */
  secret?: string;
  headers?: Record<string, string> | null;
  max_concurrency?: number;
  balance_usd?: number;
  /** `warn` or `block`; changing it alone is a valid patch. */
  balance_gate?: BalanceGate;
}

/**
 * Result of `POST /api/credentials/{id}/test`. Always 200; the outcome is also
 * recorded on the credential (`status`, `last_tested_at`).
 */
export interface CredentialTestResult {
  /** `ok` or `error`. */
  status: 'ok' | 'error';
  ok: boolean;
  /** Upstream HTTP status, when a response arrived. */
  http_status: number | null;
  /** Probe round-trip time in milliseconds. */
  latency_ms: number;
  error: string | null;
  tested_at: string;
}

/**
 * One catalog model. `modalities_*` travel as parsed arrays (null on the wire
 * when the model has none; `toModel` normalizes that to `[]`).
 * `capabilities` is free-form JSON on the wire: synced models keep an object
 * there (e.g. `{ display_name, supported_parameters }`) while the UI's editor
 * writes a string array of tags.
 */
export interface Model {
  id: string;
  canonical_name: string;
  family: string | null;
  modalities_input: string[];
  modalities_output: string[];
  context_length: number | null;
  max_output_tokens: number | null;
  /** Free-form JSON: a tag array or a structured object. */
  capabilities: unknown;
  created_at: string;
  updated_at: string;
}

/** `POST /api/models` and `PATCH /api/models/{id}`. */
export interface ModelInput {
  canonical_name: string;
  family: string | null;
  modalities_input: string[];
  modalities_output: string[];
  context_length: number | null;
  max_output_tokens: number | null;
  /** The UI writes capabilities as a free-form tag list. */
  capabilities: string[];
}

/**
 * Price row: all amounts are USD per 1M tokens, `request_fee` is USD per
 * request. Keyed by `offering_id` — the wire price carries no own id.
 */
export interface Price {
  offering_id: string;
  input_per_m: number | null;
  output_per_m: number | null;
  cache_read_per_m: number | null;
  cache_write_per_m: number | null;
  reasoning_per_m: number | null;
  request_fee: number | null;
  /** The source publishes a variable price (the `-1` sentinel). */
  variable_price: boolean;
  currency: string;
  source: PriceSource;
  source_url: string | null;
  fetched_at: string | null;
  updated_by: string | null;
  created_at: string;
  updated_at: string;
}

/** Where a price came from; manual rows are never overwritten by sync (D10). */
export type PriceSource = 'manual' | 'openrouter' | 'models_dev' | 'custom_api';

/**
 * One provider's offering of one model. Custom header values are write-only:
 * reads carry `header_names` only. Prices are read per offering via
 * `GET /api/offerings/{id}/price` (404 until the offering is priced).
 */
export interface Offering {
  id: string;
  provider_id: string;
  model_id: string;
  upstream_model_id: string;
  context_override: number | null;
  max_output_override: number | null;
  enabled: boolean;
  header_names: string[];
  created_at: string;
  updated_at: string;
}

/** `POST /api/offerings`. */
export interface OfferingInput {
  provider_id: string;
  model_id: string;
  upstream_model_id: string;
  context_override: number | null;
  max_output_override: number | null;
  enabled: boolean;
  headers: Record<string, string> | null;
}

/**
 * `PATCH /api/offerings/{id}`: only the fields the admin changed. The wire
 * patch cannot move an offering to another provider or model, and nullable
 * overrides cannot be cleared back to null.
 */
export interface OfferingPatch {
  upstream_model_id?: string;
  context_override?: number;
  max_output_override?: number;
  enabled?: boolean;
  /** Full replace, sent only when the admin edited the header rows. */
  headers?: Record<string, string> | null;
}

/**
 * `PUT /api/offerings/{id}/price`. A manual override: the server records
 * `source='manual'` itself (the body sends amounts only, `currency` omitted
 * for the USD default). Blank fields stay `NULL` and never become 0.
 */
export interface PriceInput {
  input_per_m: number | null;
  output_per_m: number | null;
  cache_read_per_m: number | null;
  cache_write_per_m: number | null;
  reasoning_per_m: number | null;
  request_fee: number | null;
  /** Omitted means the server keeps the USD default. */
  currency?: string;
}

/**
 * `POST /api/providers/{id}/discover-models`. Discovery reads the provider's
 * OpenAI-compatible `/models` endpoint and upserts canonical models and
 * offerings idempotently; manual edits and prices always win.
 */
export interface DiscoveryInput {
  /** Defaults to the provider's first active credential. */
  credential_id?: string;
  /** Preview only: compute the summary and write no catalog rows. */
  dry_run?: boolean;
}

/** What one discovery run found and changed. */
export interface DiscoverySummary {
  dry_run: boolean;
  credential_id: string;
  /** Usable upstream model ids in the payload. */
  discovered: number;
  created_models: number;
  created_offerings: number;
  /** Existing offerings re-linked to the canonical model row. */
  updated_offerings: number;
  unchanged: number;
}

/** Run statuses recorded in `price_sync_runs`. */
export type SyncRunStatus = 'success' | 'stale-if-error' | 'error';

/** Free-form run detail; failures record `{ error, served_snapshot_run_id }`. */
export type SyncRunDetail = Record<string, unknown>;

/** One row of `price_sync_runs`. */
export interface SyncRun {
  id: string;
  source: string;
  started_at: string;
  finished_at: string;
  status: SyncRunStatus;
  /** Models applied (success) or served from the snapshot (stale-if-error). */
  model_count: number;
  /** Raw source prices and error summaries; never secret material. */
  detail: SyncRunDetail | null;
}

/** Outcome of `POST /api/sync/openrouter` (always 200, run recorded). */
export interface SyncOutcome {
  run_id: string;
  status: SyncRunStatus;
  /** Models applied or served from the previous snapshot; 0 without one. */
  model_count: number;
  /** True when the previous snapshot is being served after a failed fetch. */
  stale: boolean;
  started_at: string;
  finished_at: string;
  error: string | null;
}

/** `GET /api/sync/openrouter/status`. */
export interface SyncStatus {
  /** The most recent run, when one exists. */
  last_run: SyncRun | null;
  /** True when the last successful snapshot is older than the TTL or absent. */
  snapshot_stale: boolean;
  ttl_hours: number;
}

/** The failure text recorded in a run's detail, when the run failed. */
export function runError(run: SyncRun): string | null {
  const error: unknown = run.detail?.['error'];
  return typeof error === 'string' ? error : null;
}
