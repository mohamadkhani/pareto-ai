/**
 * Pure helpers for the connector dialog: `$.` path building/resolution,
 * expression syntax checks (D61), unit conversion and display (D44), tier
 * detection, and template detection.
 *
 * Kept free of Angular so the picking and conversion rules are unit-testable.
 */
import type {
  BalanceConfig,
  BalanceUnit,
  CatalogConfig,
  ConnectorConfig,
  ConnectorKind,
  ConnectorTemplate,
  PriceDisplay,
  PriceUnit,
} from './connectors.types';

/** The tier keys the shipped providers use; detection also reads the sample. */
export const KNOWN_TIERS = ['asap', 'balanced', 'flex'] as const;

/** One pointer's origin, used by the tier rewrite. */
export interface PointerAssignment {
  field: string;
  pointer: string;
}

export interface TierInfo {
  /** Segment index shared by every picked pointer. */
  index: number;
  /** Pointer of the parent object, e.g. `$.pricing_usd_per_million_tokens`. */
  parentPointer: string;
  /** The tier currently selected, e.g. `balanced`. */
  current: string;
  /** Sibling keys found in the sample, e.g. `['asap', 'balanced', 'flex']`. */
  options: string[];
}

/**
 * Builds the expression-language path from tree segments (`D61`): the root is
 * `$`, object keys are joined with `.`, and a numeric array index uses `[n]`.
 */
export function pointerFromPath(path: readonly (string | number)[]): string {
  let out = '$';
  for (const segment of path) {
    if (typeof segment === 'number' || /^\d+$/.test(String(segment))) {
      out += `[${segment}]`;
    } else {
      out += `.${segment}`;
    }
  }
  return out;
}

/** Splits `$.data[0].pricing.input` into `['data', '0', 'pricing', 'input']`. */
export function splitPointer(pointer: string): string[] {
  const trimmed = pointer.trim();
  if (trimmed === '' || trimmed === '$') {
    return [];
  }
  if (!trimmed.startsWith('$')) {
    return [];
  }
  const segments: string[] = [];
  const rest = trimmed.slice(1);
  const token = /^(?:\.([A-Za-z0-9_-]+))|(?:\[(\d+)\])/;
  let consumed = 0;
  while (consumed < rest.length) {
    const match = token.exec(rest.slice(consumed));
    if (match === null) {
      return [];
    }
    const key = match[1];
    const index = match[2];
    segments.push(key ?? index ?? '');
    consumed += match[0].length;
  }
  return segments;
}

/**
 * Resolves a lone path against a parsed JSON document; returns `undefined`
 * when a step is missing or an array index is malformed. An expression with
 * arithmetic is not resolvable here, so it yields `undefined`.
 */
export function resolvePointer(document: unknown, pointer: string): unknown {
  const trimmed = pointer.trim();
  if (trimmed === '$') {
    return document;
  }
  if (!trimmed.startsWith('$')) {
    return undefined;
  }
  const segments = splitPointer(trimmed);
  if (segments.length === 0 && trimmed !== '$') {
    return undefined;
  }
  let current: unknown = document;
  for (const segment of segments) {
    if (Array.isArray(current)) {
      if (!/^\d+$/.test(segment)) {
        return undefined;
      }
      current = current[Number(segment)];
    } else if (isRecord(current)) {
      if (!Object.prototype.hasOwnProperty.call(current, segment)) {
        return undefined;
      }
      current = current[segment];
    } else {
      return undefined;
    }
    if (current === undefined) {
      return undefined;
    }
  }
  return current;
}

/** True when the field is a lone path the tree could resolve. */
export function isPath(expression: string): boolean {
  return expression.trim().startsWith('$');
}

/**
 * A light client-side check of one field, so the operator sees the obvious
 * mistakes before the server validates the whole document (D61). The server
 * (`server/src/expression.rs`) is authoritative; this only catches an illegal
 * character, an unbalanced parenthesis, a malformed path, and an operator in a
 * field that must be a lone path.
 */
export function expressionIssue(source: string, mustBePath = false): string | null {
  const text = source.trim();
  if (text === '') {
    return null;
  }
  if (text.startsWith('/')) {
    return 'Paths start with $, not /. Use $.pricing.prompt.';
  }
  const illegal = /[^A-Za-z0-9_$.[\]+\-*/(), \t\n]/.exec(text);
  if (illegal !== null) {
    return `The character ${JSON.stringify(illegal[0])} is not part of an expression.`;
  }
  let depth = 0;
  for (const character of text) {
    if (character === '(') {
      depth += 1;
    } else if (character === ')') {
      depth -= 1;
      if (depth < 0) {
        return 'A closing parenthesis has no opening one.';
      }
    }
  }
  if (depth !== 0) {
    return 'A parenthesis is not closed.';
  }
  if (!text.includes('$')) {
    return 'An expression needs a path, e.g. $.pricing.prompt.';
  }
  // Every `$` must begin a path with at least one key or index.
  const bare = /\$(?![.[A-Za-z0-9_-])/.test(text) || /\$(?=[\s)]|$)/.test(text);
  const first = /\$(?:\.([A-Za-z0-9_-]+)|\[(\d+)\])*/.exec(text);
  if (first === null || first[0] === '$' || bare) {
    return 'A path needs a key after $, e.g. $.data or $.data[0].id.';
  }
  if (mustBePath && text.replace(first[0], '').trim() !== '') {
    return 'This field takes one path only; remove the operator.';
  }
  return null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** True when a pointer resolves to a finite number. */
export function pointerHasNumber(document: unknown, pointer: string | null): boolean {
  const value = pointer === null ? undefined : resolvePointer(document, pointer);
  return typeof value === 'number' && Number.isFinite(value);
}

/** True when a pointer resolves to a non-empty string. */
export function pointerHasString(document: unknown, pointer: string | null): boolean {
  const value = pointer === null ? undefined : resolvePointer(document, pointer);
  return typeof value === 'string' && value.length > 0;
}

/** True when a pointer resolves to an array. */
export function pointerHasArray(document: unknown, pointer: string): boolean {
  return Array.isArray(resolvePointer(document, pointer));
}

/** A short display for one picked value, used beside the field. */
export function sampleValueLabel(document: unknown, pointer: string | null): string | null {
  if (pointer === null || pointer === '') {
    return null;
  }
  const value = resolvePointer(document, pointer);
  if (value === undefined) {
    return null;
  }
  if (typeof value === 'string') {
    return value.length > 40 ? `${value.slice(0, 40)}…` : value;
  }
  if (typeof value === 'number' || typeof value === 'boolean') {
    return String(value);
  }
  if (Array.isArray(value)) {
    return `${value.length} values`;
  }
  return isRecord(value) ? 'object' : null;
}

/** The multiplier that turns a published number into USD per 1M tokens. */
export function unitFactor(unit: Pick<PriceUnit, 'source' | 'multiplier'>): number {
  switch (unit.source) {
    case 'per_token':
      return 1_000_000;
    case 'per_million_tokens':
      return 1;
    case 'custom':
      return unit.multiplier;
  }
}

/** Converts one published number into the unit's currency per 1M tokens. */
export function convertPrice(raw: number, unit: Pick<PriceUnit, 'source' | 'multiplier'>): number {
  return raw * unitFactor(unit);
}

/** Formats a converted price with the connector's symbol and decimals. */
export function formatConverted(perMillion: number, display: PriceDisplay): string {
  const decimals = Number.isInteger(display.decimals)
    ? Math.min(8, Math.max(0, display.decimals))
    : 4;
  return `${display.symbol}${perMillion.toFixed(decimals)}`;
}

/** The live example under the unit fields, e.g. `raw 0.07 → $0.0007/M`. */
export function unitExample(unit: PriceUnit, raw = 0.07): string {
  const converted = convertPrice(raw, unit);
  return `raw ${raw} → ${formatConverted(converted, unit.display)}/M`;
}

/** The balance example's raw value: the SailResearch-shaped cents amount. */
export const BALANCE_EXAMPLE_RAW = 136_275;

/**
 * The factor that turns a published balance into USD: the unit multiplier,
 * then a static FX rate for a non-USD currency that supplies one (D52).
 */
export function balanceFactor(
  unit: Pick<BalanceUnit, 'multiplier' | 'currency' | 'fx_to_usd'>,
): number {
  const multiplier = Number.isFinite(unit.multiplier) ? unit.multiplier : 0;
  const currency = unit.currency.trim().toUpperCase() || 'USD';
  const fx =
    currency === 'USD'
      ? 1
      : unit.fx_to_usd !== null && Number.isFinite(unit.fx_to_usd) && unit.fx_to_usd > 0
        ? unit.fx_to_usd
        : 1;
  return multiplier * fx;
}

/** Converts one published balance amount into the stored (USD) amount. */
export function convertBalance(
  raw: number,
  unit: Pick<BalanceUnit, 'multiplier' | 'currency' | 'fx_to_usd'>,
): number {
  return raw * balanceFactor(unit);
}

/** Formats a balance with the connector's symbol and decimals. */
export function formatBalanceAmount(amount: number, display: PriceDisplay): string {
  const decimals = Number.isInteger(display.decimals)
    ? Math.min(8, Math.max(0, display.decimals))
    : 2;
  return `${display.symbol}${amount.toFixed(decimals)}`;
}

/** The live example under the balance unit fields, e.g. `raw 136275 → $1362.75`. */
export function balanceExample(unit: BalanceUnit, raw = BALANCE_EXAMPLE_RAW): string {
  return `raw ${raw} → ${formatBalanceAmount(convertBalance(raw, unit), unit.display)}`;
}

/**
 * Cents-quoted feeds use `custom` with multiplier 0.01; show the equivalent
 * source choice so the operator can confirm what the invoice says.
 */
export function unitSourceLabel(unit: PriceUnit): string {
  switch (unit.source) {
    case 'per_token':
      return 'USD per token (× 1,000,000)';
    case 'per_million_tokens':
      return 'USD per 1M tokens (× 1)';
    case 'custom':
      return `custom (× ${unit.multiplier})`;
  }
}

/**
 * Finds the shared tier segment of the picked price pointers. Returns null
 * unless every pointer has the same segment at the index and that parent is an
 * object with at least two object-valued siblings in the sample.
 */
export function detectTier(document: unknown, pointers: readonly string[]): TierInfo | null {
  const split = pointers
    .filter((pointer) => isPath(pointer))
    .map((pointer) => splitPointer(pointer));
  if (split.length < 2) {
    return null;
  }
  const longest = Math.max(...split.map((segments) => segments.length));
  for (let index = longest - 2; index >= 0; index -= 1) {
    const groups = new Map<string, string[][]>();
    for (const segments of split) {
      const segment = segments[index];
      if (segment === undefined) {
        continue;
      }
      const group = groups.get(segment) ?? [];
      group.push(segments);
      groups.set(segment, group);
    }
    for (const [tier, members] of groups) {
      if (members.length < 2) {
        continue;
      }
      const parentPointer = pointerFromPath(members[0].slice(0, index));
      const parent = resolvePointer(document, parentPointer);
      if (isRecord(parent)) {
        const options = Object.keys(parent).filter((key) => isRecord(parent[key]));
        if (options.length >= 2 && options.includes(tier)) {
          return { index, parentPointer, current: tier, options };
        }
      }
      if (isKnownTier(tier)) {
        return {
          index,
          parentPointer,
          current: tier,
          options: [...KNOWN_TIERS],
        };
      }
    }
  }
  return null;
}

function isKnownTier(segment: string): boolean {
  return (KNOWN_TIERS as readonly string[]).includes(segment);
}

/**
 * Catalog price pointers are relative to one entry, so a tree click below the
 * entries array must drop that prefix. Returns the pointer unchanged otherwise.
 */
export function entryRelativePointer(entries: string, pointer: string): string {
  const prefix = entries.trim();
  if (prefix === '' || pointer === prefix) {
    return pointer;
  }
  const prefixSegments = splitPointer(prefix);
  const pointerSegments = splitPointer(pointer);
  if (prefixSegments.length === 0) {
    return pointer;
  }
  if (pointerSegments.length <= prefixSegments.length) {
    return pointer;
  }
  const matchesPrefix = prefixSegments.every(
    (segment, index) => pointerSegments[index] === segment,
  );
  if (!matchesPrefix) {
    return pointer;
  }
  const entryIndex = pointerSegments[prefixSegments.length];
  // `entries` may already point at one entry (`$.data[0]`), in which case the
  // next segment is the field itself; otherwise it is the array index to drop.
  if (entryIndex === undefined || !/^\d+$/.test(entryIndex)) {
    return pointerFromPath(pointerSegments.slice(prefixSegments.length));
  }
  return pointerFromPath(pointerSegments.slice(prefixSegments.length + 1));
}

/** Rewrites the shared tier segment of every matching pointer. */
export function rewriteTier(
  assignments: readonly PointerAssignment[],
  info: TierInfo,
  tier: string,
): PointerAssignment[] {
  return assignments.map((assignment) => {
    const segments = splitPointer(assignment.pointer);
    if (segments[info.index] !== info.current) {
      return assignment;
    }
    segments[info.index] = tier;
    return { ...assignment, pointer: pointerFromPath(segments) };
  });
}

/** True when a template's document resolves against the fetched sample. */
export function templateMatches(
  kind: ConnectorKind,
  config: ConnectorConfig | undefined,
  document: unknown,
): boolean {
  if (config === undefined || document === undefined) {
    return false;
  }
  if (kind === 'catalog') {
    const catalog = config.catalog;
    return catalog !== undefined && catalog !== null && catalogMatches(catalog, document);
  }
  if (kind === 'balance') {
    const balance = config.balance;
    return balance !== undefined && balance !== null && balanceMatches(balance, document);
  }
  return true;
}

function catalogMatches(catalog: CatalogConfig, document: unknown): boolean {
  const entries = resolvePointer(document, catalog.entries);
  if (!Array.isArray(entries) || entries.length === 0) {
    return false;
  }
  const first = entries[0];
  if (!pointerHasString(first, catalog.model_id)) {
    return false;
  }
  return (
    pointerHasNumber(first, catalog.input) ||
    pointerHasNumber(first, catalog.output) ||
    pointerHasNumber(first, catalog.cache_read)
  );
}

function balanceMatches(balance: BalanceConfig, document: unknown): boolean {
  return pointerHasNumber(document, balance.balance);
}

/**
 * Picks the first template that resolves against the sample. Balance samples
 * are tried before catalog because a usage payload can also look like an
 * object with numbers.
 */
export function detectTemplateId(
  templates: readonly ConnectorTemplate[],
  kind: ConnectorKind,
  document: unknown,
): string | null {
  const match = templates.find((template) =>
    templateMatches(kind, template.configs[kind], document),
  );
  return match?.id ?? null;
}

/** The price fields that share a tier segment, in display order. */
export const PRICE_POINTER_FIELDS = [
  'input',
  'output',
  'cache_read',
  'cache_write',
  'reasoning',
] as const;

/** Which catalog pointer a click in the tree fills. */
export type CatalogPointerField =
  | 'entries'
  | 'model_id'
  | 'input'
  | 'output'
  | 'cache_read'
  | 'cache_write'
  | 'reasoning'
  | 'modalities_input'
  | 'modalities_output'
  | 'context_length'
  | 'max_output';

export type BalancePointerField =
  'balance' | 'available' | 'period_spend' | 'burn_rate' | 'days_remaining';
