import { signal } from '@angular/core';
import { Location } from '@angular/common';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { MixProfile, PricingSettings } from './mix.types';
import type { Model, Provider } from './catalog.types';
import { Models } from './models';
import type {
  ModelsOverview,
  ModelOverviewRow,
  OverviewQuality,
  ProviderOverviewRow,
} from './overview.types';

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

const provider: Provider = {
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
};

const modelFixture: Model = {
  id: 'm1',
  canonical_name: 'openai/gpt-5',
  family: 'gpt-5',
  modalities_input: ['text'],
  modalities_output: ['text'],
  context_length: 400000,
  max_output_tokens: 128000,
  capabilities: [],
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
};

const quality: OverviewQuality = {
  score: 1433,
  source: 'benchmark',
  category: 'text',
  ci: 12,
  rank: 1,
  fetched_at: '2026-01-01T00:00:00.000Z',
  overridden: false,
};

function mixProfile(overrides: Partial<MixProfile> = {}): MixProfile {
  return {
    id: 'mix-code-agent',
    name: 'code-agent',
    kind: 'preset',
    input_share: 8.1,
    output_share: 0.4,
    cache_read_share: 91.5,
    cache_write_share: 0,
    reasoning_share: 0,
    is_builtin: true,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

const codeAgent = mixProfile();
const chatProfile = mixProfile({
  id: 'mix-chat',
  name: 'chat',
  input_share: 74,
  output_share: 26,
  cache_read_share: 0,
});

/** Code-agent is the effective default; the setting itself is unset. */
function pricingSettings(overrides: Partial<PricingSettings> = {}): PricingSettings {
  return {
    default_mix_profile: null,
    effective_profile: { profile_id: 'mix-code-agent', profile_name: 'code-agent' },
    env_override: null,
    ...overrides,
  };
}

const mixRef = { profile_id: 'mix-code-agent', profile_name: 'code-agent' };

function allRow(overrides: Partial<ModelOverviewRow> = {}): ModelOverviewRow {
  return {
    model_id: 'm1',
    canonical_name: 'openai/gpt-5',
    family: 'gpt-5',
    modalities_input: ['text', 'image'],
    modalities_output: ['text'],
    context_length: 400000,
    max_output_tokens: 128000,
    providers_count: 2,
    offerings_count: 3,
    unpriced_offerings: 1,
    variable_offerings: 0,
    price_avg: { input_per_m: 1.5, output_per_m: 6, cache_read_per_m: 0.15 },
    price_min: { input_per_m: 1, output_per_m: 5, cache_read_per_m: 0.1 },
    mix_price: {
      per_m: 0.84,
      profile_id: 'mix-code-agent',
      profile_name: 'code-agent',
      basis: 'blended',
    },
    mix_price_min: {
      per_m: 0.62,
      profile_id: 'mix-code-agent',
      profile_name: 'code-agent',
      basis: 'blended',
    },
    quality,
    ...overrides,
  };
}

function providerRow(overrides: Partial<ProviderOverviewRow> = {}): ProviderOverviewRow {
  return {
    model_id: 'm1',
    canonical_name: 'openai/gpt-5',
    family: 'gpt-5',
    modalities_input: ['text', 'image'],
    modalities_output: ['text'],
    context_length: 400000,
    max_output_tokens: 128000,
    offering_id: 'o1',
    upstream_model_id: 'openai/gpt-5',
    enabled: true,
    price: {
      input_per_m: 1.25,
      output_per_m: 5,
      cache_read_per_m: null,
      cache_write_per_m: null,
      reasoning_per_m: 0.5,
      request_fee: 0.01,
      variable_price: false,
      currency: 'USD',
      source: 'openrouter',
      fetched_at: '2026-01-01T00:00:00.000Z',
    },
    mix_price: {
      per_m: 1.265,
      profile_id: 'mix-code-agent',
      profile_name: 'code-agent',
      basis: 'blended',
    },
    quality,
    ...overrides,
  };
}

const allOverview: ModelsOverview = {
  scope: 'all',
  provider_id: null,
  mix: mixRef,
  rows: [
    allRow(),
    allRow({
      model_id: 'm2',
      canonical_name: 'anthropic/claude',
      family: 'claude',
      context_length: 100000,
      price_avg: null,
      price_min: null,
      mix_price: null,
      mix_price_min: null,
      unpriced_offerings: 2,
      offerings_count: 2,
    }),
  ],
};

const providerOverview: ModelsOverview = {
  scope: 'provider',
  provider_id: 'p1',
  mix: mixRef,
  rows: [providerRow()],
};

const emptyOverview: ModelsOverview = {
  scope: 'all',
  provider_id: null,
  mix: mixRef,
  rows: [],
};

function queueReload(
  fetchMock: FetchMock,
  overview: ModelsOverview,
  options: {
    providers?: Provider[];
    profiles?: MixProfile[];
    pricing?: PricingSettings;
    benchmarkMissing?: boolean;
    benchmark?: unknown;
    priceStatus?: unknown;
  } = {},
): void {
  fetchMock.respond(jsonResponse(200, options.providers ?? [provider]));
  fetchMock.respond(jsonResponse(200, options.profiles ?? [codeAgent, chatProfile]));
  fetchMock.respond(jsonResponse(200, options.pricing ?? pricingSettings()));
  fetchMock.respond(jsonResponse(200, overview));
  fetchMock.respond(
    jsonResponse(
      200,
      options.priceStatus ?? { last_run: null, snapshot_stale: true, ttl_hours: 24 },
    ),
  );
  fetchMock.respond(
    options.benchmarkMissing === true
      ? errorResponse(404, 'not_found', 'no route')
      : jsonResponse(200, options.benchmark ?? { sources: [] }),
  );
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

function detailDialog(root: HTMLElement): HTMLElement {
  const dialog = root.querySelector('ui-dialog');
  if (!(dialog instanceof HTMLElement)) {
    throw new Error('no detail dialog');
  }
  return dialog;
}

function dialogNumberInputs(root: HTMLElement): HTMLInputElement[] {
  return Array.from(detailDialog(root).querySelectorAll<HTMLInputElement>('input[type="number"]'));
}

function typeInto(root: HTMLElement, element: HTMLInputElement, value: string): void {
  element.value = value;
  element.dispatchEvent(new Event('input'));
}

function changeProvider(root: HTMLElement, value: string): void {
  const select = root.querySelector('#models-provider-filter');
  if (!(select instanceof HTMLSelectElement)) {
    throw new Error('no provider filter');
  }
  select.value = value;
  select.dispatchEvent(new Event('change'));
}

function sortControl(root: HTMLElement, label: string): HTMLButtonElement {
  const button = Array.from(root.querySelectorAll<HTMLButtonElement>('th .sort-button')).find(
    (candidate) => candidate.textContent?.includes(label),
  );
  if (button === undefined) {
    throw new Error(`no sort control for ${label}`);
  }
  return button;
}

function modelNames(root: HTMLElement): string[] {
  return Array.from(root.querySelectorAll('tbody tr .model-name')).map(
    (cell) => cell.textContent?.trim() ?? '',
  );
}

describe('Models', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
    window.localStorage.clear();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(
    role: 'admin' | 'user' = 'admin',
    queryProvider: string | null = null,
    queryMix: string | null = null,
  ): Promise<ComponentFixture<Models>> {
    const query: Record<string, string> = {};
    if (queryProvider !== null) {
      query['provider'] = queryProvider;
    }
    if (queryMix !== null) {
      query['mix'] = queryMix;
    }
    TestBed.configureTestingModule({
      imports: [Models],
      providers: [
        { provide: AuthService, useValue: authStub(role) },
        provideRouter([
          { path: '', children: [] },
          { path: 'providers', children: [] },
          { path: 'models', children: [] },
        ]),
        {
          provide: ActivatedRoute,
          useValue: {
            snapshot: {
              queryParamMap: convertToParamMap(query),
            },
          },
        },
      ],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Models);
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

  it('renders the default mix price with its basis and the all-provider minimum', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('openai/gpt-5');
    expect(text).toContain('2 providers');
    expect(text).toContain('avg');
    // The default profile is preselected: one mix column, not three components.
    expect(text).toContain('Mix price $/M');
    expect(text).toContain('code-agent');
    expect(text).toContain('$0.84');
    expect(text).toContain('min $0.62');
    expect(text).toContain('blended');
    expect(text).not.toContain('Input $/M');
    expect(text).toContain('1433.0');
    expect(text).toContain('benchmark');
    // Unpriced offers an explicit badge and never $0.00.
    expect(text).toContain('unpriced');
    expect(text).not.toContain('$0.00');
    expect(fetchMock.requests[3].url).toBe('/api/models/overview?provider=all');
  });

  it('shows "variable" instead of a number for a sentinel-priced model', async () => {
    queueReload(
      fetchMock,
      {
        scope: 'all',
        provider_id: null,
        mix: mixRef,
        rows: [
          allRow({ model_id: 'm1', canonical_name: 'a/priced' }),
          allRow({
            model_id: 'm2',
            canonical_name: 'openrouter/auto',
            price_avg: null,
            price_min: null,
            mix_price: null,
            mix_price_min: null,
            unpriced_offerings: 1,
            variable_offerings: 1,
            offerings_count: 1,
          }),
        ],
      },
      {},
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('variable');
    expect(text).not.toContain('-$1000000');
    expect(text).not.toContain('-1000000');

    // A variable price is not a number, so the cheap end of the sort belongs
    // to the genuinely priced row in both directions.
    sortControl(root, 'Mix price $/M').click();
    fixture.detectChanges();
    expect(modelNames(root)).toEqual(['a/priced', 'openrouter/auto']);
    sortControl(root, 'Mix price $/M').click();
    fixture.detectChanges();
    expect(modelNames(root)).toEqual(['a/priced', 'openrouter/auto']);
  });

  it('switches price basis, adds the mix query, and persists the choice', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const basis = root.querySelector<HTMLSelectElement>('#models-price-basis');
    if (basis === null) {
      throw new Error('no price basis select');
    }
    expect(basis.value).toBe('mix-code-agent');

    // Component prices: the three component columns come back.
    fetchMock.respond(jsonResponse(200, allOverview));
    basis.value = 'components';
    basis.dispatchEvent(new Event('change'));
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[6].url).toBe('/api/models/overview?provider=all');
    expect(root.textContent).toContain('Input $/M');
    expect(root.textContent).toContain('$1.50');
    expect(root.textContent).toContain('min $1.00');
    expect(window.localStorage.getItem('pareto.models.priceBasis')).toBe('components');
    expect(TestBed.inject(Location).path()).toContain('mix=components');

    // A profile: the overview is fetched with `&mix=` and the column follows.
    fetchMock.respond(
      jsonResponse(200, {
        ...allOverview,
        mix: { profile_id: 'mix-chat', profile_name: 'chat' },
        rows: [
          allRow({
            mix_price: {
              per_m: 6.12,
              profile_id: 'mix-chat',
              profile_name: 'chat',
              basis: 'blended',
            },
            mix_price_min: null,
          }),
          allOverview.rows[1],
        ],
      }),
    );
    basis.value = 'mix-chat';
    basis.dispatchEvent(new Event('change'));
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[7].url).toBe('/api/models/overview?provider=all&mix=mix-chat');
    expect(root.textContent).toContain('Mix price $/M');
    expect(root.textContent).toContain('chat');
    expect(root.textContent).toContain('$6.12');
    expect(window.localStorage.getItem('pareto.models.priceBasis')).toBe('mix-chat');
    expect(TestBed.inject(Location).path()).toContain('mix=mix-chat');
  });

  it('starts from ?mix= when the URL names a profile, and falls back on an unknown one', async () => {
    queueReload(fetchMock, allOverview, { profiles: [codeAgent, chatProfile] });
    const fixture = await create('admin', null, 'mix-chat');
    const root = fixture.nativeElement as HTMLElement;

    expect(fetchMock.requests[3].url).toBe('/api/models/overview?provider=all&mix=mix-chat');
    const basis = root.querySelector<HTMLSelectElement>('#models-price-basis');
    expect(basis?.value).toBe('mix-chat');
  });

  it('switches to per-offering rows when a provider is selected', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, providerOverview));
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[6].url).toBe('/api/models/overview?provider=p1&mix=mix-code-agent');
    const text = root.textContent ?? '';
    expect(text).toContain('openrouter');
    expect(text).toContain('enabled');
    expect(text).toContain('$1.26');
    expect(text).not.toContain('2 providers');
  });

  it('sorts by a header with ascending, descending, and stable name tie-breaks', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(modelNames(root)).toEqual(['anthropic/claude', 'openai/gpt-5']);

    sortControl(root, 'Context').click();
    fixture.detectChanges();
    expect(modelNames(root)).toEqual(['anthropic/claude', 'openai/gpt-5']);
    expect(sortControl(root, 'Context').closest('th')?.getAttribute('aria-sort')).toBe('ascending');

    sortControl(root, 'Context').click();
    fixture.detectChanges();
    expect(modelNames(root)).toEqual(['openai/gpt-5', 'anthropic/claude']);
    expect(sortControl(root, 'Context').closest('th')?.getAttribute('aria-sort')).toBe(
      'descending',
    );
  });

  it('opens the detail dialog with every price component and saves the edited body', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, providerOverview));
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const dialog = detailDialog(root);
    const text = dialog.textContent ?? '';
    expect(text).toContain('openai/gpt-5');
    expect(text).toContain('openai/gpt-5'); // upstream id
    expect(text).toContain('Request fee');
    expect(text).toContain('Reasoning per 1M');
    expect(text).toContain('text, image → text');
    expect(text).toContain('Quality');
    expect(text).toContain('text'); // quality category

    const inputs = dialogNumberInputs(root);
    // Order: six price components, then the quality override.
    expect(inputs).toHaveLength(7);
    expect(inputs[0].value).toBe('1.25');
    expect(inputs[4].value).toBe('0.5');
    expect(inputs[5].value).toBe('0.01');
    typeInto(root, inputs[0], '2.5');

    fetchMock.respond(jsonResponse(200, {}));
    queueReload(fetchMock, providerOverview);
    buttonByText(dialog, 'Save price').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[7];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/offerings/o1/price');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      input_per_m: 2.5,
      output_per_m: 5,
      cache_read_per_m: null,
      cache_write_per_m: null,
      reasoning_per_m: 0.5,
      request_fee: 0.01,
      currency: 'USD',
    });
  });

  it('shows the mix price block with per-component contributions and inheritance', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, providerOverview));
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const dialog = detailDialog(root);
    const text = dialog.textContent ?? '';
    expect(text).toContain('Mix price');
    expect(text).toContain('code-agent');
    expect(text).toContain('blended');
    expect(text).toContain('Effective profile');
    // Input 8.1% at $1.25, output 0.4% at $5.00, cache read 91.5% inherited from input.
    expect(text).toContain('8.1%');
    expect(text).toContain('0.4%');
    expect(text).toContain('91.5%');
    expect(text).toContain('$0.10125');
    expect(text).toContain('$1.14');
    expect(text).toContain('input price');
    expect(text).toContain('$1.27'); // the blend total
  });

  it('toggles the offering from the detail dialog', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, providerOverview));
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const dialog = detailDialog(root);
    const toggle = dialog.querySelector<HTMLButtonElement>('button[role="switch"]');
    if (toggle === null) {
      throw new Error('no enabled switch');
    }
    fetchMock.respond(jsonResponse(200, providerRow({ enabled: false })));
    queueReload(fetchMock, providerOverview);
    toggle.click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[7];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/offerings/o1');
    expect(call.body).toEqual({ enabled: false });
  });

  it('sets a quality override from the detail dialog', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const dialog = detailDialog(root);
    const inputs = dialogNumberInputs(root);
    const override = inputs[inputs.length - 1];
    typeInto(root, override, '1500');

    fetchMock.respond(jsonResponse(200, { model_id: 'm1', score: 1500 }));
    queueReload(fetchMock, allOverview);
    buttonByText(dialog, 'Set override').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[6];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/models/m2/quality-override');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ score: 1500 });
  });

  it('triggers the OpenRouter price sync from the toolbar', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(
      jsonResponse(200, {
        run_id: 'r1',
        status: 'success',
        model_count: 5,
        stale: false,
        started_at: '2026-01-01T00:00:00.000Z',
        finished_at: '2026-01-01T00:00:02.000Z',
        error: null,
      }),
    );
    fetchMock.respond(jsonResponse(200, { last_run: null, snapshot_stale: false, ttl_hours: 24 }));
    fetchMock.respond(jsonResponse(200, { sources: [] }));
    fetchMock.respond(jsonResponse(200, allOverview));

    buttonByText(root, 'Sync prices').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[6];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/sync/openrouter');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(root.textContent).toContain('5 models imported');
  });

  it('disables the benchmark sync when the endpoint is absent', async () => {
    queueReload(fetchMock, allOverview, { benchmarkMissing: true });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const disabled = buttonByText(root, 'Sync benchmarks');
    expect(disabled.disabled).toBe(true);
    expect(root.textContent).toContain('Benchmark sync is not available in this build.');
  });

  it('renders real benchmark status attribution, licence, and staleness', async () => {
    queueReload(fetchMock, allOverview, {
      benchmark: {
        ttl_hours: 5,
        sources: [
          {
            source: 'arena',
            license: 'CC BY 4.0',
            attribution_url: 'https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset',
            fresh: false,
            status: 'success',
            counts: { published: 157, matched: 0, skipped_unknown: 157 },
            error: null,
            finished_at: '2026-11-16T18:47:14.675Z',
            source_url: 'https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset',
          },
        ],
      },
    });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('CC BY 4.0');
    expect(root.textContent).toContain('stale');
    const attribution = root.querySelector(
      'a[href="https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset"]',
    );
    expect(attribution?.textContent?.trim()).toBe('attribution');
  });

  it('sends the benchmark sync POST with CSRF when available', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(
      jsonResponse(200, {
        dry_run: false,
        results: [
          {
            source: 'arena',
            status: 'success',
            dry_run: false,
            counts: { published: 1, inserted: 1 },
            started_at: '2026-01-01T00:00:00.000Z',
            finished_at: '2026-01-01T00:00:02.000Z',
            duration_ms: 2000,
            source_url: 'https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset',
            error: null,
          },
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, { last_run: null, snapshot_stale: false, ttl_hours: 24 }));
    fetchMock.respond(jsonResponse(200, { sources: [] }));
    fetchMock.respond(jsonResponse(200, allOverview));

    buttonByText(root, 'Sync benchmarks').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[6];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/benchmarks/sync');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({});
    expect(root.textContent).toContain('1 source updated');
  });

  it('surfaces a per-source benchmark sync error instead of reporting success', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(
      jsonResponse(200, {
        dry_run: false,
        results: [
          {
            source: 'benchlm',
            status: 'error',
            dry_run: false,
            counts: {},
            started_at: '2026-01-01T00:00:00.000Z',
            finished_at: '2026-01-01T00:00:02.000Z',
            duration_ms: 2000,
            source_url: 'https://benchlm.ai/models',
            error: 'fetching https://benchlm.ai/models failed',
          },
        ],
      }),
    );
    fetchMock.respond(jsonResponse(200, { last_run: null, snapshot_stale: false, ttl_hours: 24 }));
    fetchMock.respond(jsonResponse(200, { sources: [] }));
    fetchMock.respond(jsonResponse(200, allOverview));

    buttonByText(root, 'Sync benchmarks').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.textContent).toContain('The benchmark sync failed.');
    expect(root.textContent).toContain('benchlm: fetching https://benchlm.ai/models failed');
  });

  it('explains the unpriced rule when no offering has a price', async () => {
    queueReload(fetchMock, {
      scope: 'all',
      provider_id: null,
      mix: mixRef,
      rows: [
        allRow({
          price_avg: null,
          price_min: null,
          mix_price: null,
          mix_price_min: null,
          unpriced_offerings: 3,
        }),
      ],
    });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('No priced offerings yet');
    expect(root.textContent).toContain('never counted as $0.00');
  });

  it('shows the no-catalog empty state with a link to Providers', async () => {
    queueReload(fetchMock, emptyOverview, { providers: [] });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('No catalog yet');
    expect(root.querySelector('a[href="/providers"]')).not.toBeNull();
    expect(buttonByText(root, 'New model')).toBeTruthy();
  });

  it('shows the empty state for a provider with no offerings', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(
      jsonResponse(200, {
        scope: 'provider',
        provider_id: 'p1',
        mix: mixRef,
        rows: [],
      } satisfies ModelsOverview),
    );
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.textContent).toContain('No offerings for this provider');
    expect(root.querySelector('a[href="/providers"]')).not.toBeNull();
  });

  it('creates a model from the New model dialog', async () => {
    queueReload(fetchMock, emptyOverview, { providers: [] });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New model').click();
    fixture.detectChanges();

    const formDialog = root.querySelectorAll('ui-dialog')[1];
    if (!(formDialog instanceof HTMLElement)) {
      throw new Error('no model form dialog');
    }
    const inputs = Array.from(formDialog.querySelectorAll<HTMLInputElement>('input'));
    typeInto(root, inputs[0], 'openai/gpt-5');
    typeInto(root, inputs[1], 'gpt-5');
    typeInto(root, inputs[4], '400000');

    fetchMock.respond(jsonResponse(201, {}));
    queueReload(fetchMock, allOverview);
    buttonByText(formDialog, 'Create model').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[6];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/models');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      canonical_name: 'openai/gpt-5',
      family: 'gpt-5',
      modalities_input: [],
      modalities_output: [],
      context_length: 400000,
      max_output_tokens: null,
      capabilities: [],
    });
  });

  it('lands scoped when linked from Providers with ?provider=', async () => {
    queueReload(fetchMock, providerOverview);
    const fixture = await create('admin', 'p1');
    const root = fixture.nativeElement as HTMLElement;

    expect(fetchMock.requests[3].url).toBe('/api/models/overview?provider=p1');
    expect(root.textContent).toContain('openrouter');
    expect(root.textContent).not.toContain('avg');
  });

  it('shows the deep-linked provider name in the filter, not "All providers"', async () => {
    queueReload(fetchMock, providerOverview);
    const fixture = await create('admin', 'p1');
    const root = fixture.nativeElement as HTMLElement;
    const select = root.querySelector<HTMLSelectElement>('#models-provider-filter');

    expect(select).not.toBeNull();
    expect(select?.value).toBe('p1');
    expect(select?.selectedOptions[0]?.textContent?.trim()).toBe('openrouter');
    // The explicit id is also the id the label points at.
    expect(select?.labels).toHaveLength(1);
  });

  it('renders the model price-basis select with a matching label', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const select = root.querySelector<HTMLSelectElement>('#models-price-basis');

    expect(select?.labels).toHaveLength(1);
    expect(select?.selectedOptions[0]?.textContent).toContain('code-agent');
  });

  it('adds an offering from the provider-scoped detail dialog', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, providerOverview));
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const detail = detailDialog(root);
    fetchMock.respond(jsonResponse(200, [modelFixture]));
    buttonByText(detail, 'Add offering').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const dialogs = Array.from(root.querySelectorAll('ui-dialog'));
    const offeringDialog = dialogs[2];
    if (!(offeringDialog instanceof HTMLElement)) {
      throw new Error('no offering dialog');
    }
    expect(offeringDialog.textContent).toContain('Add offering');

    typeInto(
      root,
      offeringDialog.querySelector('#offering-upstream') as HTMLInputElement,
      ' openai/gpt-5 ',
    );
    typeInto(root, offeringDialog.querySelector('#offering-context') as HTMLInputElement, '200000');
    fixture.detectChanges();

    fetchMock.respond(jsonResponse(201, {}));
    queueReload(fetchMock, providerOverview);
    buttonByText(offeringDialog, 'Create offering').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[8];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/offerings');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      provider_id: 'p1',
      model_id: 'm1',
      upstream_model_id: 'openai/gpt-5',
      context_override: 200000,
      max_output_override: null,
      enabled: true,
      headers: null,
    });
    expect(root.textContent).toContain('Offering created.');
  });

  it('removes the offering only after the confirm dialog', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, providerOverview));
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const detail = detailDialog(root);
    buttonByText(detail, 'Remove offering').click();
    fixture.detectChanges();
    expect(fetchMock.requests).toHaveLength(7);

    const removeDialog = Array.from(root.querySelectorAll('ui-dialog'))[3];
    if (!(removeDialog instanceof HTMLElement)) {
      throw new Error('no remove dialog');
    }
    expect(removeDialog.textContent).toContain('cannot be undone');

    fetchMock.respond(noContentResponse());
    queueReload(fetchMock, providerOverview);
    buttonByText(removeDialog, 'Remove offering').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[7];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/offerings/o1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(root.textContent).toContain('Offering removed.');
  });
});
