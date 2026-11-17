import { ChangeDetectionStrategy, Component, input } from '@angular/core';

/**
 * Scroll wrapper for a projected `<table>`; styling of the projected cells is
 * global (see `styles/components.css`). `sticky` pins the header, `dense`
 * tightens cell padding.
 */
@Component({
  selector: 'ui-table',
  templateUrl: './table.html',
  styleUrl: './table.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class]': 'hostClass()' },
})
export class UiTable {
  readonly sticky = input(false);
  readonly dense = input(false);

  protected hostClass(): string {
    return ['ui-table', this.sticky() ? 'is-sticky' : '', this.dense() ? 'is-dense' : ''].join(' ');
  }
}
