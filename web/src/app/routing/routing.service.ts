import { Injectable, inject } from '@angular/core';
import { requestJson, requestNoContent } from '../api';
import { AuthService } from '../auth/auth.service';
import type {
  BenchmarkScore,
  ImportOutcome,
  LeaderboardDocument,
  QualityOverride,
} from './routing.types';

/**
 * All HTTP calls for the M5 benchmark and quality-override admin API. Admin
 * only; mutations send the session cookie and the CSRF token. A malformed
 * leaderboard document fails with 400 `invalid_request` (the message names the
 * offending field) and an override outside the imported benchmark range is
 * rejected with 400 naming the accepted range (decision D25).
 *
 * **Assumption for the alignment pass**: the backend implements the import
 * and the override writes but exposes no way to *list* benchmark rows or
 * stored overrides, so `listBenchmarkScores` (`GET /api/benchmarks`) and
 * `listQualityOverrides` (`GET /api/quality-overrides`) target read endpoints
 * chosen to mirror the existing write paths. If the backend grows different
 * ones, this module and its callers are the only places to change.
 */
@Injectable({ providedIn: 'root' })
export class RoutingService {
  private readonly auth = inject(AuthService);

  /** Imports a leaderboard document; idempotent, never touches overrides. */
  importBenchmarks(document: LeaderboardDocument): Promise<ImportOutcome> {
    return requestJson<ImportOutcome>('/api/benchmarks/import', {
      method: 'POST',
      body: document,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Lists the imported benchmark rows (assumed read endpoint). */
  listBenchmarkScores(): Promise<BenchmarkScore[]> {
    return requestJson<BenchmarkScore[]>('/api/benchmarks');
  }

  /** Lists the stored quality overrides (assumed read endpoint). */
  listQualityOverrides(): Promise<QualityOverride[]> {
    return requestJson<QualityOverride[]>('/api/quality-overrides');
  }

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
}
