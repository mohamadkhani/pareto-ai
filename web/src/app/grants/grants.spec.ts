import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { Model } from '../catalog/catalog.types';
import type { ApiKeyView } from '../keys/keys.types';
import type { UserView } from '../users/users.types';
import type { GrantView } from './grants.types';
import { Grants } from './grants';

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
    id: 'u2',
    email: 'user@example.com',
    display_name: 'User',
    role: 'user',
    is_active: true,
    must_change_password: false,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    last_login_at: null,
    ...overrides,
  };
}

function modelFixture(overrides: Partial<Model> = {}): Model {
  return {
    id: 'm1',
    canonical_name: 'openai/gpt-5',
    family: 'gpt-5',
    modalities_input: ['text'],
    modalities_output: ['text'],
    context_length: null,
    max_output_tokens: null,
    capabilities: [],
    enabled: true,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

function grantFixture(overrides: Partial<GrantView> = {}): GrantView {
  return {
    id: 'g1',
    subject_type: 'user',
    subject_id: 'u2',
    resource_type: 'model',
    resource_id: 'm1',
    mode: 'allow',
    created_by: 'u1',
    created_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

function keyFixture(overrides: Partial<ApiKeyView> = {}): ApiKeyView {
  return {
    id: 'k1',
    user_id: 'u2',
    name: 'Laptop',
    prefix: 'pk_live_ab12',
    scopes: null,
    expires_at: null,
    revoked_at: null,
    last_used_at: null,
    rate_limit_rpm: null,
    rate_limit_tpm: null,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

/** Queues the five GETs `reload()` issues: grants, users, models, offerings, providers. */
function queueLoad(
  fetchMock: FetchMock,
  grants: GrantView[] = [],
  users: UserView[] = [userFixture()],
  models: Model[] = [modelFixture()],
): void {
  fetchMock.respond(jsonResponse(200, grants));
  fetchMock.respond(jsonResponse(200, users));
  fetchMock.respond(jsonResponse(200, models));
  fetchMock.respond(jsonResponse(200, []));
  fetchMock.respond(jsonResponse(200, []));
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

describe('Grants', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(role: 'admin' | 'user' = 'admin'): Promise<ComponentFixture<Grants>> {
    TestBed.configureTestingModule({
      imports: [Grants],
      providers: [{ provide: AuthService, useValue: authStub(role) }],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Grants);
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

  it('explains how a key inherits its owner (D59)', async () => {
    const fixture = await create('user');
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';

    // The page hint is the only place the console explains the inheritance
    // rule; it must stay in step with the access decision (D59).
    expect(text).toContain("inherits its owner's access");
    expect(text).toContain("The first grant on a user's key replaces that inheritance");
    expect(text).toContain('only deny rows narrow it');
  });

  it('validates the subject and resource pickers before calling the API', async () => {
    queueLoad(fetchMock);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New grant').click();
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(5);
    expect(root.textContent).toContain('Choose an account.');
    expect(root.textContent).toContain('Choose a resource.');
  });

  it('creates a grant with the picked subject, resource, and mode', async () => {
    queueLoad(fetchMock);
    fetchMock.respond(jsonResponse(201, grantFixture({ mode: 'deny' })));
    queueLoad(fetchMock, [grantFixture({ mode: 'deny' })]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New grant').click();
    fixture.detectChanges();

    // The resource picker shows friendly model names.
    expect(root.querySelector('#grant-resource')?.textContent ?? '').toContain('openai/gpt-5');

    selectInto(root, '#grant-subject', 'u2');
    selectInto(root, '#grant-resource', 'm1');
    selectInto(root, '#grant-mode', 'deny');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[5];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/grants');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      subject_type: 'user',
      subject_id: 'u2',
      resource_type: 'model',
      resource_id: 'm1',
      mode: 'deny',
    });
    expect(root.textContent).toContain('user@example.com');
    expect(root.textContent).toContain('openai/gpt-5');
    expect(root.textContent).toContain('deny');
  });

  it('surfaces a 409 duplicate clearly and keeps the form open', async () => {
    queueLoad(fetchMock);
    fetchMock.respond(
      errorResponse(409, 'conflict', 'a grant for this subject and resource already exists'),
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New grant').click();
    fixture.detectChanges();
    selectInto(root, '#grant-subject', 'u2');
    selectInto(root, '#grant-resource', 'm1');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.querySelector('.ui-banner.tone-danger')?.textContent ?? '').toContain(
      'Duplicate grant',
    );
    expect(root.querySelector('form')).not.toBeNull();
  });

  it('deletes a grant only after confirmation', async () => {
    queueLoad(fetchMock, [grantFixture()]);
    fetchMock.respond(noContentResponse());
    queueLoad(fetchMock);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Delete').click();
    fixture.detectChanges();
    expect(fetchMock.requests).toHaveLength(5);

    buttonByText(root, 'Confirm delete').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[5].method).toBe('DELETE');
    expect(fetchMock.requests[5].url).toBe('/api/grants/g1');
    expect(root.textContent).toContain('No grants yet');
  });

  it('resolves an API-key subject to its name, prefix, and owner', async () => {
    queueLoad(
      fetchMock,
      [grantFixture({ subject_type: 'api_key', subject_id: 'k1' })],
      [userFixture()],
    );
    fetchMock.respond(jsonResponse(200, [keyFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Laptop (pk_live_ab12) · user@example.com');
    expect(root.textContent).not.toContain('api_key k1');
  });

  it('degrades to a readable placeholder when the key was deleted', async () => {
    queueLoad(
      fetchMock,
      [grantFixture({ subject_type: 'api_key', subject_id: 'k-deleted-1234' })],
      [userFixture()],
    );
    fetchMock.respond(jsonResponse(200, []));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('deleted key k-delete…');
  });
});
