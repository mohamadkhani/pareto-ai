import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { FetchMock, jsonResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { Credential, Provider } from './catalog.types';
import { Credentials } from './credentials';

function authStub(): object {
  return {
    user: signal<User | null>({
      id: 'u1',
      email: 'admin@example.com',
      display_name: 'Admin',
      role: 'admin',
    }),
    isAdmin: signal(true),
    csrfToken: signal<string | null>('csrf-1'),
  };
}

const provider: Provider = {
  id: 'p1',
  name: 'openrouter',
  kind: 'openai_compatible',
  base_url: 'https://openrouter.ai/api/v1',
  enabled: true,
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
};

function credentialFixture(overrides: Partial<Credential> = {}): Credential {
  return {
    id: 'c1',
    provider_id: 'p1',
    label: 'prod',
    has_secret: true,
    secret_masked: '********',
    header_names: ['x-region'],
    max_concurrency: 4,
    balance_usd: 12.5,
    balance_source: 'manual',
    balance_updated_at: new Date(Date.now() - 5 * 60_000).toISOString(),
    balance_updated_by: 'u1',
    balance_low_threshold: null,
    status: 'ok',
    last_tested_at: null,
    cooldown_until: null,
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

describe('Credentials', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
    TestBed.configureTestingModule({
      imports: [Credentials],
      providers: [{ provide: AuthService, useValue: authStub() }],
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(credentials: Credential[]): Promise<ComponentFixture<Credentials>> {
    fetchMock.respond(jsonResponse(200, credentials));
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Credentials);
    fixture.componentRef.setInput('provider', provider);
    await fixture.whenStable();
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  it('lists credentials with masked secret, balance source and age', async () => {
    const fixture = await create([
      credentialFixture(),
      credentialFixture({
        id: 'c2',
        label: 'backup',
        has_secret: false,
        header_names: [],
        balance_usd: null,
        balance_source: 'unknown',
        balance_updated_at: null,
      }),
    ]);
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('prod');
    expect(text).toContain('•••• set');
    expect(text).toContain('not set');
    expect(text).toContain('$12.50');
    expect(text).toContain('manual');
    expect(text).toContain('5m ago');
    expect(text).toContain('x-region');
    // The secret itself must never appear anywhere on screen.
    expect(text).not.toContain('sk-');
  });

  it('creates a credential sending the write-only secret and header values once', async () => {
    const fixture = await create([]);
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New credential').click();
    fixture.detectChanges();
    typeInto(root, '#cred-label', 'prod');
    typeInto(root, '#cred-secret', 'sk-live-1');
    buttonByText(root, 'Add header').click();
    fixture.detectChanges();
    typeInto(root, '[aria-label="Header name 1"]', 'x-region');
    fixture.detectChanges();
    typeInto(root, '[aria-label="Header value 1"]', 'eu');
    fixture.detectChanges();
    fetchMock.respond(jsonResponse(201, credentialFixture()));
    fetchMock.respond(jsonResponse(200, [credentialFixture()]));
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[1];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/providers/p1/credentials');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      label: 'prod',
      secret: 'sk-live-1',
      headers: { 'x-region': 'eu' },
      max_concurrency: null,
      balance_usd: null,
      balance_low_threshold: null,
    });
  });

  it('requires a label and a secret before calling the API', async () => {
    const fixture = await create([]);
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New credential').click();
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(1);
    expect(root.textContent).toContain('Label is required.');
    expect(root.textContent).toContain('Secret is required.');
  });

  it('leaves the secret out of the patch when it is left blank', async () => {
    const fixture = await create([credentialFixture()]);
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Edit').click();
    fixture.detectChanges();
    typeInto(root, '#cred-label', 'prod-eu');
    fetchMock.respond(jsonResponse(200, credentialFixture({ label: 'prod-eu' })));
    fetchMock.respond(jsonResponse(200, [credentialFixture({ label: 'prod-eu' })]));
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[1];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/credentials/c1');
    expect(call.body).toEqual({ label: 'prod-eu' });
    expect(call.body).not.toHaveProperty('secret');
  });

  it('sends only the header rows with a value and omits untouched headers', async () => {
    const fixture = await create([credentialFixture({ header_names: ['x-region', 'x-team'] })]);
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Edit').click();
    fixture.detectChanges();
    typeInto(root, '#cred-label', 'prod-eu');
    fetchMock.respond(jsonResponse(200, credentialFixture({ label: 'prod-eu' })));
    fetchMock.respond(jsonResponse(200, [credentialFixture()]));
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    // Untouched headers are not part of the patch at all.
    expect(fetchMock.requests[1].body).toEqual({ label: 'prod-eu' });

    buttonByText(root, 'Edit').click();
    fixture.detectChanges();
    typeInto(root, '[aria-label="Header value 1"]', 'eu');
    fetchMock.respond(jsonResponse(200, credentialFixture()));
    fetchMock.respond(jsonResponse(200, [credentialFixture()]));
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    // The row whose value stayed blank (write-only, not retyped) is dropped.
    expect(fetchMock.requests[3].body).toEqual({ headers: { 'x-region': 'eu' } });
  });

  it('runs the test probe and shows pass with the last tested time', async () => {
    const fixture = await create([credentialFixture()]);
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(
      jsonResponse(200, {
        status: 'ok',
        ok: true,
        http_status: 200,
        latency_ms: 42,
        error: null,
        tested_at: '2026-01-01T00:05:00.000Z',
      }),
    );
    fetchMock.respond(
      jsonResponse(200, [credentialFixture({ last_tested_at: '2026-01-01T00:05:00.000Z' })]),
    );
    buttonByText(root, 'Test').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[1].url).toBe('/api/credentials/c1/test');
    expect(fetchMock.requests[1].headers.get('X-CSRF-Token')).toBe('csrf-1');
    const banner = root.querySelector('.banner.success')?.textContent ?? '';
    expect(banner).toContain('Probe passed for prod');
    expect(banner).toContain('last tested');
  });

  it('runs the test probe and shows fail with the recorded error', async () => {
    const fixture = await create([credentialFixture()]);
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(
      jsonResponse(200, {
        status: 'error',
        ok: false,
        http_status: 401,
        latency_ms: 30,
        error: 'upstream returned 401',
        tested_at: '2026-01-01T00:05:00.000Z',
      }),
    );
    fetchMock.respond(jsonResponse(200, [credentialFixture({ status: 'error' })]));
    buttonByText(root, 'Test').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const banner = root.querySelector('.banner.error')?.textContent ?? '';
    expect(banner).toContain('Probe failed for prod');
    expect(banner).toContain('upstream returned 401');
  });
});
