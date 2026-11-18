/**
 * Wire types for provider connectors (D42, D44): the declarative profile that
 * maps a provider endpoint to catalog prices, a balance number, or a health
 * answer.
 *
 * The shapes mirror `server/src/connectors.rs`. A connector document is plain
 * JSON, so the UI edits it field by field instead of exposing a textarea.
 */

/** The three connector families. */
export type ConnectorKind = 'catalog' | 'balance' | 'health';

/** Provider-relative (`/models`) or absolute (`https://…`) URL. */
export interface ConnectorConfig {
  url: string;
  /** Extra headers merged over the credential's headers; values may use `{{secret}}`. */
  headers: Record<string, string>;
  /** Allow loopback and private destinations (local test providers only). */
  allow_private: boolean;
  catalog?: CatalogConfig | null;
  balance?: BalanceConfig | null;
  health?: HealthConfig | null;
}

/** How the published numbers are expressed, and how to display them (D44). */
export type UnitSource = 'per_million_tokens' | 'per_token' | 'custom';

export interface PriceDisplay {
  symbol: string;
  decimals: number;
}

export interface PriceUnit {
  source: UnitSource;
  /** Applied only for `custom`; e.g. `0.01` for prices quoted in cents. */
  multiplier: number;
  /** ISO currency of the published numbers. */
  currency: string;
  /** Static conversion; non-USD values feed routing only when this is set. */
  fx_to_usd: number | null;
  display: PriceDisplay;
  /** Free-text reminder shown by the UI. */
  note: string | null;
}

/** JSON Pointers into one model entry of the catalog payload (D41). */
export interface CatalogConfig {
  entries: string;
  model_id: string;
  input: string | null;
  output: string | null;
  cache_read: string | null;
  cache_write: string | null;
  reasoning: string | null;
  unit: PriceUnit;
  modalities_input: string | null;
  modalities_output: string | null;
  context_length: string | null;
  max_output: string | null;
  apply_on_discovery: boolean;
  /** Optional scheduled refresh cadence in seconds. */
  refresh_seconds: number | null;
}

/** A balance amount's unit (D52): the catalog unit without a token base. */
export interface BalanceUnit {
  /** Applied to balance, period spend, and burn rate; e.g. `0.01` for cents. */
  multiplier: number;
  /** ISO currency of the published numbers (used when `currency` is absent). */
  currency: string;
  /** Static conversion; non-USD balances are gated on only when this is set. */
  fx_to_usd: number | null;
  display: PriceDisplay;
  /** Free-text reminder shown by the UI. */
  note: string | null;
}

/** Pointers into the balance payload (all relative to the response root). */
export interface BalanceConfig {
  balance: string;
  /**
   * Optional pointer subtracted from `balance` before the multiplier, e.g.
   * OpenRouter's remaining = total_credits − total_usage.
   */
  subtract: string | null;
  available: string | null;
  /** Literal currency code or a pointer to a currency string. */
  currency: string | null;
  period_spend: string | null;
  burn_rate: string | null;
  days_remaining: string | null;
  unit: BalanceUnit;
  /** Defaults to 900 server-side, bounded to 60…86 400. */
  refresh_seconds: number | null;
}

/** Health connector options. */
export interface HealthConfig {
  /** Optional pointer whose truthy value means healthy. */
  ok_pointer: string | null;
}

/** One shipped template; selecting it prefills the active kind's document. */
export interface ConnectorTemplate {
  id: string;
  label: string;
  description: string;
  configs: {
    catalog?: ConnectorConfig;
    balance?: ConnectorConfig;
    health?: ConnectorConfig;
  };
}

/** A compact connector entry on the provider list. */
export interface ConnectorSummary {
  kind: ConnectorKind;
  enabled: boolean;
  updated_at: string;
}

/** `GET/PUT /api/providers/{id}/connectors/{kind}`. */
export interface ConnectorView {
  provider_id: string;
  kind: ConnectorKind;
  configured: boolean;
  enabled: boolean;
  /** `null` until the connector has been saved once. */
  config: ConnectorConfig | null;
  updated_at: string | null;
  updated_by: string | null;
}

/** Body of `PUT /api/providers/{id}/connectors/{kind}`. */
export interface ConnectorPut {
  config: ConnectorConfig;
  enabled: boolean;
}

/** Optional draft config accepted by sample and test. */
export interface ConnectorDraft {
  config?: ConnectorConfig;
  credential_id?: string;
}

/** `POST .../connectors/{kind}/sample`: the raw document, truncated. */
export interface SampleView {
  kind: ConnectorKind;
  url: string;
  status: number;
  bytes: number;
  truncated: boolean;
  /** Raw JSON text; parse in the UI for the tree. */
  body: string;
  fetched_at: string;
}

/** The provider's own numbers before unit conversion. */
export interface PriceSet {
  input: number | null;
  output: number | null;
  cache_read: number | null;
  cache_write: number | null;
}

/** One resolved catalog entry in the preview. */
export interface CatalogSample {
  model_id: string | null;
  input_per_m: number | null;
  output_per_m: number | null;
  cache_read_per_m: number | null;
  cache_write_per_m: number | null;
  reasoning_per_m: number | null;
  raw: PriceSet;
  converted_per_m: PriceSet;
  warnings: string[];
  currency: string;
  routable: boolean;
  variable_price: boolean;
  error: string | null;
}

/** `POST .../connectors/{kind}/test`; writes nothing. */
export interface ConnectorTestView {
  kind: ConnectorKind;
  ok: boolean;
  url: string;
  entries_seen: number | null;
  matched: number | null;
  skipped: number | null;
  samples: CatalogSample[];
  errors: string[];
  warnings: string[];
  currency: string | null;
  balance: ExtractedBalance | null;
  health: ExtractedHealth | null;
}

export interface ExtractedBalance {
  available: boolean;
  /** USD when routable, otherwise the amount in `currency`. */
  balance_usd: number | null;
  currency: string;
  /** The value after `subtract`, before the unit multiplier. */
  raw: number | null;
  /** `raw × multiplier`, in the source currency. */
  converted: number | null;
  /** False for a non-USD balance without FX; routing and the gate ignore it. */
  routable: boolean;
  period_spend: number | null;
  burn_rate: number | null;
  days_remaining: number | null;
  warnings: string[];
}

export interface ExtractedHealth {
  ok: boolean;
  detail: string | null;
}

/** `POST /api/providers/{id}/prices/refresh`. */
export interface PriceRefreshView {
  ok: boolean;
  provider_id: string;
  run_id: string | null;
  entries_seen: number;
  created: number;
  updated: number;
  unchanged: number;
  skipped_manual: number;
  skipped_unresolved: number;
  source_url: string;
  fetched_at: string;
  error: string | null;
}

/** `POST .../credentials/{cid}/balance/refresh`. */
export interface BalanceRefreshView {
  ok: boolean;
  credential_id: string;
  available: boolean;
  balance_usd: number | null;
  currency: string;
  raw: number | null;
  converted: number | null;
  routable: boolean;
  period_spend: number | null;
  burn_rate: number | null;
  days_remaining: number | null;
  warnings: string[];
  error: string | null;
  fetched_at: string;
}

/** `POST /api/providers/{id}/balances/refresh`. */
export interface ProviderBalancesView {
  provider_id: string;
  refreshed: number;
  failed: number;
  results: BalanceRefreshView[];
}

/** The default document for a kind, used when nothing is stored yet. */
export function emptyConnectorConfig(kind: ConnectorKind): ConnectorConfig {
  return {
    url: '',
    headers: { authorization: 'Bearer {{secret}}' },
    allow_private: false,
    catalog:
      kind === 'catalog'
        ? {
            entries: '/data',
            model_id: '/id',
            input: null,
            output: null,
            cache_read: null,
            cache_write: null,
            reasoning: null,
            unit: {
              source: 'per_million_tokens',
              multiplier: 1,
              currency: 'USD',
              fx_to_usd: null,
              display: { symbol: '$', decimals: 4 },
              note: null,
            },
            modalities_input: null,
            modalities_output: null,
            context_length: null,
            max_output: null,
            apply_on_discovery: false,
            refresh_seconds: null,
          }
        : null,
    balance:
      kind === 'balance'
        ? {
            balance: '/balance',
            subtract: null,
            available: null,
            currency: 'USD',
            period_spend: null,
            burn_rate: null,
            days_remaining: null,
            unit: {
              multiplier: 1,
              currency: 'USD',
              fx_to_usd: null,
              display: { symbol: '$', decimals: 2 },
              note: null,
            },
            refresh_seconds: 900,
          }
        : null,
    health: kind === 'health' ? { ok_pointer: null } : null,
  };
}
