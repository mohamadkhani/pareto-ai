import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ApiError } from '../api';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { AuthService } from '../auth/auth.service';
import { GrantsService } from './grants.service';
import type { GrantView } from './grants.types';

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

describe('GrantsService', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
    TestBed.configureTestingModule({
      providers: [
        { provide: AuthService, useValue: { csrfToken: signal<string | null>('csrf-1') } },
      ],
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('lists every grant unfiltered', async () => {
    fetchMock.respond(jsonResponse(200, [grantFixture()]));
    const service = TestBed.inject(GrantsService);

    await service.list();

    expect(fetchMock.requests[0].url).toBe('/api/grants');
  });

  it('narrows the list to one subject with query parameters', async () => {
    fetchMock.respond(jsonResponse(200, []));
    const service = TestBed.inject(GrantsService);

    await service.list({ subject_type: 'api_key', subject_id: 'k1' });

    const url = new URL(fetchMock.requests[0].url, 'https://example.test');
    expect(url.searchParams.get('subject_type')).toBe('api_key');
    expect(url.searchParams.get('subject_id')).toBe('k1');
  });

  it('creates a grant with the CSRF header', async () => {
    fetchMock.respond(jsonResponse(201, grantFixture()));
    const service = TestBed.inject(GrantsService);

    await service.create({
      subject_type: 'user',
      subject_id: 'u2',
      resource_type: 'model',
      resource_id: 'm1',
      mode: 'allow',
    });

    const call = fetchMock.requests[0];
    expect(call.method).toBe('POST');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      subject_type: 'user',
      subject_id: 'u2',
      resource_type: 'model',
      resource_id: 'm1',
      mode: 'allow',
    });
  });

  it('surfaces the 409 duplicate as the conflict code', async () => {
    fetchMock.respond(
      errorResponse(409, 'conflict', 'a grant for this subject and resource already exists'),
    );
    const service = TestBed.inject(GrantsService);

    const error: unknown = await service
      .create({
        subject_type: 'user',
        subject_id: 'u2',
        resource_type: 'model',
        resource_id: 'm1',
        mode: 'allow',
      })
      .catch((err: unknown) => err);

    expect(error).toBeInstanceOf(ApiError);
    if (error instanceof ApiError) {
      expect(error.status).toBe(409);
      expect(error.code).toBe('conflict');
    }
  });

  it('removes a grant with a CSRF-protected DELETE', async () => {
    fetchMock.respond(noContentResponse());
    const service = TestBed.inject(GrantsService);

    await service.remove('g1');

    const call = fetchMock.requests[0];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/grants/g1');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
  });
});
