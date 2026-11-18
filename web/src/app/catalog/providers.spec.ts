import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import { BalanceCache } from './balance-cache';
import type { Credential, DiscoverySummary, Offering, Provider } from './catalog.types';
import { Providers } from './providers';

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

function providerFixture(overrides: Partial<Provider> = {}): Provider {
  return {
    id: 'p1',
    name: 'openrouter',
    kind: 'openai_compatible',
    base_url: 'https://openrouter.ai/api/v1',
    enabled: true,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    last_discovery_at: null,
    last_discovery_created: null,
    last_discovery_unchanged: null,
    connectors: [],
    ...overrides,
  };
}

function credentialFixture(overrides: Partial<Credential> = {}): Credential {
  return {
    id: 'c1',
    provider_id: 'p1',
    label: 'prod',
    has_secret: true,
    secret_masked: '********',
    header_names: [],
    max_concurrency: null,
    balance_usd: null,
    balance_source: 'unknown',
    balance_updated_at: null,
    balance_updated_by: null,
    balance_low_threshold: null,
    balance_available: null,
    balance_currency: null,
    balance_error: null,
    balance_fetched_at: null,
    balance_gate: 'warn',
    status: 'ok',
    last_tested_at: null,
    cooldown_until: null,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

function offeringFixture(overrides: Partial<Offering> = {}): Offering {
  return {
    id: 'o1',
    provider_id: 'p1',
    model_id: 'm1',
    upstream_model_id: 'openai/gpt-5',
    context_override: null,
    max_output_override: null,
    enabled: true,
    header_names: [],
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

function discoveryFixture(overrides: Partial<DiscoverySummary> = {}): DiscoverySummary {
  return {
    dry_run: true,
    credential_id: 'c1',
    discovered: 5,
    created_models: 2,
    created_offerings: 3,
    updated_offerings: 0,
    unchanged: 1,
    ...overrides,
  };
}

function typeInto(root: HTMLElement, selector: string, value: string): void {
  const input = root.querySelector(selector);
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`no input matching ${selector}`);
  }
  input.value = value;
  input.dispatchEvent(new Event('input'));
}

function buttonByText(root: HTMLElement, text: string): HTMLButtonElement {
  const button = Array.from(root.querySelectorAll('button')).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  if (button === undefined) {
    throw new Error(`no button labelled ${text}`);
  }
  return button;
}

describe('Providers', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  /** Queues the providers list, one credentials read per provider, then offerings. */
  function queueLoad(
    providers: Provider[],
    credentials: Record<string, Credential[]> = {},
    offerings: Offering[] = [],
  ): void {
    fetchMock.respond(jsonResponse(200, providers));
    for (const provider of providers) {
      fetchMock.respond(jsonResponse(200, credentials[provider.id] ?? []));
    }
    fetchMock.respond(jsonResponse(200, offerings));
  }

  async function create(
    role: 'admin' | 'user' = 'admin',
    prepare?: (cache: BalanceCache) => void,
  ): Promise<ComponentFixture<Providers>> {
    TestBed.configureTestingModule({
      imports: [Providers],
      providers: [
        { provide: AuthService, useValue: authStub(role) },
        provideRouter([
          { path: 'providers', children: [] },
          { path: 'models', children: [] },
        ]),
      ],
    });
    prepare?.(TestBed.inject(BalanceCache));
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Providers);
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  /** Waits for reactive effects and their async work to settle and render. */
  async function settle(fixture: ComponentFixture<Providers>): Promise<void> {
    await fixture.whenStable();
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
  }

  it('shows a forbidden state for non-admins without calling the API', async () => {
    const fixture = await create('user');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Forbidden');
    expect(fetchMock.requests).toHaveLength(0);
  });

  it('lists providers with base URL, kind, enabled state, and counts', async () => {
    queueLoad(
      [providerFixture(), providerFixture({ id: 'p2', name: 'azure', enabled: false })],
      { p1: [credentialFixture(), credentialFixture({ id: 'c2', label: 'backup' })] },
      [offeringFixture(), offeringFixture({ id: 'o2', provider_id: 'p2' })],
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const text = root.textContent ?? '';
    expect(text).toContain('openrouter');
    expect(text).toContain('azure');
    expect(text).toContain('https://openrouter.ai/api/v1');
    expect(text).toContain('openai_compatible');
    expect(text).toContain('enabled');
    expect(text).toContain('disabled');
    expect(text).toContain('never');
    expect(root.querySelectorAll('tbody tr')).toHaveLength(2);

    const firstRow = root.querySelector('tbody tr');
    const counts = Array.from(firstRow?.querySelectorAll('td.num') ?? []).map(
      (cell) => cell.textContent?.trim() ?? '',
    );
    expect(counts).toEqual(['2', '1']);
    expect(fetchMock.requests.map((call) => call.url)).toEqual([
      '/api/providers',
      '/api/providers/p1/credentials',
      '/api/providers/p2/credentials',
      '/api/offerings',
    ]);
  });

  it('creates a provider from the dialog and reloads the list', async () => {
    queueLoad([providerFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New provider').click();
    fixture.detectChanges();
    typeInto(root, '#provider-name', 'azure');
    typeInto(root, '#provider-base-url', 'https://a.example/v1');
    fetchMock.respond(
      jsonResponse(
        201,
        providerFixture({ id: 'p2', name: 'azure', base_url: 'https://a.example/v1' }),
      ),
    );
    queueLoad([providerFixture(), providerFixture({ id: 'p2', name: 'azure' })]);
    buttonByText(root, 'Save').click();
    await settle(fixture);

    const call = fetchMock.requests[3];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/providers');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      name: 'azure',
      base_url: 'https://a.example/v1',
      kind: 'openai_compatible',
    });
    expect(root.textContent).toContain('azure');
  });

  it('validates the form before calling the API', async () => {
    queueLoad([]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New provider').click();
    fixture.detectChanges();
    buttonByText(root, 'Save').click();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(2);
    expect(root.textContent).toContain('Name is required.');
    expect(root.textContent).toContain('Base URL is required.');

    typeInto(root, '#provider-name', 'azure');
    typeInto(root, '#provider-base-url', 'ftp://a.example');
    buttonByText(root, 'Save').click();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(2);
    expect(root.textContent).toContain('Enter an http(s) URL');
  });

  it('toggles enabled with a PATCH carrying the CSRF header', async () => {
    queueLoad([providerFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, providerFixture({ enabled: false })));
    queueLoad([providerFixture({ enabled: false })]);
    buttonByText(root, 'Disable').click();
    await settle(fixture);

    const call = fetchMock.requests[3];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/providers/p1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ enabled: false });
  });

  it('edits a provider, including the enabled switch, from the dialog', async () => {
    queueLoad([providerFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Edit').click();
    fixture.detectChanges();
    typeInto(root, '#provider-name', 'openrouter-eu');
    const toggle = root.querySelector('ui-dialog button[role="switch"]');
    if (!(toggle instanceof HTMLButtonElement)) {
      throw new Error('no enabled switch');
    }
    fetchMock.respond(
      jsonResponse(200, providerFixture({ name: 'openrouter-eu', enabled: false })),
    );
    queueLoad([providerFixture({ name: 'openrouter-eu', enabled: false })]);
    toggle.click();
    fixture.detectChanges();
    buttonByText(root, 'Save').click();
    await settle(fixture);

    const call = fetchMock.requests[3];
    expect(call.method).toBe('PATCH');
    expect(call.body).toEqual({ name: 'openrouter-eu', enabled: false });
  });

  it('deletes a provider only after the dialog confirmation', async () => {
    queueLoad([providerFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Delete').click();
    fixture.detectChanges();

    // Opening the confirmation must not delete anything yet.
    expect(fetchMock.requests).toHaveLength(3);
    fetchMock.respond(noContentResponse());
    queueLoad([]);
    buttonByText(root, 'Delete provider').click();
    await settle(fixture);

    const call = fetchMock.requests[3];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/providers/p1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(root.textContent).toContain('No providers yet');
  });

  it('nests the credentials panel under the selected provider', async () => {
    queueLoad([providerFixture()], { p1: [credentialFixture()] });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, [credentialFixture()]));
    buttonByText(root, 'Credentials').click();
    await settle(fixture);

    expect(root.textContent).toContain('Credentials — openrouter');
    expect(root.textContent).toContain('prod');
    expect(fetchMock.requests[3].url).toBe('/api/providers/p1/credentials');
  });

  it('previews discovery with a dry run, then applies and links to Models', async () => {
    queueLoad([providerFixture()], { p1: [credentialFixture()] });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Load models').click();
    fixture.detectChanges();
    expect(root.textContent).toContain('Load models — openrouter');

    fetchMock.respond(jsonResponse(200, discoveryFixture()));
    buttonByText(root, 'Preview').click();
    await settle(fixture);

    const previewCall = fetchMock.requests[3];
    expect(previewCall.method).toBe('POST');
    expect(previewCall.url).toBe('/api/providers/p1/discover-models');
    expect(previewCall.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(previewCall.body).toEqual({ credential_id: 'c1', dry_run: true });
    expect(root.textContent).toContain('Preview (no changes written)');
    expect(root.textContent).toContain('New models');
    expect(root.textContent).toContain('5');

    fetchMock.respond(jsonResponse(200, discoveryFixture({ dry_run: false })));
    // The apply reloads the provider list; the server has persisted the
    // discovery summary on the provider row by then.
    queueLoad(
      [
        providerFixture({
          last_discovery_at: new Date().toISOString(),
          last_discovery_created: 2,
          last_discovery_unchanged: 3,
        }),
      ],
      { p1: [credentialFixture()] },
    );
    buttonByText(root, 'Apply').click();
    await settle(fixture);

    const applyCall = fetchMock.requests[4];
    expect(applyCall.body).toEqual({ credential_id: 'c1', dry_run: false });
    expect(root.textContent).toContain('Applied: 5 discovered');
    const link = root.querySelector('a[href="/models?provider=p1"]');
    expect(link).not.toBeNull();
    // Persisted, not client memory: this is the value a reload renders.
    const lastLoad = root.querySelector('td.last-load');
    expect(lastLoad?.textContent).toContain('+2 models');
    expect(lastLoad?.textContent).toContain('3 unchanged');
    expect(lastLoad?.textContent).not.toContain('never');
  });

  it('shows connector badges and a balance from the connector display', async () => {
    const providerWithConnectors = providerFixture({
      connectors: [
        { kind: 'catalog', enabled: true, updated_at: '2026-01-01T00:00:00.000Z' },
        { kind: 'balance', enabled: true, updated_at: '2026-01-01T00:00:00.000Z' },
        { kind: 'health', enabled: false, updated_at: '2026-01-01T00:00:00.000Z' },
      ],
    });
    fetchMock.respond(jsonResponse(200, [providerWithConnectors]));
    fetchMock.respond(
      jsonResponse(200, [
        credentialFixture({
          balance_usd: 3.5,
          balance_available: true,
          balance_currency: 'USD',
          balance_low_threshold: 5,
          balance_source: 'provider_api',
        }),
      ]),
    );
    fetchMock.respond(jsonResponse(200, []));
    fetchMock.respond(
      jsonResponse(200, {
        provider_id: 'p1',
        kind: 'catalog',
        configured: true,
        enabled: true,
        config: {
          url: '/models',
          headers: {},
          allow_private: false,
          catalog: {
            entries: '/data',
            model_id: '/id',
            input: '/price/input',
            output: null,
            cache_read: null,
            cache_write: null,
            reasoning: null,
            unit: {
              source: 'per_million_tokens',
              multiplier: 1,
              currency: 'USD',
              fx_to_usd: null,
              display: { symbol: '€', decimals: 2 },
              note: null,
            },
            modalities_input: null,
            modalities_output: null,
            context_length: null,
            max_output: null,
            apply_on_discovery: false,
            refresh_seconds: null,
          },
        },
        updated_at: '2026-01-01T00:00:00.000Z',
        updated_by: 'u1',
      }),
    );
    const fixture = await create();
    await settle(fixture);
    const root = fixture.nativeElement as HTMLElement;

    const badges = Array.from(root.querySelectorAll('.connector-badges .ui-badge'));
    expect(badges.map((badge) => badge.textContent?.trim())).toEqual([
      'prices',
      'balance',
      'health',
    ]);
    expect(badges[0].classList.contains('tone-success')).toBe(true);
    expect(badges[2].classList.contains('tone-warning')).toBe(true);

    const balance = root.querySelector('.provider-balance');
    expect(balance?.textContent?.trim()).toBe('€3.50');
    expect(balance?.classList.contains('is-low')).toBe(true);
    expect(root.querySelector('.balance-cell')?.textContent).toContain('low');
    // The display came from the catalog connector read.
    expect(fetchMock.requests[3].url).toBe('/api/providers/p1/connectors/catalog');
  });

  it('shows how many days a balance lasts when a refresh knows it', async () => {
    queueLoad([providerFixture()], {
      p1: [
        credentialFixture({
          balance_usd: 1.5,
          balance_available: true,
          balance_source: 'provider_api',
        }),
      ],
    });
    const fixture = await create('admin', (cache) =>
      cache.set({
        ok: true,
        credential_id: 'c1',
        available: true,
        balance_usd: 1.5,
        currency: 'USD',
        period_spend: 3,
        burn_rate: 0.5,
        days_remaining: 2.4,
        error: null,
        fetched_at: '2026-01-01T00:00:00.000Z',
      }),
    );
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('.balance-days')?.textContent?.trim()).toBe('runs out in 2 days');
  });

  it('renders an unavailable provider balance as unavailable, never zero', async () => {
    fetchMock.respond(
      jsonResponse(200, [
        providerFixture({
          connectors: [{ kind: 'catalog', enabled: true, updated_at: '2026-01-01T00:00:00.000Z' }],
        }),
      ]),
    );
    fetchMock.respond(
      jsonResponse(200, [
        credentialFixture({
          balance_usd: null,
          balance_available: false,
          balance_source: 'provider_api',
        }),
      ]),
    );
    fetchMock.respond(jsonResponse(200, []));
    fetchMock.respond(
      jsonResponse(200, {
        provider_id: 'p1',
        kind: 'catalog',
        configured: true,
        enabled: true,
        config: null,
        updated_at: null,
        updated_by: null,
      }),
    );
    const fixture = await create();
    await settle(fixture);
    const root = fixture.nativeElement as HTMLElement;

    const balance = root.querySelector('.provider-balance');
    expect(balance?.textContent?.trim()).toBe('unavailable');
    expect(balance?.classList.contains('is-unavailable')).toBe(true);
    expect(root.textContent).not.toContain('$0.00');
  });

  it('opens the connectors dialog from the provider row', async () => {
    queueLoad([providerFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Connectors').click();
    fixture.detectChanges();
    await settle(fixture);

    expect(root.textContent).toContain('Connectors — openrouter');
    expect(fetchMock.requests[3].url).toBe('/api/providers/p1/connectors/templates');
  });

  it('shows the upstream error message verbatim from a failed preview', async () => {
    queueLoad([providerFixture()], { p1: [credentialFixture()] });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Load models').click();
    fixture.detectChanges();
    fetchMock.respond(
      errorResponse(
        502,
        'upstream_error',
        'The provider answered 503 after 10s; no models were loaded.',
      ),
    );
    buttonByText(root, 'Preview').click();
    await settle(fixture);

    expect(root.textContent).toContain(
      'The provider answered 503 after 10s; no models were loaded.',
    );
    expect(buttonByText(root, 'Apply').disabled).toBe(true);
  });

  it('explains a missing credential instead of calling discovery', async () => {
    queueLoad([providerFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Load models').click();
    fixture.detectChanges();

    expect(root.textContent).toContain('This provider has no credentials yet');
    expect(buttonByText(root, 'Preview').disabled).toBe(true);
    expect(fetchMock.requests).toHaveLength(3);
  });

  it('shows the server message when loading fails', async () => {
    fetchMock.respond(errorResponse(500, 'internal', 'Reading providers failed.'));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Reading providers failed.');
  });
});
