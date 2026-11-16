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

/** The three combo strategies (decision D6). */
export type ComboStrategy = 'ordered' | 'weighted' | 'round_robin';

/** The named weight presets (decision D24). */
export type WeightPreset = 'cheapest' | 'fastest' | 'balanced' | 'best';

/** The four scoring components, in display order. */
export type WeightComponent = 'price' | 'speed' | 'quality' | 'reliability';

export const WEIGHT_COMPONENTS: readonly WeightComponent[] = [
  'price',
  'speed',
  'quality',
  'reliability',
];

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
  /** Stored order: the fallback chain for `ordered`, the rotation for `round_robin`. */
  members: ComboMember[];
  enabled: boolean;
  /** The persisted round-robin rotation counter. */
  rr_cursor: number;
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
}
