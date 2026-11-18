import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { UiBadge, type BadgeTone } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import {
  type ComboPreview,
  type ComboPreviewCandidate,
  type ComboPreviewScore,
  type ExcludedGroup,
  excludedGroups,
} from './combos.types';

/**
 * The combo preview: the effective token-mix profile, the ranked candidate
 * list with its score breakdown, the exclusions grouped by reason, and the
 * current sticky pin. The data comes from the same engine the runtime uses
 * (D35); this component only renders it, so the view cannot drift from the
 * routing decision.
 */
@Component({
  selector: 'app-combo-preview',
  imports: [UiBadge, UiBanner],
  templateUrl: './combo-preview.html',
  styleUrl: './combo-preview.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ComboPreviewPanel {
  readonly preview = input.required<ComboPreview>();

  protected readonly groups = computed<ExcludedGroup[]>(() =>
    excludedGroups(this.preview().excluded),
  );
  protected readonly sticky = computed(() => this.preview().sticky);

  /** A clamped fill width; the printed total keeps its unclamped value. */
  protected barWidth(score: ComboPreviewScore): number {
    return Math.min(100, Math.max(0, score.total * 100));
  }

  protected formatScore(value: number): string {
    return value.toFixed(3);
  }

  protected formatQuality(value: number): string {
    return value.toFixed(1);
  }

  protected formatMixPrice(value: number | null): string {
    if (value === null) {
      return '—';
    }
    return Math.abs(value) >= 1 ? `$${value.toFixed(2)}` : `$${value.toFixed(4)}`;
  }

  protected qualityLabel(candidate: ComboPreviewCandidate): string {
    const source = candidate.quality_source;
    if (source === 'override') {
      return 'override';
    }
    if (source.startsWith('benchmark')) {
      return 'benchmark';
    }
    return 'default';
  }

  protected qualityTone(candidate: ComboPreviewCandidate): BadgeTone {
    const label = this.qualityLabel(candidate);
    return label === 'override' ? 'warning' : label === 'benchmark' ? 'info' : 'neutral';
  }

  protected breakdown(candidate: ComboPreviewCandidate): string {
    const score = candidate.score;
    const parts = [
      `price ${this.formatScore(score.price)}`,
      `speed ${this.formatScore(score.speed)}`,
      `quality ${this.formatScore(score.quality)}`,
      `reliability ${this.formatScore(score.reliability)}`,
      `penalties ${this.formatScore(score.penalties)}`,
    ];
    return parts.join(' · ');
  }

  protected markers(candidate: ComboPreviewCandidate): string {
    const score = candidate.score;
    const parts: string[] = [];
    if (score.imputed.length > 0) {
      parts.push(`imputed at 0.5: ${score.imputed.join(', ')}`);
    }
    if (score.shrunk.length > 0) {
      parts.push(`shrunk toward 0.5: ${score.shrunk.join(', ')}`);
    }
    return parts.join(' · ');
  }

  protected ageLabel(ageSeconds: number | null): string {
    if (ageSeconds === null) {
      return '—';
    }
    if (ageSeconds < 60) {
      return `${ageSeconds}s ago`;
    }
    return `${Math.floor(ageSeconds / 60)}m ago`;
  }
}
