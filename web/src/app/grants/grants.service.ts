import { Injectable, inject } from '@angular/core';
import { requestJson, requestNoContent } from '../api';
import { AuthService } from '../auth/auth.service';
import type { GrantFilter, GrantInput, GrantView } from './grants.types';

/**
 * All HTTP calls for the M4 access-grant API. Admin only; mutations send the
 * session cookie and the CSRF token. A duplicate (subject, resource) pair
 * fails with 409 `conflict`.
 */
@Injectable({ providedIn: 'root' })
export class GrantsService {
  private readonly auth = inject(AuthService);

  /** Lists grants, optionally narrowed to one subject. */
  list(filter: GrantFilter = {}): Promise<GrantView[]> {
    const params = new URLSearchParams();
    if (filter.subject_type !== undefined) {
      params.set('subject_type', filter.subject_type);
    }
    if (filter.subject_id !== undefined) {
      params.set('subject_id', filter.subject_id);
    }
    const query = params.toString();
    return requestJson<GrantView[]>(query === '' ? '/api/grants' : `/api/grants?${query}`);
  }

  /** Binds a subject to a resource (status 201). */
  create(input: GrantInput): Promise<GrantView> {
    return requestJson<GrantView>('/api/grants', {
      method: 'POST',
      body: input,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Removes one grant (status 204). */
  remove(id: string): Promise<void> {
    return requestNoContent(`/api/grants/${id}`, {
      method: 'DELETE',
      csrfToken: this.auth.csrfToken(),
    });
  }
}
