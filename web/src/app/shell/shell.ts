import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
  inject,
  signal,
  viewChild,
} from '@angular/core';
import { NgTemplateOutlet } from '@angular/common';
import { Router, RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { AuthService } from '../auth/auth.service';
import { DEV_MODE } from '../dev-mode';
import { SetupService } from '../setup/setup.service';
import { UiBadge } from '../ui/badge';
import { UiBanner, type BannerTone } from '../ui/banner';
import { UiButton } from '../ui/button';
import { UiIconButton } from '../ui/icon-button';
import { ThemeService } from '../ui/theme.service';

/** One navigation entry. `adminOnly` links are hidden from the self-service view. */
interface NavLink {
  readonly label: string;
  readonly path: string;
  readonly adminOnly: boolean;
  /** Hidden from production builds; the route itself also refuses to render. */
  readonly devOnly?: boolean;
}

/** A labelled group of navigation links. */
interface NavSection {
  readonly label: string;
  readonly links: readonly NavLink[];
}

/**
 * The full navigation tree. Admins see every section; a `user` account sees
 * only the self-service links (API keys, Limits, Usage), which keeps the
 * non-admin header short and matches the API's access model.
 */
const NAV_SECTIONS: readonly NavSection[] = [
  // D31: one Models page carries the catalog, prices, and quality. Offerings,
  // Benchmarks, and Price sync no longer have pages or nav links.
  {
    label: 'Catalog',
    links: [
      { label: 'Providers', path: '/providers', adminOnly: true },
      { label: 'Models', path: '/models', adminOnly: true },
    ],
  },
  {
    label: 'Routing',
    links: [{ label: 'Combos', path: '/combos', adminOnly: true }],
  },
  {
    label: 'Access',
    links: [
      { label: 'Users', path: '/users', adminOnly: true },
      { label: 'Grants', path: '/grants', adminOnly: true },
      { label: 'API keys', path: '/keys', adminOnly: false },
      { label: 'Limits', path: '/limits', adminOnly: false },
    ],
  },
  {
    label: 'Observability',
    links: [
      { label: 'Usage', path: '/usage', adminOnly: false },
      { label: 'Request logs', path: '/logs', adminOnly: true },
      { label: 'Audit log', path: '/audit', adminOnly: true },
    ],
  },
  {
    label: 'Account',
    links: [{ label: 'Settings', path: '/settings', adminOnly: true }],
  },
  {
    label: 'Development',
    links: [{ label: 'Styleguide', path: '/styleguide', adminOnly: true, devOnly: true }],
  },
];

/** The persistent exposure note shown above every page. */
interface ExposureNote {
  readonly tone: BannerTone;
  readonly text: string;
}

/** Layout shell around every authenticated route: nav, theme, user info, sign out. */
@Component({
  selector: 'app-shell',
  imports: [
    NgTemplateOutlet,
    RouterOutlet,
    RouterLink,
    RouterLinkActive,
    UiBadge,
    UiBanner,
    UiButton,
    UiIconButton,
  ],
  templateUrl: './shell.html',
  styleUrl: './shell.css',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '(document:keydown.escape)': 'onEscape()' },
})
export class Shell {
  protected readonly auth = inject(AuthService);
  protected readonly theme = inject(ThemeService);
  private readonly setup = inject(SetupService);
  private readonly router = inject(Router);
  private readonly devMode = inject(DEV_MODE);

  protected readonly signingOut = signal(false);
  /** Whether the compact (below ~900px) navigation panel is open. */
  protected readonly menuOpen = signal(false);

  /**
   * The persistent security banner: a calm `info` note on a local machine, a
   * red `danger` banner when the gateway is reachable from the network without
   * a password. `null` once protected, or before the status has loaded.
   */
  protected readonly exposure = computed<ExposureNote | null>(() => {
    const status = this.setup.status();
    if (status === null || status.mode !== 'unprotected') {
      return null;
    }
    return {
      tone: status.is_local ? 'info' : 'danger',
      text: status.warning ?? 'This gateway is running without a password.',
    };
  });

  private readonly menuButton = viewChild<ElementRef<HTMLButtonElement>>('menuButton');

  protected readonly sections = computed<readonly NavSection[]>(() => {
    const admin = this.auth.isAdmin();
    return NAV_SECTIONS.map((section) => ({
      label: section.label,
      links: section.links.filter(
        (link) => (admin || !link.adminOnly) && (this.devMode || !link.devOnly),
      ),
    })).filter((section) => section.links.length > 0);
  });

  /** The icon explains the effective theme; the label names the chosen mode. */
  protected readonly themeIcon = computed(() => this.theme.mode());

  constructor() {
    // Drives the exposure banner; a failure simply leaves it hidden.
    void this.setup.load().catch(() => undefined);
  }

  /** Opens Settings from the security banner. */
  protected openSettings(): void {
    void this.router.navigateByUrl('/settings');
  }

  protected toggleMenu(): void {
    this.menuOpen.update((open) => !open);
  }

  protected closeMenu(): void {
    this.menuOpen.set(false);
  }

  /** Escape closes the compact panel and returns focus to its toggle. */
  protected onEscape(): void {
    if (!this.menuOpen()) {
      return;
    }
    this.closeMenu();
    this.menuButton()?.nativeElement.focus();
  }

  protected async signOut(): Promise<void> {
    if (this.signingOut()) {
      return;
    }
    this.signingOut.set(true);
    try {
      await this.auth.logout();
    } catch {
      // Local session state is cleared even when the server call fails.
    }
    await this.router.navigateByUrl('/login');
    this.signingOut.set(false);
  }
}
