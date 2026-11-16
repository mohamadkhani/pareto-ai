import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { FetchMock, jsonResponse, noContentResponse } from '../../testing/fetch-mock';
import { AuthService } from '../auth/auth.service';
import { MoneyService } from './money.service';
import type { BudgetView, TokenCapView } from './money.types';

function budgetFixture(overrides: Partial<BudgetView> = {}): BudgetView {
  return {
    user_id: 'u1',
    period: 'day',
    limit_usd: 10,
    used_usd: 2.5,
    reset_at: '2026-01-02T00:00:00.000Z',
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

function capFixture(overrides: Partial<TokenCapView> = {}): TokenCapView {
  return {
    user_id: 'u1',
    model_id: 'm1',
    period: 'day',
    limit_tokens: 100000,
    used_tokens: 2500,
    reset_at: '2026-01-02T00:00:00.000Z',
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

describe('MoneyService', () => {
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

  it('lists budgets and token caps for one account', async () => {
    fetchMock.respond(jsonResponse(200, [budgetFixture()]));
    fetchMock.respond(jsonResponse(200, [capFixture()]));
    const service = TestBed.inject(MoneyService);

    await expect(service.listBudgets('u1')).resolves.toEqual([budgetFixture()]);
    await expect(service.listTokenCaps('u1')).resolves.toEqual([capFixture()]);

    expect(fetchMock.requests.map((call) => call.url)).toEqual([
      '/api/users/u1/budgets',
      '/api/users/u1/token-caps',
    ]);
  });

  it('upserts one budget period with a CSRF-protected PUT', async () => {
    fetchMock.respond(jsonResponse(200, budgetFixture({ limit_usd: 25 })));
    const service = TestBed.inject(MoneyService);

    await service.setBudget('u1', 'day', { limit_usd: 25 });

    const call = fetchMock.requests[0];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/users/u1/budgets/day');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ limit_usd: 25 });
  });

  it('clears a budget period with a CSRF-protected DELETE', async () => {
    fetchMock.respond(noContentResponse());
    const service = TestBed.inject(MoneyService);

    await service.clearBudget('u1', 'total');

    const call = fetchMock.requests[0];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/users/u1/budgets/total');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
  });

  it('upserts one token cap keyed by model and period', async () => {
    fetchMock.respond(jsonResponse(200, capFixture({ limit_tokens: 5000 })));
    const service = TestBed.inject(MoneyService);

    await service.setTokenCap('u1', 'm1', 'week', { limit_tokens: 5000 });

    const call = fetchMock.requests[0];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/users/u1/token-caps/m1/week');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ limit_tokens: 5000 });
  });

  it('clears one token cap with a CSRF-protected DELETE', async () => {
    fetchMock.respond(noContentResponse());
    const service = TestBed.inject(MoneyService);

    await service.clearTokenCap('u1', 'm1', 'month');

    const call = fetchMock.requests[0];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/users/u1/token-caps/m1/month');
  });
});
