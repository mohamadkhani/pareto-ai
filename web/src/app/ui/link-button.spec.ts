import { Component } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { UiLinkButton } from './link-button';

@Component({
  imports: [UiLinkButton],
  template: `<span
    >Filtered by <ui-link-button (clicked)="clicks = clicks + 1">ApiNex</ui-link-button></span
  >`,
})
class Host {
  clicks = 0;
}

describe('UiLinkButton', () => {
  async function create(): Promise<ComponentFixture<Host>> {
    TestBed.configureTestingModule({ imports: [Host] });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    return fixture;
  }

  it('renders a real button whose text is the accessible name', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const button = root.querySelector('ui-link-button button');

    expect(button).not.toBeNull();
    expect(button?.getAttribute('type')).toBe('button');
    expect(button?.textContent?.trim()).toBe('ApiNex');
  });

  it('emits clicked from the projected text action', async () => {
    const fixture = await create();
    const button = (fixture.nativeElement as HTMLElement).querySelector(
      'ui-link-button button',
    ) as HTMLButtonElement;

    button.click();
    fixture.detectChanges();
    expect(fixture.componentInstance.clicks).toBe(1);
  });
});
