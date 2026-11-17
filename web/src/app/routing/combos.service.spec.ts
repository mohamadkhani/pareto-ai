import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { FetchMock, jsonResponse, noContentResponse } from '../../testing/fetch-mock';
import { AuthService } from '../auth/auth.service';
import { CombosService } from './combos.service';
import type { Combo, ComboInput } from './combos.types';

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
    members: [{ offering_id: 'off1', weight: 1 }],
    enabled: true,
    rr_cursor: 0,
    stickiness: { enabled: false, ttl_seconds: 1800, key_source: 'auto' },
    ...overrides,
  };
}

function inputFixture(overrides: Partial<ComboInput> = {}): ComboInput {
  return {
    name: 'shared-fast',
    strategy: 'weighted',
    weights: { preset: 'cheapest' },
    members: [{ offering_id: 'off1' }],
    enabled: true,
    ...overrides,
  };
}

describe('CombosService', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
    TestBed.configureTestingModule({
      providers: [
        { provide: AuthService, useValue: { csrfToken: signal<string | null>('csrf-1') } },
      ],
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('lists every combo', async () => {
    fetchMock.respond(jsonResponse(200, [comboFixture()]));
    const service = TestBed.inject(CombosService);

    const combos = await service.list();

    expect(fetchMock.requests[0].url).toBe('/api/combos');
    expect(combos).toHaveLength(1);
  });

  it('creates a combo with the CSRF token and the wire body', async () => {
    fetchMock.respond(jsonResponse(201, comboFixture()));
    const service = TestBed.inject(CombosService);

    await service.create(inputFixture());

    const call = fetchMock.requests[0];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/combos');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      name: 'shared-fast',
      strategy: 'weighted',
      weights: { preset: 'cheapest' },
      members: [{ offering_id: 'off1' }],
      enabled: true,
    });
  });

  it('updates a combo by id', async () => {
    fetchMock.respond(jsonResponse(200, comboFixture()));
    const service = TestBed.inject(CombosService);

    await service.update('c1', inputFixture({ strategy: 'ordered', weights: null }));

    const call = fetchMock.requests[0];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/combos/c1');
    expect(call.body).toMatchObject({ strategy: 'ordered', weights: null });
  });

  it('deletes a combo', async () => {
    fetchMock.respond(noContentResponse());
    const service = TestBed.inject(CombosService);

    await service.remove('c1');

    const call = fetchMock.requests[0];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/combos/c1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
  });

  it('previews a draft combo without a CSRF token', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        profile: { profile_id: 'mix-code-agent', profile_name: 'code-agent' },
        scored: 0,
        candidates: [],
        excluded: [],
        sticky: null,
      }),
    );
    const service = TestBed.inject(CombosService);

    const preview = await service.preview(inputFixture(), 10);

    const call = fetchMock.requests[0];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/combos/preview');
    expect(call.headers.get('X-CSRF-Token')).toBeNull();
    expect(call.body).toMatchObject({ name: 'shared-fast', limit: 10 });
    expect(preview.profile.profile_name).toBe('code-agent');
  });

  it('previews a saved combo with an optional session key', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        profile: { profile_id: 'mix-chat', profile_name: 'chat' },
        scored: 0,
        candidates: [],
        excluded: [],
        sticky: {
          enabled: true,
          key_source: 'header',
          session_key: 's-1',
          pinned: 'off1',
          age_s: 42,
        },
      }),
    );
    const service = TestBed.inject(CombosService);

    const preview = await service.previewSaved('c1', 's-1');

    expect(fetchMock.requests[0].url).toBe('/api/combos/c1/preview?limit=50&session_key=s-1');
    expect(preview.sticky?.pinned).toBe('off1');
  });
});
