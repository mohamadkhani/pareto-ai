import { ChangeDetectionStrategy, Component, input } from '@angular/core';

export type BadgeTone = 'neutral' | 'success' | 'warning' | 'danger' | 'info' | 'accent';

/** Small status pill; the tone carries meaning, the text carries the label. */
@Component({
  selector: 'ui-badge',
  template: '<ng-content />',
  styleUrl: './badge.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class]': 'hostClass()' },
})
export class UiBadge {
  readonly tone = input<BadgeTone>('neutral');

  protected hostClass(): string {
    return ['ui-badge', `tone-${this.tone()}`].join(' ');
  }
}
