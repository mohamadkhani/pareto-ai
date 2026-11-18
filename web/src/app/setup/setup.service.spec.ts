import { TestBed } from '@angular/core/testing';
import { FetchMock, jsonResponse } from '../../testing/fetch-mock';
import { AuthService } from '../auth/auth.service';
import { SetupService, type SetupStatus } from './setup.service';

const setupStatus: SetupStatus = {
  mode: 'setup',
  bind: '127.0.0.1:8081',
  is_local: true,
  setup_required: true,
  warning: 'This gateway is not protected yet. Finish setup to set an admin password.',
  tls: { enabled: false, domain: null, reason: 'local', error: null, expires_at: null },
  master_key_source: 'file',
};

const unprotectedStatus: SetupStatus = {
  ...setupStatus,
  mode: 'unprotected',
  setup_required: false,
  warning: 'Running without a password on this machine.',
};

const protectedStatus: SetupStatus = {
  ...setupStatus,
  mode: 'protected',
  setup_required: false,
  warning: null,
};

const session = {
  user: {
    id: 'u1',
    email: 'boss@example.com',
    display_name: 'boss@example.com',
    role: 'admin' as const,
    must_change_password: false,
  },
  csrf_token: 'csrf-1',
};

describe('SetupService', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('loads the status once and caches it until forced', async () => {
    fetchMock.respond(jsonResponse(200, setupStatus));
    const setup = TestBed.inject(SetupService);

    const first = await setup.load();
    expect(first.mode).toBe('setup');
    expect(setup.status()?.setup_required).toBe(true);

    fetchMock.respond(jsonResponse(200, protectedStatus));
    await setup.load();
    expect(fetchMock.requests).toHaveLength(1);

    const forced = await setup.load(true);
    expect(forced.mode).toBe('protected');
    expect(fetchMock.requests).toHaveLength(2);
  });

  it('shares one request between concurrent loads', async () => {
    fetchMock.respond(jsonResponse(200, setupStatus));
    const setup = TestBed.inject(SetupService);

    const [a, b] = await Promise.all([setup.load(), setup.load()]);

    expect(a).toBe(b);
    expect(fetchMock.requests).toHaveLength(1);
  });

  it('createAdmin posts the first account, adopts the session, and refreshes the status', async () => {
    const auth = TestBed.inject(AuthService);
    fetchMock.respond(jsonResponse(200, session));
    fetchMock.respond(jsonResponse(200, protectedStatus));
    const setup = TestBed.inject(SetupService);

    await setup.createAdmin('boss@example.com', 'first-admin-password');

    expect(fetchMock.requests[0].url).toBe('/api/setup/admin');
    expect(fetchMock.requests[0].method).toBe('POST');
    expect(fetchMock.requests[0].credentials).toBe('include');
    expect(fetchMock.requests[0].body).toEqual({
      email: 'boss@example.com',
      password: 'first-admin-password',
    });
    expect(auth.user()?.email).toBe('boss@example.com');
    expect(auth.csrfToken()).toBe('csrf-1');
    expect(setup.status()?.mode).toBe('protected');
  });

  it('skip posts the confirmation and stores the unprotected mode', async () => {
    fetchMock.respond(jsonResponse(200, unprotectedStatus));
    const setup = TestBed.inject(SetupService);

    await setup.skip();

    expect(fetchMock.requests[0].url).toBe('/api/setup/skip');
    expect(fetchMock.requests[0].body).toEqual({ confirm: true });
    expect(setup.status()?.mode).toBe('unprotected');
    // The cached session state is dropped so the guards re-read `/api/auth/me`
    // (the unprotected gateway answers with the local operator).
    expect(TestBed.inject(AuthService).status()).toBe('unknown');
  });
});
