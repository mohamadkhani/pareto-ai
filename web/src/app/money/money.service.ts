import { Injectable, inject } from '@angular/core';
import { requestJson, requestNoContent } from '../api';
import { AuthService } from '../auth/auth.service';
import type {
  BudgetInput,
  BudgetView,
  MoneyPeriod,
  TokenCapInput,
  TokenCapView,
} from './money.types';

/**
 * All HTTP calls for the M4 budget/token-cap API. Reads are self-or-admin;
 * writes are admin-only and send the session cookie plus the CSRF token. A
 * missing row means "unlimited": `PUT` upserts one period, `DELETE` removes.
 */
@Injectable({ providedIn: 'root' })
export class MoneyService {
  private readonly auth = inject(AuthService);

  /** Lists the user's budgets (missing periods are unlimited). */
  listBudgets(userId: string): Promise<BudgetView[]> {
    return requestJson<BudgetView[]>(`/api/users/${userId}/budgets`);
  }

  /** Upserts one period's USD limit and returns the row. */
  setBudget(userId: string, period: MoneyPeriod, input: BudgetInput): Promise<BudgetView> {
    return requestJson<BudgetView>(`/api/users/${userId}/budgets/${period}`, {
      method: 'PUT',
      body: input,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Removes one period's budget (unlimited again). */
  clearBudget(userId: string, period: MoneyPeriod): Promise<void> {
    return requestNoContent(`/api/users/${userId}/budgets/${period}`, {
      method: 'DELETE',
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Lists the user's token caps (missing rows are unlimited). */
  listTokenCaps(userId: string): Promise<TokenCapView[]> {
    return requestJson<TokenCapView[]>(`/api/users/${userId}/token-caps`);
  }

  /** Upserts one model/period token limit and returns the row. */
  setTokenCap(
    userId: string,
    modelId: string,
    period: MoneyPeriod,
    input: TokenCapInput,
  ): Promise<TokenCapView> {
    return requestJson<TokenCapView>(`/api/users/${userId}/token-caps/${modelId}/${period}`, {
      method: 'PUT',
      body: input,
      csrfToken: this.auth.csrfToken(),
    });
  }

  /** Removes one model/period token cap (unlimited again). */
  clearTokenCap(userId: string, modelId: string, period: MoneyPeriod): Promise<void> {
    return requestNoContent(`/api/users/${userId}/token-caps/${modelId}/${period}`, {
      method: 'DELETE',
      csrfToken: this.auth.csrfToken(),
    });
  }
}
