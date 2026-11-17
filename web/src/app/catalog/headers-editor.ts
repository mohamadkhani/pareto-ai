import { ChangeDetectionStrategy, Component, input, output } from '@angular/core';
import { UiButton } from '../ui/button';
import { UiInput } from '../ui/input';

/** One editable custom-header row. */
export interface HeaderRow {
  name: string;
  value: string;
}

/** Converts rows back to a map, dropping rows without a name or a value. */
export function headerMapOf(rows: readonly HeaderRow[]): Record<string, string> | null {
  const headers: Record<string, string> = {};
  for (const row of rows) {
    const name = row.name.trim();
    if (name.length > 0 && row.value !== '') {
      headers[name] = row.value;
    }
  }
  return Object.keys(headers).length === 0 ? null : headers;
}

/**
 * Key/value editor for custom request headers. Presentational: the parent owns
 * the rows and reacts to `rowsChange`. With `readOnlyValues` the value inputs
 * are write-only (placeholder instead of the stored value), used for
 * credential headers whose values the server never returns.
 */
@Component({
  selector: 'app-headers-editor',
  imports: [UiButton, UiInput],
  changeDetection: ChangeDetectionStrategy.OnPush,
  styles: `
    .rows {
      display: flex;
      flex-direction: column;
      gap: var(--space-2);
    }

    .row {
      display: flex;
      align-items: flex-start;
      gap: var(--space-2);
    }

    .row ui-input {
      flex: 1;
    }

    .empty {
      margin: 0;
      color: var(--text-muted);
      font-size: var(--text-small);
    }
  `,
  template: `
    <div class="rows">
      @for (row of rows(); track $index; let i = $index) {
        <div class="row">
          <ui-input
            [value]="row.name"
            placeholder="Header name"
            [ariaLabel]="'Header name ' + (i + 1)"
            (valueChange)="updateName(i, $event)"
          />
          <ui-input
            [value]="readOnlyValues() ? '' : row.value"
            [placeholder]="readOnlyValues() && row.value === '' ? 'stored (write-only)' : 'Value'"
            [ariaLabel]="'Header value ' + (i + 1)"
            (valueChange)="updateValue(i, $event)"
          />
          <ui-button size="sm" (clicked)="removeRow(i)">Remove</ui-button>
        </div>
      } @empty {
        <p class="empty">No custom headers.</p>
      }
      <div>
        <ui-button size="sm" (clicked)="addRow()">Add header</ui-button>
      </div>
    </div>
  `,
})
export class HeadersEditor {
  /** The current rows; the parent owns this state. */
  readonly rows = input.required<readonly HeaderRow[]>();
  /** When true, stored values are hidden (write-only) and never shown back. */
  readonly readOnlyValues = input(false);
  /** Emitted with the new rows after any edit. */
  readonly rowsChange = output<HeaderRow[]>();

  protected updateName(index: number, name: string): void {
    this.replace(index, { name, value: this.rows()[index].value });
  }

  protected updateValue(index: number, value: string): void {
    this.replace(index, { name: this.rows()[index].name, value });
  }

  protected addRow(): void {
    this.rowsChange.emit([...this.rows(), { name: '', value: '' }]);
  }

  protected removeRow(index: number): void {
    this.rowsChange.emit(this.rows().filter((_, i) => i !== index));
  }

  private replace(index: number, row: HeaderRow): void {
    this.rowsChange.emit(this.rows().map((existing, i) => (i === index ? row : existing)));
  }
}
