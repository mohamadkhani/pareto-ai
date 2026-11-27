/**
 * Wire types for the M5 combo-model admin API (`/api/combos`).
 *
 * A combo is a named pool of **offerings** addressable as the model id
 * `combo:<name>` in `/v1/chat/completions`. `strategy` picks the ordering
 * (`ordered` fallback chain, `weighted` scoring, `round_robin` rotation) and
 * `weights` configures the scoring. Failures use the
 * `{ error: { code, message, request_id } }` envelope (parsed by `api.ts`);
 * mutations require the CSRF token. Aligned against the M5 backend contract
 * (2026-11-16): the create/update body takes weights as `null`, a preset
 * name, or partial custom components, while responses carry the effective
 * weights plus the preset name separately.
 */

import type { MixProfileRef } from '../catalog/mix.types';

/** The three combo strategies (decision D6). */
export type ComboStrategy = 'ordered' | 'weighted' | 'round_robin';

/**
 * Where the pool comes from (D35): an explicit member list, or every eligible
 * catalog offering that satisfies the combo's constraints.
 */
export type ComboPool = 'members' | 'all';

/** How a sticky pin finds its session (D35). */
export type StickinessKeySource = 'auto' | 'header';

/** The `pool = all` ordering axis (D65). */
export type PoolSort = 'price' | 'quality' | 'speed' | 'reliability';

/** How a catalog pool picks among the providers of one model (D65). */
export type ProviderSelection = 'mix_price' | 'speed' | 'reliability';

/** Per-session stickiness as stored and returned (D35, D65). */
export interface ComboStickiness {
  enabled: boolean;
  /** Pin lifetime; the server accepts 60 s – 24 h (default 1800). */
  ttl_seconds: number;
  key_source: StickinessKeySource;
  /**
   * How much pricier the pin may be than the current best provider before the
   * session moves (percent, 0–100; default 5).
   */
  price_tolerance_pct: number;
}

/** The named weight presets (decision D24). */
export type WeightPreset = 'cheapest' | 'fastest' | 'balanced' | 'best';

/** The four scoring components, in display order. */
export type WeightComponent = 'price' | 'speed' | 'quality' | 'reliability';

/** The four scoring weights; every component is non-negative. */
export interface ComboWeights {
  price: number;
  speed: number;
  quality: number;
  reliability: number;
}

/** One member of a combo: an offering plus its score multiplier. */
export interface ComboMember {
  offering_id: string;
  /** `weighted` strategy score multiplier; the wire defaults it to 1.0. */
  weight: number;
}

/** One combo as the admin API returns it. */
export interface Combo {
  id: string;
  /** The name behind the `combo:<name>` model-id namespace. */
  name: string;
  strategy: ComboStrategy;
  /** Effective scoring weights (server-side defaults already filled in). */
  weights: ComboWeights;
  /** The named preset behind `weights`, when one was used. */
  preset: WeightPreset | null;
  /** The combo's token-mix profile id, when it overrides the global default. */
  mix_profile: string | null;
  /** `members` for an explicit list, `all` for the constrained catalog pool. */
  pool: ComboPool;
  /** Inclusive minimum resolved quality (`pool = all` only). */
  min_quality: number | null;
  /** Inclusive maximum mix price per million tokens (`pool = all` only). */
  max_mix_price_per_m: number | null;
  /** The `pool = all` ordering axis (D65). */
  sort: PoolSort;
  /** First tie-break of the `pool = all` ordering (D65). */
  sort_2: PoolSort;
  /** Second tie-break of the `pool = all` ordering (D65). */
  sort_3: PoolSort;
  /** How the pool picks among the providers of one model (D65). */
  provider_selection: ProviderSelection;
  /** Stored order: the fallback chain for `ordered`, the rotation for `round_robin`. */
  members: ComboMember[];
  enabled: boolean;
  /** The persisted round-robin rotation counter. */
  rr_cursor: number;
  stickiness: ComboStickiness;
}

/**
 * The `weights` field of a create/update body: omit it (`null`) for the
 * balanced default, name a preset, or send any subset of the four components
 * (missing ones fall back to the balanced defaults server-side).
 */
export type ComboWeightsInput =
  null | { preset: WeightPreset } | Partial<Record<WeightComponent, number>>;

/** One member of a create/update body. */
export interface ComboMemberInput {
  offering_id: string;
  /** Score multiplier; omitted means 1.0. Must be positive. */
  weight?: number;
}

/** `POST /api/combos` and `PUT /api/combos/{id}` (the name is immutable). */
export interface ComboInput {
  name: string;
  strategy: ComboStrategy;
  weights: ComboWeightsInput;
  /** Ordered; the order is the fallback chain for `ordered`. */
  members: ComboMemberInput[];
  enabled: boolean;
  /** `members` (default) or `all` (D35). */
  pool?: ComboPool;
  /** Inclusive minimum quality for a catalog pool. */
  min_quality?: number | null;
  /** Inclusive maximum mix price per million tokens for a catalog pool. */
  max_mix_price_per_m?: number | null;
  /** A mix profile id or name; null uses the global default (D34). */
  mix_profile?: string | null;
  /** Session stickiness; absent means disabled with the defaults. */
  stickiness?: ComboStickiness;
  /** The `pool = all` ordering axis; absent means `price` (D65). */
  sort?: PoolSort;
  /** First tie-break; absent means `quality` (D65). */
  sort_2?: PoolSort;
  /** Second tie-break; absent means `reliability` (D65). */
  sort_3?: PoolSort;
  /** How the pool picks among the providers of one model (D65). */
  provider_selection?: ProviderSelection;
}

// ---------------------------------------------------------------------------
// Preview (`POST /api/combos/preview`, `GET /api/combos/{id}/preview`)
// ---------------------------------------------------------------------------

/** The score breakdown the preview returns (same shape as a decision). */
export interface ComboPreviewScore {
  price: number;
  speed: number;
  quality: number;
  reliability: number;
  penalties: number;
  total: number;
  imputed: string[];
  shrunk: string[];
}

/** One ranked candidate — exactly what routing would consider, in order. */
export interface ComboPreviewCandidate {
  offering_id: string;
  model: string;
  provider: string;
  quality: number;
  /** `override`, `benchmark`, `benchmark_shrunk`, or `model_default`. */
  quality_source: string;
  /** The blend at the effective profile; null when it cannot be computed. */
  mix_price: number | null;
  score: ComboPreviewScore;
  explanation: string;
}

/** One offering the pool rejected, with the reason and its detail line. */
export interface ComboPreviewExcluded {
  offering_id: string;
  model: string;
  provider: string;
  reason: string;
  detail: string;
}

/** The current pin for the supplied session key, when stickiness is on. */
export interface ComboPreviewSticky {
  enabled: boolean;
  key_source: StickinessKeySource;
  session_key: string | null;
  pinned: string | null;
  age_s: number | null;
}

/** The preview response: the profile used, ranked candidates, and exclusions. */
export interface ComboPreview {
  profile: MixProfileRef;
  /**
   * How many offerings passed every filter and were scored. `candidates` can
   * be shorter (the request's `limit` truncates the list, not the count);
   * every explanation says "#N of scored", so the header prints both.
   */
  scored: number;
  candidates: ComboPreviewCandidate[];
  excluded: ComboPreviewExcluded[];
  sticky: ComboPreviewSticky | null;
}

/** Human labels for the server's stable exclusion reasons. */
export const POOL_REASON_LABELS: Record<string, string> = {
  removed: 'Removed by sync (provider gone)',
  disabled: 'Offering disabled',
  no_access: 'No access (grants)',
  cooldown: 'Credential cooling down',
  breaker_open: 'Breaker open',
  no_credential: 'No credential',
  capability: 'Capability mismatch',
  context: 'Context too small',
  budget: 'Budget exhausted',
  unpriced: 'Unpriced',
  below_min_quality: 'Below minimum quality',
  above_max_price: 'Above maximum mix price',
};

/** One group of exclusions that share a reason. */
export interface ExcludedGroup {
  reason: string;
  label: string;
  items: ComboPreviewExcluded[];
}

/** Groups exclusions by reason, preserving the server's first-seen order. */
export function excludedGroups(excluded: readonly ComboPreviewExcluded[]): ExcludedGroup[] {
  const groups = new Map<string, ExcludedGroup>();
  for (const item of excluded) {
    const group = groups.get(item.reason);
    if (group !== undefined) {
      group.items.push(item);
      continue;
    }
    groups.set(item.reason, {
      reason: item.reason,
      label: POOL_REASON_LABELS[item.reason] ?? item.reason,
      items: [item],
    });
  }
  return [...groups.values()];
}
