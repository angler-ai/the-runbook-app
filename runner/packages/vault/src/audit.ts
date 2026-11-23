// The vault's audit writes: an entry that can't be written (the chain lock
// held over 2 s, the database down) fails the operation with a fixed error
// instead of hanging it.
import type pg from 'pg';
import { appendAudit, appendAuditInTransaction, AuditError, type AuditEntry } from '@runbook/runner-shared/audit';
import { VaultError } from './errors.ts';

export const AUDIT_UNAVAILABLE = 'audit.unavailable';

export async function audit(pool: pg.Pool | pg.PoolClient, entry: AuditEntry): Promise<void> {
  try {
    if ('release' in pool) await appendAuditInTransaction(pool, entry);
    else await appendAudit(pool, entry);
  } catch (e) {
    if (e instanceof AuditError) throw new VaultError(503, AUDIT_UNAVAILABLE, 'the audit log could not be written');
    throw e;
  }
}
