import { NgTemplateOutlet } from '@angular/common';
import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { AuthService } from '../auth/auth.service';
import { DEV_MODE } from '../dev-mode';
import { UiBadge } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiCard } from '../ui/card';
import { UiCheckbox } from '../ui/checkbox';
import { UiChip } from '../ui/chip';
import { type ComboboxOption, UiCombobox } from '../ui/combobox';
import { UiDialog } from '../ui/dialog';
import { UiEmpty } from '../ui/empty';
import { UiField } from '../ui/field';
import { UiIconButton } from '../ui/icon-button';
import { UiInput } from '../ui/input';
import { UiLinkButton } from '../ui/link-button';
import { UiMeter } from '../ui/meter';
import { UiSelect } from '../ui/select';
import { UiSparkline } from '../ui/sparkline';
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
 * state, rendered twice (forced light and forced dark). Development only: the
 * shell hides the link and the route renders the usual Forbidden state outside
 * a development build (D28). It is also admin-only.
 */
@Component({
  selector: 'app-styleguide',
  imports: [
    NgTemplateOutlet,
    UiBadge,
    UiBanner,
    UiButton,
    UiCard,
    UiCheckbox,
    UiChip,
    UiCombobox,
    UiDialog,
    UiEmpty,
    UiField,
    UiIconButton,
    UiInput,
    UiLinkButton,
    UiMeter,
    UiSelect,
    UiSparkline,
    UiSpinner,
    UiSwitch,
    UiTable,
    UiTabs,
    UiTextarea,
  ],
  templateUrl: './styleguide.html',
  styleUrl: './styleguide.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Styleguide {
  private readonly auth = inject(AuthService);
  private readonly devMode = inject(DEV_MODE);

  protected readonly forbidden = computed(() => !this.devMode || !this.auth.isAdmin());

  protected readonly colorTokens: readonly TokenSwatch[] = [
    { token: '--bg', label: 'Background' },
    { token: '--surface', label: 'Surface' },
    { token: '--surface-raised', label: 'Surface raised' },
    { token: '--surface-sunken', label: 'Surface sunken' },
    { token: '--surface-hover', label: 'Surface hover' },
    { token: '--border', label: 'Border (interactive)' },
    { token: '--border-subtle', label: 'Border subtle' },
    { token: '--border-strong', label: 'Border strong' },
    { token: '--border-hover', label: 'Border hover' },
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
  protected readonly numberValue = signal('1000');
  protected readonly dateValue = signal('2026-11-18');
  protected readonly timeValue = signal('09:30');
  protected readonly dateTimeValue = signal('2026-11-18T09:30');
  protected readonly textareaValue = signal('One offering per provider/model pair.');
  protected readonly selectValue = signal('openrouter');
  protected readonly compactSelect = signal('warn');
  protected readonly comboboxValue = signal('m1');
  protected readonly compactCombobox = signal('m1');
  protected readonly comboboxOptions: readonly ComboboxOption[] = [
    { value: 'm1', label: 'openai/gpt-5', searchText: 'openrouter' },
    { value: 'm2', label: 'anthropic/claude-opus', searchText: 'openrouter' },
    { value: 'm3', label: 'qwen/qwen3-235b', searchText: 'sailresearch' },
    { value: 'm4', label: 'meta/llama-3.1-405b', searchText: 'sailresearch' },
    { value: 'm5', label: 'mistral/large', searchText: 'openrouter' },
  ];
  protected readonly switchOn = signal(true);
  protected readonly switchDisabled = signal(false);
  protected readonly tab = signal('one');
  protected readonly lightDialog = signal(false);
  protected readonly darkDialog = signal(false);
  protected readonly lightDrawer = signal(false);
  protected readonly darkDrawer = signal(false);
  protected readonly lightWideDrawer = signal(false);
  protected readonly darkWideDrawer = signal(false);

  protected readonly tabs: readonly TabItem[] = [
    { id: 'one', label: 'Overview' },
    { id: 'two', label: 'Pricing' },
    { id: 'three', label: 'Disabled', disabled: true },
  ];
}
