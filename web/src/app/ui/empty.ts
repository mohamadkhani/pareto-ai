import { ChangeDetectionStrategy, Component, input } from '@angular/core';

/**
 * Empty-state block. Project an icon with `ui-empty-icon` and an action with
 * `ui-empty-action`; both slots hide when empty.
 */
@Component({
  selector: 'ui-empty',
  templateUrl: './empty.html',
  styleUrl: './empty.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class.is-compact]': 'compact()' },
})
export class UiEmpty {
  readonly title = input<string>('');
  readonly description = input<string>('');
  /** Inline, borderless variant for a compact card body. */
  readonly compact = input(false);
}
