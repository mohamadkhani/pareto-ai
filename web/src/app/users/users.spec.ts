import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import { SetupService, type SetupMode, type SetupStatus } from '../setup/setup.service';
import type { UserView } from './users.types';
import { Users } from './users';

const setupStatus: SetupStatus = {
  mode: 'protected',
  bind: '0.0.0.0:8081',
  is_local: false,
  setup_required: false,
  warning: null,
  tls: { enabled: false, domain: null, reason: 'local', error: null, expires_at: null },
  master_key_source: 'env',
};

function authStub(role: 'admin' | 'user'): object {
  return {
    user: signal<User | null>({
      id: 'u1',
      email: 'admin@example.com',
      display_name: 'Admin',
      role,
      must_change_password: false,
    }),
    isAdmin: signal(role === 'admin'),
    csrfToken: signal<string | null>('csrf-1'),
  };
}

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

function typeInto(root: HTMLElement, selector: string, value: string): void {
  const input = root.querySelector(selector);
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`no input matching ${selector}`);
  }
  input.value = value;
  input.dispatchEvent(new Event('input'));
}

function selectInto(root: HTMLElement, selector: string, value: string): void {
  const select = root.querySelector(selector);
  if (!(select instanceof HTMLSelectElement)) {
    throw new Error(`no select matching ${selector}`);
  }
  select.value = value;
  select.dispatchEvent(new Event('change'));
}

function submit(root: HTMLElement): void {
  const form = root.querySelector('form');
  if (!(form instanceof HTMLFormElement)) {
    throw new Error('no form on the page');
  }
  form.dispatchEvent(new Event('submit'));
}

function buttonByText(scope: ParentNode, text: string): HTMLButtonElement {
  const button = Array.from(scope.querySelectorAll('button')).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  if (button === undefined) {
    throw new Error(`no button labelled ${text}`);
  }
  return button;
}

describe('Users', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(
    role: 'admin' | 'user' = 'admin',
    mode: SetupMode = 'protected',
  ): Promise<ComponentFixture<Users>> {
    TestBed.configureTestingModule({
      imports: [Users],
      providers: [{ provide: AuthService, useValue: authStub(role) }, provideRouter([])],
    });
    await TestBed.compileComponents();
    // Pre-seed the shared status so `load()` resolves without a request.
    TestBed.inject(SetupService).status.set({ ...setupStatus, mode });
    const fixture = TestBed.createComponent(Users);
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  it('shows a forbidden state for non-admins without calling the API', async () => {
    const fixture = await create('user');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Forbidden');
    expect(fetchMock.requests).toHaveLength(0);
  });

  it('disables account creation and points at Settings while unprotected', async () => {
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create('admin', 'unprotected');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Account creation is off');
    const newUser = buttonByText(root, 'New user');
    expect(newUser.disabled).toBe(true);
    newUser.click();
    fixture.detectChanges();
    expect(root.querySelector('ui-dialog')).toBeNull();
    expect(fetchMock.requests).toHaveLength(1);
  });

  it('disables both create entry points in the empty unprotected state', async () => {
    // Right after `skip` there are no real accounts: the empty state is the
    // first thing the operator sees.
    fetchMock.respond(jsonResponse(200, []));
    const fixture = await create('admin', 'unprotected');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('No users yet');
    expect(root.textContent).toContain('Set it in Settings');
    const buttons = Array.from(root.querySelectorAll('button')).filter(
      (candidate) => candidate.textContent?.trim() === 'New user',
    );
    expect(buttons).toHaveLength(2);
    for (const button of buttons) {
      expect(button.disabled).toBe(true);
    }
    buttons[1].click();
    fixture.detectChanges();
    expect(root.querySelector('ui-dialog')).toBeNull();
    expect(fetchMock.requests).toHaveLength(1);
  });

  it('keeps creation closed while the setup status is unknown', async () => {
    // First request: the status read fails and stays null. Second: the list.
    fetchMock.respond(errorResponse(500, 'internal', 'status unavailable'));
    fetchMock.respond(jsonResponse(200, []));

    TestBed.configureTestingModule({
      imports: [Users],
      providers: [{ provide: AuthService, useValue: authStub('admin') }, provideRouter([])],
    });
    await TestBed.compileComponents();
    // Deliberately no status seed: `SetupService.status()` starts null.
    const fixture = TestBed.createComponent(Users);
    await fixture.whenStable();
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Checking whether this gateway has a password');
    const buttons = Array.from(root.querySelectorAll('button')).filter(
      (candidate) => candidate.textContent?.trim() === 'New user',
    );
    expect(buttons).toHaveLength(2);
    for (const button of buttons) {
      expect(button.disabled).toBe(true);
    }
    buttons[1].click();
    fixture.detectChanges();
    expect(root.querySelector('ui-dialog')).toBeNull();
  });

  it('refreshes the setup status from the Refresh button', async () => {
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    const fixture = await create('admin', 'unprotected');
    const root = fixture.nativeElement as HTMLElement;
    expect(buttonByText(root, 'New user').disabled).toBe(true);

    // The status moved in another tab or through the CLI bootstrap.
    fetchMock.respond(jsonResponse(200, { ...setupStatus, mode: 'protected' }));
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    buttonByText(root, 'Refresh').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(buttonByText(root, 'New user').disabled).toBe(false);
    expect(root.textContent).not.toContain('Account creation is off');
  });

  it('lists accounts with role, status, and rotation badges', async () => {
    fetchMock.respond(
      jsonResponse(200, [
        userFixture(),
        userFixture({
          id: 'u2',
          email: 'user@example.com',
          display_name: 'User',
          role: 'user',
          is_active: false,
          must_change_password: true,
          last_login_at: '2026-01-02T03:04:05.000Z',
        }),
      ]),
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('admin@example.com');
    expect(text).toContain('user@example.com');
    expect(text).toContain('password change required');
    expect(text).toContain('inactive');
    expect(root.querySelectorAll('ui-badge.tone-accent').length).toBe(1);
  });

  it('requires an email and a long enough password before creating', async () => {
    fetchMock.respond(jsonResponse(200, []));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New user').click();
    fixture.detectChanges();
    typeInto(root, '#user-password', 'short');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(1);
    expect(root.textContent).toContain('Email is required.');
    expect(root.textContent).toContain('Use at least 12 characters.');
  });

  it('creates an account with the rotation flag checked by default', async () => {
    fetchMock.respond(jsonResponse(200, []));
    fetchMock.respond(jsonResponse(201, userFixture({ must_change_password: true })));
    fetchMock.respond(jsonResponse(200, [userFixture({ must_change_password: true })]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New user').click();
    fixture.detectChanges();
    typeInto(root, '#user-email', 'admin@example.com');
    typeInto(root, '#user-name', 'Admin');
    typeInto(root, '#user-password', 'initial-secret-1');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[1];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/users');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      email: 'admin@example.com',
      display_name: 'Admin',
      role: 'user',
      password: 'initial-secret-1',
      must_change_password: true,
    });
  });

  it('sends only the changed fields when editing (null keeps)', async () => {
    fetchMock.respond(jsonResponse(200, [userFixture({ role: 'user' })]));
    fetchMock.respond(jsonResponse(200, userFixture({ role: 'admin', is_active: false })));
    fetchMock.respond(jsonResponse(200, []));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Edit').click();
    fixture.detectChanges();
    selectInto(root, '#edit-role', 'admin');
    const active = root.querySelector<HTMLButtonElement>('ui-switch button');
    expect(active).not.toBeNull();
    active?.click();
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[1];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/users/u1');
    expect(call.body).toEqual({ role: 'admin', is_active: false });
  });

  it('resets a password only after confirmation and warns about revoked keys', async () => {
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    fetchMock.respond(jsonResponse(200, userFixture({ must_change_password: true })));
    fetchMock.respond(jsonResponse(200, [userFixture({ must_change_password: true })]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Reset password').click();
    fixture.detectChanges();

    const panel = root.querySelector('.reset-panel');
    expect(panel?.textContent ?? '').toContain('revokes');
    typeInto(root, '#reset-password', 'reset-secret-1');
    expect(fetchMock.requests).toHaveLength(1);

    const arm = buttonByText(panel ?? root, 'Set new password');
    arm.click();
    fixture.detectChanges();
    expect(fetchMock.requests).toHaveLength(1);

    buttonByText(panel ?? root, 'Confirm reset').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[1];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/users/u1');
    expect(call.body).toEqual({ password: 'reset-secret-1' });
    expect(root.textContent).toContain('password change required');
    expect(root.querySelector('.reset-panel')).toBeNull();
  });
});

describe('Users drawer', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('opens the row drawer, closes on Escape, and returns focus to the row', async () => {
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    // Limits tab loads on open: budgets, token caps, models.
    fetchMock.respond(jsonResponse(200, []));
    fetchMock.respond(jsonResponse(200, []));
    fetchMock.respond(jsonResponse(200, []));

    TestBed.configureTestingModule({
      imports: [Users],
      providers: [{ provide: AuthService, useValue: authStub('admin') }, provideRouter([])],
    });
    await TestBed.compileComponents();
    TestBed.inject(SetupService).status.set({ ...setupStatus, mode: 'protected' });
    const fixture = TestBed.createComponent(Users);
    await fixture.whenStable();
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    const row = root.querySelector<HTMLTableRowElement>('tbody tr');
    if (row === null) {
      throw new Error('no user row');
    }
    row.focus();
    row.click();
    await fixture.whenStable();
    await Promise.resolve();
    fixture.detectChanges();

    const drawer = root.querySelector('app-user-drawer');
    expect(drawer).not.toBeNull();
    expect(drawer?.querySelectorAll('[role="tab"]')).toHaveLength(5);

    drawer?.querySelector('dialog')?.dispatchEvent(new Event('cancel', { cancelable: true }));
    await fixture.whenStable();
    await Promise.resolve();
    fixture.detectChanges();

    expect(root.querySelector('app-user-drawer')).toBeNull();
    expect(document.activeElement).toBe(row);
  });
});
