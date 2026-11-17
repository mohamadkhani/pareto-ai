import { TestBed } from '@angular/core/testing';
import { THEME_STORAGE_KEY, ThemeService } from './theme.service';

interface FakeSystemTheme {
  readonly query: {
    readonly matches: boolean;
    readonly addEventListener: (
      type: string,
      listener: (event: MediaQueryListEvent) => void,
    ) => void;
    readonly removeEventListener: (
      type: string,
      listener: (event: MediaQueryListEvent) => void,
    ) => void;
  };
  change: (dark: boolean) => void;
}

function stubSystemTheme(dark: boolean): FakeSystemTheme {
  const listeners: ((event: MediaQueryListEvent) => void)[] = [];
  let matches = dark;
  const query = {
    get matches(): boolean {
      return matches;
    },
    addEventListener: (_type: string, listener: (event: MediaQueryListEvent) => void): void => {
      listeners.push(listener);
    },
    removeEventListener: (_type: string, listener: (event: MediaQueryListEvent) => void): void => {
      const index = listeners.indexOf(listener);
      if (index >= 0) {
        listeners.splice(index, 1);
      }
    },
  };
  vi.stubGlobal('matchMedia', () => query);
  return {
    query,
    change: (next: boolean): void => {
      matches = next;
      for (const listener of listeners) {
        listener({ matches: next } as MediaQueryListEvent);
      }
    },
  };
}

describe('ThemeService', () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.removeAttribute('data-theme');
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('defaults to the system preference when nothing is stored', () => {
    stubSystemTheme(true);
    const service = TestBed.inject(ThemeService);
    TestBed.tick();

    expect(service.mode()).toBe('system');
    expect(service.resolved()).toBe('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
  });

  it('uses the persisted choice over the system preference', () => {
    window.localStorage.setItem(THEME_STORAGE_KEY, 'light');
    stubSystemTheme(true);
    const service = TestBed.inject(ThemeService);
    TestBed.tick();

    expect(service.mode()).toBe('light');
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');
  });

  it('cycles light → dark → system, updating the attribute and storage', () => {
    stubSystemTheme(false);
    window.localStorage.setItem(THEME_STORAGE_KEY, 'light');
    const service = TestBed.inject(ThemeService);
    TestBed.tick();

    service.cycle();
    TestBed.tick();
    expect(service.mode()).toBe('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe('dark');

    service.cycle();
    TestBed.tick();
    expect(service.mode()).toBe('system');
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe('system');
  });

  it('follows system changes while in system mode only', () => {
    const system = stubSystemTheme(false);
    const service = TestBed.inject(ThemeService);
    TestBed.tick();

    system.change(true);
    TestBed.tick();
    expect(service.resolved()).toBe('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');

    service.setMode('light');
    system.change(false);
    TestBed.tick();
    expect(service.resolved()).toBe('light');
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');
  });

  it('falls back to system when the stored value is not a theme', () => {
    window.localStorage.setItem(THEME_STORAGE_KEY, 'sepia');
    stubSystemTheme(false);
    const service = TestBed.inject(ThemeService);
    TestBed.tick();

    expect(service.mode()).toBe('system');
  });
});
