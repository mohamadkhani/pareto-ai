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
import { RoutingService, normalizeSyncStatus } from './routing.service';

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

  it('triggers a benchmark sync with the CSRF header and reads its results', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        dry_run: false,
        results: [
          {
            source: 'arena',
            status: 'success',
            dry_run: false,
            counts: {
              published: 157,
              matched: 0,
              skipped_unknown: 157,
              ambiguous: 0,
              inserted: 0,
              updated: 0,
              unchanged: 0,
            },
            started_at: '2026-11-16T18:47:11.539Z',
            finished_at: '2026-11-16T18:47:14.675Z',
            duration_ms: 3135,
            source_url: 'https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset',
            error: null,
          },
        ],
      }),
    );
    const service = TestBed.inject(RoutingService);

    const response = await service.syncBenchmarks({ dry_run: false });

    const call = fetchMock.requests[0];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/benchmarks/sync');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ dry_run: false });
    expect(response.results[0].source).toBe('arena');
    expect(response.results[0].counts['skipped_unknown']).toBe(157);
  });

  it('reads the per-source sync status and tolerates its absence', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        ttl_hours: 5,
        sources: [
          {
            source: 'arena',
            license: 'CC BY 4.0',
            attribution_url: 'https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset',
            fresh: true,
            status: 'success',
            counts: { inserted: 3, updated: 1 },
            error: null,
            finished_at: '2026-01-01T00:00:00.000Z',
            source_url: 'https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset',
          },
        ],
      }),
    );
    fetchMock.respond(errorResponse(404, 'not_found', 'no route'));
    const service = TestBed.inject(RoutingService);

    await expect(service.benchmarkSyncStatus()).resolves.toEqual({
      sources: [
        {
          source: 'arena',
          status: 'success',
          counts: { inserted: 3, updated: 1 },
          error: null,
          finished_at: '2026-01-01T00:00:00.000Z',
          source_url: 'https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset',
          license: 'CC BY 4.0',
          attribution_url: 'https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset',
          fresh: true,
        },
      ],
    });
    await expect(service.benchmarkSyncStatus()).resolves.toBeNull();
    expect(fetchMock.requests[0].url).toBe('/api/benchmarks/sync/status');
  });

  it('normalizes the alternate status envelopes without crashing', () => {
    expect(normalizeSyncStatus(null)).toEqual({ sources: [] });
    expect(normalizeSyncStatus({ last_run: { source: 'lmarena', status: 'error' } })).toEqual({
      sources: [
        {
          source: 'lmarena',
          status: 'error',
          counts: null,
          error: null,
          finished_at: null,
          source_url: null,
          license: null,
          attribution_url: null,
          fresh: null,
        },
      ],
    });
    expect(normalizeSyncStatus({ benchmarklist: { status: 'success', inserted: 2 } })).toEqual({
      sources: [
        {
          source: 'benchmarklist',
          status: 'success',
          counts: { inserted: 2 },
          error: null,
          finished_at: null,
          source_url: null,
          license: null,
          attribution_url: null,
          fresh: null,
        },
      ],
    });
    expect(normalizeSyncStatus({ sources: { lmarena: { status: 'success' } } })).toEqual({
      sources: [
        {
          source: 'lmarena',
          status: 'success',
          counts: null,
          error: null,
          finished_at: null,
          source_url: null,
          license: null,
          attribution_url: null,
          fresh: null,
        },
      ],
    });
  });
});
