import { signal } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { FetchMock, errorResponse, jsonResponse } from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import { MixProfiles } from './mix-profiles';
import type { MixProfile, PricingSettings } from './mix.types';

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

function profile(overrides: Partial<MixProfile> = {}): MixProfile {
  return {
    id: 'mix-code-agent',
    name: 'code-agent',
    kind: 'preset',
    input_share: 8.1,
    output_share: 0.4,
    cache_read_share: 91.5,
    cache_write_share: 0,
    reasoning_share: 0,
    is_builtin: true,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

const chat = profile({
  id: 'mix-chat',
  name: 'chat',
  input_share: 74,
  output_share: 26,
  cache_read_share: 0,
});

const team = profile({
  id: 'mix-team',
  name: 'team',
  kind: 'custom',
  input_share: 5,
  output_share: 1,
  cache_read_share: 94,
  is_builtin: false,
});

function settings(overrides: Partial<PricingSettings> = {}): PricingSettings {
  return {
    default_mix_profile: null,
    effective_profile: { profile_id: 'mix-code-agent', profile_name: 'code-agent' },
    env_override: null,
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

function selectInto(root: HTMLElement, selector: string, value: string): void {
  const select = root.querySelector(selector);
  if (!(select instanceof HTMLSelectElement)) {
    throw new Error(`no select matching ${selector}`);
  }
  select.value = value;
  select.dispatchEvent(new Event('change'));
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

describe('MixProfiles', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  async function create(
    profiles: MixProfile[] = [profile(), chat, team],
    pricing: PricingSettings = settings(),
  ): Promise<ComponentFixture<MixProfiles>> {
    fetchMock.respond(jsonResponse(200, profiles));
    fetchMock.respond(jsonResponse(200, pricing));
    TestBed.configureTestingModule({
      imports: [MixProfiles],
      providers: [{ provide: AuthService, useValue: authStub('admin') }],
    });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(MixProfiles);
    fixture.componentRef.setInput('open', true);
    fixture.detectChanges();
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    await fixture.whenStable();
    fixture.detectChanges();
    return fixture;
  }

  it('lists profiles with a built-in badge and the effective profile', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const text = root.textContent ?? '';

    expect(fetchMock.requests[0].url).toBe('/api/mix-profiles');
    expect(fetchMock.requests[1].url).toBe('/api/settings/pricing');
    expect(text).toContain('code-agent');
    expect(text).toContain('built-in');
    expect(text).toContain('8.1 / 0.4 / 91.5 (%)');
    expect(text).toContain('Effective now:');
    expect(text).toContain('(mix-code-agent)');
    // Built-ins can only be cloned; custom profiles can be edited and deleted.
    expect(buttonByText(root, 'Clone')).toBeTruthy();
    expect(buttonByText(root, 'Edit')).toBeTruthy();
    expect(buttonByText(root, 'Delete')).toBeTruthy();
  });

  it('sets the default through the pricing setting and reflects it immediately', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    fetchMock.respond(
      jsonResponse(
        200,
        settings({
          default_mix_profile: 'mix-chat',
          effective_profile: { profile_id: 'mix-chat', profile_name: 'chat' },
        }),
      ),
    );
    selectInto(root, '#mix-default-profile', 'mix-chat');
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[2];
    expect(call.method).toBe('PUT');
    expect(call.url).toBe('/api/settings/pricing');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ default_mix_profile: 'mix-chat' });
    expect(root.textContent).toContain('Effective now:');
    expect(root.textContent).toContain('(mix-chat)');
  });

  it('locks the default picker when the environment overrides it', async () => {
    const fixture = await create([profile(), chat], settings({ env_override: 'chat' }));
    const root = fixture.nativeElement as HTMLElement;

    const picker = root.querySelector<HTMLSelectElement>('#mix-default-profile');
    expect(picker?.disabled).toBe(true);
    const text = root.textContent ?? '';
    expect(text).toContain('GATEWAY_TOKEN_MIX_PROFILE=chat');
    expect(text).toContain('wins over the stored default');
  });

  it('creates a custom profile with its five shares', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New profile').click();
    fixture.detectChanges();
    typeInto(root, '#mix-profile-name', 'team-measured');
    selectInto(root, '#mix-profile-kind', 'measured');
    typeInto(root, '#mix-share-input', '12');
    typeInto(root, '#mix-share-output', '1');
    typeInto(root, '#mix-share-cache_read', '87');
    typeInto(root, '#mix-share-cache_write', '0');
    typeInto(root, '#mix-share-reasoning', '0');
    fixture.detectChanges();

    fetchMock.respond(jsonResponse(201, { ...team, name: 'team-measured' }));
    fetchMock.respond(jsonResponse(200, [profile(), chat, team]));
    fetchMock.respond(jsonResponse(200, settings()));
    buttonByText(root, 'Save profile').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[2];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/mix-profiles');
    expect(call.body).toEqual({
      name: 'team-measured',
      kind: 'measured',
      input_share: 12,
      output_share: 1,
      cache_read_share: 87,
      cache_write_share: 0,
      reasoning_share: 0,
    });
    // The list reloads after a successful create.
    expect(fetchMock.requests[3].url).toBe('/api/mix-profiles');
  });

  it('clones a built-in instead of editing it', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Clone').click();
    fixture.detectChanges();
    const name = root.querySelector<HTMLInputElement>('#mix-profile-name');
    expect(name?.value).toBe('code-agent-copy');

    fetchMock.respond(jsonResponse(201, { ...profile({ is_builtin: false, kind: 'custom' }) }));
    fetchMock.respond(jsonResponse(200, [profile(), chat, team]));
    fetchMock.respond(jsonResponse(200, settings()));
    buttonByText(root, 'Save profile').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[2];
    expect(call.method).toBe('POST');
    expect(call.url).toBe('/api/mix-profiles');
    expect(call.body).toMatchObject({ name: 'code-agent-copy', kind: 'custom' });
  });

  it('surfaces the server validation message verbatim', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'New profile').click();
    fixture.detectChanges();
    typeInto(root, '#mix-profile-name', 'bad');
    typeInto(root, '#mix-share-input', '0');
    typeInto(root, '#mix-share-output', '0');
    typeInto(root, '#mix-share-cache_read', '0');
    fixture.detectChanges();

    // The local sum check catches it before any request.
    buttonByText(root, 'Save profile').click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(fetchMock.requests).toHaveLength(2);
    expect(root.textContent).toContain('The shares must sum to more than zero.');

    // A server-side rejection (for example a duplicate name) is shown as-is.
    fetchMock.respond(
      errorResponse(400, 'invalid_request', 'mix profile name "team" already exists'),
    );
    typeInto(root, '#mix-share-input', '5');
    fixture.detectChanges();
    buttonByText(root, 'Save profile').click();
    await fixture.whenStable();
    fixture.detectChanges();

    expect(root.textContent).toContain('mix profile name "team" already exists');
  });

  it('deletes a custom profile only after the confirmation dialog', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;

    buttonByText(root, 'Delete').click();
    fixture.detectChanges();
    expect(root.textContent).toContain('This cannot be undone.');

    fetchMock.respond(new Response(null, { status: 204 }));
    fetchMock.respond(jsonResponse(200, [profile(), chat]));
    fetchMock.respond(jsonResponse(200, settings()));
    buttonByText(root, 'Delete profile').click();
    await fixture.whenStable();
    fixture.detectChanges();

    const call = fetchMock.requests[2];
    expect(call.method).toBe('DELETE');
    expect(call.url).toBe('/api/mix-profiles/mix-team');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
  });
});
