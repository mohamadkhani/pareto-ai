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
import type { MixProfile } from '../catalog/mix.types';
import type { Model, Offering, Provider } from '../catalog/catalog.types';
import type { Combo, ComboPreview } from './combos.types';
import { Combos } from './combos';

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

function modelFixture(overrides: Partial<Model> = {}): Model {
  return {
    id: 'm1',
    canonical_name: 'openai/gpt-5',
    family: 'gpt-5',
    modalities_input: ['text'],
    modalities_output: ['text'],
    context_length: null,
    max_output_tokens: null,
    capabilities: [],
    enabled: true,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

function offeringFixture(overrides: Partial<Offering> = {}): Offering {
  return {
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
    ...overrides,
  };
}

function comboFixture(overrides: Partial<Combo> = {}): Combo {
  return {
    id: 'c1',
    name: 'shared-fast',
    strategy: 'weighted',
    weights: { price: 0.25, speed: 0.3, quality: 0.3, reliability: 0.15 },
    preset: 'balanced',
    mix_profile: null,
    pool: 'members',
    min_quality: null,
    max_mix_price_per_m: null,
    sort: 'price',
    provider_selection: 'mix_price',
    members: [{ offering_id: 'off1', weight: 1 }],
    enabled: true,
    rr_cursor: 0,
    stickiness: { enabled: false, ttl_seconds: 1800, key_source: 'auto', price_tolerance_pct: 5 },
    ...overrides,
  };
}

const mixProfiles: MixProfile[] = [
  {
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
  },
  {
    id: 'mix-chat',
    name: 'chat',
    kind: 'preset',
    input_share: 74,
    output_share: 26,
    cache_read_share: 0,
    cache_write_share: 0,
    reasoning_share: 0,
    is_builtin: true,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
  },
];

const previewFixture: ComboPreview = {
  profile: { profile_id: 'mix-code-agent', profile_name: 'code-agent' },
  scored: 2,
  candidates: [
    {
      offering_id: 'off2',
      model: 'anthropic/claude',
      provider: 'openrouter',
      quality: 1450,
      quality_source: 'benchmark',
      mix_price: 0.58,
      score: {
        price: 0.9,
        speed: 0.4,
        quality: 0.8,
        reliability: 0.7,
        penalties: 0.05,
        total: 0.602,
        imputed: [],
        shrunk: ['speed'],
      },
      explanation: 'scored #1 of 2: best weighted score 0.6020',
    },
    {
      offering_id: 'off1',
      model: 'openai/gpt-5',
      provider: 'openrouter',
      quality: 1433,
      quality_source: 'benchmark',
      mix_price: null,
      score: {
        price: 0.5,
        speed: 0.5,
        quality: 0.5,
        reliability: 0.5,
        penalties: 0.1,
        total: 0.4,
        imputed: ['price'],
        shrunk: [],
      },
      explanation: 'scored #2 of 2: weighted score 0.4000 below the leader 0.6020',
    },
  ],
  excluded: [
    {
      offering_id: 'off3',
      model: 'qwen/qwen3',
      provider: 'together',
      reason: 'unpriced',
      detail: 'no mix price under the profile',
    },
    {
      offering_id: 'off4',
      model: 'meta/llama',
      provider: 'together',
      reason: 'below_min_quality',
      detail: 'quality 900 is below the minimum 1200',
    },
    {
      offering_id: 'off5',
      model: 'mistral/large',
      provider: 'mistral',
      reason: 'below_min_quality',
      detail: 'quality 800 is below the minimum 1200',
    },
  ],
  sticky: {
    enabled: true,
    key_source: 'header',
    session_key: 's-1',
    pinned: 'off2',
    age_s: 42,
  },
};

/** Queues the five GETs `reload()` issues: combos, offerings, providers, models, profiles. */
function queueLoad(
  fetchMock: FetchMock,
  combos: Combo[] = [],
  offerings: Offering[] = [offeringFixture()],
): void {
  fetchMock.respond(jsonResponse(200, combos));
  fetchMock.respond(jsonResponse(200, offerings));
  fetchMock.respond(jsonResponse(200, [provider]));
  fetchMock.respond(jsonResponse(200, [modelFixture()]));
  fetchMock.respond(jsonResponse(200, mixProfiles));
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

function typeInto(root: HTMLElement, selector: string, value: string): void {
  const input = root.querySelector(selector);
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`no input matching ${selector}`);
  }
  input.value = value;
  // Emulate typing: the design-system input listens to `input`.
  input.dispatchEvent(new Event('input'));
  input.dispatchEvent(new Event('change'));
}

function submit(root: HTMLElement): void {
  const form = root.querySelector('dialog form');
  if (!(form instanceof HTMLFormElement)) {
    throw new Error('no form on the page');
  }
  form.dispatchEvent(new Event('submit'));
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

function memberRows(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>('.member-list li'));
}

describe('Combos', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(role: 'admin' | 'user' = 'admin'): Promise<ComponentFixture<Combos>> {
    TestBed.configureTestingModule({
      imports: [Combos],
      providers: [{ provide: AuthService, useValue: authStub(role) }, provideRouter([])],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Combos);
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

  it('explains what a combo is and links to Models when the list is empty', async () => {
    queueLoad(fetchMock);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const text = root.textContent ?? '';
    expect(text).toContain('No combos yet');
    expect(text).toContain('addressable as combo:<name>');
    const link = root.querySelector('a.empty-link');
    expect(link?.getAttribute('href')).toBe('/models');
  });

  it('creates a combo with a preset and its members', async () => {
    queueLoad(fetchMock);
    fetchMock.respond(jsonResponse(201, comboFixture()));
    queueLoad(fetchMock, [comboFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New combo').click();
    fixture.detectChanges();
    typeInto(root, '#combo-name', 'shared-fast');
    selectInto(root, '#combo-weights-mode', 'cheapest');
    comboboxInto(root, '#combo-member-picker', 'off1');
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[5];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/combos');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      name: 'shared-fast',
      strategy: 'weighted',
      weights: { preset: 'cheapest' },
      members: [{ offering_id: 'off1', weight: 1 }],
      enabled: true,
      pool: 'members',
      min_quality: null,
      max_mix_price_per_m: null,
      mix_profile: null,
      stickiness: { enabled: false, ttl_seconds: 1800, key_source: 'auto', price_tolerance_pct: 5 },
    });
    expect(root.textContent).toContain('Combo saved.');
    expect(root.textContent).toContain('combo:shared-fast');
  });

  it('creates a combo with custom weights', async () => {
    queueLoad(fetchMock);
    fetchMock.respond(jsonResponse(201, comboFixture({ preset: null })));
    queueLoad(fetchMock, [comboFixture({ preset: null })]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New combo').click();
    fixture.detectChanges();
    typeInto(root, '#combo-name', 'tuned');
    selectInto(root, '#combo-weights-mode', 'custom');
    fixture.detectChanges();
    typeInto(root, '#combo-weight-price', '0.1');
    typeInto(root, '#combo-weight-speed', '0.2');
    typeInto(root, '#combo-weight-quality', '0.3');
    typeInto(root, '#combo-weight-reliability', '0.4');
    comboboxInto(root, '#combo-member-picker', 'off1');
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[5];
    expect(call.body).toEqual({
      name: 'tuned',
      strategy: 'weighted',
      weights: { price: 0.1, speed: 0.2, quality: 0.3, reliability: 0.4 },
      members: [{ offering_id: 'off1', weight: 1 }],
      enabled: true,
      pool: 'members',
      min_quality: null,
      max_mix_price_per_m: null,
      mix_profile: null,
      stickiness: { enabled: false, ttl_seconds: 1800, key_source: 'auto', price_tolerance_pct: 5 },
    });
  });

  it('orders members with the move buttons', async () => {
    queueLoad(fetchMock, [], [offeringFixture(), offeringFixture({ id: 'off2' })]);
    fetchMock.respond(jsonResponse(201, comboFixture()));
    queueLoad(fetchMock, [comboFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New combo').click();
    fixture.detectChanges();
    typeInto(root, '#combo-name', 'chain');
    selectInto(root, '#combo-strategy', 'ordered');
    comboboxInto(root, '#combo-member-picker', 'off1');
    comboboxInto(root, '#combo-member-picker', 'off2');
    fixture.detectChanges();

    // Move the second member above the first: the stored order is the fallback chain.
    const rows = memberRows(root);
    expect(rows).toHaveLength(2);
    buttonByText(rows[1], 'Up').click();
    fixture.detectChanges();

    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[5];
    expect(call.body).toMatchObject({
      strategy: 'ordered',
      members: [
        { offering_id: 'off2', weight: 1 },
        { offering_id: 'off1', weight: 1 },
      ],
    });
  });

  it('edits an existing combo with its preset and stored member order', async () => {
    const combo = comboFixture({
      members: [
        { offering_id: 'off1', weight: 1 },
        { offering_id: 'off2', weight: 2 },
      ],
    });
    queueLoad(fetchMock, [combo], [offeringFixture(), offeringFixture({ id: 'off2' })]);
    fetchMock.respond(jsonResponse(200, combo));
    queueLoad(fetchMock, [combo]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Edit').click();
    fixture.detectChanges();

    expect(memberRows(root)).toHaveLength(2);
    const name = root.querySelector<HTMLInputElement>('#combo-name');
    expect(name?.value).toBe('shared-fast');
    expect(name?.readOnly).toBe(true);

    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[5];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/combos/c1');
    expect(call.body).toEqual({
      name: 'shared-fast',
      strategy: 'weighted',
      weights: { preset: 'balanced' },
      members: [
        { offering_id: 'off1', weight: 1 },
        { offering_id: 'off2', weight: 2 },
      ],
      enabled: true,
      pool: 'members',
      min_quality: null,
      max_mix_price_per_m: null,
      mix_profile: null,
      stickiness: { enabled: false, ttl_seconds: 1800, key_source: 'auto', price_tolerance_pct: 5 },
    });
  });

  it('deletes a combo after the confirmation dialog', async () => {
    queueLoad(fetchMock, [comboFixture()]);
    fetchMock.respond(noContentResponse());
    queueLoad(fetchMock, []);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Delete').click();
    fixture.detectChanges();
    expect(root.textContent).toContain('stops every request that addresses it');

    buttonByText(root, 'Delete combo').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[5];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/combos/c1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(root.textContent).toContain('Combo deleted.');
  });

  it('toggles a combo from the enabled switch', async () => {
    queueLoad(fetchMock, [comboFixture()]);
    fetchMock.respond(jsonResponse(200, comboFixture({ enabled: false })));
    queueLoad(fetchMock, [comboFixture({ enabled: false })]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const toggle = root.querySelector('[role="switch"]');
    if (!(toggle instanceof HTMLElement)) {
      throw new Error('no enabled switch');
    }
    toggle.click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[5];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/combos/c1');
    expect(call.body).toMatchObject({ enabled: false });
    expect(root.textContent).toContain('Disabled');
  });

  it('validates the name and the members before calling the API', async () => {
    queueLoad(fetchMock);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New combo').click();
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(5);
    expect(root.textContent).toContain('This field is required.');
    expect(root.textContent).toContain('Add at least one member offering.');
  });

  it('surfaces a server validation error and keeps the form open', async () => {
    queueLoad(fetchMock);
    fetchMock.respond(
      errorResponse(
        400,
        'invalid_request',
        'combo configuration is invalid: member offering off1 does not exist',
      ),
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New combo').click();
    fixture.detectChanges();
    typeInto(root, '#combo-name', 'doomed');
    comboboxInto(root, '#combo-member-picker', 'off1');
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.querySelector('ui-banner')?.textContent ?? '').toContain(
      'member offering off1 does not exist',
    );
    expect(root.querySelector('dialog form')).not.toBeNull();
  });

  it('creates an all-models combo with constraints, ordering knobs, mix profile, and stickiness', async () => {
    queueLoad(fetchMock);
    fetchMock.respond(jsonResponse(201, comboFixture({ pool: 'all', min_quality: 1400 })));
    queueLoad(fetchMock, [comboFixture({ pool: 'all', min_quality: 1400 })]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New combo').click();
    fixture.detectChanges();
    typeInto(root, '#combo-name', 'best-value');

    // The pool tab switches the editor into the constrained catalog mode.
    buttonByText(root, 'All models with constraints').click();
    fixture.detectChanges();

    // Only weighted is selectable for a catalog pool; the others stay in the
    // list (with the server's explanation) but are disabled.
    const strategy = root.querySelector<HTMLSelectElement>('#combo-strategy');
    const ordered = strategy?.querySelector<HTMLOptionElement>('option[value="ordered"]');
    const roundRobin = strategy?.querySelector<HTMLOptionElement>('option[value="round_robin"]');
    expect(strategy?.value).toBe('weighted');
    expect(ordered?.disabled).toBe(true);
    expect(roundRobin?.disabled).toBe(true);
    expect(root.textContent).toContain('only supports the weighted strategy');

    typeInto(root, '#combo-min-quality', '1400');
    typeInto(root, '#combo-max-mix-price', '2.5');
    selectInto(root, '#combo-mix-profile', 'mix-chat');
    selectInto(root, '#combo-pool-sort', 'quality');
    selectInto(root, '#combo-provider-selection', 'reliability');

    const pinSwitch = Array.from(
      root.querySelectorAll<HTMLButtonElement>('button[role="switch"]'),
    ).find((button) => button.getAttribute('aria-label') === 'Pin the chosen offering per session');
    if (pinSwitch === undefined) {
      throw new Error('no stickiness switch');
    }
    pinSwitch.click();
    fixture.detectChanges();
    selectInto(root, '#combo-stickiness-ttl', '3600');
    selectInto(root, '#combo-stickiness-key', 'header');
    typeInto(root, '#combo-stickiness-tolerance', '12');
    fixture.detectChanges();
    expect(root.textContent).toContain('X-Session-Id');
    // Header mode has no derived-key fallback: no header means no pin.
    expect(root.textContent).toContain('not pinned');

    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[5];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/combos');
    expect(call.body).toEqual({
      name: 'best-value',
      strategy: 'weighted',
      weights: { preset: 'balanced' },
      members: [],
      enabled: true,
      pool: 'all',
      min_quality: 1400,
      max_mix_price_per_m: 2.5,
      mix_profile: 'mix-chat',
      stickiness: {
        enabled: true,
        ttl_seconds: 3600,
        key_source: 'header',
        price_tolerance_pct: 12,
      },
      sort: 'quality',
      provider_selection: 'reliability',
    });
  });

  it('refuses an all-models combo with no constraint and mirrors the server message', async () => {
    queueLoad(fetchMock);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New combo').click();
    fixture.detectChanges();
    typeInto(root, '#combo-name', 'everything');
    buttonByText(root, 'All models with constraints').click();
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    // No POST went out: the rule is checked locally with the server's words.
    expect(fetchMock.requests).toHaveLength(5);
    expect(root.textContent).toContain(
      'pool = all requires at least one constraint (min_quality or max_mix_price_per_m)',
    );
  });

  it('previews a saved combo with candidates, grouped exclusions, and the sticky pin', async () => {
    queueLoad(fetchMock, [comboFixture()]);
    fetchMock.respond(jsonResponse(200, previewFixture));
    fetchMock.respond(jsonResponse(200, previewFixture));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Preview').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[5].url).toBe('/api/combos/c1/preview?limit=50');
    const preview = root.querySelector('app-combo-preview');
    const text = preview?.textContent ?? '';
    expect(text).toContain('code-agent');
    // Ranked in the runtime's order, position 1 first.
    const models = Array.from((preview ?? root).querySelectorAll('.candidate-model')).map(
      (element) => element.textContent?.trim(),
    );
    expect(models).toEqual(['anthropic/claude', 'openai/gpt-5']);
    expect(text).toContain('#1 of 2');
    // Exclusions are grouped by reason with their counts.
    expect(text).toContain('Below minimum quality');
    expect(text).toContain('Unpriced');
    expect(text).toContain('quality 900 is below the minimum 1200');
    // The pin names the offering and its age.
    expect(text).toContain('off2');
    expect(text).toContain('42s ago');

    // A session key refetches the preview with that key.
    typeInto(root, '#preview-session-key', 'session-9');
    buttonByText(root, 'Refresh pin').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[6].url).toBe('/api/combos/c1/preview?limit=50&session_key=session-9');
  });

  it('previews a draft through the POST endpoint before the combo is saved', async () => {
    queueLoad(fetchMock);
    fetchMock.respond(jsonResponse(200, previewFixture));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New combo').click();
    fixture.detectChanges();
    typeInto(root, '#combo-name', 'draft-value');
    buttonByText(root, 'All models with constraints').click();
    fixture.detectChanges();
    typeInto(root, '#combo-min-quality', '1200');
    fixture.detectChanges();

    buttonByText(root, 'Preview priority').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[5];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/combos/preview');
    expect(call.body).toMatchObject({
      name: 'draft-value',
      pool: 'all',
      min_quality: 1200,
      members: [],
      limit: 50,
    });
    expect(root.textContent).toContain('This is exactly what routing will do');
    expect(root.textContent).toContain('anthropic/claude');
  });
});
