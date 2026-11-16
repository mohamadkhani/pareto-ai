import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { Model, Offering, Price, Provider } from './catalog.types';
import { Offerings } from './offerings';

function authStub(): object {
  return {
    user: signal<User | null>({
      id: 'u1',
      email: 'admin@example.com',
      display_name: 'Admin',
      role: 'admin',
    }),
    isAdmin: signal(true),
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

const model: Model = {
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

function priceFixture(overrides: Partial<Price> = {}): Price {
  return {
    offering_id: 'o1',
    input_per_m: 1.25,
    output_per_m: 5,
    cache_read_per_m: null,
    cache_write_per_m: null,
    reasoning_per_m: null,
    request_fee: null,
    currency: 'USD',
    source: 'openrouter',
    source_url: null,
    fetched_at: '2026-01-01T00:00:00.000Z',
    updated_by: null,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
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

function submit(root: HTMLElement): void {
  const form = root.querySelector('form');
  if (!(form instanceof HTMLFormElement)) {
    throw new Error('no form on the page');
  }
  form.dispatchEvent(new Event('submit'));
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

function changeSelect(root: HTMLElement, selector: string, value: string): void {
  const select = root.querySelector(selector);
  if (!(select instanceof HTMLSelectElement)) {
    throw new Error(`no select matching ${selector}`);
  }
  select.value = value;
  select.dispatchEvent(new Event('change'));
}

describe('Offerings', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
    TestBed.configureTestingModule({
      imports: [Offerings],
      providers: [{ provide: AuthService, useValue: authStub() }],
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  /**
   * Queues the providers/models/offerings triple plus one price GET per
   * offering (`null` prices answer 404, like an unpriced offering does).
   */
  function queueReload(
    offerings: Offering[],
    prices: (Price | null)[] = offerings.map((offering) =>
      priceFixture({ offering_id: offering.id }),
    ),
  ): void {
    fetchMock.respond(jsonResponse(200, [provider]));
    fetchMock.respond(jsonResponse(200, [model]));
    fetchMock.respond(jsonResponse(200, offerings));
    for (const price of prices) {
      if (price === null) {
        fetchMock.respond(errorResponse(404, 'not_found', 'price'));
      } else {
        fetchMock.respond(jsonResponse(200, price));
      }
    }
  }

  async function create(
    offerings: Offering[],
    providers: Provider[] = [provider],
    models: Model[] = [model],
    prices: (Price | null)[] = offerings.map((offering) =>
      priceFixture({ offering_id: offering.id }),
    ),
  ): Promise<ComponentFixture<Offerings>> {
    fetchMock.respond(jsonResponse(200, providers));
    fetchMock.respond(jsonResponse(200, models));
    fetchMock.respond(jsonResponse(200, offerings));
    for (const price of prices) {
      if (price === null) {
        fetchMock.respond(errorResponse(404, 'not_found', 'price'));
      } else {
        fetchMock.respond(jsonResponse(200, price));
      }
    }
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Offerings);
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  it('lists offerings with the price source badge and fetched age', async () => {
    const fixture = await create([offeringFixture()]);
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('openrouter');
    expect(root.textContent).toContain('openai/gpt-5');
    expect(root.textContent).toContain('in $1.25/M');
    expect(root.textContent).toContain('out $5.00/M');
    // Synced prices render the neutral badge and no manual styling.
    expect(root.querySelector('.badge.synced')?.textContent).toContain('openrouter');
    expect(root.querySelector('tr.price-manual')).toBeNull();
  });

  it('shows an unpriced offering without a price summary', async () => {
    const fixture = await create([offeringFixture()], undefined, undefined, [null]);
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('none');
    expect(root.querySelector('tr.price-manual')).toBeNull();
  });

  it('saves a manual price and marks the row as manual', async () => {
    const fixture = await create([offeringFixture()]);
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Price').click();
    fixture.detectChanges();
    typeInto(root, '#price-input', '3.5');
    typeInto(root, '#price-fee', '0.01');
    fetchMock.respond(
      jsonResponse(
        200,
        priceFixture({
          input_per_m: 3.5,
          request_fee: 0.01,
          source: 'manual',
          updated_by: 'u1',
        }),
      ),
    );
    queueReload(
      [offeringFixture()],
      [
        priceFixture({
          input_per_m: 3.5,
          request_fee: 0.01,
          source: 'manual',
          updated_by: 'u1',
        }),
      ],
    );
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[4];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/offerings/o1/price');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    // The server records `source='manual'` itself; the body is amounts only.
    expect(call.body).toEqual({
      input_per_m: 3.5,
      output_per_m: 5,
      cache_read_per_m: null,
      cache_write_per_m: null,
      reasoning_per_m: null,
      request_fee: 0.01,
    });
    expect(call.body).not.toHaveProperty('source');

    // The manual price is visually distinct from the synced one.
    const row = root.querySelector('tr.price-manual');
    expect(row).not.toBeNull();
    expect(row?.querySelector('.badge.manual')?.textContent).toContain('manual');
    expect(root.textContent).toContain('Price saved as manual');
  });

  it('rejects negative prices before calling the API', async () => {
    const fixture = await create([offeringFixture()]);
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Price').click();
    fixture.detectChanges();
    typeInto(root, '#price-input', '-1');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(4);
    expect(root.textContent).toContain('Cannot be negative.');
  });

  it('filters offerings by provider and by model', async () => {
    const secondProvider: Provider = { ...provider, id: 'p2', name: 'azure' };
    const secondModel: Model = { ...model, id: 'm2', canonical_name: 'anthropic/claude' };
    const fixture = await create(
      [
        offeringFixture(),
        offeringFixture({
          id: 'o2',
          provider_id: 'p2',
          upstream_model_id: 'azure/gpt-5',
        }),
      ],
      [provider, secondProvider],
      [model, secondModel],
      [priceFixture(), null],
    );
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelectorAll('tbody tr')).toHaveLength(2);

    changeSelect(root, '#filter-provider', 'p2');
    fixture.detectChanges();
    expect(root.querySelectorAll('tbody tr')).toHaveLength(1);
    expect(root.textContent).toContain('azure/gpt-5');

    changeSelect(root, '#filter-provider', '');
    changeSelect(root, '#filter-model', 'm2');
    fixture.detectChanges();
    expect(root.querySelectorAll('tbody tr')).toHaveLength(0);
  });

  it('creates an offering with the selected provider and model', async () => {
    const fixture = await create([]);
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New offering').click();
    fixture.detectChanges();
    typeInto(root, '#offering-upstream', ' openai/gpt-5 ');
    typeInto(root, '#offering-context', '200000');
    fetchMock.respond(jsonResponse(201, offeringFixture()));
    queueReload([offeringFixture()]);
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[3];
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
  });

  it('locks provider and model while editing and omits untouched headers', async () => {
    const fixture = await create([offeringFixture({ header_names: ['x-route'] })]);
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Edit').click();
    fixture.detectChanges();

    // An offering cannot move between providers or models over the wire.
    const providerSelect = root.querySelector('#offering-provider');
    expect(providerSelect?.hasAttribute('disabled')).toBe(true);
    const modelSelect = root.querySelector('#offering-model');
    expect(modelSelect?.hasAttribute('disabled')).toBe(true);

    typeInto(root, '#offering-upstream', 'openai/gpt-5-mini');
    fetchMock.respond(
      jsonResponse(200, offeringFixture({ upstream_model_id: 'openai/gpt-5-mini' })),
    );
    queueReload([offeringFixture({ upstream_model_id: 'openai/gpt-5-mini' })]);
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    // The write-only header values are not part of the patch at all.
    expect(fetchMock.requests[4].body).toEqual({ upstream_model_id: 'openai/gpt-5-mini' });
    expect(fetchMock.requests[4].body).not.toHaveProperty('headers');
  });

  it('replaces the headers wholesale when the rows are edited', async () => {
    const fixture = await create([offeringFixture({ header_names: ['x-route'] })]);
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Edit').click();
    fixture.detectChanges();
    typeInto(root, '[aria-label="Header value 1"]', 'fast');
    fixture.detectChanges();
    fetchMock.respond(jsonResponse(200, offeringFixture({ header_names: ['x-route'] })));
    queueReload([offeringFixture({ header_names: ['x-route'] })]);
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[4].body).toEqual({ headers: { 'x-route': 'fast' } });
  });

  it('deletes an offering only after confirmation', async () => {
    const fixture = await create([offeringFixture()]);
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Delete').click();
    fixture.detectChanges();
    expect(fetchMock.requests).toHaveLength(4);

    fetchMock.respond(noContentResponse());
    queueReload([]);
    buttonByText(root, 'Confirm delete').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[4].method).toBe('DELETE');
    expect(fetchMock.requests[4].url).toBe('/api/offerings/o1');
    expect(root.textContent).toContain('No offerings yet');
  });
});
