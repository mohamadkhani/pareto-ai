import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import { SetupService, type SetupStatus } from '../setup/setup.service';
import { Settings } from './settings';

const admin: User = {
  id: 'u1',
  email: 'admin@example.com',
  display_name: 'Admin',
  role: 'admin',
  must_change_password: false,
};

const protectedStatus: SetupStatus = {
  mode: 'protected',
  bind: '0.0.0.0:8081',
  is_local: false,
  setup_required: false,
  warning: null,
  tls: { enabled: false, domain: null, reason: 'local', error: null, expires_at: null },
  master_key_source: 'env',
};

const unprotectedStatus: SetupStatus = {
  ...protectedStatus,
  mode: 'unprotected',
  is_local: true,
  warning: 'Running without a password on this machine.',
  master_key_source: 'file',
};

const session = {
  user: { ...admin, email: 'boss@example.com' },
  csrf_token: 'csrf-2',
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
    throw new Error('settings has no form');
  }
  form.dispatchEvent(new Event('submit'));
}

describe('Settings', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(
    user: User = admin,
    status: SetupStatus = protectedStatus,
  ): Promise<ComponentFixture<Settings>> {
    TestBed.configureTestingModule({
      imports: [Settings],
      providers: [provideRouter([])],
    });
    await TestBed.compileComponents();
    const auth = TestBed.inject(AuthService);
    auth.user.set(user);
    auth.status.set('authenticated');
    auth.csrfToken.set('csrf-1');
    TestBed.inject(SetupService).status.set(status);
    const fixture = TestBed.createComponent(Settings);
    fixture.detectChanges();
    return fixture;
  }

  it('shows the exposure facts read-only', async () => {
    const fixture = await create();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';

    expect(text).toContain('Protected — passwords required');
    expect(text).toContain('0.0.0.0:8081');
    expect(text).toContain('public');
    expect(text).toContain('Off — plain HTTP');
    expect(text).toContain('GATEWAY_MASTER_KEY environment variable');
    expect(text).not.toContain('Protect this gateway');
  });

  it('shows an active certificate with its expiry', async () => {
    const fixture = await create(admin, {
      ...protectedStatus,
      tls: {
        enabled: true,
        domain: 'gateway.example.com',
        reason: null,
        error: null,
        expires_at: '2027-02-17T00:00:00Z',
      },
    });
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';

    expect(text).toContain('HTTPS — gateway.example.com, expires 2027-02-17');
  });

  it('explains a failed automatic HTTPS attempt', async () => {
    const fixture = await create(admin, {
      ...protectedStatus,
      tls: {
        enabled: false,
        domain: 'gateway.example.com',
        reason: null,
        error: 'ACME could not validate the domain',
        expires_at: null,
      },
    });
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';

    expect(text).toContain('Failing — plain HTTP (see below)');
    expect(text).toMatch(/Automatic HTTPS failed/);
    expect(text).toContain('ACME could not validate the domain');
  });

  it('changes the own password with the CSRF token and reports success', async () => {
    fetchMock.respond(noContentResponse());
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, '#settings-current', 'current-password-1');
    typeInto(root, '#settings-new', 'brand-new-password-1');
    typeInto(root, '#settings-confirm', 'brand-new-password-1');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/auth/change-password');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      current_password: 'current-password-1',
      new_password: 'brand-new-password-1',
    });
    expect(root.textContent).toContain('Password changed');
    expect(root.textContent).toContain('your API keys still work');
  });

  it('names the current password when the server rejects it', async () => {
    fetchMock.respond(errorResponse(401, 'invalid_credentials', 'invalid email or password'));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, '#settings-current', 'wrong-password-1');
    typeInto(root, '#settings-new', 'brand-new-password-1');
    typeInto(root, '#settings-confirm', 'brand-new-password-1');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.textContent).toContain('Your current password is not correct.');
  });

  it('sets the first password while unprotected and shows the file master key', async () => {
    fetchMock.respond(jsonResponse(200, session));
    fetchMock.respond(jsonResponse(200, protectedStatus));
    const fixture = await create(admin, unprotectedStatus);
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Protect this gateway');
    expect(root.textContent).toContain('Generated file in the data directory');
    expect(root.textContent).toContain('Running without a password on this machine');

    typeInto(root, '#first-email', 'boss@example.com');
    typeInto(root, '#first-password', 'first-admin-password');
    typeInto(root, '#first-confirm', 'first-admin-password');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[0].url).toBe('/api/setup/admin');
    expect(fetchMock.requests[0].body).toEqual({
      email: 'boss@example.com',
      password: 'first-admin-password',
    });
    expect(TestBed.inject(AuthService).user()?.email).toBe('boss@example.com');
    expect(root.textContent).toContain('Password set');
    expect(root.textContent).not.toContain('Protect this gateway');
  });

  it('rejects a short first password before posting', async () => {
    const fixture = await create(admin, unprotectedStatus);
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, '#first-email', 'boss@example.com');
    typeInto(root, '#first-password', 'short');
    typeInto(root, '#first-confirm', 'short');
    submit(root);
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(0);
    expect(root.textContent).toContain('Use at least 12 characters.');
  });

  it('shows Forbidden for a non-admin account', async () => {
    const fixture = await create({ ...admin, role: 'user' });
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';

    expect(text).toContain('Forbidden');
    expect(text).not.toContain('Change password');
  });
});
