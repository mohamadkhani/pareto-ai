import { ChangeDetectionStrategy, Component, input, output } from '@angular/core';

/** One editable custom-header row. */
export interface HeaderRow {
  name: string;
  value: string;
}

/** Converts a header map to editable rows (order preserved from the map). */
export function headerRowsOf(headers: Record<string, string> | null): HeaderRow[] {
  return Object.entries(headers ?? {}).map(([name, value]) => ({ name, value }));
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
  changeDetection: ChangeDetectionStrategy.OnPush,
  styles: `
    .rows {
      display: flex;
      flex-direction: column;
      gap: 0.45rem;
    }

    .row {
      display: flex;
      gap: 0.45rem;
    }

    .row input {
      flex: 1;
    }

    .empty {
      margin: 0;
      color: var(--muted);
      font-size: 0.85rem;
    }

    .add {
      margin-top: 0.5rem;
    }
  `,
  template: `
    <div class="rows">
      @for (row of rows(); track $index; let i = $index) {
        <div class="row">
          <input
            type="text"
            [value]="row.name"
            placeholder="Header name"
            [attr.aria-label]="'Header name ' + (i + 1)"
            (input)="updateName(i, $event)"
          />
          <input
            type="text"
            [value]="readOnlyValues() ? '' : row.value"
            [placeholder]="readOnlyValues() && row.value === '' ? 'stored (write-only)' : 'Value'"
            [attr.aria-label]="'Header value ' + (i + 1)"
            (input)="updateValue(i, $event)"
          />
          <button type="button" class="btn" (click)="removeRow(i)">Remove</button>
        </div>
      } @empty {
        <p class="empty">No custom headers.</p>
      }
      <div class="add">
        <button type="button" class="btn" (click)="addRow()">Add header</button>
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

  protected updateName(index: number, event: Event): void {
    this.replace(index, { name: inputValue(event), value: this.rows()[index].value });
  }

  protected updateValue(index: number, event: Event): void {
    this.replace(index, { name: this.rows()[index].name, value: inputValue(event) });
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

function inputValue(event: Event): string {
  return (event.target as HTMLInputElement).value;
}
