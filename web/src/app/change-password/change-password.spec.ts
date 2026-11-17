import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { type Provider, signal } from '@angular/core';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import { ChangePassword } from './change-password';

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

function typeInto(root: HTMLElement, selector: string, value: string): void {
  const input = root.querySelector(selector);
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`no input matching ${selector}`);
  }
  input.value = value;
  input.dispatchEvent(new Event('input'));
}

function submit(root: HTMLElement): void {
  const form = root.querySelector('form');
  if (!(form instanceof HTMLFormElement)) {
    throw new Error('no form on the page');
  }
  form.dispatchEvent(new Event('submit'));
}

function fill(root: HTMLElement, current: string, next: string, confirm = next): void {
  typeInto(root, '#current-password', current);
  typeInto(root, '#new-password', next);
  typeInto(root, '#confirm-password', confirm);
}

function submitButton(root: HTMLElement): HTMLButtonElement {
  const button = root.querySelector('button[type="submit"]');
  if (!(button instanceof HTMLButtonElement)) {
    throw new Error('no submit button');
  }
  return button;
}

describe('ChangePassword', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(providers: Provider[] = []): Promise<ComponentFixture<ChangePassword>> {
    TestBed.configureTestingModule({
      imports: [ChangePassword],
      providers: [provideRouter([]), ...providers],
    });
    await TestBed.compileComponents();
    // The session exists but is flagged for rotation; it carries the CSRF token.
    const auth = TestBed.inject(AuthService);
    auth.csrfToken.set('csrf-1');
    const fixture = TestBed.createComponent(ChangePassword);
    fixture.detectChanges();
    return fixture;
  }

  it('requires every field and matching passwords before calling the API', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(0);
    expect(root.textContent).toContain('Your current password is required.');
    expect(root.textContent).toContain('A new password is required.');

    fill(root, 'old-password-1', 'new-password-1', 'different-1');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(0);
    expect(root.textContent).toContain('The two passwords do not match.');
  });

  it('rejects a too-short new password', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fill(root, 'old-password-1', 'short');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(0);
    expect(root.textContent).toContain('Use at least 12 characters.');
  });

  it('changes the password, re-reads the session, and leaves the screen', async () => {
    fetchMock.respond(noContentResponse());
    fetchMock.respond(jsonResponse(200, session));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);

    fill(root, 'old-password-1', 'new-password-1');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/auth/change-password');
    expect(call.method).toBe('POST');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      current_password: 'old-password-1',
      new_password: 'new-password-1',
    });
    // The forced-rotation flag is cleared by re-reading the session.
    expect(fetchMock.requests[1]?.url).toBe('/api/auth/me');
    expect(navigate).toHaveBeenCalledWith('/');
  });

  it('still leaves the screen when the session refresh fails', async () => {
    fetchMock.respond(noContentResponse());
    fetchMock.respond(errorResponse(401, 'unauthorized', 'Sign in required.'));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);

    fill(root, 'old-password-1', 'new-password-1');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    // The password change already succeeded; a failed refresh must not strand
    // the account on the rotation screen or leave the button busy.
    expect(fetchMock.requests.map((call) => call.url)).toEqual([
      '/api/auth/change-password',
      '/api/auth/me',
    ]);
    expect(navigate).toHaveBeenCalledWith('/');
    expect(submitButton(root).disabled).toBe(false);
  });

  it('explains a wrong current password without the generic 401 text', async () => {
    fetchMock.respond(errorResponse(401, 'invalid_credentials', 'invalid email or password'));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fill(root, 'wrong', 'new-password-1');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.textContent).toContain('Your current password is not correct.');
  });

  it('re-enables the button once the change succeeds, even if navigation stalls', async () => {
    const user: User = { ...session.user, must_change_password: true };
    const authMock = {
      user: signal<User | null>(user),
      csrfToken: signal<string | null>('csrf-1'),
      changePassword: vi.fn().mockResolvedValue(undefined),
      loadMe: vi.fn().mockResolvedValue(true),
      logout: vi.fn().mockResolvedValue(undefined),
    };
    const fixture = await create([{ provide: AuthService, useValue: authMock }]);
    const root = fixture.nativeElement as HTMLElement;
    // A navigation that never settles must not pin the form in its busy state.
    vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockReturnValue(
      new Promise<boolean>(() => {}),
    );

    fill(root, 'old-password-1', 'new-password-1');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(authMock.changePassword).toHaveBeenCalledWith('old-password-1', 'new-password-1');
    expect(submitButton(root).disabled).toBe(false);
    expect(submitButton(root).textContent).toContain('Change password');
    expect(submitButton(root).textContent).not.toContain('Changing…');

    // The password is already changed; clicking again must not re-post it.
    submit(root);
    await fixture.whenStable();
    expect(authMock.changePassword).toHaveBeenCalledTimes(1);
  });

  it('keeps the user on the page with the API message when the change fails', async () => {
    fetchMock.respond(errorResponse(500, 'internal', 'Password hashing unavailable.'));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fill(root, 'old-password-1', 'new-password-1');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.textContent).toContain('Password hashing unavailable.');
    expect(submitButton(root).disabled).toBe(false);
  });
});
