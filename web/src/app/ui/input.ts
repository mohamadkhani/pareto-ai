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
 * Text-like input with a native picker for date/time types. Inside a `ui-field`
 * it adopts the field's id and description wiring; outside, `id` and native
 * attributes are passed through.
 *
 * The two-way `value` is always the DOM string. When the input is bound to a
 * reactive form (`formControl` / `formControlName`), a `type="number"` control
 * receives a `number` (or `null` when empty) so numeric validators keep
 * working, matching Angular's native number value accessor.
 */
@Component({
  selector: 'ui-input',
  templateUrl: './input.html',
  styleUrl: './control.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class.size-sm]': "size() === 'sm'" },
  providers: [{ provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => UiInput), multi: true }],
})
export class UiInput implements ControlValueAccessor {
  protected readonly field = inject(UI_FIELD, { optional: true });

  readonly id = input<string>('');
  /** Compact height for toolbars and inline rows; matches `ui-button size="sm"`. */
  readonly size = input<'sm' | 'md'>('md');
  readonly type = input<InputType>('text');
  readonly value = model<string>('');
  readonly placeholder = input<string>('');
  readonly disabled = input(false);
  readonly readonly = input(false);
  readonly name = input<string>('');
  readonly autocomplete = input<string>('');
  /** Native `inputmode` hint, e.g. `numeric` for amounts. */
  readonly inputmode = input<string>('');
  /** Step for number, date, time, and datetime-local inputs. */
  readonly step = input<string | number | null>(null);
  /** Minimum value for number, date, time, and datetime-local inputs. */
  readonly min = input<string | number | null>(null);
  /** Maximum value for number, date, time, and datetime-local inputs. */
  readonly max = input<string | number | null>(null);
  /** Accessible name for a control rendered without a visible `ui-field` label. */
  readonly ariaLabel = input<string>('');

  /** Disabled state coming from the reactive form, separate from the input. */
  private readonly formDisabled = signal(false);
  private onChange: (value: string | number | null) => void = () => undefined;
  private onTouched: () => void = () => undefined;

  protected readonly controlId = computed(() => this.id() || this.field?.id() || null);
  protected readonly describedBy = computed(() => this.field?.describedBy() ?? null);
  protected readonly invalid = computed(() => this.field?.invalid() ?? false);
  protected readonly isDisabled = computed(() => this.disabled() || this.formDisabled());

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
    if (!(target instanceof HTMLInputElement)) {
      return;
    }
    const raw = target.value;
    this.value.set(raw);
    this.onChange(this.type() === 'number' ? parseNumber(raw) : raw);
  }

  protected onBlur(): void {
    this.onTouched();
  }
}

/** An empty number field is `null`; anything else goes through `Number`. */
function parseNumber(raw: string): number | null {
  if (raw.trim() === '') {
    return null;
  }
  const parsed = Number(raw);
  return Number.isNaN(parsed) ? null : parsed;
}
