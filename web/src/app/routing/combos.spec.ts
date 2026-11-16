import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { FetchMock, errorResponse, jsonResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { Model, Offering, Provider } from '../catalog/catalog.types';
import type { Combo } from './combos.types';
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
  base_url: 'https://openrouter.ai/api/v1',
  enabled: true,
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
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
    members: [{ offering_id: 'off1', weight: 1 }],
    enabled: true,
    rr_cursor: 0,
    ...overrides,
  };
}

/** Queues the four GETs `reload()` issues: combos, offerings, providers, models. */
function queueLoad(
  fetchMock: FetchMock,
  combos: Combo[] = [],
  offerings: Offering[] = [offeringFixture()],
): void {
  fetchMock.respond(jsonResponse(200, combos));
  fetchMock.respond(jsonResponse(200, offerings));
  fetchMock.respond(jsonResponse(200, [provider]));
  fetchMock.respond(jsonResponse(200, [modelFixture()]));
}

function selectInto(root: HTMLElement, selector: string, value: string): void {
  const select = root.querySelector(selector);
  if (!(select instanceof HTMLSelectElement)) {
    throw new Error(`no select matching ${selector}`);
  }
  select.value = value;
  select.dispatchEvent(new Event('change'));
}

function typeInto(root: HTMLElement, selector: string, value: string): void {
  const input = root.querySelector(selector);
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`no input matching ${selector}`);
  }
  input.value = value;
  // Emulate typing and leaving the field: fields listen to `input`, `change`, or both.
  input.dispatchEvent(new Event('input'));
  input.dispatchEvent(new Event('change'));
}

function submit(root: HTMLElement): void {
  const form = root.querySelector('form');
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
      providers: [{ provide: AuthService, useValue: authStub(role) }],
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
    selectInto(root, '#combo-member-picker', 'off1');
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[4];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/combos');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      name: 'shared-fast',
      strategy: 'weighted',
      weights: { preset: 'cheapest' },
      members: [{ offering_id: 'off1', weight: 1 }],
      enabled: true,
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
    selectInto(root, '#combo-member-picker', 'off1');
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[4];
    expect(call.body).toEqual({
      name: 'tuned',
      strategy: 'weighted',
      weights: { price: 0.1, speed: 0.2, quality: 0.3, reliability: 0.4 },
      members: [{ offering_id: 'off1', weight: 1 }],
      enabled: true,
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
    selectInto(root, '#combo-member-picker', 'off1');
    selectInto(root, '#combo-member-picker', 'off2');
    fixture.detectChanges();

    // Move the second member above the first: the stored order is the fallback chain.
    const rows = memberRows(root);
    expect(rows).toHaveLength(2);
    buttonByText(rows[1], 'Up').click();
    fixture.detectChanges();

    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[4];
    expect(call.body).toMatchObject({
      strategy: 'ordered',
      members: [
        { offering_id: 'off2', weight: 1 },
        { offering_id: 'off1', weight: 1 },
      ],
    });
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

    expect(fetchMock.requests).toHaveLength(4);
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
    selectInto(root, '#combo-member-picker', 'off1');
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.querySelector('.banner.error')?.textContent ?? '').toContain(
      'member offering off1 does not exist',
    );
    expect(root.querySelector('form')).not.toBeNull();
  });
});
