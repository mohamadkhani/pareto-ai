import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import { UiBadge, type BadgeTone } from '../ui/badge';
import { UiCard } from '../ui/card';
import { UiTable } from '../ui/table';
import type { CandidateScore, DecisionCandidate, DecisionRecord } from './routing.types';

/** What kind of marker a score component carries. */
type ScoreMarker = 'imputed' | 'shrunk';

const MARKER_LABEL: Record<ScoreMarker, string> = { imputed: 'imp', shrunk: 'shr' };

const MARKER_HELP: Record<ScoreMarker, string> = {
  imputed: 'imputed: no metric for this component, so the neutral 0.5 was used',
  shrunk: 'shrunk toward the neutral 0.5: fewer than 10 telemetry samples',
};

/**
 * The "why this model" panel: every candidate the router considered with its
 * score breakdown, the chosen offering highlighted, the one-line explanation
 * per candidate, and the routing configuration that decided the order.
 * Presentational: the parent passes a parsed `DecisionRecord`.
 */
@Component({
  selector: 'app-decision-panel',
  imports: [UiBadge, UiCard, UiTable],
  templateUrl: './decision-panel.html',
  styleUrl: './decision-panel.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class DecisionPanel {
  readonly decision = input.required<DecisionRecord>();

  /** True for the candidate that served (or was chosen by) the router. */
  protected isChosen(candidate: DecisionCandidate): boolean {
    return (
      candidate.id === this.decision().chosen_offering_id ||
      (!this.hasChosenId() && candidate.outcome === 'chosen')
    );
  }

  protected outcomeLabel(candidate: DecisionCandidate): string {
    if (candidate.outcome === 'not_tried') {
      return 'not tried';
    }
    return candidate.outcome.startsWith('skipped_') ? 'skipped' : candidate.outcome;
  }

  protected outcomeTone(candidate: DecisionCandidate): BadgeTone {
    if (candidate.outcome === 'chosen') {
      return 'success';
    }
    if (candidate.outcome === 'failed') {
      return 'danger';
    }
    return candidate.outcome.startsWith('skipped_') ? 'warning' : 'neutral';
  }

  /**
   * The marker for one score component. Imputed wins when a component appears
   * in both lists: it explains the value more fundamentally than shrinkage.
   */
  protected marker(score: CandidateScore, component: string): ScoreMarker | null {
    if (score.imputed.includes(component)) {
      return 'imputed';
    }
    return score.shrunk.includes(component) ? 'shrunk' : null;
  }

  protected markerLabel(marker: ScoreMarker | null): string {
    return marker === null ? '' : MARKER_LABEL[marker];
  }

  protected markerHelp(marker: ScoreMarker | null): string {
    return marker === null ? '' : MARKER_HELP[marker];
  }

  /**
   * Components are `[0, 1]`; `total` is deliberately unclamped and can be
   * negative (documented contract) — it renders as-is.
   */
  protected formatScore(value: number): string {
    return value.toFixed(3);
  }

  /** The price basis recorded with the decision (D34). */
  protected basisLabel(candidate: DecisionCandidate): string {
    switch (candidate.price_basis) {
      case 'mix':
        return 'mix';
      case 'component_sum':
        return 'component sum';
      case 'unpriced':
        return 'unpriced';
      default:
        return candidate.price_basis;
    }
  }

  protected basisTone(candidate: DecisionCandidate): BadgeTone {
    switch (candidate.price_basis) {
      case 'mix':
        return 'accent';
      case 'component_sum':
        return 'warning';
      default:
        return 'neutral';
    }
  }

  /** The candidate's mix price, or an explicit dash (never $0.00). */
  protected formatMixPrice(candidate: DecisionCandidate): string {
    const mix = candidate.mix_price;
    if (mix === null) {
      return '—';
    }
    return Math.abs(mix.per_m) >= 1 ? `$${mix.per_m.toFixed(2)}` : `$${mix.per_m.toFixed(4)}`;
  }

  /** The candidate the session was pinned to, when this request listed it. */
  protected pinnedCandidate(): DecisionCandidate | null {
    const pinned = this.decision().sticky.pinned;
    if (pinned === null) {
      return null;
    }
    return this.decision().candidates.find((candidate) => candidate.id === pinned) ?? null;
  }

  private hasChosenId(): boolean {
    return this.decision().chosen_offering_id !== null;
  }
}
