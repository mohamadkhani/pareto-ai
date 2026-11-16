import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { FetchMock, jsonResponse, noContentResponse } from '../../testing/fetch-mock';
import { AuthService } from '../auth/auth.service';
import { KeysService } from './keys.service';
import type { ApiKeyView, CreatedKey } from './keys.types';

function keyFixture(overrides: Partial<ApiKeyView> = {}): ApiKeyView {
  return {
    id: 'k1',
    user_id: 'u1',
    name: 'laptop',
    prefix: 'pk_live_ab12',
    scopes: null,
    expires_at: null,
    revoked_at: null,
    last_used_at: null,
    rate_limit_rpm: 60,
    rate_limit_tpm: null,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

describe('KeysService', () => {
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

  it('lists one account’s keys without a CSRF header', async () => {
    fetchMock.respond(jsonResponse(200, [keyFixture()]));
    const service = TestBed.inject(KeysService);

    const keys = await service.list('u1');

    expect(keys).toEqual([keyFixture()]);
    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/users/u1/keys');
    expect(call.method).toBe('GET');
    expect(call.headers.get('X-CSRF-Token')).toBeNull();
  });

  it('returns the plaintext key exactly once and never inside the view', async () => {
    const created: CreatedKey = { key: 'pk_live_full_secret', api_key: keyFixture() };
    fetchMock.respond(jsonResponse(201, created));
    const service = TestBed.inject(KeysService);

    const result = await service.create('u1', { name: 'laptop' });

    // The plaintext exists only on the creation response...
    expect(result.key).toBe('pk_live_full_secret');
    // ...and never as a field of the key view.
    expect(Object.keys(result.api_key)).not.toContain('key');
    const call = fetchMock.requests[0];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/users/u1/keys');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ name: 'laptop' });
  });

  it('sends only the provided fields on update (null keeps)', async () => {
    fetchMock.respond(jsonResponse(200, keyFixture({ name: 'desktop' })));
    const service = TestBed.inject(KeysService);

    await service.update('k1', { name: 'desktop' });

    const call = fetchMock.requests[0];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/keys/k1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ name: 'desktop' });
    expect(call.body).not.toHaveProperty('expires_at');
    expect(call.body).not.toHaveProperty('rate_limit_rpm');
  });

  it('revokes a key with a CSRF-protected DELETE', async () => {
    fetchMock.respond(noContentResponse());
    const service = TestBed.inject(KeysService);

    await service.revoke('k1');

    const call = fetchMock.requests[0];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/keys/k1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
  });
});
