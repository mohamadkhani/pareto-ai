import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { AuthService, type User } from '../auth/auth.service';
import { DEV_MODE } from '../dev-mode';
import { Styleguide } from './styleguide';

function authStub(role: 'admin' | 'user'): object {
  return {
    user: signal<User | null>({
      id: 'u1',
      email: 'user@example.com',
      display_name: 'User',
      role,
      must_change_password: false,
    }),
    isAdmin: signal(role === 'admin'),
    csrfToken: signal<string | null>('csrf-1'),
  };
}

describe('Styleguide', () => {
  async function create(
    role: 'admin' | 'user' = 'admin',
    devMode = true,
  ): Promise<ComponentFixture<Styleguide>> {
    TestBed.configureTestingModule({
      imports: [Styleguide],
      providers: [
        { provide: AuthService, useValue: authStub(role) },
        { provide: DEV_MODE, useValue: devMode },
      ],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Styleguide);
    fixture.detectChanges();
    return fixture;
  }

  it('renders the tokens and both forced themes', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('[data-theme="light"]')).not.toBeNull();
    expect(root.querySelector('[data-theme="dark"]')).not.toBeNull();
    expect(root.querySelectorAll('.swatch').length).toBeGreaterThan(15);
    expect(root.querySelectorAll('.spacing-row').length).toBeGreaterThan(5);
  });

  it('shows the real primitives, including every state', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelectorAll('ui-button').length).toBeGreaterThan(8);
    expect(root.querySelectorAll('ui-icon-button').length).toBeGreaterThan(2);
    expect(root.querySelectorAll('ui-input').length).toBeGreaterThan(2);
    expect(root.querySelector('ui-input [aria-invalid="true"]')).not.toBeNull();
    expect(root.querySelectorAll('ui-badge').length).toBeGreaterThan(5);
    expect(root.querySelectorAll('ui-banner').length).toBeGreaterThan(3);
    expect(root.querySelectorAll('ui-dialog').length).toBe(2);
    expect(root.querySelectorAll('[role="tab"]').length).toBeGreaterThan(2);
    expect(root.querySelectorAll('ui-spinner').length).toBeGreaterThan(2);
  });

  it('shows a forbidden state instead of the design system for a user', async () => {
    const fixture = await create('user');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Forbidden');
    expect(root.textContent).toContain('development builds only');
    expect(root.querySelectorAll('ui-button')).toHaveLength(0);
    expect(root.querySelector('[data-theme="light"]')).toBeNull();
  });

  it('shows the forbidden state in a production build even for an admin', async () => {
    const fixture = await create('admin', false);
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Forbidden');
    expect(root.textContent).toContain('development builds only');
    expect(root.querySelectorAll('ui-button')).toHaveLength(0);
    expect(root.querySelector('[data-theme="light"]')).toBeNull();
  });
});
