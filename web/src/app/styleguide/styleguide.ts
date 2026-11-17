import { NgTemplateOutlet } from '@angular/common';
import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { AuthService } from '../auth/auth.service';
import { UiBadge } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiCard } from '../ui/card';
import { UiDialog } from '../ui/dialog';
import { UiEmpty } from '../ui/empty';
import { UiField } from '../ui/field';
import { UiIconButton } from '../ui/icon-button';
import { UiInput } from '../ui/input';
import { UiSelect } from '../ui/select';
import { UiSpinner } from '../ui/spinner';
import { UiSwitch } from '../ui/switch';
import { UiTable } from '../ui/table';
import type { TabItem } from '../ui/tabs';
import { UiTabs } from '../ui/tabs';
import { UiTextarea } from '../ui/textarea';

interface TokenSwatch {
  readonly token: string;
  readonly label: string;
}

/**
 * Review surface for the design system: tokens plus every primitive in every
 * state, rendered twice (forced light and forced dark). Admin-only: the route
 * carries `data.adminOnly`, the shell hides the link, and a `user` account
 * gets the same Forbidden state as the other admin surfaces.
 */
@Component({
  selector: 'app-styleguide',
  imports: [
    NgTemplateOutlet,
    UiBadge,
    UiBanner,
    UiButton,
    UiCard,
    UiDialog,
    UiEmpty,
    UiField,
    UiIconButton,
    UiInput,
    UiSelect,
    UiSpinner,
    UiSwitch,
    UiTable,
    UiTabs,
    UiTextarea,
  ],
  templateUrl: './styleguide.html',
  styleUrl: './styleguide.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Styleguide {
  private readonly auth = inject(AuthService);

  protected readonly forbidden = computed(() => !this.auth.isAdmin());

  protected readonly colorTokens: readonly TokenSwatch[] = [
    { token: '--bg', label: 'Background' },
    { token: '--surface', label: 'Surface' },
    { token: '--surface-raised', label: 'Surface raised' },
    { token: '--surface-sunken', label: 'Surface sunken' },
    { token: '--surface-hover', label: 'Surface hover' },
    { token: '--border', label: 'Border (interactive)' },
    { token: '--border-subtle', label: 'Border subtle' },
    { token: '--border-strong', label: 'Border strong' },
    { token: '--text', label: 'Text' },
    { token: '--text-muted', label: 'Text muted' },
    { token: '--accent', label: 'Accent' },
    { token: '--accent-hover', label: 'Accent hover' },
    { token: '--accent-contrast', label: 'Accent contrast' },
    { token: '--accent-soft', label: 'Accent soft' },
    { token: '--success', label: 'Success' },
    { token: '--success-soft', label: 'Success soft' },
    { token: '--warning', label: 'Warning' },
    { token: '--warning-soft', label: 'Warning soft' },
    { token: '--danger', label: 'Danger' },
    { token: '--danger-soft', label: 'Danger soft' },
    { token: '--info', label: 'Info' },
    { token: '--info-soft', label: 'Info soft' },
    { token: '--focus-ring', label: 'Focus ring' },
  ];

  protected readonly spacingTokens: readonly string[] = [
    '--space-1',
    '--space-2',
    '--space-3',
    '--space-4',
    '--space-6',
    '--space-8',
    '--space-12',
    '--space-16',
  ];

  protected readonly typeScale: readonly TokenSwatch[] = [
    { token: '--text-display', label: 'Display' },
    { token: '--text-h1', label: 'Heading 1' },
    { token: '--text-h2', label: 'Heading 2' },
    { token: '--text-h3', label: 'Heading 3' },
    { token: '--text-body', label: 'Body' },
    { token: '--text-small', label: 'Small' },
    { token: '--text-mono', label: 'Mono' },
  ];

  protected readonly inputValue = signal('pareto-ai');
  protected readonly textareaValue = signal('One offering per provider/model pair.');
  protected readonly selectValue = signal('openrouter');
  protected readonly switchOn = signal(true);
  protected readonly switchDisabled = signal(false);
  protected readonly tab = signal('one');
  protected readonly lightDialog = signal(false);
  protected readonly darkDialog = signal(false);

  protected readonly tabs: readonly TabItem[] = [
    { id: 'one', label: 'Overview' },
    { id: 'two', label: 'Pricing' },
    { id: 'three', label: 'Disabled', disabled: true },
  ];
}
