import { inject } from '@angular/core';
import { type CanActivateFn, Router } from '@angular/router';
import { AuthService } from './auth.service';

/** The forced-rotation screen; the only screen a flagged account may open. */
const ROTATION_URL = '/change-password';

function isRotationUrl(url: string): boolean {
  return (
    url === ROTATION_URL || url.startsWith(`${ROTATION_URL}?`) || url.startsWith(`${ROTATION_URL}#`)
  );
}

/**
 * Keeps unauthenticated users out of the app shell, redirecting to `/login`
 * with the requested URL preserved as `returnUrl`. An account flagged
 * `must_change_password` is held on `/change-password` until it rotates, and
 * `/change-password` itself is reachable only while the flag is set.
 */
export const authGuard: CanActivateFn = async (_route, state) => {
  const auth = inject(AuthService);
  const router = inject(Router);
  if (!(await auth.loadMe())) {
    return router.createUrlTree(['/login'], { queryParams: { returnUrl: state.url } });
  }
  const mustChange = auth.user()?.must_change_password === true;
  if (mustChange && !isRotationUrl(state.url)) {
    return router.createUrlTree([ROTATION_URL]);
  }
  if (!mustChange && isRotationUrl(state.url)) {
    return router.createUrlTree(['/']);
  }
  return true;
};

/** Keeps already signed-in users off the login page. */
export const loginGuard: CanActivateFn = async () => {
  const auth = inject(AuthService);
  const router = inject(Router);
  return (await auth.loadMe()) ? router.createUrlTree(['/']) : true;
};
