import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  model,
  output,
  signal,
} from '@angular/core';
import { describeError } from './errors';
import { MixService } from './mix.service';
import {
  MIX_COMPONENT_LABELS,
  type MixComponent,
  type MixProfile,
  type MixProfileInput,
  type MixProfileKind,
  type PricingSettings,
  profileSummary,
} from './mix.types';
import { UiBadge, type BadgeTone } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiDialog } from '../ui/dialog';
import { UiField } from '../ui/field';
import { UiInput } from '../ui/input';
import { UiSelect } from '../ui/select';
import { UiTable } from '../ui/table';

/** `kind` values an operator may choose; `preset` is reserved for built-ins. */
type EditableKind = 'custom' | 'measured';

/** The creation/edit form; shares stay strings until saved. */
interface ProfileDraft {
  name: string;
  kind: EditableKind;
  input_share: string;
  output_share: string;
  cache_read_share: string;
  cache_write_share: string;
  reasoning_share: string;
}

type EditorState =
  | { mode: 'create' }
  | { mode: 'edit'; profile: MixProfile }
  | { mode: 'clone'; profile: MixProfile };

const SHARE_FIELDS: ReadonlyArray<{
  key: Exclude<keyof ProfileDraft, 'name' | 'kind'>;
  component: MixComponent;
}> = [
  { key: 'input_share', component: 'input' },
  { key: 'output_share', component: 'output' },
  { key: 'cache_read_share', component: 'cache_read' },
  { key: 'cache_write_share', component: 'cache_write' },
  { key: 'reasoning_share', component: 'reasoning' },
];

function emptyDraft(): ProfileDraft {
  return {
    name: '',
    kind: 'custom',
    input_share: '8.1',
    output_share: '0.4',
    cache_read_share: '91.5',
    cache_write_share: '0',
    reasoning_share: '0',
  };
}

/**
 * The token-mix profile manager (D34): the default-profile setting and the
 * list of built-in and custom profiles. Built-ins are cloned, never edited;
 * custom profiles are created, edited, and deleted here.
 *
 * `open` is two-way; `changed` fires after any mutation so the Models page
 * can reload the overview at the new basis. The default setting goes through
 * `PUT /api/settings/pricing`; when `GATEWAY_TOKEN_MIX_PROFILE` is set the
 * stored default is ignored, so the picker is disabled and the reason shown.
 */
@Component({
  selector: 'app-mix-profiles',
  imports: [UiBadge, UiBanner, UiButton, UiDialog, UiField, UiInput, UiSelect, UiTable],
  templateUrl: './mix-profiles.html',
  styleUrl: './mix-profiles.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class MixProfiles {
  private readonly mix = inject(MixService);

  readonly open = model(false);
  readonly changed = output<void>();

  protected readonly profiles = signal<MixProfile[]>([]);
  protected readonly pricing = signal<PricingSettings | null>(null);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);

  protected readonly savingDefault = signal(false);
  protected readonly defaultError = signal<string | null>(null);

  protected readonly editor = signal<EditorState | null>(null);
  protected readonly draft = signal<ProfileDraft>(emptyDraft());
  protected readonly attempted = signal(false);
  protected readonly saving = signal(false);
  protected readonly formError = signal<string | null>(null);

  protected readonly deleteTarget = signal<MixProfile | null>(null);
  protected readonly deleting = signal(false);
  protected readonly deleteError = signal<string | null>(null);

  protected readonly shareFields = SHARE_FIELDS;
  protected readonly componentLabels = MIX_COMPONENT_LABELS;
  protected readonly formatSummary = profileSummary;

  protected readonly envOverride = computed(() => this.pricing()?.env_override ?? null);
  protected readonly effectiveProfile = computed(() => this.pricing()?.effective_profile ?? null);
  protected readonly defaultSelector = computed(() => this.pricing()?.default_mix_profile ?? '');
  protected readonly editorTitle = computed(() => {
    const state = this.editor();
    if (state?.mode === 'edit') {
      return `Edit ${state.profile.name}`;
    }
    return state?.mode === 'clone' ? 'Clone profile' : 'New profile';
  });
  protected readonly deleteTitle = computed(() => {
    const target = this.deleteTarget();
    return target === null ? 'Delete profile' : `Delete profile: ${target.name}`;
  });
  protected readonly sharesError = computed(() => {
    if (!this.attempted()) {
      return '';
    }
    const values = SHARE_FIELDS.map((field) => Number(this.draft()[field.key]));
    if (values.some((value) => !Number.isFinite(value) || value < 0)) {
      return 'Every share must be a number of zero or more.';
    }
    return values.reduce((total, value) => total + value, 0) > 0
      ? ''
      : 'The shares must sum to more than zero.';
  });
  protected readonly nameError = computed(() => {
    if (!this.attempted() || this.draft().name.trim() !== '') {
      return '';
    }
    return 'This field is required.';
  });

  constructor() {
    effect(() => {
      if (this.open()) {
        void this.load();
      }
    });
  }

  protected async load(): Promise<void> {
    this.loading.set(true);
    this.loadError.set(null);
    try {
      const [profiles, pricing] = await Promise.all([
        this.mix.listProfiles(),
        this.mix.pricingSettings(),
      ]);
      this.profiles.set(profiles);
      this.pricing.set(pricing);
    } catch (err) {
      this.loadError.set(describeError(err, 'Loading mix profiles failed.'));
    } finally {
      this.loading.set(false);
    }
  }

  protected close(): void {
    this.open.set(false);
    this.editor.set(null);
    this.deleteTarget.set(null);
    this.formError.set(null);
    this.defaultError.set(null);
  }

  // --- default setting -----------------------------------------------------

  /** Sets (or clears) the default; disabled while the environment overrides it. */
  protected async onDefaultChange(value: string): Promise<void> {
    if (this.envOverride() !== null || this.savingDefault()) {
      return;
    }
    const previous = this.pricing();
    this.savingDefault.set(true);
    this.defaultError.set(null);
    try {
      this.pricing.set(await this.mix.setDefaultProfile(value === '' ? null : value));
      this.changed.emit();
    } catch (err) {
      // Put the select back on the stored value; the server's message names
      // the rejected selector.
      this.pricing.set(previous);
      this.defaultError.set(describeError(err, 'Setting the default profile failed.'));
    } finally {
      this.savingDefault.set(false);
    }
  }

  // --- list ----------------------------------------------------------------

  protected isDefault(profile: MixProfile): boolean {
    return this.pricing()?.default_mix_profile === profile.id;
  }

  protected isEffective(profile: MixProfile): boolean {
    return this.effectiveProfile()?.profile_id === profile.id;
  }

  protected kindTone(profile: MixProfile): BadgeTone {
    if (profile.is_builtin) {
      return 'info';
    }
    return profile.kind === 'measured' ? 'success' : 'neutral';
  }

  protected kindLabel(profile: MixProfile): string {
    return profile.is_builtin ? 'built-in' : profile.kind;
  }

  // --- editor --------------------------------------------------------------

  protected openCreate(): void {
    this.draft.set(emptyDraft());
    this.attempted.set(false);
    this.formError.set(null);
    this.editor.set({ mode: 'create' });
  }

  protected openEdit(profile: MixProfile): void {
    this.draft.set(draftOf(profile, profile.name));
    this.attempted.set(false);
    this.formError.set(null);
    this.editor.set({ mode: 'edit', profile });
  }

  /** Built-ins are immutable: cloning starts a new custom profile. */
  protected openClone(profile: MixProfile): void {
    this.draft.set(draftOf(profile, `${profile.name}-copy`));
    this.attempted.set(false);
    this.formError.set(null);
    this.editor.set({ mode: 'clone', profile });
  }

  protected closeEditor(): void {
    this.editor.set(null);
    this.formError.set(null);
  }

  protected setDraftName(value: string): void {
    this.draft.update((draft) => ({ ...draft, name: value }));
  }

  protected setDraftKind(value: string): void {
    const kind: EditableKind = value === 'measured' ? 'measured' : 'custom';
    this.draft.update((draft) => ({ ...draft, kind }));
  }

  protected setDraftShare(field: keyof ProfileDraft, value: string): void {
    this.draft.update((draft) => ({ ...draft, [field]: value }));
  }

  protected async saveProfile(): Promise<void> {
    this.attempted.set(true);
    this.formError.set(null);
    const state = this.editor();
    if (state === null || this.nameError() !== '' || this.sharesError() !== '') {
      return;
    }
    this.saving.set(true);
    try {
      const input = this.profileInput();
      if (state.mode === 'edit') {
        await this.mix.updateProfile(state.profile.id, input);
      } else {
        await this.mix.createProfile(input);
      }
      this.editor.set(null);
      await this.load();
      this.changed.emit();
    } catch (err) {
      // The server's 400 names the rejected field; show it verbatim.
      this.formError.set(describeError(err, 'Saving the profile failed.'));
    } finally {
      this.saving.set(false);
    }
  }

  // --- delete --------------------------------------------------------------

  protected askDelete(profile: MixProfile): void {
    if (profile.is_builtin) {
      return;
    }
    this.deleteTarget.set(profile);
    this.deleteError.set(null);
  }

  protected cancelDelete(): void {
    this.deleteTarget.set(null);
    this.deleteError.set(null);
  }

  protected async confirmDelete(): Promise<void> {
    const target = this.deleteTarget();
    if (target === null) {
      return;
    }
    this.deleting.set(true);
    this.deleteError.set(null);
    try {
      await this.mix.deleteProfile(target.id);
      this.deleteTarget.set(null);
      await this.load();
      this.changed.emit();
    } catch (err) {
      this.deleteError.set(describeError(err, 'Deleting the profile failed.'));
    } finally {
      this.deleting.set(false);
    }
  }

  private profileInput(): MixProfileInput {
    const draft = this.draft();
    const kind: MixProfileKind = draft.kind;
    return {
      name: draft.name.trim(),
      kind,
      input_share: Number(draft.input_share),
      output_share: Number(draft.output_share),
      cache_read_share: Number(draft.cache_read_share),
      cache_write_share: Number(draft.cache_write_share),
      reasoning_share: Number(draft.reasoning_share),
    };
  }
}

function draftOf(profile: MixProfile, name: string): ProfileDraft {
  return {
    name,
    kind: profile.kind === 'measured' ? 'measured' : 'custom',
    input_share: String(profile.input_share),
    output_share: String(profile.output_share),
    cache_read_share: String(profile.cache_read_share),
    cache_write_share: String(profile.cache_write_share),
    reasoning_share: String(profile.reasoning_share),
  };
}
