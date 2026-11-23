// The `gcp-kms` KEK (design §7.3): Cloud KMS symmetric encryption with the
// wrap context as additional authenticated data. The vault authenticates with
// its attached service account (application default credentials), so no key
// material is on disk. Key rotation happens in KMS; `rewrap` moves data keys
// to the newest version. The client library is loaded only when used.
import { canonicalJson } from '@runbook/runner-shared/binding';
import { DecryptError } from '../crypto/envelope.ts';
import type { Kek, KekInfo, WrapContext } from './index.ts';

const KEY_NAME = /^projects\/[^/]+\/locations\/[^/]+\/keyRings\/[^/]+\/cryptoKeys\/[^/]+$/;

interface KmsClient {
  encrypt(request: {
    name: string;
    plaintext: Buffer;
    additionalAuthenticatedData: Buffer;
  }): Promise<[{ ciphertext?: Uint8Array | string | null; name?: string | null }, ...unknown[]]>;
  decrypt(request: {
    name: string;
    ciphertext: Buffer;
    additionalAuthenticatedData: Buffer;
  }): Promise<[{ plaintext?: Uint8Array | string | null }, ...unknown[]]>;
}

export class GcpKmsKek implements Kek {
  private client: Promise<KmsClient> | undefined;
  readonly keyName: string;

  constructor(keyName: string, client?: KmsClient) {
    if (!KEY_NAME.test(keyName)) {
      throw new Error('gcp-kms key must be projects/…/locations/…/keyRings/…/cryptoKeys/…');
    }
    this.keyName = keyName;
    if (client) this.client = Promise.resolve(client);
  }

  private kms(): Promise<KmsClient> {
    this.client ??= import('@google-cloud/kms').then(
      (m) => new m.KeyManagementServiceClient() as unknown as KmsClient,
    );
    return this.client;
  }

  private aad(ctx: WrapContext): Buffer {
    return Buffer.from(canonicalJson(ctx), 'utf8');
  }

  async wrap(dek: Buffer, ctx: WrapContext): Promise<{ wrapped: Buffer; kekRef: string }> {
    const kms = await this.kms();
    const [response] = await kms.encrypt({ name: this.keyName, plaintext: dek, additionalAuthenticatedData: this.aad(ctx) });
    if (!response.ciphertext || !response.name?.startsWith(`${this.keyName}/cryptoKeyVersions/`)) {
      throw new Error('Cloud KMS returned no ciphertext for the configured key');
    }
    return { wrapped: Buffer.from(response.ciphertext), kekRef: `gcp-kms/${response.name}` };
  }

  async unwrap(wrapped: Buffer, kekRef: string, ctx: WrapContext): Promise<Buffer> {
    // Decrypt picks the version from the ciphertext; the ref must still be this key's.
    if (!kekRef.startsWith(`gcp-kms/${this.keyName}/cryptoKeyVersions/`)) throw new DecryptError('data key');
    const kms = await this.kms();
    try {
      const [response] = await kms.decrypt({
        name: this.keyName,
        ciphertext: wrapped,
        additionalAuthenticatedData: this.aad(ctx),
      });
      if (!response.plaintext) throw new DecryptError('data key');
      return Buffer.from(response.plaintext);
    } catch (e) {
      if (e instanceof DecryptError) throw e;
      // INVALID_ARGUMENT is what KMS answers for a wrong AAD or ciphertext.
      if ((e as { code?: unknown }).code === 3) throw new DecryptError('data key');
      throw new Error('Cloud KMS decrypt failed', { cause: undefined });
    }
  }

  describe(): KekInfo {
    return { provider: 'gcp-kms', key: this.keyName };
  }
}
