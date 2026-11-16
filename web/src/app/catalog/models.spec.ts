import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import type { Model } from './catalog.types';
import { Models } from './models';

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
    modalities_input: ['text', 'image'],
    modalities_output: ['text'],
    context_length: 400000,
    max_output_tokens: 128000,
    capabilities: ['tools', 'reasoning'],
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

describe('Models', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(role: 'admin' | 'user' = 'admin'): Promise<ComponentFixture<Models>> {
    TestBed.configureTestingModule({
      imports: [Models],
      providers: [{ provide: AuthService, useValue: authStub(role) }],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Models);
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

  it('lists models with modalities, limits and capabilities', async () => {
    fetchMock.respond(jsonResponse(200, [modelFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(text).toContain('openai/gpt-5');
    expect(text).toContain('gpt-5');
    expect(text).toContain('text, image → text');
    expect(text).toContain('400000');
    expect(text).toContain('128000');
    expect(text).toContain('tools, reasoning');
  });

  it('requires a canonical name before calling the API', async () => {
    fetchMock.respond(jsonResponse(200, []));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New model').click();
    fixture.detectChanges();
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(1);
    expect(root.textContent).toContain('Canonical name is required.');
  });

  it('creates a model with parsed modalities and capabilities', async () => {
    fetchMock.respond(jsonResponse(200, []));
    fetchMock.respond(jsonResponse(201, modelFixture()));
    fetchMock.respond(jsonResponse(200, [modelFixture()]));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New model').click();
    fixture.detectChanges();
    typeInto(root, '#model-name', ' openai/gpt-5 ');
    typeInto(root, '#model-family', 'gpt-5');
    typeInto(root, '#model-input-modalities', 'text, image');
    typeInto(root, '#model-output-modalities', 'text');
    typeInto(root, '#model-context', '400000');
    typeInto(root, '#model-capabilities', 'tools, reasoning');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[1];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/models');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      canonical_name: 'openai/gpt-5',
      family: 'gpt-5',
      modalities_input: ['text', 'image'],
      modalities_output: ['text'],
      context_length: 400000,
      max_output_tokens: null,
      capabilities: ['tools', 'reasoning'],
    });
  });

  it('rejects non-positive limits before calling the API', async () => {
    fetchMock.respond(jsonResponse(200, []));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New model').click();
    fixture.detectChanges();
    typeInto(root, '#model-name', 'openai/gpt-5');
    typeInto(root, '#model-context', '0');
    submit(root);
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests).toHaveLength(1);
    expect(root.textContent).toContain('Use a whole number of 1 or more.');
  });

  it('deletes a model only after confirmation', async () => {
    fetchMock.respond(jsonResponse(200, [modelFixture()]));
    fetchMock.respond(noContentResponse());
    fetchMock.respond(jsonResponse(200, []));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Delete').click();
    fixture.detectChanges();
    expect(fetchMock.requests).toHaveLength(1);

    buttonByText(root, 'Confirm delete').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(fetchMock.requests[1].method).toBe('DELETE');
    expect(fetchMock.requests[1].url).toBe('/api/models/m1');
    expect(root.textContent).toContain('No models yet');
  });

  it('renders structured capabilities from a synced model without crashing', async () => {
    fetchMock.respond(
      jsonResponse(200, [
        modelFixture({
          capabilities: { display_name: 'GPT-5', supported_parameters: ['tools'] },
        }),
      ]),
    );
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('display_name');
  });

  it('shows the server message when loading fails', async () => {
    fetchMock.respond(errorResponse(500, 'internal', 'Reading models failed.'));
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.textContent).toContain('Reading models failed.');
  });
});
