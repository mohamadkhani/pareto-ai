import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  viewChild,
  computed,
  forwardRef,
  inject,
  input,
  model,
  signal,
} from '@angular/core';
import { ControlValueAccessor, NG_VALUE_ACCESSOR } from '@angular/forms';
import { UI_FIELD } from './field';

/** Native select with projected `<option>` children. */
@Component({
  selector: 'ui-select',
  templateUrl: './select.html',
  styleUrls: ['./control.css', './select.css'],
  changeDetection: ChangeDetectionStrategy.OnPush,
  providers: [{ provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => UiSelect), multi: true }],
})
export class UiSelect implements ControlValueAccessor {
  protected readonly field = inject(UI_FIELD, { optional: true });

  readonly id = input<string>('');
  readonly value = model<string>('');
  readonly disabled = input(false);
  readonly name = input<string>('');

  private readonly selectRef = viewChild<ElementRef<HTMLSelectElement>>('selectEl');
  private readonly formDisabled = signal(false);

  protected readonly controlId = computed(() => this.id() || this.field?.id() || null);
  protected readonly describedBy = computed(() => this.field?.describedBy() ?? null);
  protected readonly invalid = computed(() => this.field?.invalid() ?? false);
  protected readonly isDisabled = computed(() => this.disabled() || this.formDisabled());

  private onChange: (value: string) => void = () => {};
  private onTouched: () => void = () => {};

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

  writeValue(value: unknown): void {
    this.value.set(value === null || value === undefined ? '' : String(value));
  }

  registerOnChange(fn: (value: string) => void): void {
    this.onChange = fn;
  }

  registerOnTouched(fn: () => void): void {
    this.onTouched = fn;
  }

  setDisabledState(isDisabled: boolean): void {
    this.formDisabled.set(isDisabled);
  }

  protected onChangeEvent(event: Event): void {
    const target = event.target;
    if (target instanceof HTMLSelectElement) {
      this.onChange(target.value);
      this.value.set(target.value);
    }
  }

  protected onBlur(): void {
    this.onTouched();
  }
}
