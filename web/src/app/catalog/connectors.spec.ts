import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { FetchMock, jsonResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { Credential, Provider } from './catalog.types';
import { Connectors } from './connectors';
import type {
  ConnectorTemplate,
  ConnectorTestView,
  ConnectorView,
  PriceRefreshView,
  SampleView,
} from './connectors.types';

function authStub(): object {
  return {
    user: signal<User | null>({
      id: 'u1',
      email: 'admin@example.com',
      display_name: 'Admin',
      role: 'admin',
      must_change_password: false,
    }),
    isAdmin: signal(true),
    csrfToken: signal<string | null>('csrf-1'),
  };
}

const provider: Provider = {
  id: 'p1',
  name: 'sail',
  kind: 'openai_compatible',
  base_url: 'https://api.sailresearch.com/v1',
  enabled: true,
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
  last_discovery_at: null,
  last_discovery_created: null,
  last_discovery_unchanged: null,
  connectors: [{ kind: 'catalog', enabled: true, updated_at: '2026-01-01T00:00:00.000Z' }],
};

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

const sailTemplate: ConnectorTemplate = {
  id: 'sailresearch',
  label: 'SailResearch',
  description: 'SailResearch prices per million, quoted in cents.',
  configs: {
    catalog: {
      url: '/models',
      headers: { authorization: 'Bearer {{secret}}' },
      allow_private: false,
      catalog: {
        entries: '/data',
        model_id: '/id',
        input: '/pricing/balanced/input',
        output: '/pricing/balanced/output',
        cache_read: null,
        cache_write: null,
        reasoning: null,
        unit: {
          source: 'custom',
          multiplier: 0.01,
          currency: 'USD',
          fx_to_usd: null,
          display: { symbol: '$', decimals: 4 },
          note: 'quoted in cents',
        },
        modalities_input: null,
        modalities_output: null,
        context_length: null,
        max_output: null,
        apply_on_discovery: true,
        refresh_seconds: null,
      },
    },
    balance: {
      url: '/v2/usage/summary?range=30d',
      headers: {},
      allow_private: false,
      balance: {
        balance: '/balance',
        available: '/available',
        currency: 'USD',
        period_spend: null,
        burn_rate: null,
        days_remaining: null,
        refresh_seconds: 900,
      },
    },
  },
};

function catalogView(overrides: Partial<ConnectorView> = {}): ConnectorView {
  return {
    provider_id: 'p1',
    kind: 'catalog',
    configured: true,
    enabled: true,
    config: sailTemplate.configs.catalog ?? null,
    updated_at: '2026-01-01T00:00:00.000Z',
    updated_by: 'u1',
    ...overrides,
  };
}

function emptyView(kind: 'balance' | 'health'): ConnectorView {
  return {
    provider_id: 'p1',
    kind,
    configured: false,
    enabled: false,
    config: null,
    updated_at: null,
    updated_by: null,
  };
}

const sample: SampleView = {
  kind: 'catalog',
  url: 'https://api.sailresearch.com/v1/models',
  status: 200,
  bytes: 120,
  truncated: false,
  body: JSON.stringify({
    data: [
      {
        id: 'deepseek-v4',
        pricing: {
          asap: { input: 0.05, output: 0.1 },
          balanced: { input: 0.07, output: 0.14 },
          flex: { input: 0.09, output: 0.18 },
        },
      },
    ],
  }),
  fetched_at: '2026-01-01T00:00:00.000Z',
};

function testView(overrides: Partial<ConnectorTestView> = {}): ConnectorTestView {
  return {
    kind: 'catalog',
    ok: true,
    url: 'https://api.sailresearch.com/v1/models',
    entries_seen: 1,
    matched: 1,
    skipped: 0,
    samples: [
      {
        model_id: 'deepseek-v4',
        input_per_m: 0.0007,
        output_per_m: 0.0014,
        cache_read_per_m: null,
        cache_write_per_m: null,
        reasoning_per_m: null,
        raw: { input: 0.07, output: 0.14, cache_read: null, cache_write: null },
        converted_per_m: { input: 0.0007, output: 0.0014, cache_read: null, cache_write: null },
        warnings: ['input 0.0007 USD/M is implausibly low — check the unit'],
        currency: 'USD',
        routable: true,
        variable_price: false,
        error: null,
      },
    ],
    errors: [],
    warnings: [],
    currency: 'USD',
    balance: null,
    health: null,
    ...overrides,
  };
}

function refreshView(overrides: Partial<PriceRefreshView> = {}): PriceRefreshView {
  return {
    ok: true,
    provider_id: 'p1',
    run_id: 'r1',
    entries_seen: 1,
    created: 1,
    updated: 0,
    unchanged: 0,
    skipped_manual: 0,
    skipped_unresolved: 0,
    source_url: 'https://api.sailresearch.com/v1/models',
    fetched_at: '2026-01-01T00:00:00.000Z',
    error: null,
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

function selectValue(root: HTMLElement, selector: string, value: string): void {
  const select = root.querySelector(selector);
  if (!(select instanceof HTMLSelectElement)) {
    throw new Error(`no select matching ${selector}`);
  }
  select.value = value;
  select.dispatchEvent(new Event('change'));
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

function tabByText(root: HTMLElement, text: string): HTMLButtonElement {
  const tab = Array.from(root.querySelectorAll<HTMLButtonElement>('[role="tab"]')).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  if (tab === undefined) {
    throw new Error(`no tab labelled ${text}`);
  }
  return tab;
}

function pointerRow(root: HTMLElement, label: string): HTMLElement {
  const row = Array.from(root.querySelectorAll<HTMLElement>('.pointer-row')).find((candidate) =>
    candidate.textContent?.includes(label),
  );
  if (row === undefined) {
    throw new Error(`no pointer row for ${label}`);
  }
  return row;
}

describe('Connectors', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
    TestBed.configureTestingModule({
      imports: [Connectors],
      providers: [{ provide: AuthService, useValue: authStub() }],
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function queueLoad(credentials: Credential[] = [credentialFixture()]): void {
    fetchMock.respond(jsonResponse(200, [sailTemplate]));
    fetchMock.respond(jsonResponse(200, catalogView()));
    fetchMock.respond(jsonResponse(200, emptyView('balance')));
    fetchMock.respond(jsonResponse(200, emptyView('health')));
    fetchMock.respond(jsonResponse(200, credentials));
  }

  async function create(): Promise<ComponentFixture<Connectors>> {
    queueLoad();
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Connectors);
    fixture.componentRef.setInput('provider', provider);
    await settle(fixture);
    return fixture;
  }

  async function settle(fixture: ComponentFixture<Connectors>): Promise<void> {
    await new Promise((resolve) => setTimeout(resolve));
    await fixture.whenStable();
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
  }

  it('opens on Prices and lists the templates for the active kind', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Connectors — sail');
    expect(root.textContent).toContain('SailResearch');
    expect(tabByText(root, 'Prices').getAttribute('aria-selected')).toBe('true');
    expect(fetchMock.requests.map((call) => call.url)).toEqual([
      '/api/providers/p1/connectors/templates',
      '/api/providers/p1/connectors/catalog',
      '/api/providers/p1/connectors/balance',
      '/api/providers/p1/connectors/health',
      '/api/providers/p1/credentials',
    ]);
  });

  it('fetches a sample, then a picked tree value fills the active pointer', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, sample));
    buttonByText(root, 'Fetch sample').click();
    await settle(fixture);

    const sampleCall = fetchMock.requests[5];
    expect(sampleCall.method).toBe('POST');
    expect(sampleCall.url).toBe('/api/providers/p1/connectors/catalog/sample');
    expect(sampleCall.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(root.textContent).toContain('Sample · 200');

    // Select the input field, then click the number in the tree.
    pointerRow(root, 'Input price').querySelector('button')?.click();
    fixture.detectChanges();
    const pick = root.querySelector<HTMLButtonElement>(
      '[aria-label="Pick /data/0/pricing/asap/input"]',
    );
    if (pick === null) {
      throw new Error('the sample leaf is not pickable');
    }
    pick.click();
    fixture.detectChanges();

    const input = root.querySelector<HTMLInputElement>('#connector-catalog-input');
    expect(input?.value).toBe('/pricing/asap/input');
  });

  it('rewrites the shared tier segment from the sample', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, sample));
    buttonByText(root, 'Fetch sample').click();
    await settle(fixture);

    // Working pointers that share the balanced tier.
    typeInto(root, '#connector-catalog-input', '/data/0/pricing/balanced/input');
    typeInto(root, '#connector-catalog-output', '/data/0/pricing/balanced/output');
    fixture.detectChanges();

    expect(root.textContent).toContain('/data/0/pricing');
    const flex = Array.from(root.querySelectorAll<HTMLButtonElement>('.tier-options button')).find(
      (candidate) => candidate.textContent?.trim() === 'flex',
    );
    if (flex === undefined) {
      throw new Error('no flex tier button');
    }
    flex.click();
    fixture.detectChanges();

    expect(root.querySelector<HTMLInputElement>('#connector-catalog-input')?.value).toBe(
      '/data/0/pricing/flex/input',
    );
    expect(root.querySelector<HTMLInputElement>('#connector-catalog-output')?.value).toBe(
      '/data/0/pricing/flex/output',
    );
  });

  it('shows the live unit example and warns about an ignored multiplier', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    selectValue(root, '#connector-unit-source', 'custom');
    typeInto(root, '#connector-unit-multiplier', '0.01');
    fixture.detectChanges();

    expect(root.textContent).toContain('raw 0.07 → $0.0007/M');

    selectValue(root, '#connector-unit-source', 'per_token');
    fixture.detectChanges();
    expect(root.textContent).toContain('A multiplier of 0.01 is ignored');
  });

  it('previews extracted prices with summary counts and prominent warnings', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, testView()));
    buttonByText(root, 'Preview prices').click();
    await settle(fixture);

    const call = fetchMock.requests[5];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/providers/p1/connectors/catalog/test');
    expect(root.textContent).toContain('1 priced · 0 skipped · 0 errors · 1 entries');
    expect(root.textContent).toContain('deepseek-v4');
    expect(root.textContent).toContain('$0.0007');
    expect(root.textContent).toContain('implausibly low — check the unit');
    expect(root.querySelector('tr.is-warning')).not.toBeNull();
    expect(root.querySelector('.ui-banner.tone-warning')?.textContent).toContain(
      'implausibly low — check the unit',
    );
  });

  it('saves the wire document with the CSRF header', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, '#connector-url-catalog', '/v1/models');
    fetchMock.respond(
      jsonResponse(200, catalogView({ config: sailTemplate.configs.catalog ?? null })),
    );
    buttonByText(root, 'Save prices connector').click();
    await settle(fixture);

    const call = fetchMock.requests[5];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/providers/p1/connectors/catalog');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    const body = call.body as {
      enabled: boolean;
      config: { url: string; catalog: { input: string } };
    };
    expect(body.enabled).toBe(true);
    expect(body.config.url).toBe('/v1/models');
    expect(body.config.catalog.input).toBe('/pricing/balanced/input');
    expect(root.textContent).toContain('Prices connector saved.');
  });

  it('refreshes prices and shows the result counts', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, refreshView()));
    buttonByText(root, 'Refresh prices').click();
    await settle(fixture);

    const call = fetchMock.requests[5];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/providers/p1/prices/refresh');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(root.textContent).toContain('1 created · 0 updated');
  });

  it('explains warn vs block and saves the selected policy', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    tabByText(root, 'Balance').click();
    fixture.detectChanges();
    expect(root.textContent).toContain('Warn');
    expect(root.textContent).toContain('Block');
    expect(root.textContent).toContain('never changes routing');

    fetchMock.respond(jsonResponse(200, credentialFixture({ balance_gate: 'block' })));
    selectValue(root, '#balance-gate-c1', 'block');
    await settle(fixture);

    const call = fetchMock.requests[5];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/credentials/c1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ balance_gate: 'block' });
    expect(root.textContent).toContain('set to block');
  });

  it('renders an unavailable balance as unavailable, never zero', async () => {
    fetchMock.respond(jsonResponse(200, [sailTemplate]));
    fetchMock.respond(jsonResponse(200, catalogView()));
    fetchMock.respond(jsonResponse(200, emptyView('balance')));
    fetchMock.respond(jsonResponse(200, emptyView('health')));
    fetchMock.respond(
      jsonResponse(200, [
        credentialFixture({
          balance_available: false,
          balance_usd: null,
          balance_error: 'expired key',
        }),
      ]),
    );
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Connectors);
    fixture.componentRef.setInput('provider', provider);
    await settle(fixture);
    const root = fixture.nativeElement as HTMLElement;

    tabByText(root, 'Balance').click();
    fixture.detectChanges();
    expect(root.textContent).toContain('unavailable');
    expect(root.textContent).toContain('expired key');
    expect(root.textContent).not.toContain('$0.00');
  });

  it('tests a health endpoint and reports the result', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    tabByText(root, 'Health').click();
    fixture.detectChanges();

    fetchMock.respond(
      jsonResponse(
        200,
        testView({
          kind: 'health',
          entries_seen: null,
          matched: null,
          skipped: null,
          samples: [],
          health: { ok: true, detail: null },
        }),
      ),
    );
    buttonByText(root, 'Test health').click();
    await settle(fixture);

    expect(fetchMock.requests[5].url).toBe('/api/providers/p1/connectors/health/test');
    expect(root.textContent).toContain('The health check passed.');
  });
});
