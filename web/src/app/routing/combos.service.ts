import { Injectable, inject } from '@angular/core';
import { requestJson, requestNoContent } from '../api';
import { AuthService } from '../auth/auth.service';
import type { Combo, ComboInput, ComboPreview } from './combos.types';

/**
 * All HTTP calls for the M5 combo admin API (`/api/combos`). Admin only;
 * mutations send the session cookie and the CSRF token. Invalid input fails
 * with 400 `invalid_request` (the message names the offending field — e.g. an
 * unknown member offering) and an unknown id with 404. Responses carry the
 * effective weights plus the preset name; see `combos.types.ts`.
 */
@Injectable({ providedIn: 'root' })
export class CombosService {
  private readonly auth = inject(AuthService);

  /** Lists every combo, by name. */
  list(): Promise<Combo[]> {
    return requestJson<Combo[]>('/api/combos');
  }

  /** Creates a combo and returns the created row (status 201). */
  create(input: ComboInput): Promise<Combo> {
    return requestJson<Combo>('/api/combos', {
      method: 'POST',
      body: input,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Replaces a combo's strategy, weights, members, and enabled flag. */
  update(id: string, input: ComboInput): Promise<Combo> {
    return requestJson<Combo>(`/api/combos/${id}`, {
      method: 'PUT',
      body: input,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Deletes a combo (status 204). */
  remove(id: string): Promise<void> {
    return requestNoContent(`/api/combos/${id}`, {
      method: 'DELETE',
      csrfToken: this.auth.csrfToken(),
    });
  }

  /**
   * Previews a draft combo without saving it: the ranked candidates and the
   * exclusions, computed by the same engine the runtime uses (D35). The route
   * sits outside the CSRF guard because it writes nothing, so no token is sent.
   */
  preview(input: ComboInput, limit = 50): Promise<ComboPreview> {
    return requestJson<ComboPreview>('/api/combos/preview', {
      method: 'POST',
      body: { ...input, limit },
    });
  }

  /**
   * Previews a saved combo; `sessionKey` (an `X-Session-Id` value) shows the
   * current sticky pin for that key.
   */
  previewSaved(id: string, sessionKey: string | null, limit = 50): Promise<ComboPreview> {
    const params = new URLSearchParams({ limit: String(limit) });
    if (sessionKey !== null && sessionKey !== '') {
      params.set('session_key', sessionKey);
    }
    return requestJson<ComboPreview>(`/api/combos/${id}/preview?${params.toString()}`);
  }
}
