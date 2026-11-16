import { Injectable, inject } from '@angular/core';
import { requestJson, requestNoContent } from '../api';
import { AuthService } from '../auth/auth.service';
import type { Combo, ComboInput } from './combos.types';

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
}
