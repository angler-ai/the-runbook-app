// Append-only audit log with a hash chain (design §7.10). Process roles may
// only INSERT; nobody may UPDATE or DELETE. A BEFORE INSERT trigger owned by
// the migration owner takes the chain lock, sets `seq`, `at`, `db_role`
// (session_user), `prev_hash` and `hash` = SHA-256(prev_hash || the row's
// canonical JSON from `audit_canonical`), ignoring anything the inserter
// sent. Editing or deleting a row breaks the chain at that row or the next.
//
// Entries name what happened, never a secret value: callers pass IDs,
// versions, fingerprints and fixed codes in `detail`.
import { createHash, randomUUID } from 'node:crypto';
import type pg from 'pg';
import { withTransaction } from './db.ts';

export interface AuditActor {
  kind: 'user' | 'agent' | 'process' | 'core';
  id: string;
  /** The person sponsoring an agent principal. */
  sponsor_id?: string;
}

export interface AuditEntry {
  actor: AuditActor;
  action: string;
  resource_type: string;
  resource_id: string | null;
  group_id: string | null;
  runner_id: string | null;
  detail: Record<string, unknown>;
}

/** Writing the audit log failed (chain lock held over 2 s, database down, …). */
export class AuditError extends Error {
  readonly code = 'audit.unavailable';
  constructor(cause: unknown) {
    super('the audit log could not be written', { cause });
    this.name = 'AuditError';
  }
}

const GENESIS: Buffer = Buffer.alloc(32);
/** How long an append waits for the chain lock before failing. */
export const AUDIT_LOCK_TIMEOUT = '2s';

export function chainHash(prev: Buffer, canonical: string): Buffer {
  return createHash('sha256').update(prev).update(canonical, 'utf8').digest();
}

/** Append an entry; the database fills in the chain, the time and the writer's role. Throws AuditError. */
export async function appendAudit(pool: pg.Pool, entry: AuditEntry): Promise<{ id: string }> {
  return withTransaction(pool, (client) => appendAuditInTransaction(client, entry));
}

export async function appendAuditInTransaction(client: pg.PoolClient, entry: AuditEntry): Promise<{ id: string }> {
  const id = `au_${randomUUID().replaceAll('-', '')}`;
  try {
    await client.query(`SET LOCAL lock_timeout = '${AUDIT_LOCK_TIMEOUT}'`);
    await client.query(
      `INSERT INTO audit_log (id, actor, action, resource_type, resource_id, group_id, runner_id, detail)
       VALUES ($1, $2, $3, $4, $5, $6, $7, $8)`,
      [id, JSON.stringify(entry.actor), entry.action, entry.resource_type, entry.resource_id,
        entry.group_id, entry.runner_id, JSON.stringify(entry.detail)],
    );
  } catch (e) {
    throw new AuditError(e);
  }
  return { id };
}

/** Recompute the chain from the first row. Needs SELECT on audit_log (runner-api's role or the owner). */
export async function verifyAuditChain(pool: pg.Pool): Promise<{ ok: boolean; broken_at?: string }> {
  const client = await pool.connect();
  try {
    let prev: Buffer = GENESIS;
    let after = 0n;
    for (;;) {
      const { rows } = await client.query<{ seq: string; id: string; prev_hash: Buffer; hash: Buffer; canonical: string }>(
        `SELECT seq, id, prev_hash, hash, audit_canonical(a) AS canonical
         FROM audit_log a WHERE seq > $1 ORDER BY seq LIMIT 1000`,
        [after.toString()],
      );
      if (rows.length === 0) return { ok: true };
      for (const row of rows) {
        if (!row.prev_hash.equals(prev) || !chainHash(prev, row.canonical).equals(row.hash)) {
          return { ok: false, broken_at: row.id };
        }
        prev = row.hash;
        after = BigInt(row.seq);
      }
    }
  } finally {
    client.release();
  }
}
