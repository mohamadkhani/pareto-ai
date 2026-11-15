import { TestBed } from '@angular/core/testing';
import { ApiError } from '../api';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { AuthService } from './auth.service';

const session = {
  user: {
    id: 'u1',
    email: 'admin@example.com',
    display_name: 'Admin',
    role: 'admin' as const,
  },
  csrf_token: 'csrf-1',
};

describe('AuthService', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('login sends the credentials with cookies and stores the user and CSRF token', async () => {
    fetchMock.respond(jsonResponse(200, session));
    const auth = TestBed.inject(AuthService);

    await auth.login('admin@example.com', 'change-me-please');

    expect(fetchMock.requests).toHaveLength(1);
    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/auth/login');
    expect(call.method).toBe('POST');
    expect(call.credentials).toBe('include');
    expect(call.body).toEqual({ email: 'admin@example.com', password: 'change-me-please' });
    expect(auth.user()).toEqual(session.user);
    expect(auth.csrfToken()).toBe('csrf-1');
    expect(auth.status()).toBe('authenticated');
  });

  it('login surfaces the invalid_credentials code and message', async () => {
    fetchMock.respond(errorResponse(401, 'invalid_credentials', 'Invalid email or password.'));
    const auth = TestBed.inject(AuthService);

    const error: unknown = await auth
      .login('admin@example.com', 'wrong')
      .catch((err: unknown) => err);

    expect(error).toBeInstanceOf(ApiError);
    if (error instanceof ApiError) {
      expect(error.code).toBe('invalid_credentials');
      expect(error.message).toBe('Invalid email or password.');
    }
    expect(auth.user()).toBeNull();
  });

  it('login surfaces login_locked with the retry seconds from Retry-After', async () => {
    fetchMock.respond(
      errorResponse(429, 'login_locked', 'Too many failed attempts.', { 'Retry-After': '42' }),
    );
    const auth = TestBed.inject(AuthService);

    const error: unknown = await auth
      .login('admin@example.com', 'wrong')
      .catch((err: unknown) => err);

    expect(error).toBeInstanceOf(ApiError);
    if (error instanceof ApiError) {
      expect(error.code).toBe('login_locked');
      expect(error.retryAfterSeconds).toBe(42);
    }
  });

  it('logout sends the CSRF header and clears the session', async () => {
    fetchMock.respond(jsonResponse(200, session));
    fetchMock.respond(noContentResponse());
    const auth = TestBed.inject(AuthService);

    await auth.login('admin@example.com', 'change-me-please');
    await auth.logout();

    expect(fetchMock.requests).toHaveLength(2);
    const call = fetchMock.requests[1];
    expect(call.url).toBe('/api/auth/logout');
    expect(call.method).toBe('POST');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(auth.user()).toBeNull();
    expect(auth.csrfToken()).toBeNull();
    expect(auth.status()).toBe('anonymous');
  });

  it('loadMe restores the session and shares one request between callers', async () => {
    fetchMock.respond(jsonResponse(200, session));
    const auth = TestBed.inject(AuthService);

    const [first, second] = await Promise.all([auth.loadMe(), auth.loadMe()]);

    expect(first).toBe(true);
    expect(second).toBe(true);
    expect(fetchMock.requests).toHaveLength(1);
    expect(fetchMock.requests[0].url).toBe('/api/auth/me');
    expect(auth.csrfToken()).toBe('csrf-1');
  });

  it('loadMe resolves false when there is no session', async () => {
    fetchMock.respond(errorResponse(401, 'unauthorized', 'Sign in required.'));
    const auth = TestBed.inject(AuthService);

    await expect(auth.loadMe()).resolves.toBe(false);
    expect(auth.status()).toBe('anonymous');
  });
});
