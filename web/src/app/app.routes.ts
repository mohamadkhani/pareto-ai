import { Routes } from '@angular/router';
import { Audit } from './audit/audit';
import { authGuard, loginGuard } from './auth/auth.guard';
import { Login } from './login/login';
import { Shell } from './shell/shell';

export const routes: Routes = [
  { path: 'login', canActivate: [loginGuard], component: Login },
  {
    path: '',
    canActivate: [authGuard],
    component: Shell,
    children: [
      { path: 'audit', component: Audit },
      { path: '', pathMatch: 'full', redirectTo: 'audit' },
    ],
  },
  { path: '**', redirectTo: '' },
];
