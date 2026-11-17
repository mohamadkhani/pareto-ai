import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { FetchMock, jsonResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { UsageCounters, UsageGroup, UsageReport, MyUsageReport } from './usage.types';
import { Usage } from './usage';

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

function counters(overrides: Partial<UsageCounters> = {}): UsageCounters {
  return {
    requests: 2,
    input_tokens: 100,
    output_tokens: 50,
    cost_usd: 0.25,
    priced_requests: 1,
    unpriced_requests: 1,
    partial_requests: 0,
    refused_requests: 0,
    ...overrides,
  };
}

function myReport(): MyUsageReport {
  return {
    totals: counters({
      requests: 3,
      unpriced_requests: 2,
      partial_requests: 1,
      priced_requests: 0,
    }),
    days: [
      { day: '2026-01-01', ...counters() },
      {
        day: '2026-01-02',
        ...counters({ requests: 1, cost_usd: 0, unpriced_requests: 1, priced_requests: 0 }),
      },
    ],
  };
}

/** A report where four of seven requests were refused before forwarding. */
function refusedReport(): MyUsageReport {
  const row = counters({
    requests: 7,
    cost_usd: 0.4,
    priced_requests: 3,
    unpriced_requests: 0,
    partial_requests: 0,
    refused_requests: 4,
  });
  return { totals: row, days: [{ day: '2026-01-05', ...row }] };
}

function adminReport(): UsageReport {
  return {
    totals: counters(),
    groups: [
      {
        day: null,
        user_id: null,
        email: null,
        offering_id: 'off1',
        ...counters(),
      },
    ],
  };
}

function selectInto(root: HTMLElement, selector: string, value: string): void {
  const select = root.querySelector(selector);
  if (!(select instanceof HTMLSelectElement)) {
    throw new Error(`no select matching ${selector}`);
  }
  select.value = value;
  select.dispatchEvent(new Event('change'));
}

function typeInto(root: HTMLElement, selector: string, value: string): void {
  const input = root.querySelector(selector);
  if (!(input instanceof HTMLInputElement)) {
    throw new Error(`no input matching ${selector}`);
  }
  input.value = value;
  input.dispatchEvent(new Event('input'));
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

describe('Usage', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(role: 'admin' | 'user' = 'user'): Promise<ComponentFixture<Usage>> {
    TestBed.configureTestingModule({
      imports: [Usage],
      providers: [{ provide: AuthService, useValue: authStub(role) }],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Usage);
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  it('renders the self report with unpriced and partial counters', async () => {
    fetchMock.respond(jsonResponse(200, myReport()));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('2 unpriced');
    expect(text).toContain('1 partial');
    expect(text).toContain('2026-01-01');
    expect(text).toContain('2026-01-02');
    expect(text).toContain('$0.25');
    expect(text).toContain('100');
    // Row counters surface unpriced requests as badges too.
    expect(root.querySelectorAll('ui-badge.tone-danger').length).toBeGreaterThanOrEqual(1);
  });

  it('renders refused requests in the totals row', async () => {
    fetchMock.respond(jsonResponse(200, refusedReport()));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    // Refusals are counted as requests: 7 break down into 4 refused + 3 priced.
    expect(text).toContain('4 refused');
    expect(text).toContain('3 priced');
    expect(text).not.toContain('all priced');
  });

  it('shows refused requests as a breakdown of the day rows', async () => {
    fetchMock.respond(jsonResponse(200, refusedReport()));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Refused');
    const row = Array.from(root.querySelectorAll('tbody tr')).find((candidate) =>
      candidate.textContent?.includes('2026-01-05'),
    );
    expect(row).toBeDefined();
    const cells = row?.querySelectorAll('td');
    expect(cells?.[1]?.textContent?.trim()).toBe('7');
    expect(row?.querySelector('ui-badge.tone-warning')?.textContent?.trim()).toBe('4');
  });

  it('sends the date range when applying', async () => {
    fetchMock.respond(jsonResponse(200, myReport()));
    fetchMock.respond(jsonResponse(200, myReport()));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, '#usage-from', '2026-01-01');
    typeInto(root, '#usage-to', '2026-01-31');
    buttonByText(root, 'Apply').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const url = new URL(fetchMock.requests[1].url, 'https://example.test');
    expect(url.pathname).toBe('/api/usage/me');
    expect(url.searchParams.get('from')).toBe('2026-01-01');
    expect(url.searchParams.get('to')).toBe('2026-01-31');
  });

  it('switches the admin aggregate between groupings', async () => {
    fetchMock.respond(jsonResponse(200, [{}]));
    fetchMock.respond(jsonResponse(200, myReport()));
    fetchMock.respond(jsonResponse(200, adminReport()));
    fetchMock.respond(jsonResponse(200, adminReport()));
    const fixture = await create('admin');
    const root = fixture.nativeElement as HTMLElement;

    selectInto(root, '#usage-scope', 'all');
    await fixture.whenStable();
    fixture.detectChanges();

    const aggregateUrl = new URL(fetchMock.requests[2].url, 'https://example.test');
    expect(aggregateUrl.pathname).toBe('/api/usage');
    expect(aggregateUrl.searchParams.get('user')).toBeNull();

    buttonByText(root, 'Offering').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const groupedUrl = new URL(fetchMock.requests[3].url, 'https://example.test');
    expect(groupedUrl.searchParams.get('group_by')).toBe('offering');
    expect(root.textContent).toContain('off1');
    expect(root.textContent).toContain('Offering');
  });
});
