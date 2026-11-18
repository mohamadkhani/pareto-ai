import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { FetchMock, errorResponse, jsonResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import { Dashboard } from './dashboard';

function authStub(role: 'admin' | 'user'): object {
  return {
    user: signal<User | null>({
      id: 'u1',
      email: 'admin@example.com',
      display_name: 'Admin',
      role,
      must_change_password: false,
    }),
    isAdmin: signal(role === 'admin'),
    csrfToken: signal<string | null>('csrf-1'),
  };
}

function dayKey(date: Date): string {
  const pad = (value: number): string => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

function counters(overrides: Record<string, number> = {}): Record<string, number> {
  return {
    requests: 4,
    input_tokens: 100,
    output_tokens: 50,
    cost_usd: 0.25,
    priced_requests: 3,
    unpriced_requests: 1,
    partial_requests: 0,
    refused_requests: 0,
    ...overrides,
  };
}

const provider = {
  id: 'p1',
  name: 'openrouter',
  kind: 'openai_compatible',
  preset: null,
  base_url: 'https://openrouter.ai/api/v1',
  enabled: true,
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
  last_discovery_at: null,
  last_discovery_created: null,
  last_discovery_unchanged: null,
  connectors: [],
};

const offering = {
  id: 'off1',
  provider_id: 'p1',
  model_id: 'm1',
  upstream_model_id: 'openai/gpt-5',
  context_override: null,
  max_output_override: null,
  enabled: true,
  header_names: [],
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
};

const credential = {
  id: 'c1',
  provider_id: 'p1',
  label: 'primary',
  has_secret: true,
  secret_masked: 'sk-…',
  header_names: [],
  max_concurrency: null,
  balance_usd: 2,
  balance_source: 'provider_api',
  balance_updated_at: '2026-01-01T00:00:00.000Z',
  balance_updated_by: null,
  balance_low_threshold: 5,
  balance_available: true,
  balance_currency: 'USD',
  balance_error: null,
  balance_fetched_at: '2026-01-01T00:00:00.000Z',
  balance_gate: 'block',
  status: 'error',
  last_tested_at: '2026-01-01T00:00:00.000Z',
  cooldown_until: null,
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
};

const user = {
  id: 'u1',
  email: 'admin@example.com',
  display_name: 'Admin',
  role: 'admin',
  is_active: true,
  must_change_password: false,
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
  last_login_at: null,
};

function overview(rows: unknown[]): unknown {
  return {
    scope: 'all',
    provider_id: null,
    mix: {
      id: 'default',
      name: 'Default',
      input: 3,
      output: 1,
      cache_read: 0,
      cache_write: 0,
      reasoning: 0,
    },
    rows,
  };
}

function modelRow(priced: boolean): unknown {
  return {
    model_id: 'm1',
    canonical_name: 'openai/gpt-5',
    family: 'gpt',
    modalities_input: ['text'],
    modalities_output: ['text'],
    context_length: 128000,
    max_output_tokens: 8192,
    providers_count: 1,
    offerings_count: 1,
    unpriced_offerings: priced ? 0 : 1,
    variable_offerings: 0,
    price_avg: priced ? { input_per_m: 1, output_per_m: 2, cache_read_per_m: null } : null,
    price_min: priced ? { input_per_m: 1, output_per_m: 2, cache_read_per_m: null } : null,
    mix_price: null,
    mix_price_min: null,
    quality: null,
  };
}

function logEntry(overrides: Record<string, unknown> = {}): unknown {
  return {
    request_id: 'r1',
    client_request_id: null,
    occurred_at: '2026-01-01T00:00:00.000Z',
    method: 'POST',
    path: '/v1/chat/completions',
    requested_model: 'openai/gpt-5',
    resolved_offering_id: 'off1',
    decision_json: null,
    status: 200,
    error_class: null,
    bytes_in: 10,
    bytes_out: 20,
    user_id: 'u1',
    api_key_id: null,
    input_tokens: 10,
    output_tokens: 5,
    cost_usd: 0.02,
    tokens_estimated: null,
    latency_ms: 20,
    ...overrides,
  };
}

const syncStatus = {
  last_run: {
    id: 'run1',
    source: 'openrouter',
    started_at: '2026-01-01T00:00:00.000Z',
    finished_at: '2026-01-01T00:00:02.000Z',
    status: 'success',
    model_count: 12,
    detail: null,
  },
  snapshot_stale: false,
  ttl_hours: 6,
};

const benchmarkStatus = {
  ttl_hours: 5,
  sources: [
    {
      source: 'lmarena',
      status: 'success',
      fresh: true,
      finished_at: '2026-01-01T00:00:00.000Z',
      counts: { matched: 3 },
      error: null,
      source_url: null,
      license: null,
      attribution_url: null,
      last_run: null,
    },
  ],
};

describe('Dashboard', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    fetchMock.respondTo('/api/errors/summary', () => jsonResponse(200, []));
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(role: 'admin' | 'user' = 'admin'): Promise<ComponentFixture<Dashboard>> {
    TestBed.configureTestingModule({
      imports: [Dashboard],
      providers: [
        { provide: AuthService, useValue: authStub(role) },
        provideRouter([{ path: 'logs', component: Dashboard }]),
      ],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Dashboard);
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  function queueHappyPath(): void {
    const today = dayKey(new Date());
    fetchMock.respond(
      jsonResponse(200, {
        totals: counters(),
        groups: [{ day: today, user_id: null, email: null, offering_id: null, ...counters() }],
      }),
    );
    fetchMock.respond(
      jsonResponse(200, {
        entries: [logEntry(), logEntry({ request_id: 'r2', status: 500, cost_usd: null })],
      }),
    );
    fetchMock.respond(jsonResponse(200, [provider]));
    fetchMock.respond(jsonResponse(200, overview([modelRow(true)])));
    fetchMock.respond(jsonResponse(200, syncStatus));
    fetchMock.respond(jsonResponse(200, benchmarkStatus));
    fetchMock.respond(jsonResponse(200, [user]));
    fetchMock.respond(jsonResponse(200, [offering]));
    fetchMock.respond(jsonResponse(200, [credential]));
  }

  it('renders spend, requests, catalog, health, and recent activity from real payloads', async () => {
    queueHappyPath();
    const fixture = await create();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';

    expect(text).toContain('$0.25');
    expect(text).toContain('4');
    expect(text).toContain('3 priced');
    expect(text).toContain('1 unpriced');
    expect(text).toContain('50%');
    expect(text).toContain('openrouter');
    expect(text).toContain('success');
    expect(text).toContain('last probe failed');
    expect(text).toContain('balance');
    expect(text).toContain('admin@example.com');
    expect(text).toContain('2');
  });

  it('isolates a failing card: the catalog error leaves spend and requests intact', async () => {
    const today = dayKey(new Date());
    fetchMock.respond(
      jsonResponse(200, {
        totals: counters(),
        groups: [{ day: today, user_id: null, email: null, offering_id: null, ...counters() }],
      }),
    );
    fetchMock.respond(jsonResponse(200, { entries: [logEntry()] }));
    fetchMock.respond(jsonResponse(200, [provider]));
    fetchMock.respond(errorResponse(500, 'internal_error', 'catalog exploded'));
    fetchMock.respond(jsonResponse(200, syncStatus));
    fetchMock.respond(jsonResponse(200, benchmarkStatus));
    fetchMock.respond(jsonResponse(200, [user]));
    fetchMock.respond(jsonResponse(200, [offering]));
    fetchMock.respond(jsonResponse(200, [credential]));

    const fixture = await create();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';

    expect(text).toContain('catalog exploded');
    expect(text).toContain('$0.25');
    expect(text).toContain('4');
    expect(text).toContain('admin@example.com');
  });

  it('shows fresh-install empty states with provider links', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        totals: counters({ requests: 0, cost_usd: 0, priced_requests: 0, unpriced_requests: 0 }),
        groups: [],
      }),
    );
    fetchMock.respond(jsonResponse(200, { entries: [] }));
    fetchMock.respond(jsonResponse(200, []));
    fetchMock.respond(jsonResponse(200, overview([])));
    fetchMock.respond(jsonResponse(200, { last_run: null, snapshot_stale: true, ttl_hours: 6 }));
    fetchMock.respond(jsonResponse(200, { ttl_hours: 5, sources: [] }));
    fetchMock.respond(jsonResponse(200, []));
    fetchMock.respond(jsonResponse(200, []));

    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('No requests yet');
    expect(text).toContain('No providers yet');
    expect(root.querySelector('ui-empty a[href="/providers"], ui-empty ui-button')).not.toBeNull();
  });

  it('shows a forbidden state for non-admins without calling the API', async () => {
    const fixture = await create('user');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Forbidden');
    expect(fetchMock.requests).toHaveLength(0);
  });
});
