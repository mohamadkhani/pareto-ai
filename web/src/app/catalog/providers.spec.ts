import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { Provider } from './catalog.types';
import { Providers } from './providers';

function authStub(role: 'admin' | 'user'): object {
  return {
    user: signal<User | null>({
      id: 'u1',
      email: 'admin@example.com',
      display_name: 'Admin',
      role,
    }),
    isAdmin: signal(role === 'admin'),
    csrfToken: signal<string | null>('csrf-1'),
  };
}

function providerFixture(overrides: Partial<Provider> = {}): Provider {
  return {
    id: 'p1',
    name: 'openrouter',
    kind: 'openai_compatible',
    base_url: 'https://openrouter.ai/api/v1',
    enabled: true,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
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

function submit(root: HTMLElement): void {
  const form = root.querySelector('form');
  if (!(form instanceof HTMLFormElement)) {
    throw new Error('no form on the page');
  }
  form.dispatchEvent(new Event('submit'));
}

function buttonByText(root: HTMLElement, text: string): HTMLButtonElement {
  const button = Array.from(root.querySelectorAll('button')).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  if (button === undefined) {
    throw new Error(`no button labelled ${text}`);
  }
  return button;
}

describe('Providers', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(role: 'admin' | 'user' = 'admin'): Promise<ComponentFixture<Providers>> {
    TestBed.configureTestingModule({
      imports: [Providers],
      providers: [{ provide: AuthService, useValue: authStub(role) }],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Providers);
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  /** Waits for reactive effects and their async work to settle and render. */
  async function settle(fixture: ComponentFixture<Providers>): Promise<void> {
    await fixture.whenStable();
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
  }

  it('shows a forbidden state for non-admins without calling the API', async () => {
    const fixture = await create('user');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Forbidden');
    expect(fetchMock.requests).toHaveLength(0);
  });

  it('lists providers with base URL, kind and enabled state', async () => {
    fetchMock.respond(
      jsonResponse(200, [
        providerFixture(),
        providerFixture({ id: 'p2', name: 'azure', enabled: false }),
      ]),
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const text = root.textContent ?? '';
    expect(text).toContain('openrouter');
    expect(text).toContain('azure');
    expect(text).toContain('https://openrouter.ai/api/v1');
    expect(text).toContain('openai_compatible');
    expect(text).toContain('enabled');
    expect(text).toContain('disabled');
    expect(root.querySelectorAll('tbody tr')).toHaveLength(2);
  });

  it('creates a provider from the form and reloads the list', async () => {
    fetchMock.respond(jsonResponse(200, [providerFixture()]));
    fetchMock.respond(
      jsonResponse(
        201,
        providerFixture({ id: 'p2', name: 'azure', base_url: 'https://a.example/v1' }),
      ),
    );
    fetchMock.respond(
      jsonResponse(200, [providerFixture(), providerFixture({ id: 'p2', name: 'azure' })]),
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New provider').click();
    fixture.detectChanges();
    typeInto(root, '#provider-name', 'azure');
    typeInto(root, '#provider-base-url', 'https://a.example/v1');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[1];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/providers');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      name: 'azure',
      base_url: 'https://a.example/v1',
      kind: 'openai_compatible',
    });
    expect(root.textContent).toContain('azure');
  });

  it('validates the form before calling the API', async () => {
    fetchMock.respond(jsonResponse(200, []));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New provider').click();
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(1);
    expect(root.textContent).toContain('Name is required.');
    expect(root.textContent).toContain('Base URL is required.');

    typeInto(root, '#provider-name', 'azure');
    typeInto(root, '#provider-base-url', 'ftp://a.example');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(1);
    expect(root.textContent).toContain('Enter an http(s) URL');
  });

  it('toggles enabled with a PATCH carrying the CSRF header', async () => {
    fetchMock.respond(jsonResponse(200, [providerFixture()]));
    fetchMock.respond(jsonResponse(200, providerFixture({ enabled: false })));
    fetchMock.respond(jsonResponse(200, [providerFixture({ enabled: false })]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Disable').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[1];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/providers/p1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ enabled: false });
  });

  it('deletes a provider only after confirmation', async () => {
    fetchMock.respond(jsonResponse(200, [providerFixture()]));
    fetchMock.respond(noContentResponse());
    fetchMock.respond(jsonResponse(200, []));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Delete').click();
    fixture.detectChanges();

    // Arming the control must not delete anything yet.
    expect(fetchMock.requests).toHaveLength(1);
    buttonByText(root, 'Confirm delete').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[1];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/providers/p1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(root.textContent).toContain('No providers yet');
  });

  it('nests the credentials panel under the selected provider', async () => {
    fetchMock.respond(jsonResponse(200, [providerFixture()]));
    fetchMock.respond(
      jsonResponse(200, [
        {
          id: 'c1',
          provider_id: 'p1',
          label: 'prod',
          has_secret: true,
          secret_masked: '********',
          header_names: [],
          max_concurrency: null,
          balance_usd: null,
          balance_source: 'unknown',
          balance_updated_at: null,
          balance_updated_by: null,
          balance_low_threshold: null,
          status: 'unknown',
          last_tested_at: null,
          cooldown_until: null,
          created_at: '2026-01-01T00:00:00.000Z',
          updated_at: '2026-01-01T00:00:00.000Z',
        },
      ]),
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Credentials').click();
    await settle(fixture);

    expect(root.textContent).toContain('Credentials — openrouter');
    expect(root.textContent).toContain('prod');
    expect(fetchMock.requests[1].url).toBe('/api/providers/p1/credentials');
  });

  it('shows the server message when loading fails', async () => {
    fetchMock.respond(errorResponse(500, 'internal', 'Reading providers failed.'));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Reading providers failed.');
  });
});
