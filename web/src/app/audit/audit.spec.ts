import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { FetchMock, errorResponse, jsonResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import { Audit } from './audit';
import type { AuditEntry } from './audit.service';

function authStub(role: 'admin' | 'user'): object {
  return {
    user: signal<User | null>({
      id: 'u1',
      email: 'someone@example.com',
      display_name: 'Someone',
      role,
      must_change_password: false,
    }),
    isAdmin: signal(role === 'admin'),
    csrfToken: signal<string | null>('csrf-1'),
  };
}

function entry(i: number): AuditEntry {
  return {
    id: i + 1,
    actor_user_id: 'u1',
    action: 'auth.login',
    target_type: 'user',
    target_id: `u${i}`,
    detail_json: null,
    ip: '127.0.0.1',
    insecure: false,
    occurred_at: new Date(Date.UTC(2026, 0, 1, 0, 0, 0, i)).toISOString(),
  };
}

describe('Audit', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(role: 'admin' | 'user' = 'admin'): Promise<ComponentFixture<Audit>> {
    TestBed.configureTestingModule({
      imports: [Audit],
      providers: [{ provide: AuthService, useValue: authStub(role) }],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Audit);
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  it('shows a forbidden state for non-admins without calling the API', async () => {
    const fixture = await create('user');
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('.state')?.textContent).toContain('Forbidden');
    expect(fetchMock.requests).toHaveLength(0);
  });

  it('renders entries and expands the detail JSON on toggle', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [{ ...entry(0), action: 'auth.login_failed', detail_json: '{"reason":"bad"}' }],
      }),
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('tbody')?.textContent).toContain('auth.login_failed');
    expect(root.querySelector('.detail')).toBeNull();

    root.querySelector<HTMLButtonElement>('button.toggle')?.click();
    fixture.detectChanges();

    const detail = root.querySelector('.detail')?.textContent ?? '';
    expect(detail).toContain('"reason": "bad"');
  });

  it('flags entries done in insecure mode', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        entries: [
          { ...entry(0), action: 'provider.create', insecure: true },
          { ...entry(1), action: 'provider.update' },
        ],
      }),
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const rows = root.querySelectorAll('tbody tr');
    expect(rows[0]?.textContent).toContain('insecure');
    expect(rows[1]?.textContent).not.toContain('insecure');
  });

  it('shows an empty state when there are no entries', async () => {
    fetchMock.respond(jsonResponse(200, { entries: [] }));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('.state')?.textContent).toContain('No entries yet');
  });

  it('pages with the composite cursor and uses before_id for tied timestamps', async () => {
    const tiedAt = '2026-01-01T00:00:00.000Z';
    const firstPage = Array.from({ length: 50 }, (_, i) =>
      // The last three entries share one millisecond; only `before_id` orders them.
      i >= 47 ? { ...entry(i), occurred_at: tiedAt } : entry(i),
    );
    fetchMock.respond(
      jsonResponse(200, {
        entries: firstPage,
        next_cursor: { before: tiedAt, before_id: firstPage[49].id },
      }),
    );
    fetchMock.respond(jsonResponse(200, { entries: [entry(50)] }));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelectorAll('tbody tr').length).toBe(50);
    const moreButton = root.querySelector<HTMLButtonElement>('.more button');
    expect(moreButton).not.toBeNull();
    moreButton?.click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(2);
    const url = new URL(fetchMock.requests[1].url, 'https://example.test');
    expect(url.pathname).toBe('/api/audit');
    expect(url.searchParams.get('limit')).toBe('50');
    expect(url.searchParams.get('before')).toBe(tiedAt);
    expect(url.searchParams.get('before_id')).toBe(String(firstPage[49].id));
    expect(root.querySelectorAll('tbody tr').length).toBe(51);
  });

  it('stops paging when a page comes back short', async () => {
    const firstPage = Array.from({ length: 50 }, (_, i) => entry(i));
    fetchMock.respond(
      jsonResponse(200, {
        entries: firstPage,
        next_cursor: { before: firstPage[49].occurred_at, before_id: firstPage[49].id },
      }),
    );
    // A short page ends paging even if the server still sends a cursor.
    fetchMock.respond(
      jsonResponse(200, {
        entries: [entry(50)],
        next_cursor: { before: entry(50).occurred_at, before_id: entry(50).id },
      }),
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    root.querySelector<HTMLButtonElement>('.more button')?.click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(2);
    expect(root.querySelectorAll('tbody tr').length).toBe(51);
    expect(root.querySelector('.more')).toBeNull();
  });

  it('prefers the server next_cursor over deriving one from the last entry', async () => {
    const firstPage = Array.from({ length: 50 }, (_, i) => entry(i));
    // Deliberately unlike `firstPage[49]`, so deriving from the last entry would fail.
    const serverCursor = { before: '2025-12-31T23:59:59.000Z', before_id: 999 };
    fetchMock.respond(jsonResponse(200, { entries: firstPage, next_cursor: serverCursor }));
    fetchMock.respond(jsonResponse(200, { entries: [] }));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    root.querySelector<HTMLButtonElement>('.more button')?.click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(2);
    const url = new URL(fetchMock.requests[1].url, 'https://example.test');
    expect(url.searchParams.get('before')).toBe(serverCursor.before);
    expect(url.searchParams.get('before_id')).toBe(String(serverCursor.before_id));
    expect(root.querySelectorAll('tbody tr').length).toBe(50);
  });

  it('shows an error state when loading fails', async () => {
    fetchMock.respond(errorResponse(500, 'internal', 'Reading the audit log failed.'));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('.ui-banner.tone-danger')?.textContent).toContain(
      'Reading the audit log failed.',
    );
  });
});
