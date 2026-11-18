import { Injectable, signal } from '@angular/core';
import type { BalanceRefreshView } from './connectors.types';

/**
 * The latest balance refresh result per credential, shared by the provider
 * list, the credentials panel, and the connectors dialog. The credential view
 * stores the balance itself; this cache adds the transient extras a refresh
 * returns (burn rate and days remaining) without another round trip.
 */
@Injectable({ providedIn: 'root' })
export class BalanceCache {
  private readonly results = signal<ReadonlyMap<string, BalanceRefreshView>>(new Map());

  /** Replaces one credential's latest result. */
  set(result: BalanceRefreshView): void {
    this.results.update((current) => new Map(current).set(result.credential_id, result));
  }

  /** Replaces several results at once. */
  setAll(results: readonly BalanceRefreshView[]): void {
    this.results.update((current) => {
      const next = new Map(current);
      for (const result of results) {
        next.set(result.credential_id, result);
      }
      return next;
    });
  }

  /** The latest result for one credential, when a refresh ran this session. */
  get(credentialId: string): BalanceRefreshView | undefined {
    return this.results().get(credentialId);
  }
}
