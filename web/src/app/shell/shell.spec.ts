import { Component } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { FetchMock, noContentResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import { ThemeService } from '../ui/theme.service';
import { Shell } from './shell';

@Component({ selector: 'app-blank', template: '' })
class Blank {}

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

  async function create(user: User): Promise<ComponentFixture<Shell>> {
    TestBed.configureTestingModule({
      imports: [Shell, Blank],
      providers: [
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

    expect(labels).toEqual(['Catalog', 'Routing', 'Access', 'Observability', 'Development']);
    const links = Array.from(root.querySelectorAll('.nav-desktop .nav-section__links a')).map(
      (link) => link.textContent?.trim(),
    );
    expect(links).toContain('Providers');
    expect(links).toContain('Models');
    expect(links).toContain('Combos');
    expect(links).toContain('API keys');
    expect(links).toContain('Audit log');
    expect(links).toContain('Styleguide');
    expect(links).not.toContain('Offerings');
    expect(links).not.toContain('Benchmarks');
    expect(links).not.toContain('Price sync');
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
