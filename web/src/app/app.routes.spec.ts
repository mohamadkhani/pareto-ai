import { TestBed } from '@angular/core/testing';
import { AuthService, type User } from './auth/auth.service';
import { landingRedirect, routes } from './app.routes';

const admin: User = {
  id: 'u1',
  email: 'admin@example.com',
  display_name: 'Admin',
  role: 'admin',
  must_change_password: false,
};

const selfService: User = { ...admin, id: 'u2', email: 'user@example.com', role: 'user' };

describe('landingRedirect', () => {
  it('starts admins on the dashboard', () => {
    const auth = TestBed.inject(AuthService);
    auth.user.set(admin);

    expect(TestBed.runInInjectionContext(landingRedirect)).toBe('dashboard');
  });

  it('starts a self-service account on usage', () => {
    const auth = TestBed.inject(AuthService);
    auth.user.set(selfService);

    expect(TestBed.runInInjectionContext(landingRedirect)).toBe('usage');
  });

  it('is wired as the root redirect inside the shell', () => {
    const root = routes.find((route) => route.path === '');
    const child = root?.children?.find((route) => route.path === '' && route.pathMatch === 'full');

    expect(child?.redirectTo).toBe(landingRedirect);
  });

  it('keeps the merged Models page and retires offerings, benchmarks, and price sync', () => {
    const root = routes.find((route) => route.path === '');
    const paths = root?.children?.map((route) => route.path) ?? [];

    expect(paths).toContain('dashboard');
    expect(paths).toContain('models');
    expect(paths).toContain('providers');
    expect(paths).toContain('combos');
    expect(paths).not.toContain('offerings');
    expect(paths).not.toContain('benchmarks');
    expect(paths).not.toContain('sync');
  });
});
