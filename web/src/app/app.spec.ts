import { TestBed } from '@angular/core/testing';
import { App } from './app';
import { FetchMock, errorResponse } from '../testing/fetch-mock';

describe('App', () => {
  let fetchMock: FetchMock;

  beforeEach(async () => {
    fetchMock = new FetchMock();
    fetchMock.respond(errorResponse(401, 'unauthorized', 'Sign in required.'));
    vi.stubGlobal('fetch', fetchMock.fetch);
    await TestBed.configureTestingModule({
      imports: [App],
    }).compileComponents();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('should create the app', () => {
    const fixture = TestBed.createComponent(App);
    expect(fixture.componentInstance).toBeTruthy();
  });

  it('restores the session on startup', async () => {
    const fixture = TestBed.createComponent(App);
    await fixture.whenStable();

    expect(fetchMock.requests.map((call) => call.url)).toContain('/api/auth/me');
  });
});
