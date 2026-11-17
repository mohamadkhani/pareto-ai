import { ChangeDetectionStrategy, Component, input, output } from '@angular/core';
import { UiSpinner } from './spinner';

export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger';
export type ButtonSize = 'sm' | 'md';

/**
 * Native button wrapper. `type` defaults to `button` so a button inside a form
 * never submits by accident. `loading` disables interaction and shows a
 * spinner while keeping the label in place.
 */
@Component({
  selector: 'ui-button',
  imports: [UiSpinner],
  templateUrl: './button.html',
  styleUrl: './button.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class]': 'hostClass()' },
})
export class UiButton {
  readonly variant = input<ButtonVariant>('secondary');
  readonly size = input<ButtonSize>('md');
  readonly type = input<'button' | 'submit' | 'reset'>('button');
  readonly disabled = input(false);
  readonly loading = input(false);
  /**
   * ARIA state forwarded to the inner `<button>`: attribute bindings on the
   * `<ui-button>` host never reach it, so assistive tech would miss them.
   * `null` omits the attribute entirely.
   */
  readonly ariaExpanded = input<boolean | null>(null);
  /** The id of the region this button expands/collapses. */
  readonly ariaControls = input<string>('');

  readonly clicked = output<MouseEvent>();

  protected hostClass(): string {
    return ['ui-button', `variant-${this.variant()}`, `size-${this.size()}`].join(' ');
  }

  protected onClick(event: MouseEvent): void {
    if (this.disabled() || this.loading()) {
      return;
    }
    this.clicked.emit(event);
  }
}
