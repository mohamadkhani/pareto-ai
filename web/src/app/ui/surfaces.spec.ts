import { Component } from '@angular/core';
import { type ComponentFixture, TestBed } from '@angular/core/testing';
import { UiBadge } from './badge';
import { UiBanner } from './banner';
import { UiCard } from './card';
import { UiEmpty } from './empty';
import { UiSpinner } from './spinner';
import { UiTable } from './table';

@Component({
  imports: [UiBadge, UiBanner, UiCard, UiEmpty, UiSpinner, UiTable],
  template: `
    <ui-badge tone="success">enabled</ui-badge>
    <ui-card>
      <div ui-card-header>Offering summary</div>
      <p>Body text</p>
      <div ui-card-footer>Footer</div>
    </ui-card>
    <ui-table [sticky]="true" [dense]="true">
      <table>
        <thead>
          <tr>
            <th scope="col">Provider</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>openrouter</td>
          </tr>
        </tbody>
      </table>
    </ui-table>
    <ui-banner tone="warning" actionLabel="Review" (actioned)="actions = actions + 1">
      Manual prices are not overwritten.
    </ui-banner>
    <ui-empty title="No offerings yet" description="Create the first offering.">
      <span ui-empty-icon>icon</span>
      <span ui-empty-action>action</span>
    </ui-empty>
    <ui-spinner label="Loading prices" />
  `,
})
class SurfacesHost {
  actions = 0;
}

describe('Surface primitives', () => {
  async function create(): Promise<ComponentFixture<SurfacesHost>> {
    TestBed.configureTestingModule({ imports: [SurfacesHost] });
    await TestBed.compileComponents();
    const fixture = TestBed.createComponent(SurfacesHost);
    fixture.detectChanges();
    return fixture;
  }

  it('renders badge tones and card slots', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const badge = root.querySelector<HTMLElement>('ui-badge');
    const card = root.querySelector('ui-card');

    expect(badge?.classList.contains('tone-success')).toBe(true);
    expect(badge?.textContent).toContain('enabled');
    expect(card?.textContent).toContain('Offering summary');
    expect(card?.textContent).toContain('Body text');
    expect(card?.textContent).toContain('Footer');
  });

  it('renders the table wrapper with sticky and dense flags', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const table = root.querySelector('ui-table');

    expect(table?.classList.contains('is-sticky')).toBe(true);
    expect(table?.classList.contains('is-dense')).toBe(true);
    expect(table?.querySelector('table thead th')?.textContent).toContain('Provider');
  });

  it('emits from the banner action and renders the empty-state slots', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    root.querySelector<HTMLButtonElement>('.banner-action')?.click();
    fixture.detectChanges();

    expect(fixture.componentInstance.actions).toBe(1);
    const empties = root.querySelector('ui-empty');
    expect(empties?.textContent).toContain('No offerings yet');
    expect(empties?.textContent).toContain('action');
  });

  it('exposes the spinner as a status with a hidden label', async () => {
    const fixture = await create();
    const root = fixture.nativeElement as HTMLElement;
    const spinner = root.querySelector('ui-spinner');
    const status = spinner?.querySelector('[role="status"]');

    expect(status).not.toBeNull();
    expect(status?.textContent).toContain('Loading prices');
    expect(status?.querySelector('.visually-hidden')).not.toBeNull();
  });
});
