import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { ApiError } from '../api';
import { AuthService } from '../auth/auth.service';
import { RoutingService } from './routing.service';
import type { LeaderboardDocument } from './routing.types';

const document: LeaderboardDocument = {
  source: 'lmarena',
  entries: [{ model: 'openai/gpt-5', rank: 1, score: 1433, ci: 12, category: 'text' }],
};

describe('RoutingService', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
    TestBed.configureTestingModule({
      providers: [
        { provide: AuthService, useValue: { csrfToken: signal<string | null>('csrf-1') } },
      ],
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('imports a leaderboard document with the CSRF token', async () => {
    fetchMock.respond(
      jsonResponse(200, { inserted: 1, updated: 0, unchanged: 0, skipped_unknown_model: 0 }),
    );
    const service = TestBed.inject(RoutingService);

    const outcome = await service.importBenchmarks(document);

    const call = fetchMock.requests[0];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/benchmarks/import');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual(document);
    expect(outcome.inserted).toBe(1);
  });

  it('lists benchmark scores and quality overrides from their read endpoints', async () => {
    fetchMock.respond(jsonResponse(200, []));
    fetchMock.respond(jsonResponse(200, []));
    const service = TestBed.inject(RoutingService);

    await service.listBenchmarkScores();
    await service.listQualityOverrides();

    expect(fetchMock.requests[0].url).toBe('/api/benchmarks');
    expect(fetchMock.requests[1].url).toBe('/api/quality-overrides');
  });

  it('sets a quality override as { score }', async () => {
    fetchMock.respond(jsonResponse(200, { model_id: 'm1', score: 1400 }));
    const service = TestBed.inject(RoutingService);

    const override = await service.setQualityOverride('m1', 1400);

    const call = fetchMock.requests[0];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/models/m1/quality-override');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ score: 1400 });
    expect(override.score).toBe(1400);
  });

  it('passes the scale-range rejection through as ApiError', async () => {
    fetchMock.respond(
      errorResponse(
        400,
        'invalid_request',
        'score 0.99 is outside the imported range 851–1933; check the scale',
      ),
    );
    const service = TestBed.inject(RoutingService);

    await expect(service.setQualityOverride('m1', 0.99)).rejects.toMatchObject({
      name: 'ApiError',
      status: 400,
    } satisfies Partial<ApiError>);
  });

  it('clears a quality override', async () => {
    fetchMock.respond(noContentResponse());
    const service = TestBed.inject(RoutingService);

    await service.clearQualityOverride('m1');

    const call = fetchMock.requests[0];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/models/m1/quality-override');
  });
});
