/**
 * Wire types for the M5 routing explainability and benchmark APIs.
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
 * 2. The **benchmark and quality-override rows** for the admin UI. The
 *    import (`POST /api/benchmarks/import`) and the override writes
 *    (`PUT`/`DELETE /api/models/{id}/quality-override`) are implemented by
 *    the backend; the two read shapes (`GET /api/benchmarks`,
 *    `GET /api/quality-overrides`) were **assumed** because the backend
 *    contract exposes no way to list them (see `routing.service.ts`) — one
 *    file to adjust in the alignment pass.
 *
 * Quality scores live on the imported benchmark's native Elo-like scale
 * (decision D25), never on `[0, 1]`.
 */

import type { ComboWeights } from './combos.types';

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
    explanation: asString(record['explanation']) ?? '',
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
    rr_cursor: record === null ? null : asNumber(record['rr_cursor']),
  };
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
// Benchmarks and quality overrides
// ---------------------------------------------------------------------------

/**
 * One imported benchmark row. `score` is on the source's native Elo-like
 * scale (decision D25); `ci` is the confidence half-width when the source
 * publishes one.
 */
export interface BenchmarkScore {
  /** The catalog model the entry was matched to. */
  model_id: string;
  /** Where the score came from (the importer records `lmarena`). */
  source: string;
  /** `text` or `code`. */
  category: string;
  score: number;
  ci: number | null;
  rank: number | null;
  /** When the source document was fetched (RFC-3339 UTC). */
  fetched_at: string;
  source_url: string | null;
}

/** What one `POST /api/benchmarks/import` run changed. */
export interface ImportOutcome {
  inserted: number;
  updated: number;
  unchanged: number;
  /** Entries whose model matched no catalog model. */
  skipped_unknown_model: number;
}

/**
 * The operator's quality override for a model. It wins over every synced
 * benchmark and is never overwritten by an import (decision D25).
 */
export interface QualityOverride {
  model_id: string;
  /** On the imported benchmark's Elo-like scale. */
  score: number;
}

/** One entry of a leaderboard document (`category` is `text` or `code`). */
export interface LeaderboardEntry {
  /** Matched to `models.canonical_name`; unknown names are skipped. */
  model: string;
  vendor?: string;
  rank?: number;
  score: number;
  ci?: number;
  votes?: number;
  category: string;
}

/**
 * The leaderboard document `POST /api/benchmarks/import` accepts
 * (LMArena-style). The importer validates the shape server-side and answers
 * 400 `invalid_request` naming the offending field.
 */
export interface LeaderboardDocument {
  source?: string;
  source_url?: string;
  fetched_at?: string;
  entries: LeaderboardEntry[];
}
