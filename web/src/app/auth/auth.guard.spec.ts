import { TestBed } from '@angular/core/testing';
import {
  ActivatedRouteSnapshot,
  Router,
  RouterStateSnapshot,
  UrlTree,
  provideRouter,
} from '@angular/router';
import { FetchMock, errorResponse, jsonResponse } from '../../testing/fetch-mock';
import { authGuard, loginGuard } from './auth.guard';

const session = {
  user: {
    id: 'u1',
    email: 'admin@example.com',
    display_name: 'Admin',
    role: 'admin' as const,
  },
  csrf_token: 'csrf-1',
};

function runGuard(url: string): ReturnType<typeof authGuard> {
  return TestBed.runInInjectionContext(() =>
    authGuard({} as ActivatedRouteSnapshot, { url } as RouterStateSnapshot),
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

  it('loginGuard keeps anonymous users on /login', async () => {
    fetchMock.respond(errorResponse(401, 'unauthorized', 'Sign in required.'));
    const router = TestBed.inject(Router);
    const createUrlTree = vi.spyOn(router, 'createUrlTree');

    await expect(runLoginGuard()).resolves.toBe(true);
    expect(createUrlTree).not.toHaveBeenCalled();
  });
});
