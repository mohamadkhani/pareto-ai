/**
 * Wire types for `GET /api/models/overview?provider=<id|all>` (decision D31).
 *
 * The route merges models, offerings, prices, and the resolved quality into
 * one payload so the Models page never joins the catalog client-side:
 *
 * * `scope = "provider"` — one row per offering of that provider, with the
 *   offering identity, effective limits (offering override wins), its price
 *   row, and the model's resolved quality;
 * * `scope = "all"` — one row per model, aggregating every offering across
 *   providers, with average/minimum price components and the model-level
 *   quality.
 *
 * Price aggregation only counts *priced* offerings (at least one non-null
 * component). An offering with no price row, or an all-null row, is counted
 * in `unpriced_offerings` and never drags an average toward zero — unknown is
 * never 0 (D10).
 *
 * Every response also carries the token-mix profile (`mix`) and a per-row
 * `mix_price` (plus `mix_price_min` in the `all` scope) computed at that
 * profile's shares (D34). `mix_price` is the routing price metric, never the
 * billing price.
 */

import type { MixPriceView, MixProfileRef } from './mix.types';

/** Which shape `rows` carries. */
export type OverviewScope = 'all' | 'provider';

/** Resolved quality with provenance; the backend always sends one per row. */
export interface OverviewQuality {
  /** Override, benchmark score (possibly shrunk), or the neutral default 0.5. */
  score: number;
  /** `override`, `benchmark`, `benchmark_shrunk`, or `model_default`. */
  source: string;
  category: string | null;
  ci: number | null;
  rank: number | null;
  fetched_at: string | null;
  /** True when the operator's override supplied `score`. */
  overridden: boolean;
}

/** One offering's complete price row (`scope = "provider"`). */
export interface OverviewPrice {
  input_per_m: number | null;
  output_per_m: number | null;
  cache_read_per_m: number | null;
  cache_write_per_m: number | null;
  reasoning_per_m: number | null;
  request_fee: number | null;
  /**
   * The source publishes a variable price (the `-1` sentinel) instead of a
   * fixed number: the affected components are null and the UI shows
   * "variable", never a price.
   */
  variable_price: boolean;
  currency: string;
  /** `manual`, `openrouter`, `models_dev`, or `custom_api`. */
  source: string;
  fetched_at: string | null;
}

/** Average or minimum over a model's priced offerings (`scope = "all"`). */
export interface AggregatePrice {
  input_per_m: number | null;
  output_per_m: number | null;
  cache_read_per_m: number | null;
}

/** One offering row of a provider (`scope = "provider"`). */
export interface ProviderOverviewRow {
  model_id: string;
  canonical_name: string;
  family: string | null;
  modalities_input: string[] | null;
  modalities_output: string[] | null;
  /** Effective context: the offering override wins over the model value. */
  context_length: number | null;
  /** Effective max output: the offering override wins over the model value. */
  max_output_tokens: number | null;
  offering_id: string;
  /** The provider that owns the offering; used by per-provider notices. */
  provider_id: string;
  upstream_model_id: string;
  /**
   * The provider's own switch. A disabled provider excludes its offerings from
   * routing, so an enabled offering on a disabled provider is still inactive.
   */
  provider_enabled: boolean;
  /** The offering's own flag; `model_enabled` overrides it when false. */
  enabled: boolean;
  /**
   * False when the model is disabled for every provider. The per-provider
   * switch cannot re-enable it until the model itself is enabled.
   */
  model_enabled: boolean;
  /** Null when the offering has no price row or an all-null price row. */
  price: OverviewPrice | null;
  /** The blend at the selected mix profile; null when it cannot be computed. */
  mix_price: MixPriceView | null;
  quality: OverviewQuality | null;
}

/** One provider that offers a model, ordered by name (`scope = "all"`). */
export interface OverviewProvider {
  id: string;
  name: string;
}

/** One model aggregated across providers (`scope = "all"`). */
export interface ModelOverviewRow {
  model_id: string;
  canonical_name: string;
  family: string | null;
  modalities_input: string[] | null;
  modalities_output: string[] | null;
  /** Largest effective context across the model's offerings. */
  context_length: number | null;
  /** Largest effective max output across the model's offerings. */
  max_output_tokens: number | null;
  /**
   * False when the model is disabled for every provider. Routing and
   * `/v1/models` skip every offering of the model, including offerings created
   * after the switch was flipped.
   */
  model_enabled: boolean;
  /** Every provider that offers the model, ordered by name. */
  providers: OverviewProvider[];
  providers_count: number;
  offerings_count: number;
  /**
   * Offerings routing would actually consider: the offering, its provider, and
   * the model are all enabled (D61). Zero for a model whose every offering is
   * disabled or whose only providers are disabled, even while `model_enabled`
   * is true. The default "active" view keys on this, not `model_enabled` alone.
   */
  routable_offerings: number;
  /** Offerings with no price row or an all-null price row (never averaged). */
  unpriced_offerings: number;
  /**
   * Offerings whose source publishes a variable price. When every component
   * is variable the offering is unpriced too, so the UI shows "variable"
   * rather than "unpriced" for it.
   */
  variable_offerings: number;
  /** Null when the model has no priced offering at all. */
  price_avg: AggregatePrice | null;
  /** Null when the model has no priced offering at all. */
  price_min: AggregatePrice | null;
  /** Average mix price over offerings whose blend is computable. */
  mix_price: MixPriceView | null;
  /** Minimum mix price over offerings whose blend is computable. */
  mix_price_min: MixPriceView | null;
  quality: OverviewQuality | null;
}

/** A row of either scope; `overview.scope` selects the shape. */
export type OverviewRow = ProviderOverviewRow | ModelOverviewRow;

/** Response of `GET /api/models/overview`. */
export interface ModelsOverview {
  scope: OverviewScope;
  /** The provider id when `scope = "provider"`, else null. */
  provider_id: string | null;
  /** The token-mix profile every `mix_price` was computed with. */
  mix: MixProfileRef;
  rows: OverviewRow[];
}

/** Narrows a row to the per-offering shape. */
export function isProviderRow(row: OverviewRow): row is ProviderOverviewRow {
  return 'offering_id' in row;
}

/** A stable key for a row across reloads (`offering:<id>` or `model:<id>`). */
export function overviewRowKey(row: OverviewRow): string {
  return isProviderRow(row) ? `offering:${row.offering_id}` : `model:${row.model_id}`;
}
