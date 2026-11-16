import { ChangeDetectionStrategy, Component, input } from '@angular/core';
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
  templateUrl: './decision-panel.html',
  styleUrl: './decision-panel.css',
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

  protected outcomeClass(candidate: DecisionCandidate): string {
    if (candidate.outcome === 'chosen') {
      return 'ok';
    }
    if (candidate.outcome === 'failed') {
      return 'error';
    }
    return candidate.outcome.startsWith('skipped_') ? 'warn' : '';
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

  private hasChosenId(): boolean {
    return this.decision().chosen_offering_id !== null;
  }
}
