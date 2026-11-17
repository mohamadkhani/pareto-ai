import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { FetchMock, errorResponse, jsonResponse } from '../../testing/fetch-mock';
import { AuthService } from '../auth/auth.service';
import { SetupService, type SetupStatus } from './setup.service';
import { Setup } from './setup';

const localStatus: SetupStatus = {
  mode: 'setup',
  bind: '127.0.0.1:8081',
  is_local: true,
  setup_required: true,
  warning: null,
  tls: { enabled: false, domain: null },
  master_key_source: 'file',
};

const remoteStatus: SetupStatus = { ...localStatus, is_local: false };

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
    throw new Error('the wizard has no form');
  }
  form.dispatchEvent(new Event('submit'));
}

function buttonWithText(root: HTMLElement, text: string): HTMLButtonElement {
  const button = Array.from(root.querySelectorAll('button')).find((candidate) =>
    candidate.textContent?.includes(text),
  );
  if (!(button instanceof HTMLButtonElement)) {
    throw new Error(`no button containing ${text}`);
  }
  return button;
}

describe('Setup', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(status: SetupStatus = localStatus): Promise<ComponentFixture<Setup>> {
    TestBed.configureTestingModule({
      imports: [Setup],
      providers: [provideRouter([])],
    });
    await TestBed.compileComponents();
    TestBed.inject(SetupService).status.set(status);
    const fixture = TestBed.createComponent(Setup);
    fixture.detectChanges();
    return fixture;
  }

  it('explains the skip choice and names the bind address', async () => {
    const fixture = await create();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';

    expect(text).toContain('No account exists yet');
    expect(text).toContain('Skip for now — local only');
    expect(text).toContain('127.0.0.1:8081');
    expect(text).toContain('Admin email');
  });

  it('validates the email, password length, and confirmation before posting', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, '#setup-email', 'not-an-email');
    typeInto(root, '#setup-password', 'short');
    typeInto(root, '#setup-confirm', 'other');
    submit(root);
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(0);
    expect(root.textContent).toContain('Enter a valid email address.');
    expect(root.textContent).toContain('Use at least 12 characters.');
    expect(root.textContent).toContain('The two passwords do not match.');
  });

  it('creates the admin, adopts the session, and lands in the console', async () => {
    fetchMock.respond(jsonResponse(200, session));
    fetchMock.respond(
      jsonResponse(200, { ...localStatus, mode: 'protected', setup_required: false }),
    );
    const fixture = await create();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, '#setup-email', 'boss@example.com');
    typeInto(root, '#setup-password', 'first-admin-password');
    typeInto(root, '#setup-confirm', 'first-admin-password');
    submit(root);
    await fixture.whenStable();

    expect(fetchMock.requests[0].url).toBe('/api/setup/admin');
    expect(fetchMock.requests[0].body).toEqual({
      email: 'boss@example.com',
      password: 'first-admin-password',
    });
    expect(TestBed.inject(AuthService).user()?.email).toBe('boss@example.com');
    expect(navigate).toHaveBeenCalledWith('/');
  });

  it('skips setup and navigates into the unprotected console', async () => {
    fetchMock.respond(
      jsonResponse(200, { ...localStatus, mode: 'unprotected', setup_required: false }),
    );
    const fixture = await create();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    const root = fixture.nativeElement as HTMLElement;

    buttonWithText(root, 'Skip for now').click();
    await fixture.whenStable();

    expect(fetchMock.requests[0].url).toBe('/api/setup/skip');
    expect(fetchMock.requests[0].body).toEqual({ confirm: true });
    expect(navigate).toHaveBeenCalledWith('/');
  });

  it('surfaces a server refusal instead of navigating', async () => {
    fetchMock.respond(
      errorResponse(403, 'setup_local_only', 'setup is only available from this machine'),
    );
    const fixture = await create();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, '#setup-email', 'boss@example.com');
    typeInto(root, '#setup-password', 'first-admin-password');
    typeInto(root, '#setup-confirm', 'first-admin-password');
    submit(root);
    await fixture.whenStable();

    expect(root.textContent).toContain('Setup can only be finished from the machine');
    expect(navigate).not.toHaveBeenCalled();
  });

  it('disables both actions for a network visitor', async () => {
    const fixture = await create(remoteStatus);
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Setup is only available from this machine');
    const createButton = buttonWithText(root, 'Create admin account');
    const skipButton = buttonWithText(root, 'Skip for now');
    expect(createButton.disabled).toBe(true);
    expect(skipButton.disabled).toBe(true);
  });
});
