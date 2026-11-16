import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute } from '@angular/router';
import { BehaviorSubject } from 'rxjs';
import { FetchMock, jsonResponse, noContentResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { ApiKeyView, CreatedKey } from './keys.types';
import { Keys } from './keys';

function authStub(): object {
  return {
    user: signal<User | null>({
      id: 'u1',
      email: 'admin@example.com',
      display_name: 'Admin',
      role: 'admin',
      must_change_password: false,
    }),
    isAdmin: signal(true),
    csrfToken: signal<string | null>('csrf-1'),
  };
}

function routeStub(id: string | null): object {
  const paramMap = { get: (key: string): string | null => (key === 'id' ? id : null) };
  return { snapshot: { paramMap }, paramMap: new BehaviorSubject(paramMap) };
}

function keyFixture(overrides: Partial<ApiKeyView> = {}): ApiKeyView {
  return {
    id: 'k1',
    user_id: 'u1',
    name: 'laptop',
    prefix: 'pk_live_ab12',
    scopes: null,
    expires_at: null,
    revoked_at: null,
    last_used_at: null,
    rate_limit_rpm: 60,
    rate_limit_tpm: null,
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

function buttonByText(scope: ParentNode, text: string): HTMLButtonElement {
  const button = Array.from(scope.querySelectorAll('button')).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  if (button === undefined) {
    throw new Error(`no button labelled ${text}`);
  }
  return button;
}

describe('Keys', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(): Promise<ComponentFixture<Keys>> {
    TestBed.configureTestingModule({
      imports: [Keys],
      providers: [
        { provide: AuthService, useValue: authStub() },
        { provide: ActivatedRoute, useValue: routeStub(null) },
      ],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Keys);
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  it('shows the new key exactly once, with a warning, and never lists it', async () => {
    const created: CreatedKey = { key: 'pk_live_full_secret', api_key: keyFixture() };
    fetchMock.respond(jsonResponse(200, []));
    fetchMock.respond(jsonResponse(201, created));
    fetchMock.respond(jsonResponse(200, [keyFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New key').click();
    fixture.detectChanges();
    typeInto(root, '#key-name', 'laptop');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const panel = root.querySelector('.key-card');
    expect(panel?.textContent ?? '').toContain('pk_live_full_secret');
    expect(panel?.textContent ?? '').toContain('only copy');
    // The list shows the prefix but never the plaintext key.
    expect(root.textContent).toContain('pk_live_ab12');

    buttonByText(root, 'I have copied it').click();
    fixture.detectChanges();
    expect(root.querySelector('.key-card')).toBeNull();
    expect(root.textContent).not.toContain('pk_live_full_secret');
  });

  it('sends only changed fields on edit and keeps null fields out', async () => {
    fetchMock.respond(jsonResponse(200, [keyFixture({ expires_at: '2030-01-01T00:00:00.000Z' })]));
    fetchMock.respond(jsonResponse(200, keyFixture({ name: 'desktop' })));
    fetchMock.respond(jsonResponse(200, [keyFixture({ name: 'desktop' })]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Edit').click();
    fixture.detectChanges();
    typeInto(root, '#key-name', 'desktop');
    // Clearing a rate limit must not send anything: null keeps the stored one.
    typeInto(root, '#key-rpm', '');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[1];
    expect(call.method).toBe('PATCH');
    expect(call.url).toBe('/api/keys/k1');
    expect(call.body).toEqual({ name: 'desktop' });
  });

  it('requires a name and rejects non-positive rate limits', async () => {
    fetchMock.respond(jsonResponse(200, []));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New key').click();
    fixture.detectChanges();
    typeInto(root, '#key-rpm', '0');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(1);
    expect(root.textContent).toContain('A name is required.');
    expect(root.textContent).toContain('Use a whole number of 1 or more.');
  });

  it('revokes a key only after confirmation', async () => {
    fetchMock.respond(jsonResponse(200, [keyFixture()]));
    fetchMock.respond(noContentResponse());
    fetchMock.respond(jsonResponse(200, [keyFixture({ revoked_at: '2026-01-02T00:00:00.000Z' })]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Revoke').click();
    fixture.detectChanges();
    expect(fetchMock.requests).toHaveLength(1);

    buttonByText(root, 'Confirm revoke').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[1].method).toBe('DELETE');
    expect(fetchMock.requests[1].url).toBe('/api/keys/k1');
    expect(root.textContent).toContain('revoked');
  });
});
