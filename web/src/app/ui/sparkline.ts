import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';

const WIDTH = 100;
const HEIGHT = 32;
const PAD = 3;

/**
 * A small, non-interactive trend line. `values` is the series in order;
 * `label` is the accessible description (the chart has no per-point tooltip).
 * A flat or all-zero series draws a line through the middle instead of
 * pretending a peak at the top.
 */
@Component({
  selector: 'ui-sparkline',
  templateUrl: './sparkline.html',
  styleUrl: './sparkline.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class UiSparkline {
  readonly values = input<readonly number[]>([]);
  readonly label = input.required<string>();

  protected readonly points = computed(() => {
    const values = this.values();
    if (values.length === 0) {
      return '';
    }
    const max = Math.max(...values);
    const min = Math.min(...values);
    const flat = max === min;
    const yOf = (value: number): number =>
      flat ? HEIGHT / 2 : PAD + (1 - (value - min) / (max - min)) * (HEIGHT - PAD * 2);
    if (values.length === 1) {
      const y = yOf(values[0] ?? 0).toFixed(2);
      return `0,${y} ${WIDTH},${y}`;
    }
    const step = WIDTH / (values.length - 1);
    return values
      .map((value, index) => `${(index * step).toFixed(2)},${yOf(value).toFixed(2)}`)
      .join(' ');
  });

  protected readonly area = computed(() => {
    const points = this.points();
    return points === '' ? '' : `0,${HEIGHT} ${points} ${WIDTH},${HEIGHT}`;
  });
}
