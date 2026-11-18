import { ChangeDetectionStrategy, Component, input } from '@angular/core';

export type SpinnerSize = 'sm' | 'md' | 'lg';

/** Busy indicator with a screen-reader label; motion is disabled by base.sass. */
@Component({
  selector: 'ui-spinner',
  templateUrl: './spinner.html',
  styleUrl: './spinner.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class]': 'hostClass()' },
})
export class UiSpinner {
  readonly size = input<SpinnerSize>('md');
  readonly label = input('Loading');

  protected hostClass(): string {
    return ['ui-spinner', `size-${this.size()}`].join(' ');
  }
}
