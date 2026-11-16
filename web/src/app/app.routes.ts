import { Routes } from '@angular/router';
import { Audit } from './audit/audit';
import { authGuard, loginGuard } from './auth/auth.guard';
import { ChangePassword } from './change-password/change-password';
import { Grants } from './grants/grants';
import { Keys } from './keys/keys';
import { Limits } from './money/limits';
import { Login } from './login/login';
import { Logs } from './usage/logs';
import { Models } from './catalog/models';
import { Offerings } from './catalog/offerings';
import { Providers } from './catalog/providers';
import { Sync } from './catalog/sync';
import { Usage } from './usage/usage';
import { Users } from './users/users';
import { Shell } from './shell/shell';

export const routes: Routes = [
  { path: 'login', canActivate: [loginGuard], component: Login },
  // The forced-rotation screen (review finding 7); `authGuard` holds flagged
  // accounts here and keeps everyone else out.
  { path: 'change-password', canActivate: [authGuard], component: ChangePassword },
  {
    path: '',
    canActivate: [authGuard],
    component: Shell,
    children: [
      // Admin-only pages; the server enforces admin on every catalog call.
      { path: 'providers', component: Providers },
      { path: 'models', component: Models },
      { path: 'offerings', component: Offerings },
      { path: 'sync', component: Sync },
      { path: 'audit', component: Audit },
      { path: 'users', component: Users },
      { path: 'grants', component: Grants },
      { path: 'logs', component: Logs },
      // Self-service pages; an admin opening another account's keys uses the
      // `/users/:id/keys` form.
      { path: 'keys', component: Keys },
      { path: 'users/:id/keys', component: Keys },
      { path: 'limits', component: Limits },
      { path: 'usage', component: Usage },
      { path: '', pathMatch: 'full', redirectTo: 'audit' },
    ],
  },
  { path: '**', redirectTo: '' },
];
