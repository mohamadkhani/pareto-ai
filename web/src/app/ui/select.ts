import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  viewChild,
  computed,
  inject,
  input,
  model,
} from '@angular/core';
import { UI_FIELD } from './field';

/** Native select with projected `<option>` children. */
@Component({
  selector: 'ui-select',
  templateUrl: './select.html',
  styleUrl: './control.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class UiSelect {
  protected readonly field = inject(UI_FIELD, { optional: true });

  readonly id = input<string>('');
  readonly value = model<string>('');
  readonly disabled = input(false);
  readonly name = input<string>('');

  private readonly selectRef = viewChild<ElementRef<HTMLSelectElement>>('selectEl');

  protected readonly controlId = computed(() => this.id() || this.field?.id() || null);
  protected readonly describedBy = computed(() => this.field?.describedBy() ?? null);
  protected readonly invalid = computed(() => this.field?.invalid() ?? false);

  /**
   * `<option>`s are projected content, so on the first render the `[value]`
   * binding is applied before the options exist and the browser silently
   * selects the first one. Re-apply the model after every view check, when
   * the projected options are in the DOM; the guard keeps it a no-op when the
   * value already matches.
   */
  ngAfterViewChecked(): void {
    const select = this.selectRef()?.nativeElement;
    const value = this.value();
    if (select === undefined || select.value === value) {
      return;
    }
    // Only match an existing option; an unknown value keeps the browser's
    // default instead of blanking the control.
    if (Array.from(select.options).some((option) => option.value === value)) {
      select.value = value;
    }
  }

  protected onChange(event: Event): void {
    const target = event.target;
    if (target instanceof HTMLSelectElement) {
      this.value.set(target.value);
    }
  }
}
