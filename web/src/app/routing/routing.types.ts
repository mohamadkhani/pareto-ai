/**
 * Wire types for the M5 routing explainability, quality-override, and
 * benchmark-sync APIs.
 *
 * Two shapes live here:
 *
 * 1. The **decision record** — `request_logs.decision_json` as returned by
 *    `GET /api/logs`: every candidate the router considered, its score
 *    breakdown, the chosen offering, and the routing configuration. The JSON
 *    is parsed by [`parseDecision`], which tolerates missing or junk fields
 *    so a malformed record can never break the request log. Per the documented
 *    contract, `score.total` is **not** clamped to `[0, 1]` and can be
 *    negative (penalties, member weights above 1).
 * 2. The **quality-override row** and the **benchmark sync status** for the
 *    admin UI. D31 retired manual JSON import: quality arrives through the
 *    automatic sync (`POST /api/benchmarks/sync`) and per-model overrides
 *    written from the Models page (`PUT`/`DELETE
 *    /api/models/{id}/quality-override`).
 *
 * Quality scores live on the imported benchmark's native Elo-like scale
 * (decision D25), never on `[0, 1]`.
 */

import type { ComboWeights } from './combos.types';
import type { MixProfileRef } from '../catalog/mix.types';

// ---------------------------------------------------------------------------
// Decision record (`decision_json`)
// ---------------------------------------------------------------------------

/** The score breakdown of one scored candidate (the `weighted` strategy). */
export interface CandidateScore {
  /** Component scores, normalized to `[0, 1]` across the eligible pool. */
  price: number;
  speed: number;
  quality: number;
  reliability: number;
  /** The unscaled penalty term subtracted from the weighted sum. */
  penalties: number;
  /**
   * `member_weight * (weighted sum − penalties)`. Not clamped to `[0, 1]`:
   * member weights above 1 push it over and penalties take it below zero.
   */
  total: number;
  /** Components imputed at the neutral 0.5 (no metric available). */
  imputed: string[];
  /** Components shrunk toward 0.5 by thin telemetry windows (decision D27). */
  shrunk: string[];
}

/** The mix price recorded for one candidate (D34). */
export interface DecisionMixPrice {
  /** USD per million tokens at the effective profile's shares. */
  per_m: number;
  /** The profile the blend used, as echoed by the decision. */
  profile_id: string | null;
  profile_name: string | null;
}

/** One candidate considered by the router. */
export interface DecisionCandidate {
  /** The offering id. */
  id: string;
  provider: string;
  model: string;
  upstream_model_id: string;
  /** The price snapshot recorded with the decision (free-form JSON). */
  price: unknown;
  credential_id: string | null;
  /** `chosen`, `failed`, `not_tried`, or `skipped_*`. */
  outcome: string;
  error_class: string | null;
  status: number | null;
  /** The score breakdown; null for unscored strategies or excluded candidates. */
  score: CandidateScore | null;
  /** The blend at the effective profile; null when it could not be computed. */
  mix_price: DecisionMixPrice | null;
  /** `mix` (D34), `component_sum` (fallback), or `unpriced`. */
  price_basis: string;
  /** Why this candidate sits where it sits. */
  explanation: string;
}

/** Which strategy decided the order and with what configuration. */
export interface DecisionRouting {
  /** `fixed_order` (plain models) or the combo's strategy name. */
  strategy: string;
  /** The combo name behind `combo:<name>`, when this request used one. */
  combo: string | null;
  /** The effective scoring weights (`weighted` combos only). */
  weights: ComboWeights | null;
  /** The named weight preset, when one was used. */
  preset: string | null;
  /** The token-mix profile routing used for the price component (D34). */
  mix_profile: MixProfileRef | null;
  /** The round-robin cursor consumed by this request. */
  rr_cursor: number | null;
}

/** The parsed `decision_json` of one request log entry. */
export interface DecisionRecord {
  candidates: DecisionCandidate[];
  chosen_offering_id: string | null;
  /** The one-line reason for the request's outcome. */
  reason: string;
  routing: DecisionRouting;
  /** Session stickiness (D35); disabled defaults keep old records readable. */
  sticky: DecisionSticky;
}

/**
 * The `decision_json.sticky` block (D35). Records written before stickiness
 * existed carry no block, so the parser fills this disabled shape and the
 * panel hides it.
 */
export interface DecisionSticky {
  enabled: boolean;
  /** `header`, `auto`, or null when stickiness is off. */
  key_source: string | null;
  /** The offering the session was pinned to, when a pin applied. */
  pinned: string | null;
  /** Seconds the pin had been in force; 0 on a fresh pin. */
  age_s: number | null;
  /** Why a previous pin was dropped (`expired`, an exclusion reason, …). */
  replaced_why: string | null;
}

/** Parses one entry's `decision_json` string; null when absent or unreadable. */
export function parseDecision(raw: string | null): DecisionRecord | null {
  if (raw === null || raw.trim() === '') {
    return null;
  }
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  const record = asRecord(value);
  if (record === null) {
    return null;
  }
  return {
    candidates: asArray(record['candidates'])
      .map(parseCandidate)
      .filter((candidate): candidate is DecisionCandidate => candidate !== null),
    chosen_offering_id: asString(record['chosen_offering_id']),
    reason: asString(record['reason']) ?? '',
    routing: parseRouting(record['routing']),
    sticky: parseSticky(record['sticky']),
  };
}

function parseCandidate(value: unknown): DecisionCandidate | null {
  const record = asRecord(value);
  const id = record === null ? null : asString(record['id']);
  if (record === null || id === null) {
    return null;
  }
  return {
    id,
    provider: asString(record['provider']) ?? '',
    model: asString(record['model']) ?? '',
    upstream_model_id: asString(record['upstream_model_id']) ?? '',
    price: record['price'] ?? null,
    credential_id: asString(record['credential_id']),
    outcome: asString(record['outcome']) ?? 'unknown',
    error_class: asString(record['error_class']),
    status: asNumber(record['status']),
    score: parseScore(record['score']),
    mix_price: parseMixPrice(record['mix_price']),
    price_basis: asString(record['price_basis']) ?? 'unknown',
    explanation: asString(record['explanation']) ?? '',
  };
}

function parseMixPrice(value: unknown): DecisionMixPrice | null {
  const record = asRecord(value);
  if (record === null) {
    return null;
  }
  const perM = asNumber(record['per_m']);
  if (perM === null) {
    return null;
  }
  return {
    per_m: perM,
    profile_id: asString(record['profile_id']),
    profile_name: asString(record['profile_name']),
  };
}

function parseScore(value: unknown): CandidateScore | null {
  const record = asRecord(value);
  if (record === null) {
    return null;
  }
  return {
    price: asNumber(record['price']) ?? 0,
    speed: asNumber(record['speed']) ?? 0,
    quality: asNumber(record['quality']) ?? 0,
    reliability: asNumber(record['reliability']) ?? 0,
    penalties: asNumber(record['penalties']) ?? 0,
    total: asNumber(record['total']) ?? 0,
    imputed: asStringArray(record['imputed']),
    shrunk: asStringArray(record['shrunk']),
  };
}

function parseRouting(value: unknown): DecisionRouting {
  const record = asRecord(value);
  const weights = record === null ? null : asRecord(record['weights']);
  return {
    strategy: (record === null ? null : asString(record['strategy'])) ?? 'unknown',
    combo: record === null ? null : asString(record['combo']),
    weights:
      weights === null
        ? null
        : {
            price: asNumber(weights['price']) ?? 0,
            speed: asNumber(weights['speed']) ?? 0,
            quality: asNumber(weights['quality']) ?? 0,
            reliability: asNumber(weights['reliability']) ?? 0,
          },
    preset: record === null ? null : asString(record['preset']),
    mix_profile: parseProfileRef(record === null ? null : record['mix_profile']),
    rr_cursor: record === null ? null : asNumber(record['rr_cursor']),
  };
}

function parseSticky(value: unknown): DecisionSticky {
  const record = asRecord(value);
  return {
    enabled: record === null ? false : record['enabled'] === true,
    key_source: record === null ? null : asString(record['key_source']),
    pinned: record === null ? null : asString(record['pinned']),
    age_s: record === null ? null : asNumber(record['age_s']),
    replaced_why: record === null ? null : asString(record['replaced_why']),
  };
}

function parseProfileRef(value: unknown): MixProfileRef | null {
  const record = asRecord(value);
  if (record === null) {
    return null;
  }
  const profileId = asString(record['profile_id']);
  const profileName = asString(record['profile_name']);
  if (profileId === null || profileName === null) {
    return null;
  }
  return { profile_id: profileId, profile_name: profileName };
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function asArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function asString(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

function asNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function asStringArray(value: unknown): string[] {
  return asArray(value).filter((item): item is string => typeof item === 'string');
}

// ---------------------------------------------------------------------------
// Quality overrides and benchmark sync
// ---------------------------------------------------------------------------

/**
 * The operator's quality override for a model. It wins over every synced
 * benchmark until it is removed (decision D25).
 */
export interface QualityOverride {
  model_id: string;
  /** On the imported benchmark's Elo-like scale. */
  score: number;
}

/**
 * `POST /api/benchmarks/sync` body. Both fields are optional: omitting
 * `sources` syncs every configured source, and `dry_run` defaults to false.
 * The service treats a 404 as "not available in this build".
 */
export interface BenchmarkSyncInput {
  sources?: string[];
  dry_run?: boolean;
}

/** Counts a benchmark sync run reports (the set varies by source). */
export type BenchmarkSyncCounts = Record<string, number>;

/** The last run of one benchmark source. */
export interface BenchmarkSyncRun {
  source: string;
  /** `success`, `error`, or `never` before the first run. */
  status: string;
  counts: BenchmarkSyncCounts | null;
  error: string | null;
  finished_at: string | null;
  /** Attribution URL stored on this source's rows (CC BY 4.0 for LMArena). */
  source_url: string | null;
  /** The source's licence as published by the backend, when it reports one. */
  license: string | null;
  /** The attribution link the UI shows next to the scores. */
  attribution_url: string | null;
  /** True when the last *successful* run is inside the backend TTL. */
  fresh: boolean | null;
}

/** One source's result in a `POST /api/benchmarks/sync` response. */
export interface BenchmarkSyncResult {
  source: string;
  /** `success` or `error`; one source's failure never aborts the others. */
  status: string;
  dry_run: boolean;
  counts: BenchmarkSyncCounts;
  started_at: string;
  finished_at: string;
  duration_ms: number;
  /** The attribution URL stored on this source's rows. */
  source_url: string;
  error: string | null;
}

/**
 * `POST /api/benchmarks/sync` response: one result per requested source. The
 * endpoint answers 502 only when every requested source failed, in which case
 * there is no response body to read.
 */
export interface BenchmarkSyncResponse {
  dry_run: boolean;
  results: BenchmarkSyncResult[];
}

/** `GET /api/benchmarks/sync/status`: the last run per source. */
export interface BenchmarkSyncStatus {
  sources: BenchmarkSyncRun[];
}
