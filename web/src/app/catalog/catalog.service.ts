import { Injectable, inject } from '@angular/core';
import { ApiError, requestJson, requestNoContent } from '../api';
import { AuthService } from '../auth/auth.service';
import type {
  Credential,
  CredentialInput,
  CredentialPatch,
  CredentialTestResult,
  DiscoveryInput,
  DiscoverySummary,
  Model,
  ModelInput,
  Offering,
  OfferingInput,
  OfferingPatch,
  Price,
  PriceInput,
  Provider,
  ProviderInput,
  ProviderPatch,
  SyncOutcome,
  SyncStatus,
} from './catalog.types';
import type {
  BalanceRefreshView,
  ConnectorConfig,
  ConnectorDraft,
  ConnectorKind,
  ConnectorTemplate,
  ConnectorView,
  ConnectorTestView,
  PriceRefreshView,
  ProviderBalancesView,
  SampleView,
} from './connectors.types';
import type { BreakerEntry, BreakerListResponse, BreakerResetInput } from './breaker.types';
import type { ModelsOverview } from './overview.types';

/**
 * Copies a provider response field by field; the backend omits an empty
 * `connectors` array, so the UI always sees one.
 */
function toProvider(raw: Provider): Provider {
  return {
    id: raw.id,
    name: raw.name,
    kind: raw.kind,
    base_url: raw.base_url,
    enabled: raw.enabled,
    created_at: raw.created_at,
    updated_at: raw.updated_at,
    last_discovery_at: raw.last_discovery_at ?? null,
    last_discovery_created: raw.last_discovery_created ?? null,
    last_discovery_unchanged: raw.last_discovery_unchanged ?? null,
    connectors: raw.connectors ?? [],
  };
}

/**
 * Copies a credential response field by field so a stray `secret` (or any
 * other server-side field) can never be echoed back on a later save.
 */
function toCredential(raw: Credential): Credential {
  return {
    id: raw.id,
    provider_id: raw.provider_id,
    label: raw.label,
    has_secret: raw.has_secret,
    secret_masked: raw.secret_masked,
    header_names: raw.header_names,
    max_concurrency: raw.max_concurrency,
    balance_usd: raw.balance_usd,
    balance_source: raw.balance_source,
    balance_updated_at: raw.balance_updated_at,
    balance_updated_by: raw.balance_updated_by,
    balance_low_threshold: raw.balance_low_threshold,
    balance_available: raw.balance_available ?? null,
    balance_currency: raw.balance_currency ?? null,
    balance_error: raw.balance_error ?? null,
    balance_fetched_at: raw.balance_fetched_at ?? null,
    balance_gate: raw.balance_gate ?? 'warn',
    status: raw.status,
    last_tested_at: raw.last_tested_at,
    cooldown_until: raw.cooldown_until,
    created_at: raw.created_at,
    updated_at: raw.updated_at,
  };
}

/**
 * Copies a model response field by field; `modalities_*` may be null on the
 * wire and are normalized to empty arrays, while `capabilities` stays the
 * free-form JSON the server sent.
 */
function toModel(raw: Model): Model {
  return {
    id: raw.id,
    canonical_name: raw.canonical_name,
    family: raw.family,
    modalities_input: raw.modalities_input ?? [],
    modalities_output: raw.modalities_output ?? [],
    context_length: raw.context_length,
    max_output_tokens: raw.max_output_tokens,
    capabilities: raw.capabilities,
    created_at: raw.created_at,
    updated_at: raw.updated_at,
  };
}

/**
 * All HTTP calls for the M2 catalog admin API. Mutations send the session
 * cookie and the CSRF token; GET calls never carry a request body. Lists come
 * back as bare JSON arrays and single entities bare.
 */
@Injectable({ providedIn: 'root' })
export class CatalogService {
  private readonly auth = inject(AuthService);

  // --- providers -----------------------------------------------------------

  /** Lists every provider. Throws `ApiError` on failure. */
  async listProviders(): Promise<Provider[]> {
    const response = await requestJson<Provider[]>('/api/providers');
    return response.map(toProvider);
  }

  /** Registers a provider and returns the created row. */
  async createProvider(input: ProviderInput): Promise<Provider> {
    return toProvider(
      await requestJson<Provider>('/api/providers', {
        method: 'POST',
        body: input,
        csrfToken: this.auth.csrfToken(),
      }),
    );
  }

  /** Applies a partial provider update and returns the updated row. */
  async updateProvider(id: string, patch: ProviderPatch): Promise<Provider> {
    return toProvider(
      await requestJson<Provider>(`/api/providers/${id}`, {
        method: 'PATCH',
        body: patch,
        csrfToken: this.auth.csrfToken(),
      }),
    );
  }

  /** Deletes a provider; its credentials and offerings cascade. */
  deleteProvider(id: string): Promise<void> {
    return requestNoContent(`/api/providers/${id}`, {
      method: 'DELETE',
      csrfToken: this.auth.csrfToken(),
    });
  }

  // --- breaker -------------------------------------------------------------

  /** Every pair with live breaker state (D50). Admin only. */
  async breakerSnapshot(): Promise<BreakerEntry[]> {
    const response = await requestJson<BreakerListResponse>('/api/breaker');
    return response.entries;
  }

  /** Closes one pair manually; audited as `breaker.reset`. Admin only. */
  resetBreaker(credentialId: string, offeringId: string): Promise<void> {
    const body: BreakerResetInput = {
      credential_id: credentialId,
      offering_id: offeringId,
    };
    return requestNoContent('/api/breaker/reset', {
      method: 'POST',
      body,
      csrfToken: this.auth.csrfToken(),
    });
  }

  // --- credentials ---------------------------------------------------------

  /** Lists a provider's credentials; responses never carry secret values. */
  async listCredentials(providerId: string): Promise<Credential[]> {
    const response = await requestJson<Credential[]>(`/api/providers/${providerId}/credentials`);
    return response.map(toCredential);
  }

  /** Reads one credential (still without its secret). */
  async getCredential(id: string): Promise<Credential> {
    return toCredential(await requestJson<Credential>(`/api/credentials/${id}`));
  }

  /** Attaches a credential to a provider; the secret is write-only. */
  async createCredential(providerId: string, input: CredentialInput): Promise<Credential> {
    return toCredential(
      await requestJson<Credential>(`/api/providers/${providerId}/credentials`, {
        method: 'POST',
        body: input,
        csrfToken: this.auth.csrfToken(),
      }),
    );
  }

  /** Updates a credential; an omitted `secret` keeps the stored one. */
  async updateCredential(id: string, patch: CredentialPatch): Promise<Credential> {
    return toCredential(
      await requestJson<Credential>(`/api/credentials/${id}`, {
        method: 'PATCH',
        body: patch,
        csrfToken: this.auth.csrfToken(),
      }),
    );
  }

  /** Deletes a credential. */
  deleteCredential(id: string): Promise<void> {
    return requestNoContent(`/api/credentials/${id}`, {
      method: 'DELETE',
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Probes the provider with this credential and records the result. */
  testCredential(id: string): Promise<CredentialTestResult> {
    return requestJson<CredentialTestResult>(`/api/credentials/${id}/test`, {
      method: 'POST',
      csrfToken: this.auth.csrfToken(),
    });
  }

  /**
   * Discovers the provider's models through its OpenAI-compatible `/models`
   * endpoint. Idempotent: manual catalog edits, overrides, enabled flags, and
   * prices are never overwritten. A failed upstream fetch is a 502.
   */
  discoverModels(providerId: string, input: DiscoveryInput = {}): Promise<DiscoverySummary> {
    return requestJson<DiscoverySummary>(`/api/providers/${providerId}/discover-models`, {
      method: 'POST',
      body: input,
      csrfToken: this.auth.csrfToken(),
    });
  }

  // --- models --------------------------------------------------------------

  /** Lists every catalog model. */
  async listModels(): Promise<Model[]> {
    const response = await requestJson<Model[]>('/api/models');
    return response.map(toModel);
  }

  /**
   * The merged models overview: per-offering rows for one provider, or one
   * aggregated row per model when `providerId` is null (D31). `mixId` selects
   * the token-mix profile for every `mix_price`; null uses the resolved
   * default (D34). `search` is a case-insensitive substring over model name,
   * family, upstream id, and provider name; null or blank applies no filter.
   */
  modelsOverview(
    providerId: string | null,
    mixId: string | null = null,
    search: string | null = null,
  ): Promise<ModelsOverview> {
    const params = new URLSearchParams({ provider: providerId ?? 'all' });
    if (mixId !== null && mixId !== '') {
      params.set('mix', mixId);
    }
    const term = search?.trim() ?? '';
    if (term !== '') {
      params.set('q', term);
    }
    return requestJson<ModelsOverview>(`/api/models/overview?${params.toString()}`);
  }

  /** Creates a model and returns the created row. */
  async createModel(input: ModelInput): Promise<Model> {
    return toModel(
      await requestJson<Model>('/api/models', {
        method: 'POST',
        body: input,
        csrfToken: this.auth.csrfToken(),
      }),
    );
  }

  // --- offerings and prices ------------------------------------------------

  /** Lists every offering (prices load per offering via `getPrice`). */
  listOfferings(): Promise<Offering[]> {
    return requestJson<Offering[]>('/api/offerings');
  }

  /** Creates an offering and returns the created row. */
  createOffering(input: OfferingInput): Promise<Offering> {
    return requestJson<Offering>('/api/offerings', {
      method: 'POST',
      body: input,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Applies a partial offering update and returns the updated row. */
  updateOffering(id: string, patch: OfferingPatch): Promise<Offering> {
    return requestJson<Offering>(`/api/offerings/${id}`, {
      method: 'PATCH',
      body: patch,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Deletes an offering; its price cascades. */
  deleteOffering(id: string): Promise<void> {
    return requestNoContent(`/api/offerings/${id}`, {
      method: 'DELETE',
      csrfToken: this.auth.csrfToken(),
    });
  }

  /**
   * Reads one offering's price row; null when the offering is not priced yet
   * (the endpoint answers 404 in that case).
   */
  async getPrice(offeringId: string): Promise<Price | null> {
    try {
      return await requestJson<Price>(`/api/offerings/${offeringId}/price`);
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) {
        return null;
      }
      throw err;
    }
  }

  /** Saves a manual price override (the server records `source='manual'`). */
  setPrice(offeringId: string, price: PriceInput): Promise<Price> {
    return requestJson<Price>(`/api/offerings/${offeringId}/price`, {
      method: 'PUT',
      body: price,
      csrfToken: this.auth.csrfToken(),
    });
  }

  // --- OpenRouter sync -----------------------------------------------------

  /**
   * Triggers an OpenRouter catalog/price sync and returns the recorded
   * outcome. A failed fetch is not an error response: the run comes back as
   * `stale-if-error` (or `error` without a previous snapshot).
   */
  syncOpenRouter(): Promise<SyncOutcome> {
    return requestJson<SyncOutcome>('/api/sync/openrouter', {
      method: 'POST',
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Reads the last sync run and the snapshot's freshness. */
  syncStatus(): Promise<SyncStatus> {
    return requestJson<SyncStatus>('/api/sync/openrouter/status');
  }

  // --- provider connectors (D42, D44) --------------------------------------

  /** Lists the shipped connector templates; they are generic and editable. */
  connectorTemplates(providerId: string): Promise<ConnectorTemplate[]> {
    return requestJson<ConnectorTemplate[]>(`/api/providers/${providerId}/connectors/templates`);
  }

  /** Reads one connector; `config` is null until the operator saves it. */
  getConnector(providerId: string, kind: ConnectorKind): Promise<ConnectorView> {
    return requestJson<ConnectorView>(`/api/providers/${providerId}/connectors/${kind}`);
  }

  /** Validates and stores one connector document. */
  putConnector(
    providerId: string,
    kind: ConnectorKind,
    config: ConnectorConfig,
    enabled: boolean,
  ): Promise<ConnectorView> {
    return requestJson<ConnectorView>(`/api/providers/${providerId}/connectors/${kind}`, {
      method: 'PUT',
      body: { config, enabled },
      csrfToken: this.auth.csrfToken(),
    });
  }

  /**
   * Fetches a raw sample for the draft config (≤200 KiB). The body is JSON
   * text; parsing happens in the dialog. Writes nothing.
   */
  sampleConnector(
    providerId: string,
    kind: ConnectorKind,
    draft: ConnectorDraft = {},
  ): Promise<SampleView> {
    return requestJson<SampleView>(`/api/providers/${providerId}/connectors/${kind}/sample`, {
      method: 'POST',
      body: draft,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Previews extraction for a draft config. Writes nothing. */
  testConnector(
    providerId: string,
    kind: ConnectorKind,
    draft: ConnectorDraft = {},
  ): Promise<ConnectorTestView> {
    return requestJson<ConnectorTestView>(`/api/providers/${providerId}/connectors/${kind}/test`, {
      method: 'POST',
      body: draft,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Applies the catalog connector to the provider's offerings. */
  refreshPrices(providerId: string, credentialId?: string): Promise<PriceRefreshView> {
    return requestJson<PriceRefreshView>(`/api/providers/${providerId}/prices/refresh`, {
      method: 'POST',
      body: credentialId === undefined ? {} : { credential_id: credentialId },
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Refreshes one credential's provider-reported balance. */
  refreshCredentialBalance(providerId: string, credentialId: string): Promise<BalanceRefreshView> {
    return requestJson<BalanceRefreshView>(
      `/api/providers/${providerId}/credentials/${credentialId}/balance/refresh`,
      { method: 'POST', csrfToken: this.auth.csrfToken() },
    );
  }

  /** Refreshes every credential's balance, isolating per-credential failures. */
  refreshProviderBalances(providerId: string): Promise<ProviderBalancesView> {
    return requestJson<ProviderBalancesView>(`/api/providers/${providerId}/balances/refresh`, {
      method: 'POST',
      csrfToken: this.auth.csrfToken(),
    });
  }
}
