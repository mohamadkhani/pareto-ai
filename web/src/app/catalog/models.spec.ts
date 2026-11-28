import { signal } from '@angular/core';
import { Location } from '@angular/common';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
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

const modelFixture: Model = {
  id: 'm1',
  canonical_name: 'openai/gpt-5',
  family: 'gpt-5',
  modalities_input: ['text'],
  modalities_output: ['text'],
  context_length: 400000,
  max_output_tokens: 128000,
  capabilities: [],
  enabled: true,
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
    model_enabled: true,
    providers: [
      { id: 'p1', name: 'openrouter' },
      { id: 'p2', name: 'deepinfra' },
    ],
    providers_count: 2,
    offerings_count: 3,
    routable_offerings: 3,
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
    provider_id: 'p1',
    upstream_model_id: 'openai/gpt-5',
    provider_enabled: true,
    enabled: true,
    model_enabled: true,
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
      routable_offerings: 2,
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

/** The status badge in the row whose name matches `name`. */
function statusBadge(root: HTMLElement, name: string): HTMLElement {
  const row = Array.from(root.querySelectorAll('tbody tr')).find((candidate) =>
    candidate.textContent?.includes(name),
  );
  const badge = row?.querySelector<HTMLElement>('td ui-badge[title]');
  if (badge === null || badge === undefined) {
    throw new Error(`no status badge with a title for ${name}`);
  }
  return badge;
}

function searchBox(root: HTMLElement): HTMLInputElement {
  const input = root.querySelector<HTMLInputElement>('#models-search input');
  if (input === null) {
    throw new Error('no search box');
  }
  return input;
}

function maxPriceBox(root: HTMLElement): HTMLInputElement {
  const input = root.querySelector<HTMLInputElement>('#models-max-price input');
  if (input === null) {
    throw new Error('no max price filter');
  }
  return input;
}

function minQualityBox(root: HTMLElement): HTMLInputElement {
  const input = root.querySelector<HTMLInputElement>('#models-min-quality input');
  if (input === null) {
    throw new Error('no min quality filter');
  }
  return input;
}

/** The toolbar switch that reveals inactive rows (disabled or unroutable). */
function showDisabledSwitch(root: HTMLElement): HTMLButtonElement {
  const control = root.querySelector<HTMLButtonElement>('.models-toolbar button[role="switch"]');
  if (control === null) {
    throw new Error('no show-disabled switch');
  }
  return control;
}

/** The select-all checkbox in the table header. */
function headerCheckbox(root: HTMLElement): HTMLInputElement {
  const box = root.querySelector<HTMLInputElement>('thead .select-col input[type="checkbox"]');
  if (box === null) {
    throw new Error('no select-all checkbox');
  }
  return box;
}

/** The selection checkbox of the row whose text contains `name`. */
function rowCheckbox(root: HTMLElement, name: string): HTMLInputElement {
  const row = Array.from(root.querySelectorAll('tbody tr')).find((candidate) =>
    candidate.textContent?.includes(name),
  );
  const box = row?.querySelector<HTMLInputElement>('.select-col input[type="checkbox"]');
  if (box === null || box === undefined) {
    throw new Error(`no selection checkbox for ${name}`);
  }
  return box;
}

/** Sets a native checkbox and fires the change the component listens for. */
function checkBox(input: HTMLInputElement, checked: boolean): void {
  input.checked = checked;
  input.dispatchEvent(new Event('change'));
}

/** Queues the filtered response, then the unfiltered one used for the count. */
function queueSearch(
  fetchMock: FetchMock,
  filtered: ModelsOverview,
  unfiltered: ModelsOverview,
): void {
  fetchMock.respond(jsonResponse(200, filtered));
  fetchMock.respond(jsonResponse(200, unfiltered));
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
    querySearch: string | null = null,
    queryMaxPrice: string | null = null,
    queryMinQuality: string | null = null,
    queryModel: string | null = null,
  ): Promise<ComponentFixture<Models>> {
    const query: Record<string, string> = {};
    if (queryProvider !== null) {
      query['provider'] = queryProvider;
    }
    if (queryMix !== null) {
      query['mix'] = queryMix;
    }
    if (querySearch !== null) {
      query['q'] = querySearch;
    }
    if (queryMaxPrice !== null) {
      query['max_price'] = queryMaxPrice;
    }
    if (queryMinQuality !== null) {
      query['min_quality'] = queryMinQuality;
    }
    if (queryModel !== null) {
      query['model'] = queryModel;
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
    expect(text).toContain('openrouter');
    expect(text).toContain('deepinfra');
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

  it('shows provider chips and collapses the overflow into a "+N" chip', async () => {
    const names = ['openrouter', 'deepinfra', 'together', 'fireworks', 'novita'];
    queueReload(fetchMock, {
      ...allOverview,
      rows: [
        allRow({
          providers: names.map((name, index) => ({ id: `p${index}`, name })),
          providers_count: names.length,
        }),
      ],
    });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const chips = (): string[] =>
      Array.from(root.querySelectorAll('ui-chip')).map((chip) => chip.textContent?.trim() ?? '');

    expect(chips()).toEqual(['openrouter', 'deepinfra', 'together', '+2']);

    root.querySelector<HTMLButtonElement>('ui-chip.provider-chip--more button')?.click();
    fixture.detectChanges();
    expect(chips()).toEqual([...names, 'less']);

    root.querySelector<HTMLButtonElement>('ui-chip.provider-chip--more button')?.click();
    fixture.detectChanges();
    expect(chips()).toEqual(['openrouter', 'deepinfra', 'together', '+2']);
  });

  it('omits a minimum price when it equals the shown average', async () => {
    queueReload(fetchMock, {
      ...allOverview,
      rows: [
        allRow({
          price_avg: { input_per_m: 1, output_per_m: 2, cache_read_per_m: null },
          price_min: { input_per_m: 1, output_per_m: 2, cache_read_per_m: null },
          mix_price: {
            per_m: 0.5,
            profile_id: 'mix-code-agent',
            profile_name: 'code-agent',
            basis: 'blended',
          },
          mix_price_min: {
            per_m: 0.5,
            profile_id: 'mix-code-agent',
            profile_name: 'code-agent',
            basis: 'blended',
          },
        }),
      ],
    });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('$0.5');
    expect(root.querySelectorAll('.price-min')).toHaveLength(0);
  });

  it('filters to one provider when its chip is clicked', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, providerOverview));
    root.querySelector<HTMLButtonElement>('ui-chip button')?.click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests.at(-1)?.url).toBe(
      '/api/models/overview?provider=p1&mix=mix-code-agent',
    );
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
            routable_offerings: 1,
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

  it('scopes to one model and opens it from ?model=, ignoring the search and caps', async () => {
    queueReload(fetchMock, allOverview);
    // The URL also carries a search and caps; the exact model must win.
    const fixture = await create('admin', null, null, 'claude', '99', '9999', 'm1');
    const root = fixture.nativeElement as HTMLElement;

    // Only the named model is listed, and its detail dialog is already open.
    expect(modelNames(root)).toEqual(['openai/gpt-5']);
    expect(detailDialog(root).textContent).toContain('openai/gpt-5');
    // The search term and the caps are dropped from the URL.
    const path = TestBed.inject(Location).path();
    expect(path).toContain('model=m1');
    expect(path).not.toContain('q=');
    expect(path).not.toContain('max_price=');
    expect(path).not.toContain('min_quality=');
  });

  it('leaves the exact-model view when the operator changes a filter', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create('admin', null, null, null, null, null, 'm1');
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, allOverview));
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    expect(TestBed.inject(Location).path()).not.toContain('model=');
  });

  it('escapes a stale ?model= link with an empty state that restores the list', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create('admin', null, null, null, null, null, 'gone');
    const root = fixture.nativeElement as HTMLElement;

    // The id is not in the catalog: the exact view is empty and no dialog opens.
    expect(root.textContent).toContain('No model matches this link');
    // The remedy names the model filter it clears and never promises the full list.
    expect(root.textContent).toContain('Clear the model filter to see this list.');
    expect(root.textContent).not.toContain('see the full list');
    expect(detailDialog(root).querySelector('dialog')?.open).toBe(false);

    buttonByText(root, 'Clear model filter').click();
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['anthropic/claude', 'openai/gpt-5']);
    expect(TestBed.inject(Location).path()).not.toContain('model=');
  });

  it('leaves the exact-model view when the detail dialog closes', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create('admin', null, null, null, null, null, 'm1');
    const root = fixture.nativeElement as HTMLElement;

    expect(TestBed.inject(Location).path()).toContain('model=m1');

    buttonByText(detailDialog(root), 'Close').click();
    fixture.detectChanges();

    expect(TestBed.inject(Location).path()).not.toContain('model=');
  });

  it('keeps the exact-model view when the price basis changes', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create('admin', null, null, null, null, null, 'm1');
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, allOverview));
    const basis = root.querySelector<HTMLSelectElement>('#models-price-basis');
    if (basis === null) {
      throw new Error('no price basis select');
    }
    basis.value = 'components';
    basis.dispatchEvent(new Event('change'));
    await fixture.whenStable();
    fixture.detectChanges();

    const path = TestBed.inject(Location).path();
    expect(path).toContain('mix=components');
    expect(path).toContain('model=m1');
    // A basis change reloads the overview; the dialog stays open because the
    // URL still names the model (the loadOverview path, not only reload).
    expect(detailDialog(root).querySelector('dialog')?.open).toBe(true);
  });

  it('leaves the exact-model view when the show-disabled switch changes', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create('admin', null, null, null, null, null, 'm1');
    const root = fixture.nativeElement as HTMLElement;

    expect(TestBed.inject(Location).path()).toContain('model=m1');

    showDisabledSwitch(root).click();
    fixture.detectChanges();

    expect(TestBed.inject(Location).path()).not.toContain('model=');
    expect(modelNames(root)).toEqual(['anthropic/claude', 'openai/gpt-5']);
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
    expect(text).not.toContain('deepinfra');
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

  it('disables a model for every provider from the all-provider view', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, { ...modelFixture, enabled: false }));
    queueReload(fetchMock, allOverview);
    const row = Array.from(root.querySelectorAll('tbody tr')).find((candidate) =>
      candidate.textContent?.includes('openai/gpt-5'),
    );
    if (!(row instanceof HTMLElement)) {
      throw new Error('no row for openai/gpt-5');
    }
    buttonByText(row, 'Disable all').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[6];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/models/m1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ enabled: false });
    expect(root.textContent).toContain('disabled for all 2 providers');
    expect(root.textContent).toContain('moves to the Show disabled view');
  });

  it('toggles one provider offering from the all-provider list', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, providerOverview));
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    fetchMock.respond(jsonResponse(200, providerRow({ enabled: false })));
    queueReload(fetchMock, providerOverview);
    buttonByText(root, 'Disable').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[7];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/offerings/o1');
    expect(call.body).toEqual({ enabled: false });
    expect(root.textContent).toContain('openai/gpt-5 disabled for openrouter');
  });

  it('blocks a per-provider switch while the model is disabled for all providers', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(
      jsonResponse(200, { ...providerOverview, rows: [providerRow({ model_enabled: false })] }),
    );
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    // The disabled model is hidden by default; the switch reveals it so the
    // blocked per-provider control can be inspected.
    expect(root.textContent).toContain('No active models');
    showDisabledSwitch(root).click();
    await fixture.whenStable();
    fixture.detectChanges();

    const toggle = buttonByText(root, 'Disable');
    expect(toggle.disabled).toBe(true);
    expect(root.textContent).toContain('disabled (all)');
  });

  it('hides disabled models by default and reveals them with the switch', async () => {
    const overview: ModelsOverview = {
      ...allOverview,
      rows: [
        allRow(),
        allRow({ model_id: 'm2', canonical_name: 'anthropic/claude', model_enabled: false }),
      ],
    };
    queueReload(fetchMock, overview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(modelNames(root)).toEqual(['openai/gpt-5']);
    expect(root.textContent).toContain('Show disabled (1)');

    showDisabledSwitch(root).click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['anthropic/claude', 'openai/gpt-5']);
  });

  it('hides a disabled offering in provider scope until the switch is on', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(
      jsonResponse(200, { ...providerOverview, rows: [providerRow({ enabled: false })] }),
    );
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.textContent).toContain('No active models');

    showDisabledSwitch(root).click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['openai/gpt-5']);
    expect(root.textContent).toContain('disabled');
  });

  it('reveals every disabled model from the empty-state button', async () => {
    const overview: ModelsOverview = {
      ...allOverview,
      rows: [allRow({ model_enabled: false })],
    };
    queueReload(fetchMock, overview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('No active models');
    buttonByText(root, 'Show disabled').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['openai/gpt-5']);
  });

  it('hides a model whose offerings are all disabled even while the model is enabled', async () => {
    // model_enabled is true, but no offering is routable: the active view must
    // hide it, because routing would skip every offering.
    const overview: ModelsOverview = {
      ...allOverview,
      rows: [
        allRow(),
        allRow({
          model_id: 'm2',
          canonical_name: 'anthropic/claude',
          model_enabled: true,
          offerings_count: 2,
          routable_offerings: 0,
        }),
      ],
    };
    queueReload(fetchMock, overview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(modelNames(root)).toEqual(['openai/gpt-5']);
    expect(root.textContent).toContain('Show disabled (1)');

    showDisabledSwitch(root).click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['anthropic/claude', 'openai/gpt-5']);
    // The revealed row explains itself instead of looking merely disabled.
    expect(root.textContent).toContain('not routable');
    expect(statusBadge(root, 'anthropic/claude').getAttribute('title')).toContain(
      'routing skips this model',
    );
  });

  it('hides an enabled offering on a disabled provider in provider scope', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    // The offering and model are enabled, but the provider is disabled, so the
    // offering is not routable and belongs in the Show disabled view.
    fetchMock.respond(
      jsonResponse(200, {
        ...providerOverview,
        rows: [providerRow({ enabled: true, provider_enabled: false })],
      }),
    );
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.textContent).toContain('No active models');

    showDisabledSwitch(root).click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['openai/gpt-5']);
    expect(root.textContent).toContain('provider disabled');
    expect(statusBadge(root, 'openai/gpt-5').getAttribute('title')).toContain(
      'This provider is disabled',
    );

    // The detail dialog explains the disabled provider rather than leaving the
    // checked "Offering enabled" switch unexplained.
    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(detailDialog(root).textContent).toContain(
      'This provider is disabled, so none of its offerings are routable',
    );
  });

  it('tells a provider-disabled row that the offering is off too', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    // Both switches are off: enabling only the provider must not be presented
    // as enough, so the tooltip and the detail banner say "Enable both".
    fetchMock.respond(
      jsonResponse(200, {
        ...providerOverview,
        rows: [providerRow({ enabled: false, provider_enabled: false })],
      }),
    );
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    showDisabledSwitch(root).click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['openai/gpt-5']);
    expect(statusBadge(root, 'openai/gpt-5').getAttribute('title')).toContain('Enable both');

    buttonByText(root, 'Details').click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(detailDialog(root).textContent).toContain(
      'This provider is disabled and the offering is disabled too',
    );
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
    // The counts stay on one scannable line instead of a raw key/value dump.
    expect(root.textContent).toContain('0/157 matched');
    expect(root.textContent).toContain('157 unknown');
  });

  it('collapses the sync details behind a summary and remembers the choice', async () => {
    window.localStorage.removeItem('pareto.models.syncDetails');
    queueReload(fetchMock, allOverview, {
      benchmark: {
        sources: [
          {
            source: 'arena',
            status: 'success',
            counts: { published: 10, matched: 8 },
            error: null,
            finished_at: '2026-11-16T18:47:14.675Z',
            source_url: null,
            license: 'CC BY 4.0',
            attribution_url: null,
            fresh: true,
          },
          {
            source: 'benchlm',
            status: 'error',
            counts: null,
            error: 'fetch failed',
            finished_at: '2026-11-16T18:40:00.000Z',
            source_url: null,
            license: null,
            attribution_url: null,
            fresh: false,
          },
        ],
      },
    });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const toggle = root.querySelector<HTMLButtonElement>('.sync-toggle');
    const details = root.querySelector<HTMLElement>('#sync-details');

    expect(toggle?.getAttribute('aria-expanded')).toBe('false');
    expect(details?.hidden).toBe(true);
    expect(root.textContent).toContain('Sync status');
    // The worst source state and the failing source stay visible without
    // expanding.
    expect(
      Array.from(root.querySelectorAll('.sync-summary ui-badge')).some(
        (badge) => badge.textContent?.trim() === '1 error',
      ),
    ).toBe(true);
    expect(root.querySelector('.sync-summary')?.textContent).toContain('benchlm');

    toggle?.click();
    fixture.detectChanges();
    expect(toggle?.getAttribute('aria-expanded')).toBe('true');
    expect(details?.hidden).toBe(false);
    expect(window.localStorage.getItem('pareto.models.syncDetails')).toBe('open');

    window.localStorage.removeItem('pareto.models.syncDetails');
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

  it('hints that prices need setup when the provider has none', async () => {
    queueReload(fetchMock, providerOverview);
    const fixture = await create('admin', 'p1');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Prices need setup for this provider.');
    const button = root.querySelector('.sync-details button');
    expect(button).not.toBeNull();
    (button as HTMLElement).click();
    const router = TestBed.inject(Router);
    await vi.waitFor(() => expect(router.url).toContain('setup=p1'));
    expect(router.url).toContain('tab=prices');
  });

  it('does not show the connector hint when a catalog connector exists', async () => {
    queueReload(fetchMock, providerOverview, {
      providers: [
        {
          ...provider,
          connectors: [{ kind: 'catalog', enabled: true, updated_at: '2026-01-01T00:00:00.000Z' }],
        },
      ],
    });
    const fixture = await create('admin', 'p1');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).not.toContain('Prices need a connector for this provider.');
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

  it('debounces typing into one API call with the trimmed term and counts results', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    vi.useFakeTimers();
    try {
      typeInto(root, searchBox(root), 'dee');
      await vi.advanceTimersByTimeAsync(100);
      typeInto(root, searchBox(root), '  deepseek  ');
      await vi.advanceTimersByTimeAsync(100);
      // Still inside the debounce window: no search call yet.
      expect(fetchMock.requests.filter((request) => request.url.includes('q='))).toHaveLength(0);

      queueSearch(fetchMock, emptyOverview, allOverview);
      await vi.advanceTimersByTimeAsync(150);
      fixture.detectChanges();

      const searches = fetchMock.requests.filter((request) => request.url.includes('q='));
      expect(searches.map((request) => request.url)).toEqual([
        '/api/models/overview?provider=all&mix=mix-code-agent&q=deepseek',
      ]);
      expect(TestBed.inject(Location).path()).toContain('q=deepseek');
      expect(root.textContent).toContain('0 of 2 models');
      expect(root.textContent).toContain('No models match “deepseek”');
    } finally {
      vi.useRealTimers();
    }
  });

  it('restores a searched page from ?q= together with the provider filter', async () => {
    const filtered: ModelsOverview = {
      scope: 'provider',
      provider_id: 'p1',
      mix: mixRef,
      rows: [providerRow({ canonical_name: 'deepseek/v3' })],
    };
    queueReload(fetchMock, filtered, { providers: [provider] });
    const fixture = await create('admin', 'p1', null, 'deepseek');
    const root = fixture.nativeElement as HTMLElement;

    expect(fetchMock.requests[3].url).toBe('/api/models/overview?provider=p1&q=deepseek');
    expect(searchBox(root).value).toBe('deepseek');
  });

  it('focuses the search box with the "/" shortcut', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const input = searchBox(root);

    document.dispatchEvent(new KeyboardEvent('keydown', { key: '/', bubbles: true }));
    fixture.detectChanges();

    expect(document.activeElement).toBe(input);
  });

  it('clears the search and restores the full list', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    vi.useFakeTimers();
    typeInto(root, searchBox(root), 'claude');
    queueSearch(fetchMock, { ...allOverview, rows: [allOverview.rows[1]] }, allOverview);
    await vi.advanceTimersByTimeAsync(250);
    vi.useRealTimers();
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['anthropic/claude']);
    expect(root.textContent).toContain('1 of 2 models');

    fetchMock.respond(jsonResponse(200, allOverview));
    buttonByText(root, 'Clear').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['anthropic/claude', 'openai/gpt-5']);
    expect(searchBox(root).value).toBe('');
    expect(TestBed.inject(Location).path()).not.toContain('q=');
    expect(root.textContent).toContain('2 models');
  });

  it('offers a clear action from the search empty state', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    vi.useFakeTimers();
    typeInto(root, searchBox(root), 'zzz');
    queueSearch(fetchMock, emptyOverview, allOverview);
    await vi.advanceTimersByTimeAsync(250);
    vi.useRealTimers();
    fixture.detectChanges();

    expect(root.textContent).toContain('No models match “zzz”');

    fetchMock.respond(jsonResponse(200, allOverview));
    buttonByText(root, 'Clear search').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['anthropic/claude', 'openai/gpt-5']);
    expect(root.textContent).not.toContain('No models match');
  });

  it('caps the list by the shown mix price without another API call', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Max mix price ($/M)');
    const callsBefore = fetchMock.requests.length;

    typeInto(root, maxPriceBox(root), '0.9');
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['openai/gpt-5']);
    expect(root.textContent).toContain('1 of 2 models');
    expect(TestBed.inject(Location).path()).toContain('max_price=0.9');
    expect(fetchMock.requests).toHaveLength(callsBefore);
  });

  it('filters by the minimum quality and excludes rows without a score', async () => {
    queueReload(fetchMock, {
      ...allOverview,
      rows: [
        allRow({ model_id: 'm1', canonical_name: 'a/high' }),
        allRow({
          model_id: 'm2',
          canonical_name: 'b/low',
          quality: { ...quality, score: 1200, rank: 2 },
        }),
        allRow({ model_id: 'm3', canonical_name: 'c/unknown', quality: null }),
      ],
    });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, minQualityBox(root), '1300');
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['a/high']);
    expect(root.textContent).toContain('1 of 3 models');
    expect(TestBed.inject(Location).path()).toContain('min_quality=1300');
  });

  it('applies the price cap to the input column when component prices are selected', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create('admin', null, 'components');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Max input price ($/M)');

    typeInto(root, maxPriceBox(root), '2');
    fixture.detectChanges();

    // The priced model averages $1.50 input; the unpriced one never passes.
    expect(modelNames(root)).toEqual(['openai/gpt-5']);
    expect(root.textContent).toContain('1 of 2 models');
  });

  it('restores price and quality filters from the URL', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create('admin', null, null, null, '1', '1400');
    const root = fixture.nativeElement as HTMLElement;

    expect(maxPriceBox(root).value).toBe('1');
    expect(minQualityBox(root).value).toBe('1400');
    // The default row costs $0.84 and scores 1433, so it survives both caps.
    expect(modelNames(root)).toEqual(['openai/gpt-5']);
    expect(root.textContent).toContain('1 of 2 models');
  });

  it('clears both caps from the filtered empty state', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, minQualityBox(root), '9999');
    fixture.detectChanges();

    expect(root.textContent).toContain('No models match these limits');
    const empty = root.querySelector('ui-empty');
    if (empty === null) {
      throw new Error('no empty state');
    }
    buttonByText(empty as HTMLElement, 'Clear limits').click();
    fixture.detectChanges();

    expect(modelNames(root)).toEqual(['anthropic/claude', 'openai/gpt-5']);
    expect(minQualityBox(root).value).toBe('');
    expect(TestBed.inject(Location).path()).not.toContain('min_quality=');
  });

  it('clears the provider, the search, and the limits from the combined empty state', async () => {
    fetchMock.respond(jsonResponse(200, [provider]));
    fetchMock.respond(jsonResponse(200, [codeAgent, chatProfile]));
    fetchMock.respond(jsonResponse(200, pricingSettings()));
    queueSearch(fetchMock, emptyOverview, allOverview);
    fetchMock.respond(jsonResponse(200, { last_run: null, snapshot_stale: true, ttl_hours: 24 }));
    fetchMock.respond(jsonResponse(200, { sources: [] }));
    const fixture = await create('admin', 'p1', null, 'gpt', '0', null);
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('No models match “gpt”');

    fetchMock.respond(jsonResponse(200, allOverview));
    buttonByText(root, 'Clear all filters').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const providerSelect = root.querySelector('#models-provider-filter');
    expect(providerSelect instanceof HTMLSelectElement ? providerSelect.value : null).toBe('all');
    expect(searchBox(root).value).toBe('');
    expect(maxPriceBox(root).value).toBe('');
    expect(modelNames(root)).toEqual(['anthropic/claude', 'openai/gpt-5']);
    const path = TestBed.inject(Location).path();
    expect(path).not.toContain('q=');
    expect(path).not.toContain('max_price=');
    expect(path).not.toContain('provider=');
  });

  it('combines the search with the provider filter and counts in that scope', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, providerOverview));
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    vi.useFakeTimers();
    typeInto(root, searchBox(root), 'gpt');
    queueSearch(fetchMock, providerOverview, providerOverview);
    await vi.advanceTimersByTimeAsync(250);
    vi.useRealTimers();
    fixture.detectChanges();

    const searches = fetchMock.requests.filter((request) => request.url.includes('q='));
    expect(searches.map((request) => request.url)).toEqual([
      '/api/models/overview?provider=p1&mix=mix-code-agent&q=gpt',
    ]);
    expect(root.textContent).toContain('1 of 1 model');
  });

  it('selects every visible row and disables them in one batch', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('.batch-bar')).toBeNull();

    checkBox(headerCheckbox(root), true);
    fixture.detectChanges();

    expect(headerCheckbox(root).checked).toBe(true);
    expect(root.querySelector('.batch-bar')?.textContent).toContain('2 selected');
    expect(root.querySelector('.batch-bar')?.textContent).toContain('every provider');

    fetchMock.respond(jsonResponse(200, { ...modelFixture, enabled: false }));
    fetchMock.respond(
      jsonResponse(200, {
        ...modelFixture,
        id: 'm2',
        canonical_name: 'anthropic/claude',
        enabled: false,
      }),
    );
    queueReload(fetchMock, allOverview);
    buttonByText(root, 'Disable selected').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const calls = fetchMock.requests.slice(6, 8);
    expect(calls.map((call) => call.url).sort()).toEqual(['/api/models/m1', '/api/models/m2']);
    expect(calls.every((call) => call.method === 'PATCH')).toBe(true);
    expect(
      calls.every((call) => call.body && (call.body as { enabled: boolean }).enabled === false),
    ).toBe(true);
    expect(root.textContent).toContain('Disabled 2 models for every provider');
    // The selection clears once the batch finishes.
    expect(root.querySelector('.batch-bar')).toBeNull();
  });

  it('shows the select-all box as indeterminate while only some rows are checked', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    checkBox(rowCheckbox(root, 'openai/gpt-5'), true);
    fixture.detectChanges();

    expect(headerCheckbox(root).indeterminate).toBe(true);
    expect(headerCheckbox(root).checked).toBe(false);

    checkBox(rowCheckbox(root, 'anthropic/claude'), true);
    fixture.detectChanges();

    expect(headerCheckbox(root).indeterminate).toBe(false);
    expect(headerCheckbox(root).checked).toBe(true);

    buttonByText(root, 'Clear').click();
    fixture.detectChanges();

    expect(root.querySelector('.batch-bar')).toBeNull();
    expect(headerCheckbox(root).checked).toBe(false);
  });

  it('disables the selected offering for the provider scope only', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, providerOverview));
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    checkBox(rowCheckbox(root, 'openai/gpt-5'), true);
    fixture.detectChanges();

    expect(root.querySelector('.batch-bar')?.textContent).toContain(
      "Changes this provider's offerings (openrouter)",
    );

    fetchMock.respond(jsonResponse(200, providerRow({ enabled: false })));
    queueReload(fetchMock, providerOverview);
    buttonByText(root, 'Disable selected').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[7];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/offerings/o1');
    expect(call.body).toEqual({ enabled: false });
    expect(root.textContent).toContain('Disabled 1 offering for openrouter.');
  });

  it('skips a globally disabled model when batch enabling offerings', async () => {
    queueReload(fetchMock, allOverview);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const disabled: ModelsOverview = {
      ...providerOverview,
      rows: [providerRow({ enabled: false, model_enabled: false })],
    };
    fetchMock.respond(jsonResponse(200, disabled));
    changeProvider(root, 'p1');
    await fixture.whenStable();
    fixture.detectChanges();

    // The disabled row is hidden by default; reveal it to select it.
    showDisabledSwitch(root).click();
    await fixture.whenStable();
    fixture.detectChanges();

    checkBox(rowCheckbox(root, 'openai/gpt-5'), true);
    fixture.detectChanges();

    const before = fetchMock.requests.length;
    queueReload(fetchMock, disabled);
    buttonByText(root, 'Enable selected').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests.slice(before).some((call) => call.method === 'PATCH')).toBe(false);
    expect(root.textContent).toContain('1 selected model is disabled for all providers');
  });
});
