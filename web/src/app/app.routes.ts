import { Routes } from '@angular/router';
import { Audit } from './audit/audit';
import { authGuard, loginGuard } from './auth/auth.guard';
import { Login } from './login/login';
import { Models } from './catalog/models';
import { Offerings } from './catalog/offerings';
import { Providers } from './catalog/providers';
import { Sync } from './catalog/sync';
import { Shell } from './shell/shell';

export const routes: Routes = [
  { path: 'login', canActivate: [loginGuard], component: Login },
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
      { path: '', pathMatch: 'full', redirectTo: 'audit' },
    ],
  },
  { path: '**', redirectTo: '' },
];
