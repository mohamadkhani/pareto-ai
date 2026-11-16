import { type AbstractControl, type ValidationErrors, Validators } from '@angular/forms';

/** Requires an http(s) URL; empty is left to `required`. */
export function httpUrl(control: AbstractControl): ValidationErrors | null {
  const value: unknown = control.value;
  if (typeof value !== 'string' || value.trim() === '') {
    return null;
  }
  return /^https?:\/\/[^\s]+$/.test(value.trim()) ? null : { httpUrl: true };
}

/** Requires a whole number >= 1 when a value is present. */
export function positiveInteger(control: AbstractControl): ValidationErrors | null {
  const value: unknown = control.value;
  if (value === null || value === '') {
    return null;
  }
  const valid = typeof value === 'number' && Number.isInteger(value) && value >= 1;
  return valid ? null : { positiveInteger: true };
}

/** Rejects negative numbers; empty is allowed (unknown). */
export const nonNegative = Validators.min(0);
