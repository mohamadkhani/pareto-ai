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
import type { BenchmarkScore, QualityOverride } from './routing.types';
import { Benchmarks } from './benchmarks';

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
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

function scoreFixture(overrides: Partial<BenchmarkScore> = {}): BenchmarkScore {
  return {
    model_id: 'm1',
    source: 'lmarena',
    category: 'text',
    score: 1433,
    ci: 12,
    rank: 1,
    fetched_at: '2026-01-01T00:00:00.000Z',
    source_url: 'https://lmarena.example/leaderboard',
    ...overrides,
  };
}

function overrideFixture(overrides: Partial<QualityOverride> = {}): QualityOverride {
  return { model_id: 'm1', score: 1400, ...overrides };
}

/** Queues the three GETs `reload()` issues: models, scores, overrides. */
function queueLoad(
  fetchMock: FetchMock,
  scores: BenchmarkScore[] = [],
  overrides: QualityOverride[] = [],
): void {
  fetchMock.respond(jsonResponse(200, [modelFixture()]));
  fetchMock.respond(jsonResponse(200, scores));
  fetchMock.respond(jsonResponse(200, overrides));
}

function typeInto(root: HTMLElement, selector: string, value: string): void {
  const input = root.querySelector(selector);
  if (!(input instanceof HTMLInputElement) && !(input instanceof HTMLTextAreaElement)) {
    throw new Error(`no input matching ${selector}`);
  }
  input.value = value;
  // Emulate typing and leaving the field: fields listen to `input`, `change`, or both.
  input.dispatchEvent(new Event('input'));
  input.dispatchEvent(new Event('change'));
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

describe('Benchmarks', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(role: 'admin' | 'user' = 'admin'): Promise<ComponentFixture<Benchmarks>> {
    TestBed.configureTestingModule({
      imports: [Benchmarks],
      providers: [{ provide: AuthService, useValue: authStub(role) }],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Benchmarks);
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

  it('imports a leaderboard document and shows the four counters', async () => {
    queueLoad(fetchMock);
    fetchMock.respond(
      jsonResponse(200, { inserted: 2, updated: 1, unchanged: 3, skipped_unknown_model: 4 }),
    );
    fetchMock.respond(jsonResponse(200, [scoreFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, '#benchmark-document', '{"source": "lmarena", "entries": []}');
    buttonByText(root, 'Import').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[3];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/benchmarks/import');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ source: 'lmarena', entries: [] });

    const text = root.textContent ?? '';
    expect(text).toContain('Import finished.');
    expect(text).toContain('Inserted');
    expect(text).toContain('Skipped (unknown model)');
    expect(root.querySelector('.counters')?.textContent ?? '').toContain('4');
    // The import re-reads the score table.
    expect(fetchMock.requests[4].url).toBe('/api/benchmarks');
  });

  it('rejects a document that is not JSON before calling the API', async () => {
    queueLoad(fetchMock);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, '#benchmark-document', 'not json at all');
    buttonByText(root, 'Import').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(3);
    expect(root.textContent).toContain('That is not valid JSON.');
  });

  it('lists imported scores with their provenance', async () => {
    queueLoad(fetchMock, [scoreFixture()]);
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    const text = root.textContent ?? '';
    expect(text).toContain('lmarena');
    expect(text).toContain('text');
    expect(text).toContain('1433.0');
    expect(text).toContain('#1');
  });

  it('surfaces the 400 scale-range error on an override and keeps the editor', async () => {
    queueLoad(fetchMock, [scoreFixture()]);
    fetchMock.respond(
      errorResponse(
        400,
        'invalid_request',
        'score 0.99 is outside the imported range 851–1933; check the scale',
      ),
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    typeInto(root, '#override-m1', '0.99');
    buttonByText(root, 'Set override').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[3];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/models/m1/quality-override');
    expect(call.body).toEqual({ score: 0.99 });
    expect(root.querySelector('.row-error')?.textContent ?? '').toContain(
      'outside the imported range 851–1933',
    );
    expect(root.querySelector('#override-m1')).not.toBeNull();
  });

  it('removes a quality override after confirmation', async () => {
    queueLoad(fetchMock, [], [overrideFixture()]);
    fetchMock.respond(noContentResponse());
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Remove override').click();
    fixture.detectChanges();
    buttonByText(root, 'Confirm remove').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[3];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/models/m1/quality-override');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(root.textContent).toContain('none');
  });
});
