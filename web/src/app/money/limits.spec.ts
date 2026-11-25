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
import type { UserView } from '../users/users.types';
import type { BudgetView, TokenCapView } from './money.types';
import { Limits } from './limits';

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

function budgetFixture(overrides: Partial<BudgetView> = {}): BudgetView {
  return {
    user_id: 'u1',
    period: 'day',
    limit_usd: 10,
    used_usd: 2.5,
    reset_at: '2026-01-02T00:00:00.000Z',
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

function capFixture(overrides: Partial<TokenCapView> = {}): TokenCapView {
  return {
    user_id: 'u1',
    model_id: 'm1',
    period: 'day',
    limit_tokens: 100000,
    used_tokens: 2500,
    reset_at: '2026-01-02T00:00:00.000Z',
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

function modelFixture(overrides: Partial<Model> = {}): Model {
  return {
    id: 'm1',
    canonical_name: 'openai/gpt-5',
    family: null,
    modalities_input: [],
    modalities_output: [],
    context_length: null,
    max_output_tokens: null,
    capabilities: [],
    enabled: true,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

/** The two GETs every `reload()` issues: the caller's budgets and token caps. */
function queueLimits(
  fetchMock: FetchMock,
  budgets: BudgetView[] = [],
  caps: TokenCapView[] = [],
): void {
  fetchMock.respond(jsonResponse(200, budgets));
  fetchMock.respond(jsonResponse(200, caps));
}

/** The admin-only model catalog that follows the limits reads. */
function queueModels(fetchMock: FetchMock, models: Model[] = [modelFixture()]): void {
  fetchMock.respond(jsonResponse(200, models));
}

function typeInto(root: HTMLElement, selector: string, value: string): void {
  const input = root.querySelector(selector);
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`no input matching ${selector}`);
  }
  input.value = value;
  input.dispatchEvent(new Event('input'));
}

/** Picks a combobox option by visible label (focus, type, click the option). */
function comboboxInto(root: HTMLElement, selector: string, label: string): void {
  const input = root.querySelector<HTMLInputElement>(selector);
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`no combobox input matching ${selector}`);
  }
  input.dispatchEvent(new FocusEvent('focus'));
  input.value = label;
  input.dispatchEvent(new Event('input'));
  const option = Array.from(root.querySelectorAll<HTMLElement>('[role="option"]')).find(
    (candidate) => candidate.textContent?.trim() === label,
  );
  if (!option) {
    throw new Error(`no combobox option labelled ${label}`);
  }
  option.dispatchEvent(new MouseEvent('click', { bubbles: true }));
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

describe('Limits', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(role: 'admin' | 'user' = 'admin'): Promise<ComponentFixture<Limits>> {
    TestBed.configureTestingModule({
      imports: [Limits],
      providers: [{ provide: AuthService, useValue: authStub(role) }],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Limits);
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  /** The admin constructor also loads the account list first. */
  function queueAdminLoad(budgets: BudgetView[] = [], caps: TokenCapView[] = []): void {
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    queueLimits(fetchMock, budgets, caps);
    queueModels(fetchMock);
  }

  it('renders budgets with usage, limits, and reset times', async () => {
    queueAdminLoad([budgetFixture()], [capFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('$2.50 of $10.00');
    expect(text).toContain('2,500 of 100,000 tokens');
    expect(text).toContain('openai/gpt-5');
    expect(text).toContain('402 budget_exceeded');
    expect(text).toContain('429 token_cap_exceeded');
  });

  it('keeps stored limits when a draft is empty or unchanged (null keeps)', async () => {
    queueAdminLoad([budgetFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    expect(fetchMock.requests).toHaveLength(4);

    // Prefilled with the stored limit: unchanged means "keep".
    buttonByText(root, 'Save').click();
    await fixture.whenStable();
    fixture.detectChanges();

    // A cleared draft also means "keep": only Remove can delete a budget.
    typeInto(root, 'input[aria-label="USD limit for day"]', '');
    buttonByText(root, 'Save').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(4);
  });

  it('saves an edited budget with a CSRF-protected PUT', async () => {
    queueAdminLoad([budgetFixture()]);
    fetchMock.respond(jsonResponse(200, budgetFixture({ limit_usd: 25 })));
    queueLimits(fetchMock, [budgetFixture({ limit_usd: 25 })]);
    queueModels(fetchMock);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, 'input[aria-label="USD limit for day"]', '25');
    buttonByText(root, 'Save').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[4];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/users/u1/budgets/day');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ limit_usd: 25 });
    expect(root.textContent).toContain('$25.00');
  });

  it('removes a budget only after confirmation', async () => {
    queueAdminLoad([budgetFixture()]);
    fetchMock.respond(noContentResponse());
    queueLimits(fetchMock);
    queueModels(fetchMock);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Remove').click();
    fixture.detectChanges();
    expect(fetchMock.requests).toHaveLength(4);

    buttonByText(root, 'Confirm remove').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[4].method).toBe('DELETE');
    expect(fetchMock.requests[4].url).toBe('/api/users/u1/budgets/day');
  });

  it('creates a token cap for a model and period', async () => {
    queueAdminLoad();
    fetchMock.respond(jsonResponse(200, capFixture({ limit_tokens: 5000 })));
    queueLimits(fetchMock, [], [capFixture({ limit_tokens: 5000 })]);
    queueModels(fetchMock);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New cap').click();
    fixture.detectChanges();
    comboboxInto(root, '#cap-model', 'openai/gpt-5');
    typeInto(root, '#cap-limit', '5000');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[4];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/users/u1/token-caps/m1/day');
    expect(call.body).toEqual({ limit_tokens: 5000 });
  });

  it('shows a read-only view for non-admins without edit controls', async () => {
    queueLimits(fetchMock, [budgetFixture()], [capFixture()]);
    const fixture = await create('user');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Limits are managed by an administrator.');
    expect(root.querySelector('input[aria-label="USD limit for day"]')).toBeNull();
    expect(root.textContent).not.toContain('New cap');
    // Budgets and caps only: no admin-only catalog request.
    expect(fetchMock.requests).toHaveLength(2);
    expect(fetchMock.requests.map((call) => call.url)).not.toContain('/api/models');
  });

  it('shows a non-admin own budgets and caps instead of a forbidden banner', async () => {
    queueLimits(fetchMock, [budgetFixture()], [capFixture()]);
    const fixture = await create('user');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('$2.50 of $10.00');
    expect(root.textContent).toContain('2,500 of 100,000 tokens');
    expect(root.textContent).not.toContain('You may only read budgets and token caps');
  });

  it('keeps the limits when the admin-only model list fails', async () => {
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    queueLimits(fetchMock, [budgetFixture()]);
    fetchMock.respond(errorResponse(403, 'forbidden', 'admin only'));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('$2.50 of $10.00');
    expect(root.textContent).toContain('The model catalog is restricted to administrators.');
    expect(root.textContent).not.toContain('You may only read budgets and token caps');
  });

  it('names the forbidden limits state accurately when the budgets read fails', async () => {
    fetchMock.respond(jsonResponse(200, [userFixture()]));
    fetchMock.respond(errorResponse(403, 'forbidden', 'not your account'));
    fetchMock.respond(jsonResponse(200, []));
    queueModels(fetchMock);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain(
      'You may only read budgets and token caps for your own account.',
    );
  });
});
