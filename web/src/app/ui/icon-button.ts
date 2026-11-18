import { ChangeDetectionStrategy, Component, input, output } from '@angular/core';
import type { ButtonVariant, ButtonSize } from './button';

/**
 * Square icon-only button. `label` is required and becomes the accessible
 * name (and tooltip); project an inline SVG as the visible icon.
 */
@Component({
  selector: 'ui-icon-button',
  templateUrl: './icon-button.html',
  styleUrl: './icon-button.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class]': 'hostClass()' },
})
export class UiIconButton {
  readonly label = input.required<string>();
  readonly variant = input<ButtonVariant>('ghost');
  readonly size = input<ButtonSize>('md');
  readonly type = input<'button' | 'submit'>('button');
  readonly disabled = input(false);

  readonly clicked = output<MouseEvent>();

  protected hostClass(): string {
    return ['ui-icon-button', `variant-${this.variant()}`, `size-${this.size()}`].join(' ');
  }

  protected onClick(event: MouseEvent): void {
    if (!this.disabled()) {
      this.clicked.emit(event);
    }
  }
}
