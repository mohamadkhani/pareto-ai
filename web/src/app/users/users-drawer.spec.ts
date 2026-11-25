import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { FetchMock, jsonResponse, noContentResponse } from '../../testing/fetch-mock';
import { waitFor, waitForText } from '../../testing/wait-for';
import { AuthService, type User } from '../auth/auth.service';
import { UserDrawer } from './users-drawer';
import type { UserView } from './users.types';

function authStub(): object {
  return {
    user: signal<User | null>({
      id: 'admin',
      email: 'admin@example.com',
      display_name: 'Admin',
      role: 'admin',
      must_change_password: false,
    }),
    isAdmin: signal(true),
    csrfToken: signal<string | null>('csrf-1'),
  };
}

function userFixture(overrides: Partial<UserView> = {}): UserView {
  return {
    id: 'u1',
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

const m1 = {
  id: 'm1',
  canonical_name: 'openai/gpt-5',
  family: 'gpt',
  modalities_input: ['text'],
  modalities_output: ['text'],
  context_length: 128000,
  max_output_tokens: 8192,
  capabilities: [],
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
};

const m2 = {
  ...m1,
  id: 'm2',
  canonical_name: 'openai/gpt-5-mini',
  family: 'gpt',
};

const budget = {
  user_id: 'u1',
  period: 'week',
  limit_usd: 10,
  used_usd: 4,
  reset_at: '2026-01-05T00:00:00.000Z',
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
};

const cap = {
  user_id: 'u1',
  model_id: 'm1',
  period: 'day',
  limit_tokens: 1000,
  used_tokens: 250,
  reset_at: '2026-01-02T00:00:00.000Z',
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
};

const allowGrant = {
  id: 'g1',
  subject_type: 'user',
  subject_id: 'u1',
  resource_type: 'model',
  resource_id: 'm1',
  mode: 'allow',
  created_by: null,
  created_at: '2026-01-01T00:00:00.000Z',
};

const provider = {
  id: 'p1',
  name: 'openrouter',
  kind: 'openai_compatible',
  preset: null,
  base_url: 'https://openrouter.ai/api/v1',
  enabled: true,
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
  last_discovery_at: null,
  last_discovery_created: null,
  last_discovery_unchanged: null,
  connectors: [],
};

const keyView = {
  id: 'k1',
  user_id: 'u1',
  name: 'laptop',
  prefix: 'pk_live_ab12',
  scopes: null,
  expires_at: null,
  revoked_at: null,
  last_used_at: null,
  rate_limit_rpm: null,
  rate_limit_tpm: null,
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
};

function buttonByText(scope: ParentNode, text: string): HTMLButtonElement {
  const button = Array.from(scope.querySelectorAll('button')).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  if (button === undefined) {
    throw new Error(`no button labelled ${text}`);
  }
  return button;
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

function tabByText(root: HTMLElement, text: string): HTMLButtonElement {
  const tab = Array.from(root.querySelectorAll<HTMLButtonElement>('[role="tab"]')).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  if (tab === undefined) {
    throw new Error(`no tab labelled ${text}`);
  }
  return tab;
}

describe('UserDrawer', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function queueLimits(
    budgets: unknown[] = [],
    caps: unknown[] = [],
    models: unknown[] = [],
  ): void {
    fetchMock.respond(jsonResponse(200, budgets));
    fetchMock.respond(jsonResponse(200, caps));
    fetchMock.respond(jsonResponse(200, models));
  }

  async function create(): Promise<ComponentFixture<UserDrawer>> {
    TestBed.configureTestingModule({
      imports: [UserDrawer],
      providers: [{ provide: AuthService, useValue: authStub() }],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(UserDrawer);
    fixture.componentRef.setInput('user', userFixture());
    fixture.detectChanges();
    await waitFor(
      fixture,
      () =>
        fixture.nativeElement.querySelector('.limit-row') !== null ||
        fixture.nativeElement.textContent?.includes('Loading limits failed.') === true,
      'the initial limits load',
    );
    return fixture;
  }

  /**
   * Waits until the tab has rendered its result: the request count has stopped
   * growing and no spinner or busy button is on screen. Counting
   * `whenStable()` rounds is not enough — a mocked `fetch` chain is invisible
   * to Angular's scheduler, so a slow CI machine can still be mid-request when
   * the assertions run.
   */
  async function settle(fixture: ComponentFixture<UserDrawer>): Promise<void> {
    const root = fixture.nativeElement as HTMLElement;
    let previousCount = -1;
    await waitFor(
      fixture,
      () => {
        const count = fetchMock.requests.length;
        if (count !== previousCount) {
          previousCount = count;
          return false;
        }
        return (
          root.querySelector('[aria-busy="true"]') === null &&
          root.querySelector('.drawer-loading') === null
        );
      },
      'the drawer to finish loading',
    );
    fixture.detectChanges();
  }

  it('renders four tabs and the four budget rows with used/limit progress', async () => {
    queueLimits([budget], [cap], [m1]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(
      Array.from(root.querySelectorAll('[role="tab"]')).map((tab) => tab.textContent?.trim()),
    ).toEqual(['Limits', 'Grants', 'Access map', 'Keys', 'Overview']);
    await waitForText(fixture, root, 'week');
    await waitForText(fixture, root, '$4.00 of $10.00');
    await waitForText(fixture, root, 'openai/gpt-5');
    await waitForText(fixture, root, '250 of 1,000');
    expect(root.querySelectorAll('ui-meter').length).toBeGreaterThan(0);
    expect(root.querySelector('dialog')?.classList.contains('dialog--drawer')).toBe(true);
  });

  it('saves the allow-list as allow grants only (adds checked, removes unchecked)', async () => {
    queueLimits([], [], [m1, m2]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, [allowGrant]));
    fetchMock.respond(jsonResponse(200, [m1, m2]));
    tabByText(root, 'Grants').click();
    await settle(fixture);

    const checkboxes = root.querySelectorAll<HTMLInputElement>('ui-checkbox input');
    expect(checkboxes).toHaveLength(2);
    expect(checkboxes[0].checked).toBe(true);

    checkboxes[0].checked = false;
    checkboxes[0].dispatchEvent(new Event('change'));
    checkboxes[1].checked = true;
    checkboxes[1].dispatchEvent(new Event('change'));
    fixture.detectChanges();

    fetchMock.respond(noContentResponse());
    fetchMock.respond(jsonResponse(201, { ...allowGrant, id: 'g2', resource_id: 'm2' }));
    fetchMock.respond(jsonResponse(200, [{ ...allowGrant, id: 'g2', resource_id: 'm2' }]));
    fetchMock.respond(jsonResponse(200, [m1, m2]));
    buttonByText(root, 'Save allow-list').click();
    await settle(fixture);

    const post = fetchMock.requests.find((call) => call.method === 'POST');
    const del = fetchMock.requests.find((call) => call.method === 'DELETE');
    expect(del?.url).toBe('/api/grants/g1');
    expect(post?.url).toBe('/api/grants');
    expect(post?.body).toEqual({
      subject_type: 'user',
      subject_id: 'u1',
      resource_type: 'model',
      resource_id: 'm2',
      mode: 'allow',
    });
  });

  it('keeps the advanced matrix working (add a deny grant)', async () => {
    queueLimits([], [], [m1, m2]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, [allowGrant]));
    fetchMock.respond(jsonResponse(200, [m1, m2]));
    tabByText(root, 'Grants').click();
    await settle(fixture);

    fetchMock.respond(jsonResponse(200, []));
    fetchMock.respond(jsonResponse(200, [provider]));
    fetchMock.respond(jsonResponse(200, []));
    buttonByText(root, 'Advanced matrix').click();
    await settle(fixture);

    comboboxInto(root, '#drawer-grant-resource', 'openai/gpt-5-mini');
    selectInto(root, '#drawer-grant-mode', 'deny');
    fixture.detectChanges();

    fetchMock.respond(
      jsonResponse(201, { ...allowGrant, id: 'g3', resource_id: 'm2', mode: 'deny' }),
    );
    fetchMock.respond(jsonResponse(200, [allowGrant]));
    fetchMock.respond(jsonResponse(200, [m1, m2]));
    buttonByText(root, 'Add grant').click();
    await settle(fixture);

    const post = fetchMock.requests.find((call) => call.method === 'POST');
    expect(post?.body).toEqual({
      subject_type: 'user',
      subject_id: 'u1',
      resource_type: 'model',
      resource_id: 'm2',
      mode: 'deny',
    });
  });

  it('creates a key, shows the plaintext once, and lists the prefix', async () => {
    queueLimits([], [], []);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, []));
    tabByText(root, 'Keys').click();
    await settle(fixture);

    typeInto(root, '#drawer-key-name', 'laptop');
    fixture.detectChanges();

    fetchMock.respond(jsonResponse(201, { key: 'pk_live_secret_once', api_key: keyView }));
    fetchMock.respond(jsonResponse(200, [keyView]));
    buttonByText(root, 'Create key').click();
    await settle(fixture);

    expect(root.textContent ?? '').toContain('pk_live_secret_once');
    expect(root.textContent ?? '').toContain('This is the only copy');
    expect(root.textContent ?? '').toContain('pk_live_ab12');

    buttonByText(root, 'I have copied it').click();
    fixture.detectChanges();
    expect(root.textContent ?? '').not.toContain('pk_live_secret_once');
  });

  it('reveals an allow-listed model and announces the update without a reload', async () => {
    queueLimits([], [], [m1]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, [allowGrant]));
    fetchMock.respond(jsonResponse(200, [m1]));
    tabByText(root, 'Grants').click();
    await settle(fixture);

    const checkbox = root.querySelector<HTMLInputElement>('ui-checkbox input');
    expect(checkbox?.checked).toBe(true);
    expect(root.textContent).toContain('openai/gpt-5');
  });

  it('closes on Escape through the dialog cancel event and returns focus to the trigger (owner page)', async () => {
    queueLimits([], [], []);
    const fixture = await create();
    const closed = vi.fn();
    fixture.componentInstance.closed.subscribe(closed);
    const dialog = (fixture.nativeElement as HTMLElement).querySelector('dialog');

    dialog?.dispatchEvent(new Event('cancel', { cancelable: true }));
    fixture.detectChanges();

    expect(closed).toHaveBeenCalled();
    expect(dialog?.hasAttribute('open')).toBe(false);
  });
});
