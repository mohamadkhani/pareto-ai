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

/** One selectable entry in a combobox. */
export interface ComboboxOption {
  /** The value written back through `value` / the form control. */
  value: string;
  /** The visible text; also the default search haystack. */
  label: string;
  /**
   * Extra text to match against when searching (a provider name, an upstream
   * id, an email). Optional: when absent only `label` is searched.
   */
  searchText?: string;
}

let nextComboboxId = 0;

/**
 * Searchable single-select for long lists (models, offerings, accounts).
 *
 * The trigger is a text input that filters `options` as the operator types;
 * the popup is a `role="listbox"` navigated with the arrow keys, Enter, and
 * Escape. It is a `ControlValueAccessor`, so it binds to a reactive form
 * (`formControl` / `formControlName`) or to the two-way `value` model, exactly
 * like `ui-select`. Inside a `ui-field` it adopts the field's id and
 * description wiring.
 *
 * Unlike `ui-select`, the options are data (`[options]`), not projected
 * `<option>` children, because filtering needs the list in memory.
 */
@Component({
  selector: 'ui-combobox',
  templateUrl: './combobox.html',
  styleUrls: ['./control.sass', './combobox.sass'],
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class.size-sm]': "size() === 'sm'" },
  providers: [
    { provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => UiCombobox), multi: true },
  ],
})
export class UiCombobox implements ControlValueAccessor {
  protected readonly field = inject(UI_FIELD, { optional: true });

  readonly id = input<string>('');
  /** Compact height for toolbars and inline rows; matches `ui-button size="sm"`. */
  readonly size = input<'sm' | 'md'>('md');
  readonly options = input<readonly ComboboxOption[]>([]);
  readonly value = model<string>('');
  readonly placeholder = input<string>('');
  readonly disabled = input(false);
  readonly name = input<string>('');
  /** Accessible name for a combobox rendered without a visible `ui-field` label. */
  readonly ariaLabel = input<string>('');

  /** Disabled state coming from the reactive form, separate from the input. */
  private readonly formDisabled = signal(false);
  private onChange: (value: string) => void = () => undefined;
  private onTouched: () => void = () => undefined;

  private readonly inputRef = viewChild<ElementRef<HTMLInputElement>>('inputEl');

  protected readonly listId = `ui-combobox-list-${++nextComboboxId}`;
  protected readonly open = signal(false);
  /** The text in the input: the picked label, or the operator's filter. */
  protected readonly query = signal('');
  /** Index into `filteredOptions` of the keyboard-highlighted row, or -1. */
  protected readonly activeIndex = signal(-1);

  protected readonly controlId = computed(() => this.id() || this.field?.id() || null);
  protected readonly describedBy = computed(() => this.field?.describedBy() ?? null);
  protected readonly invalid = computed(() => this.field?.invalid() ?? false);
  protected readonly isDisabled = computed(() => this.disabled() || this.formDisabled());

  /** The label of the current value, for the closed trigger. */
  private readonly selectedLabel = computed(
    () => this.options().find((option) => option.value === this.value())?.label ?? '',
  );

  /** Options matching the current query, in their original order. */
  protected readonly filteredOptions = computed<readonly ComboboxOption[]>(() => {
    const term = this.query().trim().toLowerCase();
    const options = this.options();
    if (term === '' || term === this.selectedLabel().toLowerCase()) {
      return options;
    }
    return options.filter((option) => {
      if (option.label.toLowerCase().includes(term)) {
        return true;
      }
      return option.searchText?.toLowerCase().includes(term) ?? false;
    });
  });

  /** The `aria-activedescendant` id, or null when nothing is highlighted. */
  protected readonly activeDescendant = computed(() => {
    const index = this.activeIndex();
    return index >= 0 ? this.optionId(index) : null;
  });

  /** The text shown in the input: the filter while open, the label when closed. */
  protected readonly displayValue = computed(() =>
    this.open() ? this.query() : this.selectedLabel(),
  );

  writeValue(value: unknown): void {
    const next = value === null || value === undefined ? '' : String(value);
    this.value.set(next);
    if (!this.open()) {
      this.query.set(this.selectedLabel());
    }
  }

  registerOnChange(fn: (value: string) => void): void {
    this.onChange = fn;
  }

  registerOnTouched(fn: () => void): void {
    this.onTouched = fn;
  }

  setDisabledState(isDisabled: boolean): void {
    this.formDisabled.set(isDisabled);
    if (isDisabled) {
      this.close();
    }
  }

  protected optionId(index: number): string {
    return `${this.listId}-opt-${index}`;
  }

  protected onFocus(): void {
    if (this.isDisabled()) {
      return;
    }
    this.query.set('');
    this.activeIndex.set(-1);
    this.open.set(true);
  }

  protected onInput(event: Event): void {
    const target = event.target;
    if (!(target instanceof HTMLInputElement)) {
      return;
    }
    this.query.set(target.value);
    this.activeIndex.set(-1);
    this.open.set(true);
  }

  protected onKeydown(event: KeyboardEvent): void {
    if (this.isDisabled()) {
      return;
    }
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault();
        this.moveActive(1);
        break;
      case 'ArrowUp':
        event.preventDefault();
        this.moveActive(-1);
        break;
      case 'Enter':
        event.preventDefault();
        this.commitActive();
        break;
      case 'Escape':
        event.preventDefault();
        this.close();
        break;
      case 'Tab':
        this.close();
        break;
    }
  }

  protected onOptionClick(option: ComboboxOption): void {
    this.select(option);
  }

  protected onOptionMove(option: ComboboxOption): void {
    const index = this.filteredOptions().indexOf(option);
    if (index >= 0) {
      this.activeIndex.set(index);
    }
  }

  protected onBlur(): void {
    // A click on a popup row blurs the input first; defer so the click's
    // selection wins over the close-and-revert.
    queueMicrotask(() => {
      if (this.open()) {
        this.close();
      }
    });
    this.onTouched();
  }

  private moveActive(delta: -1 | 1): void {
    if (!this.open()) {
      this.open.set(true);
    }
    const count = this.filteredOptions().length;
    if (count === 0) {
      this.activeIndex.set(-1);
      return;
    }
    const current = this.activeIndex();
    const next = current < 0 ? (delta > 0 ? 0 : count - 1) : (current + delta + count) % count;
    this.activeIndex.set(next);
  }

  private commitActive(): void {
    const index = this.activeIndex();
    if (index >= 0) {
      const option = this.filteredOptions()[index];
      if (option !== undefined) {
        this.select(option);
        return;
      }
    }
    this.close();
  }

  private select(option: ComboboxOption): void {
    this.value.set(option.value);
    this.onChange(option.value);
    this.close();
    this.inputRef()?.nativeElement.blur();
  }

  private close(): void {
    this.open.set(false);
    this.activeIndex.set(-1);
    this.query.set(this.selectedLabel());
  }
}
