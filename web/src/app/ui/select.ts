import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
  forwardRef,
  inject,
  input,
  model,
  signal,
  viewChild,
} from '@angular/core';
import { ControlValueAccessor, NG_VALUE_ACCESSOR } from '@angular/forms';
import { UI_FIELD } from './field';

/** Native select with projected `<option>` children and our own chevron. */
@Component({
  selector: 'ui-select',
  templateUrl: './select.html',
  styleUrls: ['./control.css', './select.css'],
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class.size-sm]': "size() === 'sm'" },
  providers: [{ provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => UiSelect), multi: true }],
})
export class UiSelect implements ControlValueAccessor {
  protected readonly field = inject(UI_FIELD, { optional: true });

  readonly id = input<string>('');
  /** Compact height for toolbars and inline rows; matches `ui-button size="sm"`. */
  readonly size = input<'sm' | 'md'>('md');
  readonly value = model<string>('');
  readonly disabled = input(false);
  readonly name = input<string>('');

  /** Disabled state coming from the reactive form, separate from the input. */
  private readonly formDisabled = signal(false);
  private onChange: (value: string) => void = () => undefined;
  private onTouched: () => void = () => undefined;

  private readonly selectRef = viewChild<ElementRef<HTMLSelectElement>>('selectEl');

  protected readonly controlId = computed(() => this.id() || this.field?.id() || null);
  protected readonly describedBy = computed(() => this.field?.describedBy() ?? null);
  protected readonly invalid = computed(() => this.field?.invalid() ?? false);
  protected readonly isDisabled = computed(() => this.disabled() || this.formDisabled());

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

  protected onChangeEvent(event: Event): void {
    const target = event.target;
    if (target instanceof HTMLSelectElement) {
      this.value.set(target.value);
      this.onChange(target.value);
    }
  }

  protected onBlur(): void {
    this.onTouched();
  }
}
