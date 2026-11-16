import { ApiError } from '../api';

/** True when the failure is the admin API refusing a non-admin caller. */
export function isForbidden(err: unknown): boolean {
  return err instanceof ApiError && (err.code === 'forbidden' || err.status === 403);
}

/** Turns an unknown failure into a user-facing message. */
export function describeError(err: unknown, fallback: string): string {
  return err instanceof ApiError ? err.message : fallback;
}
