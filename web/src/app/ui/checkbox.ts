import { ChangeDetectionStrategy, Component, input, model } from '@angular/core';

/**
 * Native checkbox with a token-styled box. The visible `label` is also the
 * accessible name; the input is the focus target, so keyboard and
 * `:focus-visible` work without extra wiring.
 */
@Component({
  selector: 'ui-checkbox',
  templateUrl: './checkbox.html',
  styleUrl: './checkbox.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class.is-disabled]': 'disabled()' },
})
export class UiCheckbox {
  readonly checked = model(false);
  readonly disabled = input(false);
  readonly label = input<string>('');

  protected onChange(event: Event): void {
    if (event.target instanceof HTMLInputElement) {
      this.checked.set(event.target.checked);
    }
  }
}
