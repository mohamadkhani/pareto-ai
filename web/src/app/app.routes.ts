import { inject } from '@angular/core';
import { Routes } from '@angular/router';
import { Audit } from './audit/audit';
import { AuthService } from './auth/auth.service';
import { authGuard, loginGuard, setupGuard, setupPageGuard } from './auth/auth.guard';
import { ChangePassword } from './change-password/change-password';
import { Combos } from './routing/combos';
import { Grants } from './grants/grants';
import { Keys } from './keys/keys';
import { Limits } from './money/limits';
import { Login } from './login/login';
import { Settings } from './settings/settings';
import { Setup } from './setup/setup';
import { Logs } from './usage/logs';
import { Models } from './catalog/models';
import { Providers } from './catalog/providers';
import { Usage } from './usage/usage';
import { Users } from './users/users';
import { Shell } from './shell/shell';

/**
 * Where `/` lands for a signed-in account. Admins start on the dashboard;
 * a `user` account starts on usage, a page it is allowed to read. A plain
 * redirect to an admin page would greet self-service accounts with 403s.
 */
export function landingRedirect(): string {
  return inject(AuthService).isAdmin() ? 'dashboard' : 'usage';
}

export const routes: Routes = [
  { path: 'login', canActivate: [loginGuard], component: Login },
  // The first-run wizard is the only unauthenticated page besides login; it
  // redirects away once an account exists (protected) or setup was skipped.
  { path: 'setup', canActivate: [setupPageGuard], component: Setup },
  // The forced-rotation screen (review finding 7); `authGuard` holds flagged
  // accounts here and keeps everyone else out.
  { path: 'change-password', canActivate: [authGuard], component: ChangePassword },
  {
    path: '',
    // `setupGuard` sends a first visit to the wizard before `authGuard` can
    // bounce an account-less gateway to /login.
    canActivate: [setupGuard, authGuard],
    component: Shell,
    children: [
      // Admin landing page: cards composed client-side from existing endpoints.
      {
        path: 'dashboard',
        loadComponent: () => import('./dashboard/dashboard').then((m) => m.Dashboard),
      },
      // Admin-only pages; the server enforces admin on every catalog call.
      { path: 'providers', component: Providers },
      // D31: models, prices, and quality live on one page; `/offerings`,
      // `/benchmarks`, and `/sync` are retired (their actions moved to the
      // Models toolbar and detail dialog).
      { path: 'models', component: Models },
      { path: 'combos', component: Combos },
      { path: 'audit', component: Audit },
      { path: 'users', component: Users },
      { path: 'grants', component: Grants },
      { path: 'logs', component: Logs },
      // Review surface for the design system; development builds only. The
      // shell hides the nav link and the component renders Forbidden in a
      // production build (D28).
      // Lazy-loaded: it renders every primitive twice and is not needed on
      // first paint (it kept the initial bundle over the 600 kB budget).
      {
        path: 'styleguide',
        canActivate: [authGuard],
        data: { adminOnly: true },
        loadComponent: () => import('./styleguide/styleguide').then((m) => m.Styleguide),
      },
      // Self-service pages; an admin opening another account's keys uses the
      // `/users/:id/keys` form.
      { path: 'keys', component: Keys },
      { path: 'users/:id/keys', component: Keys },
      { path: 'limits', component: Limits },
      { path: 'usage', component: Usage },
      // Admin: own password, first password while unprotected, exposure facts.
      { path: 'settings', component: Settings },
      { path: '', pathMatch: 'full', redirectTo: landingRedirect },
    ],
  },
  { path: '**', redirectTo: '' },
];
