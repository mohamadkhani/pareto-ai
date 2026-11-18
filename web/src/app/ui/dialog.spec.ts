import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { UiDialog } from './dialog';

describe('UiDialog', () => {
  async function create(): Promise<ComponentFixture<UiDialog>> {
    TestBed.configureTestingModule({ imports: [UiDialog] });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(UiDialog);
    fixture.componentRef.setInput('title', 'Delete offering');
    fixture.detectChanges();
    return fixture;
  }

  it('opens and closes through the model, with the native open attribute', async () => {
    const fixture = await create();
    const dialog = (fixture.nativeElement as HTMLElement).querySelector('dialog');
    expect(dialog?.hasAttribute('open')).toBe(false);

    fixture.componentInstance.show();
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(true);
    expect(dialog?.hasAttribute('open')).toBe(true);

    fixture.componentInstance.close();
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(false);
    expect(dialog?.hasAttribute('open')).toBe(false);
  });

  it('adds the drawer class only for the drawer variant', async () => {
    const fixture = await create();
    const dialog = (fixture.nativeElement as HTMLElement).querySelector('dialog');
    expect(dialog?.classList.contains('dialog--drawer')).toBe(false);

    fixture.componentRef.setInput('variant', 'drawer');
    fixture.detectChanges();
    expect(dialog?.classList.contains('dialog--drawer')).toBe(true);
  });

  it('widens the drawer only when size is lg', async () => {
    const fixture = await create();
    const dialog = (fixture.nativeElement as HTMLElement).querySelector('dialog');
    expect(dialog?.classList.contains('dialog--wide')).toBe(false);

    fixture.componentRef.setInput('variant', 'drawer');
    fixture.componentRef.setInput('size', 'lg');
    fixture.detectChanges();
    expect(dialog?.classList.contains('dialog--wide')).toBe(true);
  });

  it('names the dialog from its title', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const dialog = root.querySelector('dialog');
    const title = root.querySelector('.dialog-title');

    expect(title?.textContent).toContain('Delete offering');
    expect(dialog?.getAttribute('aria-labelledby')).toBe(title?.id);
  });

  it('closes on Escape (cancel) and emits closed', async () => {
    const fixture = await create();
    const closed = vi.fn();
    fixture.componentInstance.closed.subscribe(closed);
    fixture.componentInstance.show();
    fixture.detectChanges();

    const dialog = (fixture.nativeElement as HTMLElement).querySelector('dialog');
    dialog?.dispatchEvent(new Event('cancel', { cancelable: true }));
    fixture.detectChanges();

    expect(fixture.componentInstance.open()).toBe(false);
    expect(closed).toHaveBeenCalled();
  });

  it('closes on backdrop click only when enabled', async () => {
    const fixture = await create();
    fixture.componentInstance.show();
    fixture.detectChanges();
    const dialog = (fixture.nativeElement as HTMLElement).querySelector('dialog');

    dialog?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(false);

    fixture.componentRef.setInput('closeOnBackdrop', false);
    fixture.componentInstance.show();
    fixture.detectChanges();
    dialog?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(true);
  });
});
