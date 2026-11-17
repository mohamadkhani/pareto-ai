import { ChangeDetectionStrategy, Component, input } from '@angular/core';

/**
 * Empty-state block. Project an icon with `ui-empty-icon` and an action with
 * `ui-empty-action`; both slots hide when empty.
 */
@Component({
  selector: 'ui-empty',
  templateUrl: './empty.html',
  styleUrl: './empty.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class UiEmpty {
  readonly title = input<string>('');
  readonly description = input<string>('');
}
