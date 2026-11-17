import { ChangeDetectionStrategy, Component, computed, inject, input, model } from '@angular/core';
import { UI_FIELD } from './field';

export type InputType = 'text' | 'email' | 'password' | 'number' | 'search' | 'tel' | 'url';

/**
 * Text-like input. Inside a `ui-field` it adopts the field's id and
 * description wiring; outside, `id` and native attributes are passed through.
 */
@Component({
  selector: 'ui-input',
  templateUrl: './input.html',
  styleUrl: './control.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class UiInput {
  protected readonly field = inject(UI_FIELD, { optional: true });

  readonly id = input<string>('');
  readonly type = input<InputType>('text');
  readonly value = model<string>('');
  readonly placeholder = input<string>('');
  readonly disabled = input(false);
  readonly readonly = input(false);
  readonly name = input<string>('');
  readonly autocomplete = input<string>('');
  /** Native `inputmode` hint, e.g. `numeric` for amounts. */
  readonly inputmode = input<string>('');
  /** Accessible name for a control rendered without a visible `ui-field` label. */
  readonly ariaLabel = input<string>('');

  protected readonly controlId = computed(() => this.id() || this.field?.id() || null);
  protected readonly describedBy = computed(() => this.field?.describedBy() ?? null);
  protected readonly invalid = computed(() => this.field?.invalid() ?? false);

  protected onInput(event: Event): void {
    const target = event.target;
    if (target instanceof HTMLInputElement) {
      this.value.set(target.value);
    }
  }
}
