import {
  AfterViewInit,
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  effect,
  input,
  model,
  output,
  viewChild,
} from '@angular/core';

let nextDialogId = 0;

/** `modal` is the centred dialog; `drawer` pins it to the right edge. */
export type DialogVariant = 'modal' | 'drawer';

/**
 * Modal built on native `<dialog>`. `open` is two-way bindable; `show()` and
 * `close()` are the imperative API. ESC closes and returns focus to the
 * previously focused element (native behavior); backdrop click is optional.
 * `title` names the dialog via `aria-labelledby` when present. `variant`
 * selects the centred modal or a right-edge drawer with the same focus and
 * Escape behavior.
 */
@Component({
  selector: 'ui-dialog',
  templateUrl: './dialog.html',
  styleUrl: './dialog.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class UiDialog implements AfterViewInit {
  readonly open = model(false);
  readonly variant = input<DialogVariant>('modal');
  readonly title = input<string>('');
  readonly closeOnBackdrop = input(true);
  /** Accessible name when no visible title is rendered. */
  readonly ariaLabel = input<string>('');

  readonly closed = output<void>();

  readonly titleId = `ui-dialog-title-${++nextDialogId}`;
  private readonly dialogEl = viewChild<ElementRef<HTMLDialogElement>>('dialog');

  constructor() {
    effect(() => {
      const el = this.dialogEl()?.nativeElement;
      if (el === undefined) {
        return;
      }
      if (this.open()) {
        if (!el.open) {
          this.showNative(el);
        }
      } else if (el.open) {
        this.closeNative(el);
      }
    });
  }

  show(): void {
    this.open.set(true);
  }

  /**
   * A dialog created already open (a caller that renders it behind `@if` with
   * `[open]="true"`) can miss the effect's first run: the view child is not
   * resolved yet. Opening here closes that gap once the view exists.
   */
  ngAfterViewInit(): void {
    const el = this.dialogEl()?.nativeElement;
    if (el !== undefined && this.open() && !el.open) {
      this.showNative(el);
    }
  }

  close(): void {
    this.open.set(false);
  }

  protected onCancel(event: Event): void {
    event.preventDefault();
    this.close();
  }

  protected onClose(): void {
    if (this.open()) {
      this.open.set(false);
    }
    this.closed.emit();
  }

  protected onDialogClick(event: MouseEvent): void {
    if (this.closeOnBackdrop() && event.target === event.currentTarget) {
      this.close();
    }
  }

  /** jsdom lacks `showModal`; the `open` attribute keeps non-modal fallbacks working. */
  private showNative(el: HTMLDialogElement): void {
    if (typeof el.showModal === 'function') {
      el.showModal();
    } else {
      el.setAttribute('open', '');
    }
  }

  private closeNative(el: HTMLDialogElement): void {
    if (typeof el.close === 'function') {
      el.close();
    } else {
      el.removeAttribute('open');
      this.closed.emit();
    }
  }
}
