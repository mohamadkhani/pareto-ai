/**
 * Wire types for the M4 budget and token-cap API (`/api/users/{id}/budgets`,
 * `/api/users/{id}/token-caps`).
 *
 * Caps are hard gates: the data plane refuses with `402 budget_exceeded` or
 * `429 token_cap_exceeded` before forwarding once `used + estimate > limit`.
 * Reads are self-or-admin, writes admin-only (audited). A missing row means
 * "unlimited"; `PUT` upserts one period and `DELETE` removes the cap.
 */

/** Budget and cap periods; `total` never resets. */
export type MoneyPeriod = 'day' | 'week' | 'month' | 'total';

/** Every period in display order. */
export const MONEY_PERIODS: readonly MoneyPeriod[] = ['day', 'week', 'month', 'total'];

/** One budget as the API returns it (the current-period view). */
export interface BudgetView {
  user_id: string;
  period: MoneyPeriod;
  limit_usd: number;
  /** Spend booked into the current period; 0 before it has any. */
  used_usd: number;
  /** When the current period resets; null for `total`. */
  reset_at: string | null;
  created_at: string;
  updated_at: string;
}

/** One token cap as the API returns it (the current-period view). */
export interface TokenCapView {
  user_id: string;
  model_id: string;
  period: MoneyPeriod;
  limit_tokens: number;
  /** Tokens booked into the current period; 0 before it has any. */
  used_tokens: number;
  /** When the current period resets; null for `total`. */
  reset_at: string | null;
  created_at: string;
  updated_at: string;
}

/** `PUT /api/users/{id}/budgets/{period}`; 0 blocks all spend. */
export interface BudgetInput {
  limit_usd: number;
}

/** `PUT /api/users/{id}/token-caps/{model_id}/{period}`; 0 blocks all use. */
export interface TokenCapInput {
  limit_tokens: number;
}
