import { ChangeDetectionStrategy, Component, input, output } from '@angular/core';

export type BannerTone = 'neutral' | 'success' | 'warning' | 'danger' | 'info';

/** Inline page-level message. `actionLabel` renders one optional action. */
@Component({
  selector: 'ui-banner',
  templateUrl: './banner.html',
  styleUrl: './banner.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class]': 'hostClass()' },
})
export class UiBanner {
  readonly tone = input<BannerTone>('neutral');
  readonly actionLabel = input<string>('');

  readonly actioned = output<void>();

  protected hostClass(): string {
    return ['ui-banner', `tone-${this.tone()}`].join(' ');
  }
}
