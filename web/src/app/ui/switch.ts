import { ChangeDetectionStrategy, Component, input, model } from '@angular/core';

/**
 * Native button with `role="switch"`. Space/Enter work through the button, and
 * the visible label supplies the accessible name.
 */
@Component({
  selector: 'ui-switch',
  templateUrl: './switch.html',
  styleUrl: './switch.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class.is-disabled]': 'disabled()' },
})
export class UiSwitch {
  readonly checked = model(false);
  readonly label = input<string>('');
  readonly disabled = input(false);

  protected toggle(): void {
    if (!this.disabled()) {
      this.checked.set(!this.checked());
    }
  }
}
