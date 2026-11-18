import { Component } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { FetchMock, noContentResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import { DEV_MODE } from '../dev-mode';
import { SetupService, type SetupStatus } from '../setup/setup.service';
import { ThemeService } from '../ui/theme.service';
import { Shell } from './shell';

@Component({ selector: 'app-blank', template: '' })
class Blank {}

const protectedStatus: SetupStatus = {
  mode: 'protected',
  bind: '127.0.0.1:8081',
  is_local: true,
  setup_required: false,
  warning: null,
  tls: { enabled: false, domain: null },
  master_key_source: 'file',
};

const admin: User = {
  id: 'u1',
  email: 'admin@example.com',
  display_name: 'Admin',
  role: 'admin',
  must_change_password: false,
};

const selfService: User = {
  id: 'u2',
  email: 'user@example.com',
  display_name: 'User',
  role: 'user',
  must_change_password: false,
};

describe('Shell', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(user: User, devMode = true): Promise<ComponentFixture<Shell>> {
    TestBed.configureTestingModule({
      imports: [Shell, Blank],
      providers: [
        { provide: DEV_MODE, useValue: devMode },
        provideRouter([
          { path: 'audit', component: Blank },
          { path: 'keys', component: Blank },
          { path: 'usage', component: Blank },
        ]),
      ],
    });
    await TestBed.compileComponents();
    const auth = TestBed.inject(AuthService);
    auth.user.set(user);
    auth.status.set('authenticated');
    // Pre-set the status so the shell does not spend the queued response.
    TestBed.inject(SetupService).status.set(protectedStatus);
    const fixture = TestBed.createComponent(Shell);
    fixture.detectChanges();
    return fixture;
  }

  it('groups the admin links into labelled sections', async () => {
    const fixture = await create(admin);
    const root = fixture.nativeElement as HTMLElement;
    const labels = Array.from(root.querySelectorAll('.nav-desktop .nav-section__label')).map(
      (label) => label.textContent?.trim(),
    );

    expect(labels).toEqual([
      'Overview',
      'Catalog',
      'Routing',
      'Access',
      'Observability',
      'Account',
      'Development',
    ]);
    const links = Array.from(root.querySelectorAll('.nav-desktop .nav-section__links a')).map(
      (link) => link.textContent?.trim(),
    );
    expect(links).toContain('Providers');
    expect(links).toContain('Models');
    expect(links).toContain('Combos');
    expect(links).toContain('API keys');
    expect(links).toContain('Audit log');
    expect(links).toContain('Settings');
    expect(links).toContain('Styleguide');
    expect(links).not.toContain('Offerings');
    expect(links).not.toContain('Benchmarks');
    expect(links).not.toContain('Price sync');
  });

  it('hides the development-only styleguide in a production build', async () => {
    const fixture = await create(admin, false);
    const root = fixture.nativeElement as HTMLElement;
    const labels = Array.from(root.querySelectorAll('.nav-desktop .nav-section__label')).map(
      (label) => label.textContent?.trim(),
    );
    const hrefs = Array.from(root.querySelectorAll('.nav-desktop a')).map((link) =>
      link.getAttribute('href'),
    );

    expect(labels).not.toContain('Development');
    expect(hrefs).not.toContain('/styleguide');
  });

  it('never shows the setup wizard as a navigation entry', async () => {
    const fixture = await create(admin);
    const hrefs = Array.from(
      (fixture.nativeElement as HTMLElement).querySelectorAll('.nav-desktop a'),
    ).map((link) => link.getAttribute('href'));

    expect(hrefs).toContain('/settings');
    expect(hrefs).not.toContain('/setup');
  });

  it('shows the calm local banner while unprotected on this machine', async () => {
    const fixture = await create(admin);
    TestBed.inject(SetupService).status.set({
      ...protectedStatus,
      mode: 'unprotected',
      is_local: true,
      warning: 'Running without a password on this machine.',
    });
    fixture.detectChanges();
    const banner = (fixture.nativeElement as HTMLElement).querySelector('ui-banner');

    expect(banner?.textContent).toContain('Running without a password on this machine.');
    expect(banner?.classList).toContain('tone-info');
    expect(banner?.querySelector('button')?.textContent).toContain('Open Settings');
  });

  it('shows the red banner when a network peer is unprotected', async () => {
    const fixture = await create(admin);
    TestBed.inject(SetupService).status.set({
      ...protectedStatus,
      mode: 'unprotected',
      is_local: false,
      warning:
        'This gateway is reachable from the network without a password. Set one in Settings.',
    });
    fixture.detectChanges();
    const banner = (fixture.nativeElement as HTMLElement).querySelector('ui-banner');

    expect(banner?.textContent).toContain('reachable from the network without a password');
    expect(banner?.classList).toContain('tone-danger');
  });

  it('hides the banner once the gateway is protected', async () => {
    const fixture = await create(admin);

    expect((fixture.nativeElement as HTMLElement).querySelector('ui-banner')).toBeNull();
  });

  it('does not link to the retired offerings, benchmarks, or sync routes', async () => {
    const fixture = await create(admin);
    const hrefs = Array.from(
      (fixture.nativeElement as HTMLElement).querySelectorAll('.nav-desktop a'),
    ).map((link) => link.getAttribute('href'));

    expect(hrefs).not.toContain('/offerings');
    expect(hrefs).not.toContain('/benchmarks');
    expect(hrefs).not.toContain('/sync');
  });

  it('shows only the self-service subset to a user account', async () => {
    const fixture = await create(selfService);
    const root = fixture.nativeElement as HTMLElement;
    const labels = Array.from(root.querySelectorAll('.nav-desktop .nav-section__label')).map(
      (label) => label.textContent?.trim(),
    );
    const links = Array.from(root.querySelectorAll('.nav-desktop .nav-section__links a')).map(
      (link) => link.textContent?.trim(),
    );

    expect(labels).toEqual(['Access', 'Observability']);
    expect(links).toEqual(['API keys', 'Limits', 'Usage']);
  });

  it('marks the active route with aria-current', async () => {
    const fixture = await create(admin);
    const root = fixture.nativeElement as HTMLElement;

    await TestBed.inject(Router).navigateByUrl('/audit');
    fixture.detectChanges();

    const active = root.querySelector('.nav-desktop a[href="/audit"]');
    expect(active?.getAttribute('aria-current')).toBe('page');
  });

  it('truncates the email but keeps it available as a tooltip', async () => {
    const fixture = await create(admin);
    const email = (fixture.nativeElement as HTMLElement).querySelector('.user__email');

    expect(email?.getAttribute('title')).toBe('admin@example.com');
    expect(email?.textContent).toContain('admin@example.com');
  });

  it('opens the compact menu, closes it on Escape, and returns focus', async () => {
    const fixture = await create(admin);
    const root = fixture.nativeElement as HTMLElement;
    const toggle = root.querySelector<HTMLButtonElement>('.menu-toggle');
    const panel = root.querySelector('#primary-navigation-mobile');
    if (toggle === null || panel === null) {
      throw new Error('menu toggle or panel missing');
    }

    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    toggle.click();
    fixture.detectChanges();
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    expect(panel.classList).toContain('nav-mobile--open');

    const focus = vi.spyOn(toggle, 'focus');
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    fixture.detectChanges();
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(panel.classList).not.toContain('nav-mobile--open');
    expect(focus).toHaveBeenCalled();
  });

  it('reflects the theme mode in the toggle label', async () => {
    const fixture = await create(admin);
    const theme = TestBed.inject(ThemeService);
    theme.setMode('dark');
    fixture.detectChanges();

    const toggle = (fixture.nativeElement as HTMLElement).querySelector('ui-icon-button button');
    expect(toggle?.getAttribute('aria-label')).toContain('Theme: dark');
  });

  it('signs out and returns to the login screen', async () => {
    fetchMock.respond(noContentResponse());
    const fixture = await create(admin);
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    const root = fixture.nativeElement as HTMLElement;
    const signOut = Array.from(root.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Sign out'),
    );
    if (signOut === undefined) {
      throw new Error('sign-out button missing');
    }

    signOut.click();
    await fixture.whenStable();

    expect(fetchMock.requests.map((call) => call.url)).toContain('/api/auth/logout');
    expect(navigate).toHaveBeenCalledWith('/login');
  });
});
