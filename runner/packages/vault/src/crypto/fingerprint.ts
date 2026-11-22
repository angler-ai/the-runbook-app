// Secret fingerprints (design §7.3): HMAC-SHA-256 under a vault-held key,
// first 8 hex characters, so the database is not an offline guessing oracle
// for short secrets (a plain hash would be).
import { createHmac } from 'node:crypto';

export function fingerprint(displayKey: Buffer, plaintext: Buffer): string {
  if (displayKey.length < 32) throw new RangeError('display key must be at least 32 bytes');
  return createHmac('sha256', displayKey).update(plaintext).digest('hex').slice(0, 8);
}
