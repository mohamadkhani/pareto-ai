/**
 * Wire types and pure helpers for token-mix profiles and mix prices (D34).
 *
 * A profile is the client's token shape: disjoint shares of input, output,
 * cache read, cache write, and reasoning tokens. The **mix price** is the
 * offering's price blended at those shares — it drives routing and the Models
 * page, but never billing (`usage_events.cost_usd` keeps component pricing per
 * D22).
 *
 * The blend mirrors the backend exactly (`server/src/mix.rs`):
 *
 * * shares are normalised to sum to 1 before use;
 * * a share of zero is skipped entirely;
 * * cache read and cache write fall back to the *input* price, reasoning to
 *   the *output* price — never dropped and never renormalised;
 * * a positive-share component with no price (and no fallback) makes the
 *   blend unknown, never $0.
 */

/** How a profile was obtained; only `measured` changes the price badge. */
export type MixProfileKind = 'preset' | 'custom' | 'measured';

/** A stored token-mix profile (`GET /api/mix-profiles`). */
export interface MixProfile {
  id: string;
  name: string;
  kind: MixProfileKind;
  input_share: number;
  output_share: number;
  cache_read_share: number;
  cache_write_share: number;
  reasoning_share: number;
  /** Built-ins can be cloned but never edited or deleted. */
  is_builtin: boolean;
  created_at: string;
  updated_at: string;
}

/** The create/update body of a profile (`POST`/`PUT /api/mix-profiles`). */
export interface MixProfileInput {
  name: string;
  kind: MixProfileKind;
  input_share: number;
  output_share: number;
  cache_read_share: number;
  cache_write_share: number;
  reasoning_share: number;
}

/** A secret-free reference to a profile, as responses echo it. */
export interface MixProfileRef {
  profile_id: string;
  profile_name: string;
}

/** `GET`/`PUT /api/settings/pricing`. */
export interface PricingSettings {
  /** The stored selector (canonical profile id), or null when unset. */
  default_mix_profile: string | null;
  /** Environment > setting > built-in: what routing actually uses. */
  effective_profile: MixProfileRef;
  /** `GATEWAY_TOKEN_MIX_PROFILE` when set; it wins over the setting. */
  env_override: string | null;
}

/** `measured` when calibrated from our traffic, else `blended`. */
export type MixPriceBasis = 'blended' | 'measured';

/** One offering's mix price as the overview API returns it. */
export interface MixPriceView {
  per_m: number;
  profile_id: string;
  profile_name: string;
  basis: MixPriceBasis;
}

/** The five blend components, in display order. */
export const MIX_COMPONENTS = [
  'input',
  'output',
  'cache_read',
  'cache_write',
  'reasoning',
] as const;

export type MixComponent = (typeof MIX_COMPONENTS)[number];

export const MIX_COMPONENT_LABELS: Record<MixComponent, string> = {
  input: 'Input',
  output: 'Output',
  cache_read: 'Cache read',
  cache_write: 'Cache write',
  reasoning: 'Reasoning',
};

/** The stored share of one component on a profile. */
export function profileShare(profile: MixProfile, component: MixComponent): number {
  switch (component) {
    case 'input':
      return profile.input_share;
    case 'output':
      return profile.output_share;
    case 'cache_read':
      return profile.cache_read_share;
    case 'cache_write':
      return profile.cache_write_share;
    case 'reasoning':
      return profile.reasoning_share;
  }
}

/** The raw stored shares, before normalisation (they may not sum to 1). */
export function profileShareTotal(profile: MixProfile): number {
  return MIX_COMPONENTS.reduce((total, component) => total + profileShare(profile, component), 0);
}

/**
 * The profile's shares as percentages for display, e.g. `8.1 / 0.4 / 91.5`.
 * Input, output, and cache read are the research's three-component model; the
 * optional write/reasoning shares are appended only when they are non-zero.
 */
export function profileSummary(profile: MixProfile): string {
  const parts = [profile.input_share, profile.output_share, profile.cache_read_share].map(
    formatShare,
  );
  if (profile.cache_write_share !== 0) {
    parts.push(`w ${formatShare(profile.cache_write_share)}`);
  }
  if (profile.reasoning_share !== 0) {
    parts.push(`r ${formatShare(profile.reasoning_share)}`);
  }
  return parts.join(' / ');
}

function formatShare(value: number): string {
  return Number.isInteger(value) ? value.toFixed(0) : String(value);
}

/** One offering's component prices, as the detail dialog needs them. */
export interface MixComponentPrices {
  input_per_m: number | null;
  output_per_m: number | null;
  cache_read_per_m: number | null;
  cache_write_per_m: number | null;
  reasoning_per_m: number | null;
}

/** One row of the per-component contribution table. */
export interface MixBreakdownRow {
  component: MixComponent;
  /** The normalised share the blend used. */
  share: number;
  /** The price the component resolved to (its own, or the inherited one). */
  price_per_m: number;
  contribution_per_m: number;
  /** The component whose price was inherited, or null when own price used. */
  inherited_from: 'input' | 'output' | null;
}

/** A computable mix price with its contribution breakdown. */
export interface MixBreakdown {
  per_m: number;
  rows: MixBreakdownRow[];
}

/** The price a component falls back to when it has no price of its own. */
function fallbackOf(component: MixComponent): 'input' | 'output' | null {
  switch (component) {
    case 'cache_read':
    case 'cache_write':
      return 'input';
    case 'reasoning':
      return 'output';
    default:
      return null;
  }
}

function ownPrice(prices: MixComponentPrices, component: MixComponent): number | null {
  switch (component) {
    case 'input':
      return prices.input_per_m;
    case 'output':
      return prices.output_per_m;
    case 'cache_read':
      return prices.cache_read_per_m;
    case 'cache_write':
      return prices.cache_write_per_m;
    case 'reasoning':
      return prices.reasoning_per_m;
  }
}

/**
 * Blends component prices at a profile's shares. Null when the profile's
 * shares are invalid (sum <= 0) or a positive-share component is unknown even
 * after its fallback — unknown is never rendered as $0.
 */
export function mixBreakdown(profile: MixProfile, prices: MixComponentPrices): MixBreakdown | null {
  const total = profileShareTotal(profile);
  if (!Number.isFinite(total) || total <= 0) {
    return null;
  }
  let perM = 0;
  const rows: MixBreakdownRow[] = [];
  for (const component of MIX_COMPONENTS) {
    const share = profileShare(profile, component) / total;
    if (share <= 0) {
      continue;
    }
    const own = ownPrice(prices, component);
    const fallback = fallbackOf(component);
    const inherited = own === null && fallback !== null ? ownPrice(prices, fallback) : own;
    if (inherited === null) {
      return null;
    }
    const contribution = share * inherited;
    perM += contribution;
    rows.push({
      component,
      share,
      price_per_m: inherited,
      contribution_per_m: contribution,
      inherited_from: own === null ? fallback : null,
    });
  }
  return { per_m: perM, rows };
}
