import type { Credential, Model, Offering, Provider } from '../catalog/catalog.types';
import type { GrantView } from '../grants/grants.types';
import { computeModelAccess } from './access-model';

function model(overrides: Partial<Model> = {}): Model {
  return {
    id: 'm1',
    canonical_name: 'openai/gpt-5',
    family: 'gpt-5',
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

function provider(overrides: Partial<Provider> = {}): Provider {
  return {
    id: 'p1',
    name: 'Mock',
    kind: 'openai_compatible',
    base_url: 'https://mock.example',
    enabled: true,
    preset: null,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    last_discovery_at: null,
    last_discovery_created: null,
    last_discovery_unchanged: null,
    connectors: [],
    ...overrides,
  };
}

function offering(overrides: Partial<Offering> = {}): Offering {
  return {
    id: 'off1',
    provider_id: 'p1',
    model_id: 'm1',
    upstream_model_id: 'openai/gpt-5',
    context_override: null,
    max_output_override: null,
    enabled: true,
    header_names: [],
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

function credential(overrides: Partial<Credential> = {}): Credential {
  return {
    id: 'c1',
    provider_id: 'p1',
    label: 'Primary',
    has_secret: true,
    secret_masked: '••••',
    header_names: [],
    max_concurrency: null,
    balance_usd: null,
    balance_source: 'unknown',
    balance_updated_at: null,
    balance_updated_by: null,
    balance_low_threshold: null,
    balance_available: null,
    balance_currency: null,
    balance_error: null,
    balance_fetched_at: null,
    balance_gate: 'warn',
    status: 'ok',
    last_tested_at: null,
    cooldown_until: null,
    created_at: '2026-01-01T00:00:00.000Z',
    updated_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

function grant(overrides: Partial<GrantView> = {}): GrantView {
  return {
    id: 'g1',
    subject_type: 'user',
    subject_id: 'u1',
    resource_type: 'model',
    resource_id: 'm1',
    mode: 'allow',
    created_by: 'admin',
    created_at: '2026-01-01T00:00:00.000Z',
    ...overrides,
  };
}

const MODELS = [model()];
const PROVIDERS = [provider()];
const OFFERINGS = [offering()];

function access(role: 'admin' | 'user', grants: GrantView[], credentials: Credential[]) {
  return computeModelAccess({
    role,
    grants,
    models: MODELS,
    offerings: OFFERINGS,
    credentials,
    providers: PROVIDERS,
  })[0];
}

describe('computeModelAccess', () => {
  it('reaches nothing without an offering and credential to serve the model', () => {
    const row = access('admin', [], []);
    expect(row?.credentials).toHaveLength(0);
  });

  it('an admin reaches a model through every credential by role default', () => {
    const row = access('admin', [], [credential({ id: 'c1' }), credential({ id: 'c2' })]);
    expect(row?.credentials.map((item) => item.credential.id).sort()).toEqual(['c1', 'c2']);
    expect(row?.credentials[0]?.reason.kind).toBe('role');
  });

  it('a user with no grant reaches nothing (role default denies)', () => {
    const row = access('user', [], [credential()]);
    expect(row?.credentials).toHaveLength(0);
  });

  it('a credential grant opens its model through that credential only', () => {
    // allow on c1, none on c2: c2 falls back to the user default (deny).
    const row = access(
      'user',
      [grant({ resource_type: 'credential', resource_id: 'c1' })],
      [credential({ id: 'c1' }), credential({ id: 'c2' })],
    );
    expect(row?.credentials.map((item) => item.credential.id)).toEqual(['c1']);
    expect(row?.credentials[0]?.reason.resourceType).toBe('credential');
  });

  it('a model grant opens the model through every credential of the provider', () => {
    const row = access(
      'user',
      [grant({ resource_type: 'model', resource_id: 'm1' })],
      [credential({ id: 'c1' }), credential({ id: 'c2' })],
    );
    expect(row?.credentials.map((item) => item.credential.id).sort()).toEqual(['c1', 'c2']);
  });

  it('a deny on one credential blocks only that credential', () => {
    // Model grant allows both, but a deny on c1 removes it.
    const row = access(
      'user',
      [
        grant({ resource_type: 'model', resource_id: 'm1' }),
        grant({ id: 'g2', resource_type: 'credential', resource_id: 'c1', mode: 'deny' }),
      ],
      [credential({ id: 'c1' }), credential({ id: 'c2' })],
    );
    expect(row?.credentials.map((item) => item.credential.id)).toEqual(['c2']);
  });

  it('a deny on the model blocks every credential', () => {
    const row = access(
      'admin',
      [grant({ resource_type: 'model', resource_id: 'm1', mode: 'deny' })],
      [credential({ id: 'c1' }), credential({ id: 'c2' })],
    );
    expect(row?.credentials).toHaveLength(0);
  });

  it('ignores disabled offerings', () => {
    const row = computeModelAccess({
      role: 'admin',
      grants: [],
      models: MODELS,
      offerings: [offering({ enabled: false })],
      credentials: [credential()],
      providers: PROVIDERS,
    })[0];
    expect(row?.credentials).toHaveLength(0);
  });

  it('ignores offerings of a disabled model (D61)', () => {
    const row = computeModelAccess({
      role: 'admin',
      grants: [],
      models: [model({ enabled: false })],
      offerings: [offering()],
      credentials: [credential()],
      providers: PROVIDERS,
    })[0];
    expect(row?.credentials).toHaveLength(0);
  });

  it('ignores offerings of a disabled provider', () => {
    const row = computeModelAccess({
      role: 'admin',
      grants: [],
      models: MODELS,
      offerings: [offering()],
      credentials: [credential()],
      providers: [provider({ enabled: false })],
    })[0];
    expect(row?.credentials).toHaveLength(0);
  });
});
