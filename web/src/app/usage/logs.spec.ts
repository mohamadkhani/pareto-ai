import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { FetchMock, jsonResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { UserView } from '../users/users.types';
import type { RequestLogEntry } from './usage.types';
import { Logs } from './logs';

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

function userFixture(overrides: Partial<UserView> = {}): UserView {
  return {
    id: 'u1',
    email: 'admin@example.com',
    display_name: 'Admin',
    role: 'admin',
    is_active: true,
    must_change_password: false,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    last_login_at: null,
    ...overrides,
  };
}

function entry(i: number, overrides: Partial<RequestLogEntry> = {}): RequestLogEntry {
  return {
    request_id: `r${i}`,
    client_request_id: null,
    occurred_at: new Date(Date.UTC(2026, 0, 1, 0, 0, 0, i)).toISOString(),
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
    api_key_id: 'k1',
    input_tokens: 100,
    output_tokens: 50,
    cost_usd: 0.25,
    tokens_estimated: 120,
    latency_ms: 800,
    ...overrides,
  };
}

function selectInto(root: HTMLElement, selector: string, value: string): void {
  const select = root.querySelector(selector);
  if (!(select instanceof HTMLSelectElement)) {
    throw new Error(`no select matching ${selector}`);
  }
  select.value = value;
  select.dispatchEvent(new Event('change'));
}

function buttonByText(scope: ParentNode, text: string): HTMLButtonElement {
  const button = Array.from(scope.querySelectorAll('button')).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  if (button === undefined) {
    throw new Error(`no button labelled ${text}`);
  }
  return button;
}

describe('Logs', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(role: 'admin' | 'user' = 'admin'): Promise<ComponentFixture<Logs>> {
    TestBed.configureTestingModule({
      imports: [Logs],
      providers: [{ provide: AuthService, useValue: authStub(role) }],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Logs);
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  it('shows a forbidden state for non-admins without calling the API', async () => {
    const fixture = await create('user');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Forbidden');
    expect(fetchMock.requests).toHaveLength(0);
  });

  it('renders request metadata and emphasizes 402/429 money events', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          entry(0, { status: 402, error_class: 'budget_exceeded', cost_usd: null }),
          entry(1, { status: 429, error_class: 'token_cap_exceeded', cost_usd: null }),
          entry(2),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('openai/gpt-5');
    expect(text).toContain('800 ms');
    expect(text).toContain('402 budget');
    expect(text).toContain('429 cap');
    expect(root.querySelectorAll('tbody tr.money-event').length).toBe(2);
  });

  it('pages with the composite cursor and passes before_request_id', async () => {
    const tiedAt = '2026-01-01T00:00:00.000Z';
    const firstPage = Array.from({ length: 50 }, (_, i) => entry(i));
    fetchMock.respond(
      jsonResponse(200, {
        entries: firstPage,
        next_cursor: { before: tiedAt, before_request_id: 'r49' },
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    fetchMock.respond(jsonResponse(200, { entries: [entry(50)] }));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelectorAll('tbody tr').length).toBe(50);
    const moreButton = root.querySelector<HTMLButtonElement>('.more button');
    expect(moreButton).not.toBeNull();
    moreButton?.click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(3);
    const url = new URL(fetchMock.requests[2].url, 'https://example.test');
    expect(url.pathname).toBe('/api/logs');
    expect(url.searchParams.get('before')).toBe(tiedAt);
    expect(url.searchParams.get('before_request_id')).toBe('r49');
    expect(root.querySelectorAll('tbody tr').length).toBe(51);
    // A short second page ends paging even though the cursor was offered.
    expect(root.querySelector('.more')).toBeNull();
  });

  it('narrows the log to one user', async () => {
    fetchMock.respond(jsonResponse(200, { entries: [entry(0)] }));
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    fetchMock.respond(jsonResponse(200, { entries: [entry(1)] }));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    selectInto(root, '#logs-user', 'u1');
    await fixture.whenStable();
    fixture.detectChanges();

    const url = new URL(fetchMock.requests[2].url, 'https://example.test');
    expect(url.searchParams.get('user')).toBe('u1');
  });

  it('opens the why-this-model panel with the recorded decision', async () => {
    const decision = {
      candidates: [
        {
          id: 'off1',
          provider: 'openrouter',
          model: 'openai/gpt-5',
          upstream_model_id: 'openai/gpt-5',
          price: null,
          credential_id: 'c1',
          outcome: 'chosen',
          error_class: null,
          status: 200,
          score: {
            price: 0.8,
            speed: 0.5,
            quality: 0.9,
            reliability: 0.7,
            penalties: 0,
            total: -0.125,
            imputed: ['speed'],
            shrunk: [],
          },
          explanation: 'chosen: served the request',
        },
      ],
      chosen_offering_id: 'off1',
      reason: 'off1 served the request',
      routing: {
        strategy: 'weighted',
        combo: 'shared-fast',
        weights: { price: 0.25, speed: 0.3, quality: 0.3, reliability: 0.15 },
        preset: 'balanced',
        rr_cursor: null,
      },
    };
    fetchMock.respond(
      jsonResponse(200, {
        entries: [entry(0, { decision_json: JSON.stringify(decision) })],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Why this model').click();
    fixture.detectChanges();

    expect(root.querySelector('app-decision-panel')).not.toBeNull();
    expect(root.textContent).toContain('combo:shared-fast');
    expect(root.textContent).toContain('-0.125');
    expect(root.querySelector('tr.chosen')).not.toBeNull();

    buttonByText(root, 'Hide').click();
    fixture.detectChanges();
    expect(root.querySelector('app-decision-panel')).toBeNull();
  });

  it('falls back gracefully for an unreadable decision record', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [entry(0, { decision_json: 'not a decision' })],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Why this model').click();
    fixture.detectChanges();

    expect(root.querySelector('app-decision-panel')).toBeNull();
    expect(root.textContent).toContain('no readable routing decision');
  });
});
