import { createHash, randomBytes } from 'node:crypto';
import type pg from 'pg';
import type { UserAssertionClaims } from '@runbook/runner-shared/contracts/core';
import { VaultError } from '../errors.ts';
import type { CredentialRow, SecretStore } from '../secrets.ts';
import { record } from '../types/common.ts';

export const digest = (value: string) => createHash('sha256').update(value, 'utf8').digest();
export const random = () => randomBytes(32).toString('base64url');
export const digestPattern = /^[A-Za-z0-9_-]{43}$/;

export const unavailable = () => new VaultError(409, 'oauth.reconnect_required', 'reconnect the MCP credential');
export const retryRefresh = () => new VaultError(503, 'oauth.refresh_retry', 'OAuth refresh is temporarily unavailable');
export const mismatch = () => new VaultError(409, 'oauth.binding_mismatch', 'the confirmed OAuth binding changed');
export const conflict = () => new VaultError(409, 'oauth.operation_conflict', 'the OAuth operation is no longer current');
export const invalidCallback = () => new VaultError(400, 'oauth.invalid_callback', 'the OAuth callback could not be verified');

export interface ConnectionRow {
  generation: number;
  candidate: Record<string, unknown> | null;
  candidate_hash: Buffer | null;
  registration_operation: string | null;
  client_secret_expires_at: Date | null;
  connection_hash: Buffer | null;
  connection_config: Record<string, unknown> | null;
  connection_groups: 'all' | string[] | null;
  pending_user: string | null;
  pending_session: string | null;
  pending_expires_at: Date | null;
  pending_state: Buffer | null;
  pending_version: number | null;
  pending_hash: Buffer | null;
  pending_claim: string | null;
  access_version: number | null;
  refresh_version: number | null;
  access_issued_at: Date | null;
  access_expires_at: Date | null;
  grant_expires_at: Date | null;
  refreshed_at: Date | null;
  refresh_operation: string | null;
  refresh_after: Date | null;
  refresh_failures: number;
  refresh_failure_since: Date | null;
  refresh_failure_warned_at: Date | null;
  forced_refreshes: number;
  access_rejected_since: Date | null;
  purge_until: Date | null;
  warned_at: Date | null;
}

export interface OAuthCredential extends CredentialRow {
  config: Record<string, unknown>;
}

export function requireAdmin(claims: UserAssertionClaims) {
  if (claims.principal !== 'human' || !['owner', 'admin'].includes(claims.org_role) ||
      !claims.scopes.includes('runner:admin')) {
    throw new VaultError(403, 'auth.denied', 'OAuth operation refused');
  }
}

export function sessionHash(claims: UserAssertionClaims): string {
  requireAdmin(claims);
  if (typeof claims.session_hash !== 'string' || !digestPattern.test(claims.session_hash)) {
    throw new VaultError(403, 'auth.denied', 'OAuth requires a full human session');
  }
  return claims.session_hash;
}

export async function inTransaction<T>(db: pg.PoolClient, action: () => Promise<T>): Promise<T> {
  await db.query('BEGIN');
  try {
    const result = await action();
    await db.query('COMMIT');
    return result;
  } catch (error) {
    await db.query('ROLLBACK').catch(() => undefined);
    throw error;
  }
}

export async function locked(db: pg.PoolClient, id: string) {
  await db.query('SELECT pg_advisory_xact_lock(hashtextextended($1, 7419006))', [id]);
}

export async function oauthCredential(secrets: SecretStore, db: pg.PoolClient | pg.Pool, id: string): Promise<OAuthCredential> {
  const credential = await secrets.credential(id, db);
  if (credential.type !== 'mcp-oauth' || credential.scope !== 'org' || credential.status === 'revoked') throw unavailable();
  return { ...credential, config: record(credential.config) };
}

export async function getConnection(db: pg.PoolClient | pg.Pool, id: string): Promise<ConnectionRow | undefined> {
  const { rows } = await db.query<ConnectionRow>('SELECT * FROM credential_oauth WHERE credential_id = $1', [id]);
  return rows[0];
}

export async function destroyFields(db: pg.PoolClient, credential: string, fields: string[], now: Date) {
  await db.query(
    'UPDATE credential_secret_versions SET wrapped_dek = NULL, destroyed_at = $3 WHERE credential_id = $1 AND field = ANY($2) AND destroyed_at IS NULL',
    [credential, fields, now],
  );
}

export async function writeSecret(secrets: SecretStore, db: pg.PoolClient, credential: OAuthCredential, field: string, value: string, actor: string) {
  const bytes = Buffer.from(value);
  try {
    const version = await secrets.currentVersion(credential.id, field, db);
    return (await secrets.write(db, { credential, field, value: bytes, expectedVersion: version, createdBy: actor })).version;
  } finally {
    bytes.fill(0);
  }
}
