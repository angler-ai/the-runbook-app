import { randomBytes, sign, type KeyObject } from 'node:crypto';
import type pg from 'pg';
import type { IngestKeyResponse } from '@runbook/runner-shared/contracts/internal';
import { vaultIdentityFingerprint, vaultIdentityPrivateKey, vaultIdentityPublicKey } from '@runbook/runner-shared/vault-identity';
import { withTransaction } from '@runbook/runner-shared/db';
import { audit } from './audit.ts';
import { lengthPrefixed } from '@runbook/runner-shared/encoding';
import type { Kek } from './kek/index.ts';
import { loadOrCreateKey } from './keys.ts';

const PURPOSE = 'ed25519-identity';

export class VaultIdentity {
  readonly publicKey: string;
  private readonly privateKey: KeyObject;

  private constructor(seed: Buffer) {
    this.privateKey = vaultIdentityPrivateKey(seed);
    this.publicKey = vaultIdentityPublicKey(this.privateKey);
  }

  static async load(pool: pg.Pool, kek: Kek): Promise<VaultIdentity> {
    const seed = await withTransaction(pool, async (client) => {
      let bootstrap = false;
      return loadOrCreateKey(client, kek, PURPOSE, () => {
        const key = kek.bootstrapIdentitySeed?.();
        bootstrap = key !== undefined;
        return key;
      }, async (key) => {
        if (!bootstrap) return;
        const fingerprint = vaultIdentityFingerprint(new VaultIdentity(key).publicKey);
        await audit(client, {
          actor: { kind: 'process', id: 'installer bootstrap' }, action: 'vault.identity_enrolled',
          resource_type: 'vault_identity', resource_id: fingerprint, group_id: null, runner_id: null, detail: {},
        });
      });
    });
    try {
      return new VaultIdentity(seed);
    } finally {
      seed.fill(0);
    }
  }

  static async rotate(pool: pg.Pool, kek: Kek): Promise<VaultIdentity> {
    const seed = randomBytes(32);
    try {
      const { wrapped, kekRef } = await kek.wrap(seed, { purpose: `vault-key:${PURPOSE}` });
      const identity = new VaultIdentity(seed);
      await withTransaction(pool, async (client) => {
        await client.query(
          `INSERT INTO vault_keys (purpose, wrapped, kek_ref) VALUES ($1, $2, $3)
           ON CONFLICT (purpose) DO UPDATE SET wrapped = EXCLUDED.wrapped, kek_ref = EXCLUDED.kek_ref`,
          [PURPOSE, wrapped, kekRef],
        );
        await audit(client, {
          actor: { kind: 'process', id: 'vault-operator' }, action: 'vault.identity_rotated',
          resource_type: 'vault_identity', resource_id: vaultIdentityFingerprint(identity.publicKey),
          group_id: null, runner_id: null, detail: {},
        });
      });
      return identity;
    } finally {
      seed.fill(0);
    }
  }

  credentialReceipt(org: string, install: string, credential: {
    id: string; name: string; binding_hash: string; label: string; sharing: { projects: string | string[] };
  }): string {
    return sign(null, lengthPrefixed([
      'runbook-runner/credential-create/v1', org, install, credential.id, credential.name,
      credential.binding_hash, credential.label, JSON.stringify(credential.sharing.projects),
    ]), this.privateKey).toString('base64url');
  }

  attest(key: IngestKeyResponse, org: string, install: string): IngestKeyResponse {
    const statement = lengthPrefixed([
      'runbook-runner/ingest-key/v1', org, install, key.ingest_key_id, key.public_key, key.expires_at,
    ]);
    return {
      ...key, org_id: org, install_id: install,
      identity_signature: sign(null, statement, this.privateKey).toString('base64url'),
    };
  }
}
