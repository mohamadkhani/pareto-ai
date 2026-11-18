import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { waitFor, waitForButton, waitForText } from '../../testing/wait-for';
import { AuthService, type User } from '../auth/auth.service';
import { BalanceCache } from './balance-cache';
import type {
  Credential,
  DiscoverySummary,
  Offering,
  Provider,
  ProviderPreset,
} from './catalog.types';
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
    preset: null,
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

function selectValue(root: HTMLElement, selector: string, value: string): void {
  const select = root.querySelector(selector);
  if (!(select instanceof HTMLSelectElement)) {
    throw new Error(`no select matching ${selector}`);
  }
  select.value = value;
  select.dispatchEvent(new Event('change'));
}

function tabByLabel(root: HTMLElement, text: string): HTMLButtonElement {
  const tab = Array.from(root.querySelectorAll<HTMLButtonElement>('[role="tab"]')).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  if (tab === undefined) {
    throw new Error(`no tab labelled ${text}`);
  }
  return tab;
}

function typeInto(root: HTMLElement, selector: string, value: string): void {
  const input = root.querySelector(selector);
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`no input matching ${selector}`);
  }
  input.value = value;
  input.dispatchEvent(new Event('input'));
}

/** A loading button carries `aria-busy`, so this covers in-flight actions. */
function isBusy(root: ParentNode): boolean {
  return root.querySelector('[aria-busy="true"]') !== null;
}

describe('Providers', () => {
  let fetchMock: FetchMock;
  let presetResponse: ProviderPreset[] = [];

  beforeEach(() => {
    fetchMock = new FetchMock();
    presetResponse = [];
    fetchMock.respondTo('/api/breaker', () => jsonResponse(200, { entries: [] }));
    fetchMock.respondTo('/api/provider-presets', () => jsonResponse(200, presetResponse));
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
    const root = fixture.nativeElement as HTMLElement;
    await waitFor(
      fixture,
      () =>
        (root.textContent ?? '').includes('Forbidden') ||
        (fetchMock.requests.length > 0 && !isBusy(root)),
      'the initial providers load',
    );
    return fixture;
  }

  /** Waits until at least `count` calls were made and no action is in flight. */
  async function waitForRequests(
    fixture: ComponentFixture<Providers>,
    root: ParentNode,
    count: number,
  ): Promise<void> {
    await waitFor(
      fixture,
      () => fetchMock.requests.length >= count && !isBusy(root),
      `${count} requests to finish`,
    );
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
    expect(root.querySelectorAll('.provider-card')).toHaveLength(2);

    const firstRow = root.querySelector('.provider-card');
    const counts = [
      firstRow?.querySelector('.credential-count')?.textContent?.trim() ?? '',
      firstRow?.querySelector('.offering-count')?.textContent?.trim() ?? '',
    ];
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

    (await waitForButton(fixture, root, 'New provider')).click();
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
    (await waitForButton(fixture, root, 'Save')).click();
    await waitForRequests(fixture, root, 8);

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

    (await waitForButton(fixture, root, 'New provider')).click();
    fixture.detectChanges();
    (await waitForButton(fixture, root, 'Save')).click();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(2);
    expect(root.textContent).toContain('Name is required.');
    expect(root.textContent).toContain('Base URL is required.');

    typeInto(root, '#provider-name', 'azure');
    typeInto(root, '#provider-base-url', 'ftp://a.example');
    (await waitForButton(fixture, root, 'Save')).click();
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
    (await waitForButton(fixture, root, 'Disable')).click();
    await waitForRequests(fixture, root, 6);

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

    (await waitForButton(fixture, root, 'Edit')).click();
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
    (await waitForButton(fixture, root, 'Save')).click();
    await waitForRequests(fixture, root, 6);

    const call = fetchMock.requests[3];
    expect(call.method).toBe('PATCH');
    expect(call.body).toEqual({ name: 'openrouter-eu', enabled: false });
  });

  it('deletes a provider only after the dialog confirmation', async () => {
    queueLoad([providerFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    (await waitForButton(fixture, root, 'Delete')).click();
    fixture.detectChanges();

    // Opening the confirmation must not delete anything yet.
    expect(fetchMock.requests).toHaveLength(3);
    fetchMock.respond(noContentResponse());
    queueLoad([]);
    (await waitForButton(fixture, root, 'Delete provider')).click();
    await waitForText(fixture, root, 'No providers yet');

    const call = fetchMock.requests[3];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/providers/p1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(root.textContent).toContain('No providers yet');
  });

  it('opens credentials in a drawer from the provider card', async () => {
    queueLoad([providerFixture()], { p1: [credentialFixture()] });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('.provider-detail')).toBeNull();
    const drawer = root.querySelector<HTMLDialogElement>('.credentials-drawer dialog');
    expect(drawer?.hasAttribute('open')).toBe(false);

    // Opening the drawer mounts the credentials panel and re-reads the list.
    fetchMock.respond(jsonResponse(200, [credentialFixture()]));
    (await waitForButton(fixture, root, 'Credentials')).click();
    await waitForText(fixture, root, 'prod');

    expect(root.querySelector('.credentials-drawer')?.textContent).toContain(
      'Credentials — openrouter',
    );
    expect(drawer?.hasAttribute('open')).toBe(true);
    expect(fetchMock.requests[3].url).toBe('/api/providers/p1/credentials');

    const close = Array.from(
      root.querySelectorAll<HTMLButtonElement>('.credentials-drawer button'),
    ).find((button) => button.textContent?.trim() === 'Close');
    if (close === undefined) {
      throw new Error('no credential drawer close button');
    }
    close.click();
    await waitFor(fixture, () => drawer?.hasAttribute('open') === false, 'the drawer to close');
    expect(document.activeElement?.textContent).toContain('Credentials');
  });

  it('previews discovery with a dry run, then applies and links to Models', async () => {
    queueLoad([providerFixture()], { p1: [credentialFixture()] });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    // Opening the dialog re-reads credentials before Preview can run.
    fetchMock.respond(jsonResponse(200, [credentialFixture()]));
    (await waitForButton(fixture, root, 'Load models')).click();
    await waitForText(fixture, root, 'Load models — openrouter');

    fetchMock.respond(jsonResponse(200, discoveryFixture()));
    (await waitForButton(fixture, root, 'Preview')).click();
    await waitForText(fixture, root, 'Preview (no changes written)');

    const previewCall = fetchMock.requests.find((call) => call.url.endsWith('/discover-models'));
    expect(previewCall?.method).toBe('POST');
    expect(previewCall?.url).toBe('/api/providers/p1/discover-models');
    expect(previewCall?.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(previewCall?.body).toEqual({ credential_id: 'c1', dry_run: true });
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
    (await waitForButton(fixture, root, 'Apply')).click();
    await waitFor(
      fixture,
      () => (root.querySelector('.last-load')?.textContent ?? '').includes('+2 models'),
      'the applied discovery row to reload',
    );

    const applyCall = fetchMock.requests
      .filter((call) => call.url.endsWith('/discover-models'))
      .at(-1);
    expect(applyCall?.body).toEqual({ credential_id: 'c1', dry_run: false });
    expect(root.textContent).toContain('Applied: 5 discovered');
    const link = root.querySelector('a[href="/models?provider=p1"]');
    expect(link).not.toBeNull();
    // Persisted, not client memory: this is the value a reload renders.
    const lastLoad = root.querySelector('.last-load');
    expect(lastLoad?.textContent).toContain('+2 models');
    expect(lastLoad?.textContent).toContain('3 unchanged');
    expect(lastLoad?.textContent).not.toContain('never');
  });

  it('uses a credential created in the panel for Load models without a page reload', async () => {
    queueLoad([providerFixture()], { p1: [] });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const created = credentialFixture({ id: 'c2', label: 'fresh' });

    // Open the drawer so the create form is reachable.
    fetchMock.respond(jsonResponse(200, []));
    (await waitForButton(fixture, root, 'Credentials')).click();
    await waitForText(fixture, root, 'No credentials yet');

    (await waitForButton(fixture, root, 'New credential')).click();
    fixture.detectChanges();
    typeInto(root, '#cred-label', 'fresh');
    typeInto(root, '#cred-secret', 'sk-fresh');
    fetchMock.respond(jsonResponse(201, created));
    fetchMock.respond(jsonResponse(200, [created]));
    (await waitForButton(fixture, root, 'Save')).click();
    const save = Array.from(
      root.querySelectorAll<HTMLButtonElement>('.credentials-drawer button'),
    ).find((button) => button.textContent?.trim() === 'Save');
    if (save === undefined) {
      throw new Error('no credential save button');
    }
    save.click();
    await waitForRequests(fixture, root, 6);

    // Opening Load models re-reads the list; the new credential is selectable.
    fetchMock.respond(jsonResponse(200, [created]));
    (await waitForButton(fixture, root, 'Load models')).click();
    await waitForText(fixture, root, 'fresh (ok)');

    const select = root.querySelector<HTMLSelectElement>('#discovery-credential');
    expect(select?.value).toBe('c2');
    expect(root.textContent).toContain('fresh (ok)');

    fetchMock.respond(jsonResponse(200, discoveryFixture({ credential_id: 'c2' })));
    (await waitForButton(fixture, root, 'Preview')).click();
    await waitForText(fixture, root, 'Preview (no changes written)');

    const previewCall = fetchMock.requests.find((call) => call.url.endsWith('/discover-models'));
    expect(previewCall?.body).toEqual({ credential_id: 'c2', dry_run: true });
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
        kind: 'balance',
        configured: true,
        enabled: true,
        config: {
          url: '/v2/usage',
          headers: {},
          allow_private: false,
          balance: {
            balance: '/balance',
            subtract: null,
            available: null,
            currency: 'USD',
            period_spend: null,
            burn_rate: null,
            days_remaining: null,
            unit: {
              multiplier: 1,
              currency: 'USD',
              fx_to_usd: null,
              display: { symbol: '€', decimals: 2 },
              note: null,
            },
            refresh_seconds: 900,
          },
        },
        updated_at: '2026-01-01T00:00:00.000Z',
        updated_by: 'u1',
      }),
    );
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
    const root = fixture.nativeElement as HTMLElement;
    await waitFor(
      fixture,
      () => root.querySelector('.provider-balance')?.textContent?.trim() === '€3.50',
      'the connector price display to load',
    );

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
    // The balance column prefers the balance unit's own display (D52).
    expect(fetchMock.requests[3].url).toBe('/api/providers/p1/connectors/balance');
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
        raw: 150,
        converted: 1.5,
        routable: true,
        period_spend: 3,
        burn_rate: 0.5,
        days_remaining: 2.4,
        warnings: [],
        error: null,
        fetched_at: '2026-01-01T00:00:00.000Z',
      }),
    );
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('.metric-note')?.textContent?.trim()).toBe('runs out in 2 days');
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
    const root = fixture.nativeElement as HTMLElement;
    await waitFor(
      fixture,
      () => root.querySelector('.provider-balance')?.textContent?.trim() === 'unavailable',
      'the unavailable provider balance to render',
    );

    const balance = root.querySelector('.provider-balance');
    expect(balance?.textContent?.trim()).toBe('unavailable');
    expect(balance?.classList.contains('is-unavailable')).toBe(true);
    expect(root.textContent).not.toContain('$0.00');
  });

  it('opens the connectors dialog from the provider row', async () => {
    queueLoad([providerFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    (await waitForButton(fixture, root, 'Provider setup')).click();
    await waitForText(fixture, root, 'Provider setup — openrouter');
    await waitForRequests(fixture, root, 4);

    expect(root.textContent).toContain('Provider setup — openrouter');
    expect(fetchMock.requests[3].url).toBe('/api/providers/p1/connectors/templates');
  });

  it('shows the upstream error message verbatim from a failed preview', async () => {
    queueLoad([providerFixture()], { p1: [credentialFixture()] });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, [credentialFixture()]));
    (await waitForButton(fixture, root, 'Load models')).click();
    await waitForText(fixture, root, 'Load models — openrouter');
    fetchMock.respond(
      errorResponse(
        502,
        'upstream_error',
        'The provider answered 503 after 10s; no models were loaded.',
      ),
    );
    (await waitForButton(fixture, root, 'Preview')).click();
    await waitForText(fixture, root, 'The provider answered 503 after 10s; no models were loaded.');

    expect(root.textContent).toContain(
      'The provider answered 503 after 10s; no models were loaded.',
    );
    expect((await waitForButton(fixture, root, 'Apply')).disabled).toBe(true);
  });

  it('explains a missing credential instead of calling discovery', async () => {
    queueLoad([providerFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, []));
    (await waitForButton(fixture, root, 'Load models')).click();
    await waitForText(fixture, root, 'This provider has no credentials yet');

    expect(root.textContent).toContain('This provider has no credentials yet');
    expect((await waitForButton(fixture, root, 'Preview')).disabled).toBe(true);
    // The dialog's open-time refresh made one read and discovery was never called.
    expect(fetchMock.requests).toHaveLength(4);
    expect(fetchMock.requests[3].url).toBe('/api/providers/p1/credentials');
  });

  it('shows the server message when loading fails', async () => {
    fetchMock.respond(errorResponse(500, 'internal', 'Reading providers failed.'));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    await waitForText(fixture, root, 'Reading providers failed.');

    expect(root.textContent).toContain('Reading providers failed.');
  });

  it('shows the preset badge for a provider created with a preset', async () => {
    queueLoad([providerFixture({ preset: 'sailresearch' })]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('sailresearch');
    expect(root.querySelector('ui-badge[title*="sailresearch preset"]')).not.toBeNull();
    expect(root.textContent).toContain('Apply preset defaults');
  });

  it('creates a provider with a preset and a prefilled base URL', async () => {
    presetResponse = [
      {
        id: 'openrouter',
        label: 'OpenRouter',
        description: 'One key, many models.',
        base_url: 'https://openrouter.ai/api/v1',
        auth: 'bearer',
        flags: {
          skip_batch_variants: true,
          variable_price_sentinel: true,
          apply_on_discovery: false,
        },
        configs: {},
      },
    ];
    queueLoad([]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    (await waitForButton(fixture, root, 'New provider')).click();
    fixture.detectChanges();
    await waitForText(fixture, root, 'OpenRouter');
    selectValue(root, '#provider-preset', 'openrouter');
    fixture.detectChanges();

    const baseUrl = root.querySelector('#provider-base-url');
    expect(baseUrl instanceof HTMLInputElement ? baseUrl.value : null).toBe(
      'https://openrouter.ai/api/v1',
    );
    expect(root.textContent).toContain('configured automatically');

    typeInto(root, '#provider-name', 'openrouter');
    fetchMock.respond(jsonResponse(201, providerFixture({ id: 'p2', preset: 'openrouter' })));
    queueLoad([providerFixture({ id: 'p2', preset: 'openrouter' })]);
    (await waitForButton(fixture, root, 'Save')).click();
    await waitForRequests(fixture, root, 6);

    const post = fetchMock.requests.find((call) => call.method === 'POST');
    expect(post?.body).toEqual({
      name: 'openrouter',
      base_url: 'https://openrouter.ai/api/v1',
      kind: 'openai_compatible',
      preset: 'openrouter',
    });
  });

  it('opens provider setup from a badge and follows URL changes both ways', async () => {
    queueLoad([providerFixture({ preset: 'openrouter' })]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const router = TestBed.inject(Router);

    const badge = root.querySelector('[aria-label="Open balance setup"]');
    expect(badge).not.toBeNull();
    (badge as HTMLElement).click();
    await waitForText(fixture, root, 'Provider setup — openrouter');
    await waitFor(
      fixture,
      () => router.url.includes('setup=p1'),
      'the URL to carry the setup link',
    );
    expect(router.url).toContain('tab=balance');
    expect(tabByLabel(root, 'Balance').getAttribute('aria-selected')).toBe('true');

    await router.navigate([], { queryParams: { setup: null, tab: null } });
    await waitFor(
      fixture,
      () => !root.textContent?.includes('Provider setup — openrouter'),
      'close',
    );

    await router.navigate([], { queryParams: { setup: 'p1', tab: 'health' } });
    await waitForText(fixture, root, 'Provider setup — openrouter');
    expect(tabByLabel(root, 'Health').getAttribute('aria-selected')).toBe('true');
  });

  it('offers Set up balance when a provider balance is unavailable', async () => {
    queueLoad(
      [
        providerFixture({
          connectors: [{ kind: 'balance', enabled: true, updated_at: '2026-01-01T00:00:00.000Z' }],
        }),
      ],
      {
        p1: [
          credentialFixture({
            balance_available: false,
            balance_error: 'connect: unreachable',
            balance_currency: 'USD',
          }),
        ],
      },
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    await waitForText(fixture, root, 'unavailable');

    const cta = await waitForButton(fixture, root, 'Set up balance');
    cta.click();
    await waitForText(fixture, root, 'Provider setup — openrouter');
    expect(tabByLabel(root, 'Balance').getAttribute('aria-selected')).toBe('true');
  });
});
