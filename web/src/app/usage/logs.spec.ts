import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { FetchMock, jsonResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { UserView } from '../users/users.types';
import type { LogAttempt, RequestLogEntry } from './usage.types';
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

/**
 * One attempt of the fixture entry (D49): names come from the server join
 * (D64), never from a client-side catalog lookup.
 */
function attemptFixture(overrides: Partial<LogAttempt> = {}): LogAttempt {
  return {
    id: 1,
    request_id: 'r0',
    occurred_at: '2026-01-01T00:00:01.000Z',
    offering_id: 'off1',
    provider_id: 'p1',
    provider_name: 'OpenRouter',
    route: 'azure',
    model_id: 'm1',
    model_canonical_name: 'openai/gpt-5',
    model_upstream_model_id: 'openai/gpt-5-2025-01-01',
    credential_id: 'c1',
    credential_name: 'primary',
    outcome: 'upstream_error',
    upstream_status: 502,
    provider_error_code: 'bad_gateway',
    error_detail: 'boom',
    latency_ms: 120,
    ttft_ms: null,
    output_tokens: null,
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

/** Picks a combobox option by its `data-value` (focus, then click the row). */
function comboboxInto(root: HTMLElement, selector: string, value: string): void {
  const input = root.querySelector<HTMLInputElement>(selector);
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`no combobox input matching ${selector}`);
  }
  input.dispatchEvent(new FocusEvent('focus'));
  const option = Array.from(root.querySelectorAll<HTMLElement>('[role="option"]')).find(
    (candidate) => candidate.dataset['value'] === value,
  );
  if (!option) {
    throw new Error(`no combobox option with value ${value}`);
  }
  option.dispatchEvent(new MouseEvent('click', { bubbles: true }));
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
  /** What the `/api/logs/{id}/attempts` route answers with. */
  let attempts: LogAttempt[] = [];

  beforeEach(() => {
    attempts = [];
    fetchMock = new FetchMock();
    fetchMock.respondTo('/api/providers', () => jsonResponse(200, []));
    fetchMock.respondTo('/api/logs/', () => jsonResponse(200, attempts));
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(
    role: 'admin' | 'user' = 'admin',
    query: Record<string, string> = {},
  ): Promise<ComponentFixture<Logs>> {
    TestBed.configureTestingModule({
      imports: [Logs],
      providers: [
        provideRouter([]),
        { provide: AuthService, useValue: authStub(role) },
        {
          provide: ActivatedRoute,
          useValue: { snapshot: { queryParamMap: convertToParamMap(query) } },
        },
      ],
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

  it('renders a 403 access_denied row with the same money/refusal emphasis as a 402 row', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          entry(0, { status: 402, error_class: 'budget_exceeded', cost_usd: null }),
          entry(1, { status: 403, error_class: 'access_denied', cost_usd: null }),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('402 budget');
    expect(text).toContain('403 access');
    // Both refusals carry the row highlight and the labelled refusal badge.
    expect(root.querySelectorAll('tbody tr.money-event').length).toBe(2);
    expect(
      root.querySelectorAll('tbody tr.money-event td:nth-child(5) ui-badge.tone-danger').length,
    ).toBe(2);
    expect(
      root.querySelector('ui-badge[title="Refused: access denied"]')?.textContent?.trim(),
    ).toBe('403 access');
  });

  it('names a refusal only when the class is the gateway denials, not for a provider status', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          // An upstream 429 shares the status with the token cap but is not it.
          entry(0, {
            status: 429,
            error_class: 'upstream_rate_limited',
            upstream_status: 429,
            provider_error_code: 'rate_limit_exceeded',
          }),
          entry(1, { status: 429, error_class: 'token_cap_exceeded', cost_usd: null }),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const badges = Array.from(
      root.querySelectorAll('tbody tr:not(.details-row) td:nth-child(5) ui-badge'),
    );
    // The provider's 429 reads as its status, not as the gateway's cap.
    expect(badges[0]?.textContent?.trim()).toBe('429');
    expect(badges[0]?.classList.contains('tone-danger')).toBe(true);
    expect(badges[0]?.getAttribute('title')).toBeNull();
    // The real token-cap refusal keeps its name.
    expect(badges[1]?.textContent?.trim()).toBe('429 cap');
    expect(badges[1]?.getAttribute('title')).toBe('Refused: token cap exceeded');
    // Both consumed nothing, so both keep the money-event emphasis.
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

    comboboxInto(root, '#logs-user', 'u1');
    await fixture.whenStable();
    fixture.detectChanges();

    const url = new URL(fetchMock.requests[2].url, 'https://example.test');
    expect(url.searchParams.get('user')).toBe('u1');
  });

  it('names and filters the OpenRouter route that served a request', async () => {
    fetchMock.respond(jsonResponse(200, { entries: [entry(0)] }));
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    fetchMock.respond(jsonResponse(200, { entries: [entry(1, { route: 'vertex' })] }));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    // The provider cell names the pinned upstream route next to the provider.
    expect(root.querySelector('td.col-access .route-link')?.textContent?.trim()).toBe('· azure');

    comboboxInto(root, '#logs-route', 'azure');
    await fixture.whenStable();
    fixture.detectChanges();

    const url = new URL(fetchMock.requests[2].url, 'https://example.test');
    expect(url.searchParams.get('route')).toBe('azure');
    expect(url.searchParams.get('provider')).toBeNull();
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

    buttonByText(root, 'Details').click();
    fixture.detectChanges();

    expect(root.querySelector('app-decision-panel')).not.toBeNull();
    expect(root.textContent).toContain('combo:shared-fast');
    expect(root.textContent).toContain('-0.125');
    expect(root.querySelector('tr.chosen')).not.toBeNull();
    // The state lives on the real <button>, not on the <ui-button> host.
    const expanded = root.querySelector<HTMLButtonElement>('button[aria-expanded="true"]');
    expect(expanded?.getAttribute('aria-controls')).toBe('details-r0');
    expect(root.querySelector('ui-button[aria-expanded]')).toBeNull();
    expect(root.querySelector('#details-r0')).not.toBeNull();

    buttonByText(root, 'Hide').click();
    fixture.detectChanges();
    expect(root.querySelector('app-decision-panel')).toBeNull();
    expect(root.querySelector('button[aria-expanded="false"]')).not.toBeNull();
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

    buttonByText(root, 'Details').click();
    fixture.detectChanges();

    expect(root.querySelector('app-decision-panel')).toBeNull();
    expect(root.textContent).toContain('no readable routing decision');
  });

  it('shows the requested and the real target model, with names not GUIDs', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          entry(0, {
            requested_model: 'gpt-5',
            model_canonical_name: 'openai/gpt-5',
            model_upstream_model_id: 'openai/gpt-5-2025-01-01',
          }),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    // Both models are visible: the request and the real target.
    expect(text).toContain('gpt-5');
    expect(text).toContain('openai/gpt-5 · openai/gpt-5-2025-01-01');
    // Provider and credential show their names; no offering/provider GUID.
    expect(text).toContain('OpenRouter');
    expect(text).toContain('primary');
    expect(text).not.toContain('off1');
    // The resolved model opens the exact Models filter.
    const link = root.querySelector<HTMLAnchorElement>('a.resolved-model');
    expect(link?.getAttribute('href')).toBe('/models?model=m1');

    const headers = Array.from(root.querySelectorAll('thead th')).map((th) =>
      th.textContent?.trim(),
    );
    expect(headers).not.toContain('Offering');
    expect(headers).toEqual([
      'Time',
      'User',
      'Model',
      'Provider / upstream / credential',
      'Status',
      'Error',
      'Cost',
      'Tokens',
      'Latency',
      'Details',
    ]);
  });

  it('puts the credential under its provider in one cell, or the provider alone', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          entry(0),
          // A request with a provider but no credential (settled before 0021).
          entry(1, { credential_id: null, credential_name: null }),
          // A pre-send refusal: neither.
          entry(2, {
            status: 402,
            error_class: 'budget_exceeded',
            provider_id: null,
            provider_name: null,
            model_id: null,
            model_canonical_name: null,
            model_upstream_model_id: null,
            credential_id: null,
            credential_name: null,
          }),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const cells = Array.from(root.querySelectorAll('td.col-access'));
    expect(cells).toHaveLength(3);
    expect(cells[0]?.querySelector('ui-link-button')?.textContent?.trim()).toBe('OpenRouter');
    expect(cells[0]?.querySelector('.route-link')?.textContent?.trim()).toBe('· azure');
    expect(cells[0]?.querySelector('.credential-name')?.textContent?.trim()).toBe('primary');
    expect(cells[1]?.querySelector('ui-link-button')?.textContent?.trim()).toBe('OpenRouter');
    expect(cells[1]?.querySelector('.credential-name')).toBeNull();
    expect(cells[2]?.textContent?.trim()).toBe('—');
    expect(cells[2]?.querySelector('.route-link')).toBeNull();
  });

  it('stacks the token counts on two lines, and shows a dash when there are none', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [entry(0), entry(1, { input_tokens: null, output_tokens: null })],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const cells = Array.from(root.querySelectorAll('td.col-tokens'));
    expect(cells).toHaveLength(2);
    expect(cells[0]?.querySelector('.tokens-in .metric-value')?.textContent?.trim()).toBe('100 in');
    expect(cells[0]?.querySelector('.tokens-out .metric-value')?.textContent?.trim()).toBe(
      '50 out',
    );
    expect(cells[1]?.querySelector('.tokens-in .metric-value')?.textContent?.trim()).toBe('—');
    expect(cells[1]?.querySelector('.tokens-out')).toBeNull();
  });

  it('shows a dash for an unknown count instead of a measured zero', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          entry(0, { input_tokens: 1200, output_tokens: null }),
          entry(1, { input_tokens: null, output_tokens: 300 }),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const cells = Array.from(root.querySelectorAll('td.col-tokens'));
    // Input known, output unknown: the missing count never reads as 0.
    expect(cells[0]?.querySelector('.tokens-in .metric-value')?.textContent?.trim()).toBe(
      '1200 in',
    );
    expect(cells[0]?.querySelector('.tokens-out .metric-value')?.textContent?.trim()).toBe('—');
    // The output line has no bar while its own count is unknown.
    expect(cells[0]?.querySelector('.tokens-out .metric-track')).toBeNull();
    // Output known, input unknown: the same rule, mirrored.
    expect(cells[1]?.querySelector('.tokens-in .metric-value')?.textContent?.trim()).toBe('—');
    expect(cells[1]?.querySelector('.tokens-in .metric-track')).toBeNull();
    expect(cells[1]?.querySelector('.tokens-out .metric-value')?.textContent?.trim()).toBe(
      '300 out',
    );
  });

  it('graphs every metric against the largest value in its own column', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          entry(0, {
            input_tokens: 1_000,
            output_tokens: 100,
            latency_ms: 1_000,
            cost_usd: 0.5,
          }),
          entry(1, {
            input_tokens: 500,
            output_tokens: 50,
            latency_ms: 2_000,
            cost_usd: 1,
          }),
          // A pre-send refusal consumed nothing: no cost bar, a dash for a value.
          entry(2, {
            status: 402,
            error_class: 'budget_exceeded',
            cost_usd: 0,
            input_tokens: null,
            output_tokens: null,
            latency_ms: null,
          }),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const widths = (selector: string): string[] =>
      Array.from(root.querySelectorAll<HTMLElement>(selector)).map((fill) => fill.style.width);

    expect(widths('td.col-latency .metric-fill')).toEqual(['50%', '100%']);
    expect(widths('td.col-cost .metric-fill')).toEqual(['50%', '100%']);
    expect(widths('td.col-tokens .tokens-in .metric-fill')).toEqual(['100%', '50%']);
    expect(widths('td.col-tokens .tokens-out .metric-fill')).toEqual(['100%', '50%']);
    // The refusal has no cost and no bars at all.
    expect(root.querySelectorAll('td.col-cost .metric-track')).toHaveLength(2);
    expect(root.querySelectorAll('td.col-cost')[2]?.textContent?.trim()).toBe('—');
    // The bars are decorative; the number beside each one carries the fact.
    expect(root.querySelector('td.col-latency .metric-track')?.getAttribute('aria-hidden')).toBe(
      'true',
    );
  });

  it('collapses the model cell when the request and the target agree', async () => {
    fetchMock.respond(jsonResponse(200, { entries: [entry(0)] }));
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    // Requested and canonical are both `openai/gpt-5`: one line, one link.
    expect(root.querySelectorAll('.col-model .resolved-model').length).toBe(1);
    expect(root.querySelector('.col-model .requested-model')).toBeNull();
    expect(root.querySelector('.col-model a.resolved-model')?.textContent?.trim()).toBe(
      'openai/gpt-5',
    );
  });

  it('narrows the log to the provider clicked in a row', async () => {
    fetchMock.respond(jsonResponse(200, { entries: [entry(0)] }));
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    fetchMock.respond(jsonResponse(200, { entries: [entry(1)] }));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'OpenRouter').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const url = new URL(fetchMock.requests[2].url, 'https://example.test');
    expect(url.pathname).toBe('/api/logs');
    expect(url.searchParams.get('provider')).toBe('p1');
  });

  it('shows sub-cent costs with adaptive precision instead of $0.00', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          entry(0, { cost_usd: 0.000123 }),
          entry(1, { cost_usd: 0.0234 }),
          entry(2, { cost_usd: 2.1 }),
          entry(3, { cost_usd: null }),
          entry(4, { status: 402, error_class: 'budget_exceeded', cost_usd: 0 }),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const cells = Array.from(root.querySelectorAll('td.col-cost')).map((cell) =>
      cell.textContent?.trim(),
    );
    expect(cells).toEqual(['$0.000123', '$0.0234', '$2.10', '—', '—']);
  });

  it('shows the origin error with the provider status, code, and message', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          entry(0, {
            status: 502,
            error_class: 'upstream_error',
            error_detail: 'the provider exploded',
            upstream_status: 502,
            provider_error_code: 'bad_gateway',
          }),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const block = root.querySelector('.origin-error');
    expect(block).not.toBeNull();
    const text = block?.textContent ?? '';
    expect(text).toContain('Origin error');
    expect(text).toContain('upstream_error');
    expect(text).toContain('502');
    expect(text).toContain('bad_gateway');
    expect(text).toContain('the provider exploded');
  });

  it('omits the origin error block when the request itself succeeded', async () => {
    fetchMock.respond(jsonResponse(200, { entries: [entry(0)] }));
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.querySelector('.origin-error')).toBeNull();
  });

  it('omits the origin error block for a pre-send budget refusal', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          entry(0, {
            status: 402,
            error_class: 'budget_exceeded',
            error_detail: null,
            upstream_status: null,
            provider_error_code: null,
            cost_usd: null,
          }),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    // The class is set, but nothing was sent: no provider origin to show.
    expect(root.querySelector('.origin-error')).toBeNull();
  });

  it('omits the origin error block for a gateway model_not_found, which has no provider origin', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          entry(0, {
            status: 404,
            error_class: 'model_not_found',
            error_detail: null,
            upstream_status: null,
            provider_error_code: null,
            cost_usd: null,
          }),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    // The gateway rejected the request before sending: the gate must key off
    // the provider-origin facts, not `error_class !== null`.
    expect(root.querySelector('.origin-error')).toBeNull();
  });

  it('shows the origin error for an upstream timeout that records only the detail', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          entry(0, {
            status: 504,
            error_class: 'upstream_timeout',
            error_detail: 'request timed out after 60s',
            upstream_status: null,
            provider_error_code: null,
          }),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    // A transport failure has no provider status or code, only the transport
    // text: the gate must not be status-only.
    const block = root.querySelector('.origin-error');
    expect(block).not.toBeNull();
    const text = block?.textContent ?? '';
    expect(text).toContain('upstream_timeout');
    expect(text).toContain('request timed out after 60s');
  });

  it('shows the origin error for an upstream 429, which does have a provider origin', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          entry(0, {
            status: 429,
            error_class: 'upstream_rate_limited',
            error_detail: 'slow down',
            upstream_status: 429,
            provider_error_code: 'rate_limit_exceeded',
          }),
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const block = root.querySelector('.origin-error');
    expect(block).not.toBeNull();
    const text = block?.textContent ?? '';
    expect(text).toContain('upstream_rate_limited');
    expect(text).toContain('429');
    expect(text).toContain('rate_limit_exceeded');
    expect(text).toContain('slow down');
  });

  it('names the provider, model, and credential on every attempt', async () => {
    attempts = [attemptFixture()];
    fetchMock.respond(jsonResponse(200, { entries: [entry(0)] }));
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const row = root.querySelector('.attempts-table tbody tr');
    const text = row?.textContent ?? '';
    expect(text).toContain('OpenRouter');
    expect(text).toContain('openai/gpt-5 · openai/gpt-5-2025-01-01');
    expect(text).toContain('primary');
    expect(text).toContain('upstream_error');
    expect(text).not.toContain('off1');

    const link = row?.querySelector<HTMLAnchorElement>('a.resolved-model');
    expect(link?.getAttribute('href')).toBe('/models?model=m1');
  });

  it('keeps the raw identifiers in a collapsed technical block', async () => {
    fetchMock.respond(
      jsonResponse(200, { entries: [entry(0, { client_request_id: 'client-9' })] }),
    );
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const details = root.querySelector<HTMLDetailsElement>('details.technical');
    expect(details).not.toBeNull();
    expect(details?.open).toBe(false);
    const text = details?.textContent ?? '';
    expect(text).toContain('r0');
    expect(text).toContain('client-9');
    expect(text).toContain('off1');
    expect(text).toContain('m1');
  });
});
