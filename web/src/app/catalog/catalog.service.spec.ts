import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ApiError } from '../api';
import {
  FetchMock,
  errorResponse,
  jsonResponse,
  noContentResponse,
} from '../../testing/fetch-mock';
import { AuthService, type User } from '../auth/auth.service';
import { CatalogService } from './catalog.service';
import type { Credential, Price, PriceInput, Provider, SyncOutcome } from './catalog.types';
import type { ModelsOverview } from './overview.types';
import { emptyConnectorConfig } from './connectors.types';

function authStub(): object {
  return {
    user: signal<User | null>({
      id: 'u1',
      email: 'admin@example.com',
      display_name: 'Admin',
      role: 'admin',
      must_change_password: false,
    }),
    isAdmin: signal(true),
    csrfToken: signal<string | null>('csrf-1'),
  };
}

const provider: Provider = {
  id: 'p1',
  name: 'openrouter',
  kind: 'openai_compatible',
  base_url: 'https://openrouter.ai/api/v1',
  enabled: true,
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
  last_discovery_at: null,
  last_discovery_created: null,
  last_discovery_unchanged: null,
  connectors: [],
};

const credential: Credential = {
  id: 'c1',
  provider_id: 'p1',
  label: 'prod',
  has_secret: true,
  secret_masked: '********',
  header_names: ['x-region'],
  max_concurrency: 4,
  balance_usd: 12.5,
  balance_source: 'manual',
  balance_updated_at: '2026-01-01T00:00:00.000Z',
  balance_updated_by: 'u1',
  balance_low_threshold: null,
  balance_available: null,
  balance_currency: null,
  balance_error: null,
  balance_fetched_at: null,
  balance_gate: 'warn',
  status: 'ok',
  last_tested_at: '2026-01-01T00:05:00.000Z',
  cooldown_until: null,
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
};

const price: Price = {
  offering_id: 'o1',
  input_per_m: 1.25,
  output_per_m: 5,
  cache_read_per_m: null,
  cache_write_per_m: null,
  reasoning_per_m: null,
  request_fee: null,
  variable_price: false,
  currency: 'USD',
  source: 'manual',
  source_url: null,
  fetched_at: '2026-01-01T00:00:00.000Z',
  updated_by: 'u1',
  created_at: '2026-01-01T00:00:00.000Z',
  updated_at: '2026-01-01T00:00:00.000Z',
};

describe('CatalogService', () => {
  let fetchMock: FetchMock;

  beforeEach(() => {
    fetchMock = new FetchMock();
    vi.stubGlobal('fetch', fetchMock.fetch);
    TestBed.configureTestingModule({ providers: [{ provide: AuthService, useValue: authStub() }] });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('lists providers from the bare array with the session cookie and no body', async () => {
    fetchMock.respond(jsonResponse(200, [provider]));
    const catalog = TestBed.inject(CatalogService);

    await expect(catalog.listProviders()).resolves.toEqual([provider]);

    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/providers');
    expect(call.method).toBe('GET');
    expect(call.credentials).toBe('include');
    expect(call.body).toBeNull();
    expect(call.headers.get('X-CSRF-Token')).toBeNull();
  });

  it('normalizes an omitted connectors array on the provider view', async () => {
    const { connectors: _omitted, ...withoutConnectors } = provider;
    fetchMock.respond(jsonResponse(200, [withoutConnectors]));
    const catalog = TestBed.inject(CatalogService);

    const [loaded] = await catalog.listProviders();
    expect(loaded.connectors).toEqual([]);
  });

  it('createProvider posts the provider with the CSRF header', async () => {
    fetchMock.respond(jsonResponse(201, provider));
    const catalog = TestBed.inject(CatalogService);

    await catalog.createProvider({ name: 'openrouter', base_url: provider.base_url });

    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/providers');
    expect(call.method).toBe('POST');
    expect(call.credentials).toBe('include');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ name: 'openrouter', base_url: provider.base_url });
  });

  it('deleteProvider sends DELETE with the CSRF header', async () => {
    fetchMock.respond(noContentResponse());
    const catalog = TestBed.inject(CatalogService);

    await catalog.deleteProvider('p1');

    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/providers/p1');
    expect(call.method).toBe('DELETE');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
  });

  it('never carries a secret back out of a credential GET response', async () => {
    // A misbehaving server echo must not be able to bounce the secret around.
    fetchMock.respond(jsonResponse(200, [{ ...credential, secret: 'sk-leaked' }]));
    const catalog = TestBed.inject(CatalogService);

    const [loaded] = await catalog.listCredentials('p1');

    expect(loaded).toEqual(credential);
    expect('secret' in loaded).toBe(false);
  });

  it('createCredential sends the write-only secret once', async () => {
    fetchMock.respond(jsonResponse(201, credential));
    const catalog = TestBed.inject(CatalogService);

    await catalog.createCredential('p1', {
      label: 'prod',
      secret: 'sk-live-1',
      headers: { 'x-region': 'eu' },
      max_concurrency: 4,
      balance_usd: 12.5,
      balance_low_threshold: null,
    });

    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/providers/p1/credentials');
    expect(call.method).toBe('POST');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({
      label: 'prod',
      secret: 'sk-live-1',
      headers: { 'x-region': 'eu' },
      max_concurrency: 4,
      balance_usd: 12.5,
      balance_low_threshold: null,
    });
  });

  it('updateCredential posts the patch as given and never echoes a secret from reads', async () => {
    fetchMock.respond(jsonResponse(200, credential));
    fetchMock.respond(jsonResponse(200, credential));
    const catalog = TestBed.inject(CatalogService);

    await catalog.getCredential('c1');
    await catalog.updateCredential('c1', { label: 'prod-eu' });

    const patchCall = fetchMock.requests[1];
    expect(patchCall.url).toBe('/api/credentials/c1');
    expect(patchCall.method).toBe('PATCH');
    expect(patchCall.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(patchCall.body).toEqual({ label: 'prod-eu' });
    expect(patchCall.body).not.toHaveProperty('secret');
  });

  it('testCredential probes with a CSRF-protected POST', async () => {
    fetchMock.respond(
      jsonResponse(200, {
        ok: true,
        status: 'ok',
        last_tested_at: '2026-01-01T00:05:00.000Z',
        error: null,
      }),
    );
    const catalog = TestBed.inject(CatalogService);

    const result = await catalog.testCredential('c1');

    expect(result.ok).toBe(true);
    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/credentials/c1/test');
    expect(call.method).toBe('POST');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toBeNull();
  });

  it('setPrice sends the price fields and the server records the manual source', async () => {
    fetchMock.respond(jsonResponse(200, price));
    const catalog = TestBed.inject(CatalogService);
    const input: PriceInput = {
      input_per_m: 1.25,
      output_per_m: 5,
      cache_read_per_m: null,
      cache_write_per_m: null,
      reasoning_per_m: null,
      request_fee: null,
    };

    await expect(catalog.setPrice('o1', input)).resolves.toEqual(price);

    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/offerings/o1/price');
    expect(call.method).toBe('PUT');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual(input);
    expect(call.body).not.toHaveProperty('source');
  });

  it('getPrice returns the price row or null when the offering is unpriced', async () => {
    fetchMock.respond(jsonResponse(200, price));
    fetchMock.respond(errorResponse(404, 'not_found', 'price'));
    const catalog = TestBed.inject(CatalogService);

    await expect(catalog.getPrice('o1')).resolves.toEqual(price);
    await expect(catalog.getPrice('o2')).resolves.toBeNull();
  });

  it('syncStatus returns the status object with a null run before the first sync', async () => {
    const status = { last_run: null, snapshot_stale: true, ttl_hours: 24 };
    fetchMock.respond(jsonResponse(200, status));
    const catalog = TestBed.inject(CatalogService);

    await expect(catalog.syncStatus()).resolves.toEqual(status);
    expect(fetchMock.requests[0].url).toBe('/api/sync/openrouter/status');
  });

  it('syncOpenRouter triggers a run and returns the recorded outcome', async () => {
    const outcome: SyncOutcome = {
      run_id: 'r1',
      status: 'success',
      model_count: 42,
      stale: false,
      started_at: '2026-01-01T00:00:00.000Z',
      finished_at: '2026-01-01T00:00:02.000Z',
      error: null,
    };
    fetchMock.respond(jsonResponse(200, outcome));
    const catalog = TestBed.inject(CatalogService);

    await expect(catalog.syncOpenRouter()).resolves.toEqual(outcome);

    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/sync/openrouter');
    expect(call.method).toBe('POST');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toBeNull();
  });

  it('reads the merged overview for all providers or one provider', async () => {
    const mix = { profile_id: 'mix-code-agent', profile_name: 'code-agent' };
    const all: ModelsOverview = { scope: 'all', provider_id: null, mix, rows: [] };
    const one: ModelsOverview = { scope: 'provider', provider_id: 'p1', mix, rows: [] };
    fetchMock.respond(jsonResponse(200, all));
    fetchMock.respond(jsonResponse(200, one));
    const catalog = TestBed.inject(CatalogService);

    await expect(catalog.modelsOverview(null)).resolves.toEqual(all);
    await expect(catalog.modelsOverview('p1')).resolves.toEqual(one);

    expect(fetchMock.requests[0].url).toBe('/api/models/overview?provider=all');
    expect(fetchMock.requests[0].method).toBe('GET');
    expect(fetchMock.requests[1].url).toBe('/api/models/overview?provider=p1');
  });

  it('selects a mix profile for the overview when one is given', async () => {
    const mix = { profile_id: 'mix-chat', profile_name: 'chat' };
    fetchMock.respond(jsonResponse(200, { scope: 'all', provider_id: null, mix, rows: [] }));
    const catalog = TestBed.inject(CatalogService);

    await catalog.modelsOverview(null, 'mix-chat');

    expect(fetchMock.requests[0].url).toBe('/api/models/overview?provider=all&mix=mix-chat');
  });

  it('discovers a provider’s models with a CSRF-protected POST', async () => {
    const summary = {
      dry_run: false,
      credential_id: 'c1',
      discovered: 2,
      created_models: 2,
      created_offerings: 2,
      updated_offerings: 0,
      unchanged: 0,
    };
    fetchMock.respond(jsonResponse(200, summary));
    const catalog = TestBed.inject(CatalogService);

    await expect(catalog.discoverModels('p1', { dry_run: true })).resolves.toEqual(summary);

    const call = fetchMock.requests[0];
    expect(call.url).toBe('/api/providers/p1/discover-models');
    expect(call.method).toBe('POST');
    expect(call.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(call.body).toEqual({ dry_run: true });
  });

  it('reads connector templates and one connector', async () => {
    const view = {
      provider_id: 'p1',
      kind: 'catalog' as const,
      configured: false,
      enabled: false,
      config: null,
      updated_at: null,
      updated_by: null,
    };
    fetchMock.respond(jsonResponse(200, []));
    fetchMock.respond(jsonResponse(200, view));
    const catalog = TestBed.inject(CatalogService);

    await catalog.connectorTemplates('p1');
    await expect(catalog.getConnector('p1', 'catalog')).resolves.toEqual(view);

    expect(fetchMock.requests[0].url).toBe('/api/providers/p1/connectors/templates');
    expect(fetchMock.requests[0].method).toBe('GET');
    expect(fetchMock.requests[1].url).toBe('/api/providers/p1/connectors/catalog');
    expect(fetchMock.requests[1].headers.get('X-CSRF-Token')).toBeNull();
  });

  it('saves a connector and previews it with CSRF-protected calls', async () => {
    const config = emptyConnectorConfig('catalog');
    fetchMock.respond(jsonResponse(200, { configured: true }));
    fetchMock.respond(jsonResponse(200, { kind: 'catalog', body: '{}' }));
    fetchMock.respond(jsonResponse(200, { kind: 'catalog', ok: true }));
    const catalog = TestBed.inject(CatalogService);

    await catalog.putConnector('p1', 'catalog', config, true);
    await catalog.sampleConnector('p1', 'catalog', { config, credential_id: 'c1' });
    await catalog.testConnector('p1', 'catalog', { config });

    const [put, sample, test] = fetchMock.requests;
    expect(put.url).toBe('/api/providers/p1/connectors/catalog');
    expect(put.method).toBe('PUT');
    expect(put.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(put.body).toEqual({ config, enabled: true });

    expect(sample.method).toBe('POST');
    expect(sample.url).toBe('/api/providers/p1/connectors/catalog/sample');
    expect(sample.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(sample.body).toEqual({ config, credential_id: 'c1' });

    expect(test.method).toBe('POST');
    expect(test.url).toBe('/api/providers/p1/connectors/catalog/test');
    expect(test.body).toEqual({ config });
  });

  it('refreshes prices, one balance, and every provider balance', async () => {
    fetchMock.respond(jsonResponse(200, { ok: true, created: 1 }));
    fetchMock.respond(jsonResponse(200, { ok: true, balance_usd: 5 }));
    fetchMock.respond(jsonResponse(200, { refreshed: 2, failed: 0, results: [] }));
    const catalog = TestBed.inject(CatalogService);

    await catalog.refreshPrices('p1', 'c1');
    await catalog.refreshCredentialBalance('p1', 'c1');
    await catalog.refreshProviderBalances('p1');

    const [prices, balance, balances] = fetchMock.requests;
    expect(prices.url).toBe('/api/providers/p1/prices/refresh');
    expect(prices.method).toBe('POST');
    expect(prices.headers.get('X-CSRF-Token')).toBe('csrf-1');
    expect(prices.body).toEqual({ credential_id: 'c1' });

    expect(balance.url).toBe('/api/providers/p1/credentials/c1/balance/refresh');
    expect(balance.method).toBe('POST');
    expect(balance.headers.get('X-CSRF-Token')).toBe('csrf-1');

    expect(balances.url).toBe('/api/providers/p1/balances/refresh');
    expect(balances.method).toBe('POST');
    expect(balances.headers.get('X-CSRF-Token')).toBe('csrf-1');
  });

  it('surfaces the stable error code and message from the envelope', async () => {
    fetchMock.respond(errorResponse(409, 'conflict', 'A provider with this name exists.'));
    const catalog = TestBed.inject(CatalogService);

    const error: unknown = await catalog
      .createProvider({ name: 'openrouter', base_url: provider.base_url })
      .catch((err: unknown) => err);

    expect(error).toBeInstanceOf(ApiError);
    if (error instanceof ApiError) {
      expect(error.code).toBe('conflict');
      expect(error.message).toBe('A provider with this name exists.');
    }
  });
});
