import { ChangeDetectionStrategy, Component, input, output, signal } from '@angular/core';

/**
 * A two-step delete button: the first click arms the control and swaps in
 * "Confirm"/"Cancel", the second click emits `confirmed`. Keeps destructive
 * actions confirmable without a modal or `window.confirm`.
 */
@Component({
  selector: 'app-delete-confirm',
  changeDetection: ChangeDetectionStrategy.OnPush,
  styles: `
    .confirm {
      display: inline-flex;
      gap: 0.4rem;
    }
  `,
  template: `
    @if (!armed()) {
      <button type="button" class="btn danger" (click)="arm()" [disabled]="pending()">
        {{ label() }}
      </button>
    } @else {
      <span class="confirm">
        <button type="button" class="btn danger" (click)="fire()" [disabled]="pending()">
          {{ pending() ? 'Deleting…' : confirmLabel() }}
        </button>
        <button type="button" class="btn" (click)="disarm()" [disabled]="pending()">Cancel</button>
      </span>
    }
  `,
})
export class DeleteConfirm {
  /** Label of the first, arming click. */
  readonly label = input('Delete');
  /** Label of the second, confirming click. */
  readonly confirmLabel = input('Confirm delete');
  /** While true the buttons are disabled and the confirm shows "Deleting…". */
  readonly pending = input(false);
  /** Emitted when the admin confirms the deletion. */
  readonly confirmed = output<void>();

  protected readonly armed = signal(false);

  protected arm(): void {
    this.armed.set(true);
  }

  protected disarm(): void {
    this.armed.set(false);
  }

  protected fire(): void {
    this.armed.set(false);
    this.confirmed.emit();
  }
}
