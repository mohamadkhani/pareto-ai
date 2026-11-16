import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { AuthService } from '../auth/auth.service';
import { CatalogService } from '../catalog/catalog.service';
import type { Model } from '../catalog/catalog.types';
import { DeleteConfirm } from '../catalog/delete-confirm';
import { describeError, isForbidden } from '../catalog/errors';
import { formatTime } from '../catalog/format';
import { RoutingService } from './routing.service';
import type {
  BenchmarkScore,
  ImportOutcome,
  LeaderboardDocument,
  QualityOverride,
} from './routing.types';

/**
 * Benchmarks: import an LMArena-style leaderboard, browse the imported
 * scores, and manage the per-model quality override (decision D25: scores
 * live on the imported benchmark's Elo-like scale, and the override wins over
 * every synced score but is never overwritten by an import).
 */
@Component({
  selector: 'app-benchmarks',
  imports: [DeleteConfirm],
  templateUrl: './benchmarks.html',
  styleUrl: './benchmarks.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Benchmarks {
  private readonly auth = inject(AuthService);
  private readonly catalog = inject(CatalogService);
  private readonly routing = inject(RoutingService);

  protected readonly models = signal<Model[]>([]);
  protected readonly scores = signal<BenchmarkScore[]>([]);
  protected readonly overrides = signal<QualityOverride[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly scoresError = signal<string | null>(null);
  protected readonly overridesError = signal<string | null>(null);
  protected readonly forbidden = signal(false);

  protected readonly documentText = signal('');
  /** Shown in the empty textarea; kept in code so the template stays ICU-free. */
  protected readonly documentPlaceholder =
    '{"source": "lmarena", "entries": [{"model": "openai/gpt-5", "rank": 1, "score": 1433, "ci": 12, "category": "text"}]}';
  protected readonly importing = signal(false);
  protected readonly importError = signal<string | null>(null);
  protected readonly importOutcome = signal<ImportOutcome | null>(null);

  /** Score being typed per model row, keyed by model id. */
  private readonly drafts = signal<Map<string, number>>(new Map());
  /** Per-row save/remove failures (e.g. the 400 scale-range error). */
  private readonly rowErrors = signal<Map<string, string>>(new Map());
  protected readonly savingModelId = signal<string | null>(null);
  protected readonly removingModelId = signal<string | null>(null);

  protected readonly formatTime = formatTime;

  private readonly overrideByModel = computed(
    () => new Map(this.overrides().map((override) => [override.model_id, override])),
  );
  private readonly modelById = computed(
    () => new Map(this.models().map((model) => [model.id, model])),
  );

  constructor() {
    // Non-admins never call the API: it would answer 403 and the read is audited.
    if (this.auth.isAdmin()) {
      void this.reload();
    } else {
      this.forbidden.set(true);
    }
  }

  /** Reloads models plus the two score tables; each table keeps its own error. */
  protected async reload(): Promise<void> {
    this.loading.set(true);
    this.loadError.set(null);
    try {
      this.models.set(await this.catalog.listModels());
    } catch (err) {
      if (isForbidden(err)) {
        this.forbidden.set(true);
        return;
      }
      this.loadError.set(describeError(err, 'Loading models failed.'));
    } finally {
      this.loading.set(false);
    }
    await Promise.all([this.reloadScores(), this.reloadOverrides()]);
  }

  protected async reloadScores(): Promise<void> {
    this.scoresError.set(null);
    try {
      this.scores.set(await this.routing.listBenchmarkScores());
    } catch (err) {
      this.scoresError.set(describeError(err, 'Loading the imported scores failed.'));
    }
  }

  protected async reloadOverrides(): Promise<void> {
    this.overridesError.set(null);
    try {
      this.overrides.set(await this.routing.listQualityOverrides());
    } catch (err) {
      this.overridesError.set(describeError(err, 'Loading the quality overrides failed.'));
    }
  }

  // --- import --------------------------------------------------------------

  protected onDocumentInput(event: Event): void {
    const textarea = event.target;
    if (textarea instanceof HTMLTextAreaElement) {
      this.documentText.set(textarea.value);
    }
  }

  /** Loads the picked JSON file into the document textarea. */
  protected async onFileChange(event: Event): Promise<void> {
    const input = event.target;
    if (!(input instanceof HTMLInputElement) || input.files === null || input.files.length === 0) {
      return;
    }
    const file = input.files[0];
    try {
      this.documentText.set(await file.text());
      this.importError.set(null);
    } catch {
      this.importError.set('Could not read that file.');
    }
  }

  protected async importDocument(): Promise<void> {
    this.importError.set(null);
    this.importOutcome.set(null);
    let document: LeaderboardDocument;
    try {
      // Only valid JSON is the UI's job; the importer validates the shape and
      // answers 400 naming the offending field.
      document = JSON.parse(this.documentText()) as LeaderboardDocument;
    } catch {
      this.importError.set('That is not valid JSON.');
      return;
    }
    this.importing.set(true);
    try {
      const outcome = await this.routing.importBenchmarks(document);
      this.importOutcome.set(outcome);
      await this.reloadScores();
    } catch (err) {
      this.importError.set(describeError(err, 'The import failed.'));
    } finally {
      this.importing.set(false);
    }
  }

  // --- quality overrides ---------------------------------------------------

  protected overrideFor(modelId: string): QualityOverride | null {
    return this.overrideByModel().get(modelId) ?? null;
  }

  protected draftFor(modelId: string): number | string {
    return this.drafts().get(modelId) ?? this.overrideFor(modelId)?.score ?? '';
  }

  protected rowError(modelId: string): string | null {
    return this.rowErrors().get(modelId) ?? null;
  }

  protected setDraft(modelId: string, event: Event): void {
    const input = event.target;
    if (!(input instanceof HTMLInputElement)) {
      return;
    }
    const drafts = new Map(this.drafts());
    drafts.set(modelId, Number(input.value));
    this.drafts.set(drafts);
  }

  protected async saveOverride(model: Model): Promise<void> {
    const score = this.drafts().get(model.id) ?? this.overrideFor(model.id)?.score;
    if (score === undefined || !Number.isFinite(score)) {
      this.setRowError(model.id, 'Enter a numeric score first.');
      return;
    }
    this.savingModelId.set(model.id);
    this.setRowError(model.id, null);
    try {
      const saved = await this.routing.setQualityOverride(model.id, score);
      this.overrides.set([
        ...this.overrides().filter((override) => override.model_id !== model.id),
        saved,
      ]);
    } catch (err) {
      // The scale check rejects a likely scale error with 400 and names the
      // accepted range (decision D25) — show that message as-is.
      this.setRowError(model.id, describeError(err, 'Saving the override failed.'));
    } finally {
      this.savingModelId.set(null);
    }
  }

  protected async removeOverride(model: Model): Promise<void> {
    this.removingModelId.set(model.id);
    this.setRowError(model.id, null);
    try {
      await this.routing.clearQualityOverride(model.id);
      this.overrides.set(this.overrides().filter((override) => override.model_id !== model.id));
    } catch (err) {
      this.setRowError(model.id, describeError(err, 'Removing the override failed.'));
    } finally {
      this.removingModelId.set(null);
    }
  }

  /** The catalog name of a model id, falling back to the id. */
  protected modelName(modelId: string): string {
    return this.modelById().get(modelId)?.canonical_name ?? modelId;
  }

  /** The newest imported score of one model, or '—' when it has none. */
  protected latestScoreOf(modelId: string): string {
    const rows = this.scores().filter((score) => score.model_id === modelId);
    if (rows.length === 0) {
      return '—';
    }
    const latest = rows.reduce((a, b) => (a.fetched_at >= b.fetched_at ? a : b));
    return this.formatScoreValue(latest.score);
  }

  protected formatScoreValue(value: number | null): string {
    return value === null ? '—' : value.toFixed(1);
  }

  protected formatRank(value: number | null): string {
    return value === null ? '—' : `#${value}`;
  }

  private setRowError(modelId: string, message: string | null): void {
    const errors = new Map(this.rowErrors());
    if (message === null) {
      errors.delete(modelId);
    } else {
      errors.set(modelId, message);
    }
    this.rowErrors.set(errors);
  }
}
