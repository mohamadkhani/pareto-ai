import { InjectionToken, isDevMode } from '@angular/core';

/**
 * True in development builds and tests, false in a production build. The
 * design-system review surface (`/styleguide`) and its nav link exist only for
 * developers (D28); production renders the usual Forbidden state instead.
 */
export const DEV_MODE = new InjectionToken<boolean>('DEV_MODE', {
  providedIn: 'root',
  factory: () => isDevMode(),
});
