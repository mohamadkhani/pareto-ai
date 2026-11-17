import { DOCUMENT, isPlatformBrowser } from '@angular/common';
import {
  DestroyRef,
  Injectable,
  PLATFORM_ID,
  computed,
  effect,
  inject,
  signal,
} from '@angular/core';

export type ThemeMode = 'light' | 'dark' | 'system';
export type ResolvedTheme = 'light' | 'dark';

/** localStorage key shared with the pre-boot script in `index.html`. */
export const THEME_STORAGE_KEY = 'gateway.theme';

function isThemeMode(value: string | null): value is ThemeMode {
  return value === 'light' || value === 'dark' || value === 'system';
}

/**
 * Holds the theme choice (`light`, `dark`, or `system`), resolves it against
 * `prefers-color-scheme`, applies `data-theme` to `<html>`, and persists
 * explicit choices. Falls back to `system` when storage is unavailable.
 */
@Injectable({ providedIn: 'root' })
export class ThemeService {
  private readonly document = inject(DOCUMENT);
  private readonly platformId = inject(PLATFORM_ID);
  private readonly browser = isPlatformBrowser(this.platformId);

  private readonly system = signal<ResolvedTheme>(this.readSystem());
  private readonly stored = this.readStored();

  readonly mode = signal<ThemeMode>(this.stored ?? 'system');
  readonly resolved = computed<ResolvedTheme>(() => {
    const mode = this.mode();
    return mode === 'system' ? this.system() : mode;
  });
  /** Label for the toggle: the chosen mode plus the effective theme. */
  readonly label = computed(() => {
    const mode = this.mode();
    return mode === 'system' ? `Theme: system (${this.system()})` : `Theme: ${mode}`;
  });

  constructor() {
    if (this.browser && typeof window.matchMedia === 'function') {
      const query = window.matchMedia('(prefers-color-scheme: dark)');
      const onChange = (event: MediaQueryListEvent): void => {
        this.system.set(event.matches ? 'dark' : 'light');
      };
      query.addEventListener('change', onChange);
      inject(DestroyRef).onDestroy(() => query.removeEventListener('change', onChange));
    }

    effect(() => {
      const theme = this.resolved();
      if (this.browser) {
        this.document.documentElement.setAttribute('data-theme', theme);
      }
    });
  }

  /** Applies a mode explicitly and persists it for the next visit. */
  setMode(mode: ThemeMode): void {
    this.mode.set(mode);
    this.persist(mode);
  }

  /** Cycles light → dark → system, used by the shell toggle. */
  cycle(): void {
    const order: readonly ThemeMode[] = ['light', 'dark', 'system'];
    const next = order[(order.indexOf(this.mode()) + 1) % order.length] ?? 'system';
    this.setMode(next);
  }

  private readStored(): ThemeMode | null {
    if (!this.browser) {
      return null;
    }
    try {
      const value = window.localStorage.getItem(THEME_STORAGE_KEY);
      return isThemeMode(value) ? value : null;
    } catch {
      return null;
    }
  }

  private persist(mode: ThemeMode): void {
    if (!this.browser) {
      return;
    }
    try {
      window.localStorage.setItem(THEME_STORAGE_KEY, mode);
    } catch {
      // Storage can be unavailable (private mode); the in-memory choice stands.
    }
  }

  private readSystem(): ResolvedTheme {
    if (
      this.browser &&
      typeof window.matchMedia === 'function' &&
      window.matchMedia('(prefers-color-scheme: dark)').matches
    ) {
      return 'dark';
    }
    return 'light';
  }
}
