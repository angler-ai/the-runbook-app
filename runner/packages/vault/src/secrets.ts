// Secret versions (design §7.3): written and read only by the vault. A new
// version supersedes the previous one, which stays decryptable for 24 hours
// and only for releases under a grant already pinned to it; then its wrapped
// data key is deleted. Every open recomputes the destination binding from
// the credential's current config.
import type pg from 'pg';
import { bindingHash } from '@runbook/runner-shared/binding';
import type { Logger } from '@runbook/runner-shared/log';
import { BindingMismatch, open, seal } from './crypto/envelope.ts';
import { fingerprint } from './crypto/fingerprint.ts';
import { VaultError } from './errors.ts';
import type { Kek } from './kek/index.ts';
import { canonicalGroups, credentialBinding, type Groups, typeImpl } from './types/index.ts';

export const PREVIOUS_VERSION_GRACE_MS = 24 * 3600_000;

export interface CredentialRow {
  id: string;
  org_id: string;
  name: string;
  type: string;
  scope: 'org' | 'project';
  project_id: string | null;
  created_by: string | null;
  config: unknown;
  groups: unknown;
  sharing: { projects?: unknown };
  label: string;
  label_initialized: boolean;
  status: string;
}

export interface OpenedSecret {
  credential: CredentialRow;
  field: string;
  version: number;
  value: Buffer;
  binding: Record<string, unknown>;
  groups: Groups;
}

interface VersionRow {
  version: number;
  ciphertext: Buffer;
  nonce: Buffer;
  wrapped_dek: Buffer | null;
  kek_ref: string;
  binding_hash: Buffer;
  superseded_at: Date | null;
  destroyed_at: Date | null;
}

export interface SecretStoreOptions {
  pool: pg.Pool;
  kek: Kek;
  orgId: string;
  displayKey: Buffer;
  now: () => Date;
  log: Logger;
}

type Queryable = Pick<pg.PoolClient, 'query'>;

export class SecretStore {
  private readonly o: SecretStoreOptions;
  constructor(options: SecretStoreOptions) {
    this.o = options;
  }

  async credential(id: string, db: Queryable = this.o.pool, lock = false): Promise<CredentialRow> {
    const { rows } = await db.query<CredentialRow>(
      `SELECT id, org_id, name, type, scope, project_id, created_by, config, groups, sharing, label, label_initialized, status FROM credentials WHERE id = $1${lock ? ' FOR UPDATE' : ''}`,
      [id],
    );
    const row = rows[0];
    if (!row || row.org_id !== this.o.orgId) throw new VaultError(404, 'credential.not_found', 'no such credential');
    return row;
  }

  /** The binding and its hash, recomputed from the credential's current config. */
  binding(credential: CredentialRow): { binding: Record<string, unknown>; hash: Buffer; groups: Groups } {
    const groups = canonicalGroups(credential.groups);
    const binding = credentialBinding(credential.type, credential.config, groups);
    return { binding, hash: bindingHash(credential.type, binding), groups };
  }

  private aad(credential: CredentialRow, field: string, version: number, hash: Buffer) {
    return { orgId: this.o.orgId, credentialId: credential.id, field, version, type: credential.type, bindingHash: hash };
  }

  /** The newest version number of a field (destroyed or not), 0 when none. */
  async currentVersion(credentialId: string, field: string, db: Queryable = this.o.pool): Promise<number> {
    const { rows } = await db.query<{ v: number | null }>(
      `SELECT max(version) AS v FROM credential_secret_versions WHERE credential_id = $1 AND field = $2`,
      [credentialId, field],
    );
    return rows[0]?.v ?? 0;
  }

  /** The version a new release gets: the newest one, if it is usable. */
  async latestVersion(credentialId: string, field: string): Promise<number | undefined> {
    const { rows } = await this.o.pool.query<{ version: number }>(
      `SELECT version FROM credential_secret_versions
       WHERE credential_id = $1 AND field = $2 AND superseded_at IS NULL AND destroyed_at IS NULL
       ORDER BY version DESC LIMIT 1`,
      [credentialId, field],
    );
    return rows[0]?.version;
  }

  /**
   * Store `value` as version `expectedVersion + 1` inside the caller's
   * transaction. The previous version is marked superseded now.
   */
  async write(
    db: Queryable,
    input: { credential: CredentialRow; field: string; expectedVersion: number; value: Buffer; createdBy: string },
  ): Promise<{ version: number; fingerprint: string }> {
    const { credential, field } = input;
    const current = await this.currentVersion(credential.id, field, db);
    if (current !== input.expectedVersion) {
      throw new VaultError(409, 'seal.stale_version', 'the secret changed since this value was sealed; seal it again');
    }
    const version = current + 1;
    const { hash } = this.binding(credential);
    const row = await seal(this.o.kek, input.value, this.aad(credential, field, version, hash), {
      org_id: this.o.orgId,
      credential_id: credential.id,
    });
    const print = fingerprint(this.o.displayKey, input.value);
    const now = this.o.now();
    try {
      await db.query(
        `INSERT INTO credential_secret_versions
           (credential_id, field, version, ciphertext, nonce, wrapped_dek, kek_ref, binding_hash, fingerprint, created_at, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)`,
        [credential.id, field, version, row.ciphertext, row.nonce, row.wrappedDek, row.kekRef, row.bindingHash, print, now, input.createdBy],
      );
    } catch (e) {
      if ((e as { code?: string }).code === '23505') {
        throw new VaultError(409, 'seal.stale_version', 'the secret changed since this value was sealed; seal it again');
      }
      throw e;
    }
    await db.query(
      `UPDATE credential_secret_versions SET superseded_at = $4
       WHERE credential_id = $1 AND field = $2 AND version < $3 AND superseded_at IS NULL`,
      [credential.id, field, version, now],
    );
    return { version, fingerprint: print };
  }

  /**
   * Decrypt a field for a release. Without `pinnedVersion` (a new grant) only
   * the newest version opens; with it (renewing an existing grant) that
   * version opens while it is current or within 24 hours of being superseded.
   */
  async openForRelease(input: {
    credentialId: string;
    field?: string;
    pinnedVersion?: number;
    authorize?(credential: CredentialRow): Promise<void>;
  }): Promise<OpenedSecret> {
    const credential = await this.credential(input.credentialId);
    await input.authorize?.(credential);
    if (credential.status === 'revoked') throw new VaultError(409, 'credential.revoked', 'the credential is revoked');
    const impl = typeImpl(credential.type);
    const field = input.field ?? impl.secretFields[0];
    if (!field || !impl.secretFields.includes(field)) {
      throw new VaultError(400, 'credential.not_secret_field', 'not a secret field of this credential type');
    }
    const version = input.pinnedVersion ?? (await this.latestVersion(credential.id, field));
    if (version === undefined) throw new VaultError(409, 'secret.not_set', 'the credential has no secret yet');
    const { rows } = await this.o.pool.query<VersionRow>(
      `SELECT version, ciphertext, nonce, wrapped_dek, kek_ref, binding_hash, superseded_at, destroyed_at
       FROM credential_secret_versions WHERE credential_id = $1 AND field = $2 AND version = $3`,
      [credential.id, field, version],
    );
    const row = rows[0];
    if (!row) throw new VaultError(404, 'secret.not_found', 'no such secret version');
    if (row.destroyed_at || !row.wrapped_dek) {
      throw new VaultError(409, 'secret.version_destroyed', 'that secret version was destroyed');
    }
    if (row.superseded_at) {
      const age = this.o.now().getTime() - row.superseded_at.getTime();
      if (input.pinnedVersion === undefined || age >= PREVIOUS_VERSION_GRACE_MS) {
        throw new VaultError(409, 'secret.version_expired', 'that secret version was replaced');
      }
    }
    const { binding, hash, groups } = this.binding(credential);
    try {
      const value = await open(
        this.o.kek,
        { ciphertext: row.ciphertext, nonce: row.nonce, wrappedDek: row.wrapped_dek, kekRef: row.kek_ref, bindingHash: row.binding_hash },
        this.aad(credential, field, version, hash),
      );
      return { credential, field, version, value, binding, groups };
    } catch (e) {
      if (e instanceof BindingMismatch) {
        this.o.log.warn('binding mismatch at unwrap', { credential_id: credential.id, field, version });
        throw new VaultError(409, 'vault.binding_mismatch', 'the credential destination changed; enter the secret again');
      }
      throw e;
    }
  }

  /** Delete the wrapped data keys of versions superseded more than 24 hours ago. */
  async destroyExpiredVersions(): Promise<number> {
    const now = this.o.now();
    const { rowCount } = await this.o.pool.query(
      `UPDATE credential_secret_versions SET wrapped_dek = NULL, destroyed_at = $1
       WHERE superseded_at IS NOT NULL AND superseded_at <= $2 AND destroyed_at IS NULL`,
      [now, new Date(now.getTime() - PREVIOUS_VERSION_GRACE_MS)],
    );
    return rowCount ?? 0;
  }
}
