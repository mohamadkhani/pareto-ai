import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { AuthService } from '../auth/auth.service';
import { CatalogService } from '../catalog/catalog.service';
import type { Credential, Model, Offering, Provider } from '../catalog/catalog.types';
import { describeError, isForbidden } from '../catalog/errors';
import { GrantsService } from '../grants/grants.service';
import type { GrantView } from '../grants/grants.types';
import { UiBadge } from '../ui/badge';
import { UiBanner } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiEmpty } from '../ui/empty';
import { UiField } from '../ui/field';
import { UiInput } from '../ui/input';
import { UsersService } from '../users/users.service';
import type { UserView } from '../users/users.types';
import { AccessGraph, type AccessGraphLink, type AccessGraphNode } from './access-graph';
import { computeModelAccess } from './access-model';

/**
 * Admin page wiring every account to the models it can actually reach. The
 * edges are the final result (an allowed model/offering/credential triple),
 * never raw grants; hovering an edge names the credentials that open it. A
 * search narrows the models and accounts so a large catalog stays readable.
 */
@Component({
  selector: 'app-access-map',
  imports: [UiBadge, UiBanner, UiButton, UiEmpty, UiField, UiInput, AccessGraph],
  templateUrl: './access-map.html',
  styleUrl: './access-map.sass',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class AccessMapPage {
  private readonly auth = inject(AuthService);
  private readonly usersService = inject(UsersService);
  private readonly catalog = inject(CatalogService);
  private readonly grantsService = inject(GrantsService);

  protected readonly users = signal<UserView[]>([]);
  protected readonly models = signal<Model[]>([]);
  private readonly grants = signal<GrantView[]>([]);
  private readonly offerings = signal<Offering[]>([]);
  private readonly credentials = signal<Credential[]>([]);
  private readonly providers = signal<Provider[]>([]);
  protected readonly loading = signal(false);
  protected readonly loadError = signal<string | null>(null);
  protected readonly forbidden = signal(false);
  protected readonly search = signal('');
  protected readonly selectedLink = signal<AccessGraphLink | null>(null);

  private readonly grantsByUser = computed(() => {
    const map = new Map<string, GrantView[]>();
    for (const grant of this.grants()) {
      if (grant.subject_type !== 'user') {
        continue;
      }
      const list = map.get(grant.subject_id) ?? [];
      list.push(grant);
      map.set(grant.subject_id, list);
    }
    return map;
  });

  protected readonly filteredUsers = computed(() => {
    const term = this.search().trim().toLowerCase();
    const users = this.users();
    if (term === '') {
      return users;
    }
    return users.filter(
      (user) =>
        user.email.toLowerCase().includes(term) || user.display_name.toLowerCase().includes(term),
    );
  });

  protected readonly filteredModels = computed(() => {
    const term = this.search().trim().toLowerCase();
    const models = this.models();
    if (term === '') {
      return models;
    }
    return models.filter(
      (model) =>
        model.canonical_name.toLowerCase().includes(term) ||
        (model.family ?? '').toLowerCase().includes(term),
    );
  });

  /** Each filtered user's reachable models, computed once per render pass. */
  private readonly accessByUser = computed(() => {
    const models = this.filteredModels();
    const offerings = this.offerings();
    const credentials = this.credentials();
    const providers = this.providers();
    return this.filteredUsers().map((user) => ({
      user,
      rows: computeModelAccess({
        role: user.role,
        grants: this.grantsByUser().get(user.id) ?? [],
        models,
        offerings,
        credentials,
        providers,
      }),
    }));
  });

  /** Model ids reachable by at least one shown account (drops dead rows). */
  private readonly reachableModelIds = computed(() => {
    const ids = new Set<string>();
    for (const entry of this.accessByUser()) {
      for (const row of entry.rows) {
        if (row.credentials.length > 0) {
          ids.add(row.model.id);
        }
      }
    }
    return ids;
  });

  protected readonly leftNodes = computed<AccessGraphNode[]>(() =>
    this.filteredUsers().map((user) => ({
      id: user.id,
      label: user.email,
      sublabel: user.role,
    })),
  );

  protected readonly rightNodes = computed<AccessGraphNode[]>(() => {
    const reachable = this.reachableModelIds();
    return this.filteredModels()
      .filter((model) => reachable.has(model.id))
      .map((model) => ({ id: model.id, label: model.canonical_name }));
  });

  /** One connector per (user, reachable model); the reason names credentials. */
  protected readonly links = computed<AccessGraphLink[]>(() => {
    const result: AccessGraphLink[] = [];
    for (const entry of this.accessByUser()) {
      for (const row of entry.rows) {
        if (row.credentials.length === 0) {
          continue;
        }
        const via = row.credentials
          .map((access) => `${access.providerName} · ${access.credential.label}`)
          .join(', ');
        result.push({
          id: `${entry.user.id}:${row.model.id}`,
          source: entry.user.id,
          target: row.model.id,
          verdict: 'allow',
          reason: `${entry.user.email} → ${row.model.canonical_name} via ${via}`,
        });
      }
    }
    return result;
  });

  protected readonly allowedCount = computed(() => this.links().length);

  constructor() {
    if (this.auth.isAdmin()) {
      void this.reload();
    } else {
      this.forbidden.set(true);
    }
  }

  protected async reload(): Promise<void> {
    this.loading.set(true);
    this.loadError.set(null);
    try {
      const [users, models, grants, offerings, providers] = await Promise.all([
        this.usersService.list(),
        this.catalog.listModels(),
        this.grantsService.list(),
        this.catalog.listOfferings().catch(() => [] as Offering[]),
        this.catalog.listProviders().catch(() => [] as Provider[]),
      ]);
      const credentials = (
        await Promise.all(
          providers.map((provider) =>
            this.catalog.listCredentials(provider.id).catch(() => [] as Credential[]),
          ),
        )
      ).flat();
      this.users.set(users);
      this.models.set(models);
      this.grants.set(grants);
      this.offerings.set(offerings);
      this.providers.set(providers);
      this.credentials.set(credentials);
    } catch (err) {
      this.loadError.set(
        isForbidden(err) ? 'Forbidden' : describeError(err, 'Loading access failed.'),
      );
    } finally {
      this.loading.set(false);
    }
  }

  protected onSearch(value: string | number | null): void {
    this.search.set(typeof value === 'string' ? value : '');
    this.selectedLink.set(null);
  }

  protected selectLink(link: AccessGraphLink): void {
    this.selectedLink.set(link);
  }
}
