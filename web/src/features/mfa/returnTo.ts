import { safeReturnTo as validateReturnTo } from '../../../../auth/src/return-to';

export function safeReturnTo(
  value: unknown,
  origin: string = globalThis.location?.origin ?? 'http://localhost',
): string | null {
  return validateReturnTo(value, origin);
}

/** Leave the SPA for the auth service's page (the OAuth authorize or consent route). */
export function resumeAt(path: string) {
  globalThis.location.assign(path);
}
