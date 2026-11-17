import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { UiIconButton } from './icon-button';

describe('UiIconButton', () => {
  async function create(): Promise<ComponentFixture<UiIconButton>> {
    TestBed.configureTestingModule({ imports: [UiIconButton] });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(UiIconButton);
    fixture.componentRef.setInput('label', 'Refresh prices');
    fixture.detectChanges();
    return fixture;
  }

  it('uses the required label as accessible name and tooltip', async () => {
    const fixture = await create();
    const button = (fixture.nativeElement as HTMLElement).querySelector('button');

    expect(button?.getAttribute('aria-label')).toBe('Refresh prices');
    expect(button?.getAttribute('title')).toBe('Refresh prices');
    expect(button?.type).toBe('button');
  });

  it('emits clicked unless disabled', async () => {
    const fixture = await create();
    const clicked = vi.fn();
    fixture.componentInstance.clicked.subscribe(clicked);
    const button = (fixture.nativeElement as HTMLElement).querySelector('button');

    button?.click();
    expect(clicked).toHaveBeenCalledTimes(1);

    fixture.componentRef.setInput('disabled', true);
    fixture.detectChanges();
    button?.click();
    expect(clicked).toHaveBeenCalledTimes(1);
  });
});
