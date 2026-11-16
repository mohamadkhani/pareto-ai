import { Injectable, inject } from '@angular/core';
import { requestJson, requestNoContent } from '../api';
import { AuthService } from '../auth/auth.service';
import type { ApiKeyInput, ApiKeyPatch, ApiKeyView, CreatedKey } from './keys.types';

/**
 * All HTTP calls for the M4 API-key API. Self-or-admin: callers may manage
 * their own keys (or an admin any account's). Mutations send the session
 * cookie and the CSRF token; the plaintext key travels exactly once, in the
 * `create` response.
 */
@Injectable({ providedIn: 'root' })
export class KeysService {
  private readonly auth = inject(AuthService);

  /** Lists one user's keys (prefixes only — never key material). */
  list(userId: string): Promise<ApiKeyView[]> {
    return requestJson<ApiKeyView[]>(`/api/users/${userId}/keys`);
  }

  /**
   * Issues a key (status 201). The returned `key` is the only copy of the
   * plaintext; it never appears in any later response.
   */
  create(userId: string, input: ApiKeyInput): Promise<CreatedKey> {
    return requestJson<CreatedKey>(`/api/users/${userId}/keys`, {
      method: 'POST',
      body: input,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Updates a key's name, expiry, or rate limits; omitted fields keep. */
  update(id: string, patch: ApiKeyPatch): Promise<ApiKeyView> {
    return requestJson<ApiKeyView>(`/api/keys/${id}`, {
      method: 'PATCH',
      body: patch,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Revokes a key: it stops authenticating on its next request. */
  revoke(id: string): Promise<void> {
    return requestNoContent(`/api/keys/${id}`, {
      method: 'DELETE',
      csrfToken: this.auth.csrfToken(),
    });
  }
}
