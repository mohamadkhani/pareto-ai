import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { FetchMock, jsonResponse } from '../../testing/fetch-mock';
import { AuthService } from '../auth/auth.service';
import { UsersService } from './users.service';
import type { UserView } from './users.types';

function userFixture(overrides: Partial<UserView> = {}): UserView {
  return {
    id: 'u1',
    email: 'admin@example.com',
    display_name: 'Admin',
    role: 'admin',
    is_active: true,
    must_change_password: false,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    last_login_at: null,
    ...overrides,
  };
}

describe('UsersService', () => {
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

  it('lists every account without a CSRF header', async () => {
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const service = TestBed.inject(UsersService);

    const users = await service.list();

    expect(users).toEqual([userFixture()]);
    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/users');
    expect(call.method).toBe('GET');
    expect(call.headers.get('X-CSRF-Token')).toBeNull();
  });

  it('creates an account with the CSRF header and the rotation default', async () => {
    fetchMock.respond(jsonResponse(201, userFixture({ must_change_password: true })));
    const service = TestBed.inject(UsersService);

    await service.create({
      email: 'admin@example.com',
      display_name: 'Admin',
      role: 'admin',
      password: 'initial-secret-1',
      must_change_password: true,
    });

    const call = fetchMock.requests[0];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/users');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      email: 'admin@example.com',
      display_name: 'Admin',
      role: 'admin',
      password: 'initial-secret-1',
      must_change_password: true,
    });
  });

  it('sends only the changed fields on update (null keeps)', async () => {
    fetchMock.respond(jsonResponse(200, userFixture({ role: 'user' })));
    const service = TestBed.inject(UsersService);

    await service.update('u1', { role: 'user' });

    const call = fetchMock.requests[0];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/users/u1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ role: 'user' });
  });

  it('carries a password reset in the patch, forcing rotation', async () => {
    fetchMock.respond(jsonResponse(200, userFixture({ must_change_password: true })));
    const service = TestBed.inject(UsersService);

    await service.update('u1', { password: 'reset-secret-1' });

    expect(fetchMock.requests[0].body).toEqual({ password: 'reset-secret-1' });
  });
});
