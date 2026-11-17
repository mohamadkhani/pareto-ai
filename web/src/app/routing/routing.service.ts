import { Injectable, inject } from '@angular/core';
import { ApiError, requestJson, requestNoContent } from '../api';
import { AuthService } from '../auth/auth.service';
import type {
  BenchmarkSyncCounts,
  BenchmarkSyncInput,
  BenchmarkSyncResponse,
  BenchmarkSyncRun,
  BenchmarkSyncStatus,
  QualityOverride,
} from './routing.types';

/**
 * The M5 quality-override and benchmark-sync admin API. Admin only; mutations
 * send the session cookie and the CSRF token. An override outside the imported
 * benchmark range is rejected with 400 naming the accepted range (decision
 * D25). Manual JSON import and the benchmark listing page are retired (D31):
 * quality comes from the automatic sync and per-model overrides, both driven
 * from the Models page.
 */
@Injectable({ providedIn: 'root' })
export class RoutingService {
  private readonly auth = inject(AuthService);

  /**
   * Stores the operator's quality override for a model. Throws `ApiError`
   * 400 when the score is off the imported benchmark scale.
   */
  setQualityOverride(modelId: string, score: number): Promise<QualityOverride> {
    return requestJson<QualityOverride>(`/api/models/${modelId}/quality-override`, {
      method: 'PUT',
      body: { score },
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Removes a model's quality override; synced benchmarks take over again. */
  clearQualityOverride(modelId: string): Promise<void> {
    return requestNoContent(`/api/models/${modelId}/quality-override`, {
      method: 'DELETE',
      csrfToken: this.auth.csrfToken(),
    });
  }

  // --- live benchmark sync (driven from the Models toolbar) ----------------

  /**
   * Reads the last run per benchmark source. Returns null when the backend
   * does not expose the endpoint yet (404), so the Models toolbar can disable
   * the action instead of failing the page.
   */
  async benchmarkSyncStatus(): Promise<BenchmarkSyncStatus | null> {
    try {
      return normalizeSyncStatus(await requestJson<unknown>('/api/benchmarks/sync/status'));
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) {
        return null;
      }
      throw err;
    }
  }

  /**
   * Triggers a benchmark sync, optionally limited to `sources` and optionally
   * as a dry run. Returns one result per requested source; the run rows are
   * also readable from the status endpoint. Throws `ApiError` 404 when the
   * endpoint is absent and 502 when every requested source failed.
   */
  syncBenchmarks(input: BenchmarkSyncInput = {}): Promise<BenchmarkSyncResponse> {
    return requestJson<BenchmarkSyncResponse>('/api/benchmarks/sync', {
      method: 'POST',
      body: input,
      csrfToken: this.auth.csrfToken(),
    });
  }
}

/**
 * The sync status envelope accepts the documented `{ sources: [...] }`, a bare
 * array, the other likely list keys, a `{ <source>: {...} }` map, and a single
 * `{ last_run: {...} }`. Unknown junk becomes an empty list rather than
 * crashing the page.
 */
export function normalizeSyncStatus(raw: unknown): BenchmarkSyncStatus {
  const record = asRecord(raw);
  if (Array.isArray(raw)) {
    return {
      sources: raw
        .map((value) => runFrom(value))
        .filter((run): run is BenchmarkSyncRun => run !== null),
    };
  }
  if (record === null) {
    return { sources: [] };
  }
  for (const key of ['sources', 'last_runs', 'runs', 'statuses']) {
    const value = record[key];
    if (Array.isArray(value)) {
      return {
        sources: value
          .map((item) => runFrom(item))
          .filter((run): run is BenchmarkSyncRun => run !== null),
      };
    }
    const map = asRecord(value);
    if (map !== null) {
      return {
        sources: Object.entries(map)
          .map(([source, item]) => runFrom(item, source))
          .filter((run): run is BenchmarkSyncRun => run !== null),
      };
    }
  }
  if (record['last_run'] !== undefined && record['last_run'] !== null) {
    const run = runFrom(record['last_run']);
    return { sources: run === null ? [] : [run] };
  }
  const mapped = Object.entries(record)
    .map(([source, value]) => runFrom(value, source))
    .filter((run): run is BenchmarkSyncRun => run !== null);
  return { sources: mapped };
}

function runFrom(value: unknown, fallbackSource?: string): BenchmarkSyncRun | null {
  const record = asRecord(value);
  if (record === null) {
    return null;
  }
  const source = asString(record['source']) ?? fallbackSource ?? '';
  if (source === '') {
    return null;
  }
  return {
    source,
    status: asString(record['status']) ?? 'unknown',
    counts: countsFrom(record),
    error: asString(record['error']),
    finished_at: asString(record['finished_at']) ?? asString(record['finishedAt']),
    source_url: asString(record['source_url']) ?? asString(record['sourceUrl']),
    license: asString(record['license']),
    attribution_url: asString(record['attribution_url']) ?? asString(record['attributionUrl']),
    fresh: typeof record['fresh'] === 'boolean' ? record['fresh'] : null,
  };
}

function countsFrom(record: Record<string, unknown>): BenchmarkSyncCounts | null {
  const raw = asRecord(record['counts']);
  const source = raw ?? record;
  const counts: BenchmarkSyncCounts = {};
  for (const [key, value] of Object.entries(source)) {
    if (typeof value === 'number' && Number.isFinite(value)) {
      counts[key] = value;
    }
  }
  return Object.keys(counts).length === 0 ? null : counts;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function asString(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}
