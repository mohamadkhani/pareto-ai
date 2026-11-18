import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';

export type MeterTone = 'accent' | 'success' | 'warning' | 'danger';

/**
 * Horizontal usage meter. `value` is a 0–100 percentage; `label` is the
 * accessible name and should be the thing being measured ("Daily budget").
 * The tone carries the state (normal, near limit, exhausted).
 */
@Component({
  selector: 'ui-meter',
  templateUrl: './meter.html',
  styleUrl: './meter.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class]': 'hostClass()' },
})
export class UiMeter {
  readonly value = input(0);
  readonly label = input('');
  readonly tone = input<MeterTone>('accent');

  protected readonly clamped = computed(() => Math.min(100, Math.max(0, Math.round(this.value()))));

  protected hostClass(): string {
    return ['ui-meter', `tone-${this.tone()}`].join(' ');
  }
}
