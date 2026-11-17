import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, provideRouter } from '@angular/router';
import { FetchMock, errorResponse, jsonResponse } from '../../testing/fetch-mock';
import { AuthService } from '../auth/auth.service';
import { Login } from './login';

function routeStub(returnUrl: string | null): object {
  return {
    snapshot: {
      queryParamMap: {
        get: (key: string): string | null => (key === 'returnUrl' ? returnUrl : null),
      },
    },
  };
}

function typeInto(fixture: ComponentFixture<Login>, selector: string, value: string): void {
  const input = fixture.nativeElement.querySelector(selector);
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`no input matching ${selector}`);
  }
  input.value = value;
  input.dispatchEvent(new Event('input'));
}

function submit(fixture: ComponentFixture<Login>): void {
  const form = fixture.nativeElement.querySelector('form');
  if (!(form instanceof HTMLFormElement)) {
    throw new Error('login page has no form');
  }
  form.dispatchEvent(new Event('submit'));
}

function bannerText(fixture: ComponentFixture<Login>): string {
  return fixture.nativeElement.querySelector('.banner')?.textContent ?? '';
}

describe('Login', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(returnUrl: string | null = null): Promise<ComponentFixture<Login>> {
    TestBed.configureTestingModule({
      imports: [Login],
      providers: [provideRouter([]), { provide: ActivatedRoute, useValue: routeStub(returnUrl) }],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Login);
    fixture.detectChanges();
    return fixture;
  }

  it('requires email and password before calling the API', async () => {
    const fixture = await create();

    submit(fixture);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(0);
    const text = fixture.nativeElement.textContent as string;
    expect(text).toContain('Email is required.');
    expect(text).toContain('Password is required.');
  });

  it('rejects an address that is not an email before calling the API', async () => {
    const fixture = await create();

    typeInto(fixture, '#login-email', 'not-an-email');
    typeInto(fixture, '#login-password', 'change-me-please');
    submit(fixture);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(0);
    expect(fixture.nativeElement.textContent).toContain('Enter a valid email address.');
  });

  it('sends the credentials, stores the CSRF token, and navigates to the return URL', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        user: {
          id: 'u1',
          email: 'admin@example.com',
          display_name: 'Admin',
          role: 'admin',
          must_change_password: false,
        },
        csrf_token: 'csrf-1',
      }),
    );
    const fixture = await create('/audit');
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);

    typeInto(fixture, '#login-email', 'admin@example.com');
    typeInto(fixture, '#login-password', 'change-me-please');
    submit(fixture);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(1);
    expect(fetchMock.requests[0].body).toEqual({
      email: 'admin@example.com',
      password: 'change-me-please',
    });
    expect(navigate).toHaveBeenCalledWith('/audit');
    expect(TestBed.inject(AuthService).csrfToken()).toBe('csrf-1');
  });

  it('shows the server message when the credentials are invalid', async () => {
    fetchMock.respond(errorResponse(401, 'invalid_credentials', 'Invalid email or password.'));
    const fixture = await create();

    typeInto(fixture, '#login-email', 'admin@example.com');
    typeInto(fixture, '#login-password', 'wrong');
    submit(fixture);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(bannerText(fixture)).toContain('Invalid email or password.');
  });

  it('shows the retry countdown when the login is locked out', async () => {
    fetchMock.respond(
      errorResponse(429, 'login_locked', 'Too many failed attempts.', { 'Retry-After': '42' }),
    );
    const fixture = await create();

    typeInto(fixture, '#login-email', 'admin@example.com');
    typeInto(fixture, '#login-password', 'wrong');
    submit(fixture);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(bannerText(fixture)).toContain('Too many failed attempts.');
    expect(bannerText(fixture)).toContain('Try again in 42 seconds.');
  });

  it('focuses the email field and disables the submit button while signing in', async () => {
    let respond: ((response: Response) => void) | undefined;
    const calls: string[] = [];
    vi.stubGlobal('fetch', (input: RequestInfo | URL): Promise<Response> => {
      calls.push(String(input));
      return new Promise<Response>((resolve) => {
        respond = resolve;
      });
    });
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.querySelector('#login-email')).toBe(document.activeElement);

    typeInto(fixture, '#login-email', 'admin@example.com');
    typeInto(fixture, '#login-password', 'change-me-please');
    submit(fixture);
    fixture.detectChanges();

    const button = root.querySelector('button[type="submit"]');
    if (!(button instanceof HTMLButtonElement)) {
      throw new Error('no submit button');
    }
    expect(button.disabled).toBe(true);
    expect(button.getAttribute('aria-busy')).toBe('true');

    // A second submit while the first is still in flight must be ignored.
    submit(fixture);
    expect(calls).toEqual(['/api/auth/login']);

    respond?.(
      jsonResponse(200, {
        user: {
          id: 'u1',
          email: 'admin@example.com',
          display_name: 'Admin',
          role: 'admin',
          must_change_password: false,
        },
        csrf_token: 'csrf-1',
      }),
    );
    await fixture.whenStable();
  });

  it('re-enables the button after a failure so the user can retry', async () => {
    fetchMock.respond(errorResponse(401, 'invalid_credentials', 'Invalid email or password.'));
    const fixture = await create();

    typeInto(fixture, '#login-email', 'admin@example.com');
    typeInto(fixture, '#login-password', 'wrong');
    submit(fixture);
    await fixture.whenStable();
    fixture.detectChanges();

    const button = fixture.nativeElement.querySelector('button[type="submit"]');
    if (!(button instanceof HTMLButtonElement)) {
      throw new Error('no submit button');
    }
    expect(button.disabled).toBe(false);
  });

  it('navigates to the app root when no return URL was requested', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        user: {
          id: 'u1',
          email: 'admin@example.com',
          display_name: 'Admin',
          role: 'admin',
          must_change_password: false,
        },
        csrf_token: 'csrf-1',
      }),
    );
    const fixture = await create();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);

    typeInto(fixture, '#login-email', 'admin@example.com');
    typeInto(fixture, '#login-password', 'change-me-please');
    submit(fixture);
    await fixture.whenStable();

    expect(navigate).toHaveBeenCalledWith('/');
  });
});
