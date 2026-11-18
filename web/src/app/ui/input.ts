import {
  ChangeDetectionStrategy,
  Component,
  computed,
  forwardRef,
  inject,
  input,
  model,
  signal,
} from '@angular/core';
import { ControlValueAccessor, NG_VALUE_ACCESSOR } from '@angular/forms';
import { UI_FIELD } from './field';

export type InputType =
  | 'text'
  | 'email'
  | 'password'
  | 'number'
  | 'search'
  | 'tel'
  | 'url'
  | 'date'
  | 'time'
  | 'datetime-local';

/**
 * Text-like input. Inside a `ui-field` it adopts the field's id and
 * description wiring; outside, `id` and native attributes are passed through.
 * It is also a `ControlValueAccessor`, so reactive forms may bind it with
 * `formControlName`; numeric inputs emit numbers, every other type a string.
 */
@Component({
  selector: 'ui-input',
  templateUrl: './input.html',
  styleUrl: './control.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
  providers: [{ provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => UiInput), multi: true }],
})
export class UiInput implements ControlValueAccessor {
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
  /** Native `step` for number and date/time inputs. */
  readonly step = input<string>('');
  /** Native `min` for number and date/time inputs. */
  readonly min = input<string>('');
  /** Native `max` for number and date/time inputs. */
  readonly max = input<string>('');
  /** Accessible name for a control rendered without a visible `ui-field` label. */
  readonly ariaLabel = input<string>('');

  private readonly formDisabled = signal(false);

  protected readonly controlId = computed(() => this.id() || this.field?.id() || null);
  protected readonly describedBy = computed(() => this.field?.describedBy() ?? null);
  protected readonly invalid = computed(() => this.field?.invalid() ?? false);
  protected readonly isDisabled = computed(() => this.disabled() || this.formDisabled());

  private onChange: (value: string | number | null) => void = () => {};
  private onTouched: () => void = () => {};

  writeValue(value: unknown): void {
    this.value.set(value === null || value === undefined ? '' : String(value));
  }

  registerOnChange(fn: (value: string | number | null) => void): void {
    this.onChange = fn;
  }

  registerOnTouched(fn: () => void): void {
    this.onTouched = fn;
  }

  setDisabledState(isDisabled: boolean): void {
    this.formDisabled.set(isDisabled);
  }

  protected onInput(event: Event): void {
    const target = event.target;
    if (target instanceof HTMLInputElement) {
      // Forms see a number for `type="number"` (empty clears to null); the
      // two-way `value` model stays a string for the text-like call sites.
      this.onChange(this.emitValue(target.value));
      this.value.set(target.value);
    }
  }

  protected onBlur(): void {
    this.onTouched();
  }

  private emitValue(raw: string): string | number | null {
    if (this.type() !== 'number') {
      return raw;
    }
    if (raw.trim() === '') {
      return null;
    }
    const parsed = Number(raw);
    return Number.isNaN(parsed) ? null : parsed;
  }
}
