import { TestBed } from '@angular/core/testing';
import { FetchMock, jsonResponse } from '../../testing/fetch-mock';
import { UsageService } from './usage.service';
import type { MyUsageReport, RequestLogEntry } from './usage.types';

const report: MyUsageReport = {
  totals: {
    requests: 2,
    input_tokens: 100,
    output_tokens: 50,
    cost_usd: 0.25,
    priced_requests: 1,
    unpriced_requests: 1,
    partial_requests: 0,
    refused_requests: 0,
  },
  days: [
    {
      day: '2026-01-01',
      requests: 2,
      input_tokens: 100,
      output_tokens: 50,
      cost_usd: 0.25,
      priced_requests: 1,
      unpriced_requests: 1,
      partial_requests: 0,
      refused_requests: 0,
    },
  ],
};

function logFixture(overrides: Partial<RequestLogEntry> = {}): RequestLogEntry {
  return {
    request_id: 'r50',
    client_request_id: null,
    occurred_at: '2026-01-01T00:00:00.000Z',
    method: 'POST',
    path: '/v1/chat/completions',
    requested_model: 'openai/gpt-5',
    resolved_offering_id: 'off1',
    provider_id: 'p1',
    provider_name: 'OpenRouter',
    route: 'azure',
    model_id: 'm1',
    model_canonical_name: 'openai/gpt-5',
    model_upstream_model_id: 'openai/gpt-5',
    credential_id: 'c1',
    credential_name: 'primary',
    decision_json: null,
    status: 200,
    error_class: null,
    error_detail: null,
    upstream_status: null,
    provider_error_code: null,
    bytes_in: 10,
    bytes_out: 20,
    user_id: 'u1',
    api_key_id: 'k1',
    input_tokens: 100,
    output_tokens: 50,
    cost_usd: 0.25,
    tokens_estimated: 120,
    latency_ms: 800,
    ...overrides,
  };
}

describe('UsageService', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('requests the self report with the date range', async () => {
    fetchMock.respond(jsonResponse(200, report));
    const service = TestBed.inject(UsageService);

    await service.myUsage({ from: '2026-01-01', to: '2026-01-31' });

    const url = new URL(fetchMock.requests[0].url, 'https://example.test');
    expect(url.pathname).toBe('/api/usage/me');
    expect(url.searchParams.get('from')).toBe('2026-01-01');
    expect(url.searchParams.get('to')).toBe('2026-01-31');
  });

  it('omits the query string when the range is unbounded', async () => {
    fetchMock.respond(jsonResponse(200, report));
    const service = TestBed.inject(UsageService);

    await service.myUsage();

    expect(fetchMock.requests[0].url).toBe('/api/usage/me');
  });

  it('sends user, range, and group_by to the admin aggregate', async () => {
    fetchMock.respond(jsonResponse(200, { totals: report.totals, groups: [] }));
    const service = TestBed.inject(UsageService);

    await service.adminUsage({
      user: 'u2',
      from: '2026-01-01',
      to: '2026-01-31',
      groupBy: 'offering',
    });

    const url = new URL(fetchMock.requests[0].url, 'https://example.test');
    expect(url.pathname).toBe('/api/usage');
    expect(url.searchParams.get('user')).toBe('u2');
    expect(url.searchParams.get('from')).toBe('2026-01-01');
    expect(url.searchParams.get('to')).toBe('2026-01-31');
    expect(url.searchParams.get('group_by')).toBe('offering');
  });

  it('pages request logs with the composite before/before_request_id cursor', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [logFixture()],
        next_cursor: { before: '2026-01-01T00:00:00.000Z', before_request_id: 'r50' },
      }),
    );
    fetchMock.respond(jsonResponse(200, { entries: [] }));
    const service = TestBed.inject(UsageService);

    const first = await service.listLogs({ limit: 50 });
    expect(first.next_cursor).toEqual({
      before: '2026-01-01T00:00:00.000Z',
      before_request_id: 'r50',
    });

    await service.listLogs({ limit: 50, cursor: first.next_cursor ?? undefined, user: 'u1' });

    const url = new URL(fetchMock.requests[1].url, 'https://example.test');
    expect(url.pathname).toBe('/api/logs');
    expect(url.searchParams.get('limit')).toBe('50');
    expect(url.searchParams.get('before')).toBe('2026-01-01T00:00:00.000Z');
    expect(url.searchParams.get('before_request_id')).toBe('r50');
    expect(url.searchParams.get('user')).toBe('u1');
  });

  it('normalizes a missing next_cursor to null', async () => {
    fetchMock.respond(jsonResponse(200, { entries: [logFixture()] }));
    const service = TestBed.inject(UsageService);

    const page = await service.listLogs({ limit: 50 });

    expect(page.next_cursor).toBeNull();
  });
});
