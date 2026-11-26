/** Small display helpers shared by the catalog pages. */

/** Formats an RFC-3339 timestamp for display; '—' when absent or invalid. */
export function formatTime(iso: string | null): string {
  if (iso === null) {
    return '—';
  }
  const time = new Date(iso);
  return Number.isNaN(time.getTime()) ? iso : time.toLocaleString();
}

/** Compact age such as "5m ago"; pass `now` for deterministic tests. */
export function formatAge(iso: string | null, now: number = Date.now()): string {
  if (iso === null) {
    return '—';
  }
  const time = new Date(iso).getTime();
  if (Number.isNaN(time)) {
    return iso;
  }
  const seconds = Math.max(0, Math.round((now - time) / 1000));
  if (seconds < 60) {
    return `${seconds}s ago`;
  }
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes}m ago`;
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 48) {
    return `${hours}h ago`;
  }
  return `${Math.floor(hours / 24)}d ago`;
}

/** USD amount for display; unknown stays '—' and never renders as 0. */
export function formatUsd(value: number | null): string {
  return value === null ? '—' : `$${value.toFixed(2)}`;
}

/**
 * USD amount with adaptive precision for per-request cost, where a two-decimal
 * formatter collapses almost every request to `$0.00`. Whole-dollar amounts
 * keep two decimals, cents keep four, and smaller amounts keep up to six
 * significant decimals — mirroring `formatPricePerM`, so the console has one
 * idea of "small money". Unknown stays '—', never `$0`.
 */
export function formatUsdPrecise(value: number | null): string {
  if (value === null) {
    return '—';
  }
  const magnitude = Math.abs(value);
  if (magnitude >= 1) {
    return `$${value.toFixed(2)}`;
  }
  if (magnitude >= 0.01) {
    return `$${value.toFixed(4)}`;
  }
  const trimmed = value.toFixed(6).replace(/0+$/, '').replace(/\.$/, '');
  return `$${trimmed}`;
}

/**
 * USD per 1M tokens for the models table. Exact whole-cent values keep two
 * decimals; sub-cent prices keep up to six significant decimals so a real
 * price never collapses to `$0.00`. Unknown is always '—', never `$0`.
 */
export function formatPricePerM(value: number | null): string {
  if (value === null) {
    return '—';
  }
  if (Math.abs(value) >= 1) {
    return `$${value.toFixed(2)}`;
  }
  const trimmed = value.toFixed(6).replace(/0+$/, '').replace(/\.$/, '');
  return `$${trimmed}`;
}

/** Groups a count (tokens, models) for display; unknown stays '—'. */
export function formatCount(value: number | null): string {
  return value === null ? '—' : value.toLocaleString('en-US');
}

/** Splits a comma-separated tag field into trimmed, non-empty tags. */
export function parseTags(value: string): string[] {
  return value
    .split(',')
    .map((tag) => tag.trim())
    .filter((tag) => tag.length > 0);
}

/** Joins tags back into the comma-separated form field representation. */
export function joinTags(tags: readonly string[]): string {
  return tags.join(', ');
}
