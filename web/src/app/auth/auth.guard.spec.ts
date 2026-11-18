import { TestBed } from '@angular/core/testing';
import {
  ActivatedRouteSnapshot,
  Router,
  RouterStateSnapshot,
  UrlTree,
  provideRouter,
} from '@angular/router';
import { FetchMock, errorResponse, jsonResponse } from '../../testing/fetch-mock';
import { authGuard, loginGuard, setupGuard, setupPageGuard } from './auth.guard';

const session = {
  user: {
    id: 'u1',
    email: 'admin@example.com',
    display_name: 'Admin',
    role: 'admin' as const,
    must_change_password: false,
  },
  csrf_token: 'csrf-1',
};

const setupStatus = {
  mode: 'setup' as const,
  bind: '127.0.0.1:8081',
  is_local: true,
  setup_required: true,
  warning: null,
  tls: { enabled: false, domain: null, reason: 'local', error: null, expires_at: null },
  master_key_source: 'file' as const,
};

const rotationSession = {
  user: { ...session.user, must_change_password: true },
  csrf_token: 'csrf-1',
};

function runGuard(url: string): ReturnType<typeof authGuard> {
  return TestBed.runInInjectionContext(() =>
    authGuard({} as ActivatedRouteSnapshot, { url } as RouterStateSnapshot),
  );
}

function runSetupGuard(): ReturnType<typeof setupGuard> {
  return TestBed.runInInjectionContext(() =>
    setupGuard({} as ActivatedRouteSnapshot, { url: '/' } as RouterStateSnapshot),
  );
}

function runSetupPageGuard(): ReturnType<typeof setupPageGuard> {
  return TestBed.runInInjectionContext(() =>
    setupPageGuard({} as ActivatedRouteSnapshot, { url: '/setup' } as RouterStateSnapshot),
  );
}

function runLoginGuard(): ReturnType<typeof loginGuard> {
  return TestBed.runInInjectionContext(() =>
    loginGuard({} as ActivatedRouteSnapshot, { url: '/login' } as RouterStateSnapshot),
  );
}

describe('authGuard', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
    TestBed.configureTestingModule({ providers: [provideRouter([])] });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('redirects unauthenticated users to /login and preserves the return URL', async () => {
    fetchMock.respond(errorResponse(401, 'unauthorized', 'Sign in required.'));

    const result = await runGuard('/audit');

    expect(result).toBeInstanceOf(UrlTree);
    if (result instanceof UrlTree) {
      expect(result.toString()).toContain('/login');
      expect(result.queryParams['returnUrl']).toBe('/audit');
    }
  });

  it('lets authenticated users through', async () => {
    fetchMock.respond(jsonResponse(200, session));

    await expect(runGuard('/audit')).resolves.toBe(true);
    expect(fetchMock.requests.map((call) => call.url)).toContain('/api/auth/me');
  });

  it('loginGuard redirects signed-in users away from /login', async () => {
    fetchMock.respond(jsonResponse(200, session));

    const result = await runLoginGuard();

    expect(result).toBeInstanceOf(UrlTree);
    if (result instanceof UrlTree) {
      expect(result.toString()).toBe('/');
    }
  });

  it('holds accounts flagged for rotation on /change-password', async () => {
    fetchMock.respond(jsonResponse(200, rotationSession));

    const result = await runGuard('/audit');

    expect(result).toBeInstanceOf(UrlTree);
    if (result instanceof UrlTree) {
      expect(result.toString()).toBe('/change-password');
    }
  });

  it('lets flagged accounts reach /change-password', async () => {
    fetchMock.respond(jsonResponse(200, rotationSession));

    await expect(runGuard('/change-password')).resolves.toBe(true);
  });

  it('keeps accounts without a pending rotation off /change-password', async () => {
    fetchMock.respond(jsonResponse(200, session));

    const result = await runGuard('/change-password');

    expect(result).toBeInstanceOf(UrlTree);
    if (result instanceof UrlTree) {
      expect(result.toString()).toBe('/');
    }
  });

  it('loginGuard keeps anonymous users on /login', async () => {
    fetchMock.respond(errorResponse(401, 'unauthorized', 'Sign in required.'));
    const router = TestBed.inject(Router);
    const createUrlTree = vi.spyOn(router, 'createUrlTree');

    await expect(runLoginGuard()).resolves.toBe(true);
    expect(createUrlTree).not.toHaveBeenCalled();
  });

  describe('setup guards', () => {
    it('sends the first visit to the wizard while no account exists', async () => {
      fetchMock.respond(jsonResponse(200, setupStatus));

      const result = await runSetupGuard();

      expect(result).toBeInstanceOf(UrlTree);
      if (result instanceof UrlTree) {
        expect(result.toString()).toBe('/setup');
      }
    });

    it('lets a protected or already-skipped gateway through to the console', async () => {
      fetchMock.respond(
        jsonResponse(200, { ...setupStatus, mode: 'unprotected', setup_required: false }),
      );

      await expect(runSetupGuard()).resolves.toBe(true);
    });

    it('falls through when the status cannot be read', async () => {
      fetchMock.respond(errorResponse(500, 'internal', 'boom'));

      await expect(runSetupGuard()).resolves.toBe(true);
    });

    it('keeps /setup reachable while setup is required', async () => {
      fetchMock.respond(jsonResponse(200, setupStatus));

      await expect(runSetupPageGuard()).resolves.toBe(true);
    });

    it('moves /setup to /login once protected', async () => {
      fetchMock.respond(
        jsonResponse(200, { ...setupStatus, mode: 'protected', setup_required: false }),
      );

      const result = await runSetupPageGuard();

      expect(result).toBeInstanceOf(UrlTree);
      if (result instanceof UrlTree) {
        expect(result.toString()).toBe('/login');
      }
    });

    it('moves /setup to the console when setup was skipped', async () => {
      fetchMock.respond(
        jsonResponse(200, { ...setupStatus, mode: 'unprotected', setup_required: false }),
      );

      const result = await runSetupPageGuard();

      expect(result).toBeInstanceOf(UrlTree);
      if (result instanceof UrlTree) {
        expect(result.toString()).toBe('/');
      }
    });
  });
});
