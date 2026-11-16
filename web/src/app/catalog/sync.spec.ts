import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { FetchMock, jsonResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { SyncOutcome, SyncRun } from './catalog.types';
import { Sync } from './sync';

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

function runFixture(overrides: Partial<SyncRun> = {}): SyncRun {
  return {
    id: 'r1',
    source: 'openrouter',
    status: 'success',
    model_count: 42,
    started_at: '2026-01-01T00:00:00.000Z',
    finished_at: '2026-01-01T00:00:02.000Z',
    detail: null,
    ...overrides,
  };
}

function outcomeFixture(overrides: Partial<SyncOutcome> = {}): SyncOutcome {
  return {
    run_id: 'r1',
    status: 'success',
    model_count: 42,
    stale: false,
    started_at: '2026-01-01T00:00:00.000Z',
    finished_at: '2026-01-01T00:00:02.000Z',
    error: null,
    ...overrides,
  };
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

describe('Sync', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  /** Builds the page against a status response with the given last run. */
  async function create(
    lastRun: SyncRun | null,
    role: 'admin' | 'user' = 'admin',
  ): Promise<ComponentFixture<Sync>> {
    fetchMock.respond(
      jsonResponse(200, { last_run: lastRun, snapshot_stale: true, ttl_hours: 24 }),
    );
    TestBed.configureTestingModule({
      imports: [Sync],
      providers: [{ provide: AuthService, useValue: authStub(role) }],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Sync);
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  it('shows a forbidden state for non-admins without calling the API', async () => {
    const fixture = await create(null, 'user');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Forbidden');
    expect(fetchMock.requests).toHaveLength(0);
  });

  it('shows the empty state before the first sync', async () => {
    const fixture = await create(null);
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('No sync yet');
  });

  it('renders the last run with status, model count and times', async () => {
    const fixture = await create(runFixture());
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('success');
    expect(text).toContain('42');
    expect(text).toContain(new Date('2026-01-01T00:00:00.000Z').toLocaleString());
    expect(root.querySelector('.badge')?.textContent).toContain('success');
  });

  it('shows the stale-if-error banner when the last run kept stale prices', async () => {
    const fixture = await create(
      runFixture({ status: 'stale-if-error', detail: { error: 'upstream timed out' } }),
    );
    const root = fixture.nativeElement as HTMLElement;

    const banner = root.querySelector('.banner.warn')?.textContent ?? '';
    expect(banner).toContain('may be stale');
    expect(banner).toContain('upstream timed out');
    expect(root.querySelector('.banner.error')).toBeNull();
  });

  it('shows the error state when the run failed outright', async () => {
    const fixture = await create(
      runFixture({
        status: 'error',
        model_count: 0,
        detail: { error: 'upstream returned 500' },
      }),
    );
    const root = fixture.nativeElement as HTMLElement;

    const banner = root.querySelector('.banner.error')?.textContent ?? '';
    expect(banner).toContain('no snapshot was kept');
    expect(banner).toContain('upstream returned 500');
    expect(root.textContent).toContain('error');
  });

  it('renders a run without detail without crashing', async () => {
    const fixture = await create(runFixture({ status: 'error', model_count: 0 }));
    const root = fixture.nativeElement as HTMLElement;

    const banner = root.querySelector('.banner.error')?.textContent ?? '';
    expect(banner).toContain('no snapshot was kept');
    expect(root.textContent).toContain('—');
  });

  it('triggers a sync and shows the recorded run', async () => {
    const fixture = await create(null);
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(jsonResponse(200, outcomeFixture({ run_id: 'r2', model_count: 55 })));
    buttonByText(root, 'Sync OpenRouter').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[1];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/sync/openrouter');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toBeNull();
    expect(root.textContent).toContain('55');
  });

  it('renders a failed trigger (blocked upstream) as an error run, not a crash', async () => {
    const fixture = await create(null);
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(
      jsonResponse(
        200,
        outcomeFixture({
          run_id: 'r3',
          status: 'error',
          model_count: 0,
          error: 'error sending request for url (https://openrouter.ai/api/v1/models)',
        }),
      ),
    );
    buttonByText(root, 'Sync OpenRouter').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const banner = root.querySelector('.banner.error')?.textContent ?? '';
    expect(banner).toContain('no snapshot was kept');
    expect(banner).toContain('error sending request');
    expect(root.textContent).toContain('0');
  });
});
