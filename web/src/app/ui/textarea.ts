import { ChangeDetectionStrategy, Component, computed, inject, input, model } from '@angular/core';
import { UI_FIELD } from './field';

/** Multi-line input with the same field wiring as `ui-input`. */
@Component({
  selector: 'ui-textarea',
  templateUrl: './textarea.html',
  styleUrl: './control.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class UiTextarea {
  protected readonly field = inject(UI_FIELD, { optional: true });

  readonly id = input<string>('');
  readonly value = model<string>('');
  readonly placeholder = input<string>('');
  readonly rows = input(3);
  readonly disabled = input(false);
  readonly readonly = input(false);
  readonly name = input<string>('');

  protected readonly controlId = computed(() => this.id() || this.field?.id() || null);
  protected readonly describedBy = computed(() => this.field?.describedBy() ?? null);
  protected readonly invalid = computed(() => this.field?.invalid() ?? false);

  protected onInput(event: Event): void {
    const target = event.target;
    if (target instanceof HTMLTextAreaElement) {
      this.value.set(target.value);
    }
  }
}
