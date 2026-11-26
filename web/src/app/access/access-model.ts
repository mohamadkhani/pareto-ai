/**
 * Client-side reconstruction of the server's access verdict (decision D9,
 * amended by D59) for the admin "who can use which model, through which
 * credential" views. The verdict is **account-level**: only the user's own
 * grant rows are evaluated, so a key that narrows its owner's access (D59)
 * may see less than the map shows; the map never widens beyond the account.
 *
 * A model is not reachable in the abstract: a request is served by one
 * offering on one provider using one credential, and the server evaluates the
 * three resources together. The verdict for one `(model, offering, credential)`
 * triple is: any matching `deny` row wins, else any matching `allow` row
 * allows, else the role default (admin allow, user deny). A model is accessible
 * when at least one such triple allows it, and the credentials of the allowing
 * triples are exactly the ones through which the user reaches the model.
 *
 * This is why the raw grants are hard to read: an `allow` on a credential can
 * open every model that credential serves, and an `allow` on a model opens it
 * through every credential of its providers — the combination decides, not the
 * individual rows.
 */

import type { Credential, Model, Offering, Provider } from '../catalog/catalog.types';
import type { GrantMode, GrantResourceType, GrantView } from '../grants/grants.types';
import type { UserRole } from '../users/users.types';

/** Why a model is reachable through one credential. */
export interface AccessReason {
  /** `grant` when a grant row decided it, `role` for the account default. */
  kind: 'grant' | 'role';
  /** The verdict this reason produces; only `allow` reasons are surfaced. */
  mode: GrantMode;
  /** The matching allow grant, when `kind` is `grant`. */
  grant?: GrantView;
  /** The resource kind that supplied the allow (model, offering, credential). */
  resourceType?: GrantResourceType;
  /** A short human explanation, e.g. "allow grant on credential Mock · Primary". */
  label: string;
}

/** One credential through which a model is reachable, with the deciding rule. */
export interface CredentialAccess {
  credential: Credential;
  providerName: string;
  reason: AccessReason;
}

/** One model's effective access for one user. Only reachable models are kept. */
export interface ModelAccess {
  model: Model;
  /** The credentials through which the user reaches the model; empty = denied. */
  credentials: CredentialAccess[];
}

/** Inputs for {@link computeModelAccess}. */
export interface AccessInput {
  role: UserRole;
  /** The user's own grant rows (subject_type `user`). */
  grants: readonly GrantView[];
  models: readonly Model[];
  offerings: readonly Offering[];
  credentials: readonly Credential[];
  providers: readonly Provider[];
}

/** A resource that can match a grant row. */
interface Resource {
  type: GrantResourceType;
  id: string;
}

/** The verdict for one `(model, offering, credential)` triple. */
interface PairVerdict {
  allowed: boolean;
  reason: AccessReason;
}

/**
 * Computes each model's reachable credentials for one user.
 *
 * Disabled offerings are skipped (the server never routes to them), and so are
 * offerings of a disabled model (D61) or a disabled provider. Only pairs that
 * allow are returned, so callers can drop empty models to show the final
 * result: which models the account can actually use, and through which
 * credentials.
 */
export function computeModelAccess(input: AccessInput): ModelAccess[] {
  const { role, grants, models, offerings, credentials, providers } = input;
  const userGrants = grants.filter((grant) => grant.subject_type === 'user');

  const providerName = new Map(providers.map((provider) => [provider.id, provider.name]));
  const credentialsByProvider = new Map<string, Credential[]>();
  for (const credential of credentials) {
    const list = credentialsByProvider.get(credential.provider_id) ?? [];
    list.push(credential);
    credentialsByProvider.set(credential.provider_id, list);
  }
  const offeringsByModel = new Map<string, Offering[]>();
  // A disabled model or a disabled provider excludes its offerings from routing
  // (D61 for models, and the provider's own switch), so they never reach the
  // account: mirror the server's filters here instead of drawing phantom edges.
  const enabledModelIds = new Set(models.filter((m) => m.enabled).map((m) => m.id));
  const enabledProviderIds = new Set(providers.filter((p) => p.enabled).map((p) => p.id));
  for (const offering of offerings) {
    if (!offering.enabled) {
      continue;
    }
    if (!enabledModelIds.has(offering.model_id)) {
      continue;
    }
    if (!enabledProviderIds.has(offering.provider_id)) {
      continue;
    }
    const list = offeringsByModel.get(offering.model_id) ?? [];
    list.push(offering);
    offeringsByModel.set(offering.model_id, list);
  }

  return models.map((model) => {
    const byCredential = new Map<string, CredentialAccess>();
    for (const offering of offeringsByModel.get(model.id) ?? []) {
      const name = providerName.get(offering.provider_id) ?? offering.provider_id;
      for (const credential of credentialsByProvider.get(offering.provider_id) ?? []) {
        if (byCredential.has(credential.id)) {
          continue;
        }
        const verdict = evaluatePair(userGrants, role, model, offering, credential, name);
        if (verdict.allowed) {
          byCredential.set(credential.id, {
            credential,
            providerName: name,
            reason: verdict.reason,
          });
        }
      }
    }
    const reachable = [...byCredential.values()].sort(
      (a, b) =>
        a.providerName.localeCompare(b.providerName) ||
        a.credential.label.localeCompare(b.credential.label),
    );
    return { model, credentials: reachable };
  });
}

/** One subject's verdict over the triple's resources, with the deciding rule. */
function evaluatePair(
  grants: readonly GrantView[],
  role: UserRole,
  model: Model,
  offering: Offering,
  credential: Credential,
  credentialProviderName: string,
): PairVerdict {
  const resources: Resource[] = [
    { type: 'model', id: model.id },
    { type: 'offering', id: offering.id },
    { type: 'credential', id: credential.id },
  ];

  let allow: GrantView | undefined;
  for (const grant of grants) {
    const matches = resources.some(
      (resource) => resource.type === grant.resource_type && resource.id === grant.resource_id,
    );
    if (!matches) {
      continue;
    }
    if (grant.mode === 'deny') {
      return {
        allowed: false,
        reason: {
          kind: 'grant',
          mode: 'deny',
          grant,
          resourceType: grant.resource_type,
          label: `deny grant on ${resourceLabel(grant, model, credential, credentialProviderName)}`,
        },
      };
    }
    // Prefer a model/offering allow over a credential allow as the reason: it
    // is the broader justification when both match.
    if (
      allow === undefined ||
      (allow.resource_type === 'credential' && grant.resource_type !== 'credential')
    ) {
      allow = grant;
    }
  }

  if (allow !== undefined) {
    return {
      allowed: true,
      reason: {
        kind: 'grant',
        mode: 'allow',
        grant: allow,
        resourceType: allow.resource_type,
        label: `allow grant on ${resourceLabel(allow, model, credential, credentialProviderName)}`,
      },
    };
  }

  if (role === 'admin') {
    return {
      allowed: true,
      reason: { kind: 'role', mode: 'allow', label: 'admin default (allowed without a grant)' },
    };
  }
  return {
    allowed: false,
    reason: { kind: 'role', mode: 'deny', label: 'no grant — user default denies' },
  };
}

/** A readable name for the resource a grant matched, for the explanation. */
function resourceLabel(
  grant: GrantView,
  model: Model,
  credential: Credential,
  credentialProviderName: string,
): string {
  switch (grant.resource_type) {
    case 'model':
      return `model ${model.canonical_name}`;
    case 'offering':
      return `offering of ${model.canonical_name}`;
    case 'credential':
      return `credential ${credentialProviderName} · ${credential.label}`;
  }
}
