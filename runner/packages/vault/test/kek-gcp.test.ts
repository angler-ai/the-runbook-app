// Runs against a real Cloud KMS key only when RUNNER_TEST_GCP_KMS_KEY names
// one (projects/…/locations/…/keyRings/…/cryptoKeys/…) and application
// default credentials can use it; skipped otherwise.
import { describe, expect, it } from 'vitest';
import { DecryptError } from '../src/crypto/envelope.ts';
import { GcpKmsKek } from '../src/kek/gcp-kms.ts';

const KEY = process.env.RUNNER_TEST_GCP_KMS_KEY;
const ctx = { org_id: 'org_test', credential_id: 'cr_test' };

describe.skipIf(!KEY)('gcp-kms KEK', () => {
  it('wraps and unwraps a data key with the context as AAD', async () => {
    const kek = new GcpKmsKek(KEY ?? '');
    const dek = Buffer.alloc(32, 9);
    const { wrapped, kekRef } = await kek.wrap(dek, ctx);
    expect(kekRef).toMatch(/^gcp-kms\/projects\/.+\/cryptoKeyVersions\/\d+$/);
    expect((await kek.unwrap(wrapped, kekRef, ctx)).equals(dek)).toBe(true);
    await expect(kek.unwrap(wrapped, kekRef, { ...ctx, credential_id: 'cr_other' })).rejects.toBeInstanceOf(
      DecryptError,
    );
  });
});

describe('gcp-kms KEK (offline)', () => {
  it('refuses a key name that is not a crypto key', () => {
    expect(() => new GcpKmsKek('projects/p/locations/l/keyRings/r')).toThrow(/cryptoKeys/);
  });

  it('refuses a kekRef for another key', async () => {
    const kek = new GcpKmsKek('projects/p/locations/l/keyRings/r/cryptoKeys/k');
    await expect(
      kek.unwrap(Buffer.alloc(8), 'gcp-kms/projects/p/locations/l/keyRings/r/cryptoKeys/other/cryptoKeyVersions/1', ctx),
    ).rejects.toBeInstanceOf(DecryptError);
  });
});
