import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { UiButton } from './button';

describe('UiButton', () => {
  async function create(): Promise<ComponentFixture<UiButton>> {
    TestBed.configureTestingModule({ imports: [UiButton] });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(UiButton);
    fixture.detectChanges();
    return fixture;
  }

  it('renders a native button with type button by default', async () => {
    const fixture = await create();
    const button = (fixture.nativeElement as HTMLElement).querySelector('button');

    expect(button?.type).toBe('button');
    expect(button?.disabled).toBe(false);
  });

  it('moves focus to the native button on request', async () => {
    const fixture = await create();
    const button = (fixture.nativeElement as HTMLElement).querySelector('button');
    fixture.componentInstance.focus();

    expect(document.activeElement).toBe(button);
  });

  it('emits clicked only when enabled', async () => {
    const fixture = await create();
    const clicked = vi.fn();
    fixture.componentInstance.clicked.subscribe(clicked);
    const root = fixture.nativeElement as HTMLElement;
    const button = root.querySelector('button');
    button?.click();

    expect(clicked).toHaveBeenCalledTimes(1);

    fixture.componentRef.setInput('disabled', true);
    fixture.detectChanges();
    button?.click();
    expect(clicked).toHaveBeenCalledTimes(1);
  });

  it('disables the button and shows a spinner while loading', async () => {
    const fixture = await create();
    fixture.componentRef.setInput('loading', true);
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    const button = root.querySelector('button');

    expect(button?.disabled).toBe(true);
    expect(button?.getAttribute('aria-busy')).toBe('true');
    expect(root.querySelector('ui-spinner')).not.toBeNull();
  });

  it('applies variant and size classes on the host', async () => {
    const fixture = await create();
    fixture.componentRef.setInput('variant', 'danger');
    fixture.componentRef.setInput('size', 'sm');
    fixture.detectChanges();
    const host = fixture.nativeElement as HTMLElement;

    expect(host.classList.contains('variant-danger')).toBe(true);
    expect(host.classList.contains('size-sm')).toBe(true);
  });

  it('forwards aria-expanded and aria-controls to the inner button', async () => {
    const fixture = await create();
    fixture.componentRef.setInput('ariaExpanded', true);
    fixture.componentRef.setInput('ariaControls', 'decision-row-1');
    fixture.detectChanges();
    const host = fixture.nativeElement as HTMLElement;
    const button = host.querySelector('button');

    expect(button?.getAttribute('aria-expanded')).toBe('true');
    expect(button?.getAttribute('aria-controls')).toBe('decision-row-1');
    expect(host.getAttribute('aria-expanded')).toBeNull();

    fixture.componentRef.setInput('ariaExpanded', false);
    fixture.detectChanges();
    expect(button?.getAttribute('aria-expanded')).toBe('false');
  });

  it('omits aria-expanded when the caller does not set it', async () => {
    const fixture = await create();
    const button = (fixture.nativeElement as HTMLElement).querySelector('button');

    expect(button?.hasAttribute('aria-expanded')).toBe(false);
    expect(button?.hasAttribute('aria-controls')).toBe(false);
  });
});
