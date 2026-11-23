import type pg from 'pg';
import { errorFields, type Logger } from '@runbook/runner-shared/log';
import { rewrapKey } from './crypto/envelope.ts';
import type { Kek, WrapContext } from './kek/index.ts';

const BATCH = 100;
type Wrap = { wrapped: Buffer; kek_ref: string };

export async function rewrapVaultKeys(pool: pg.Pool, kek: Kek, orgId: string, log: Logger): Promise<number> {
  // An encryption probe also works with encrypt/decrypt-only cloud roles.
  const { kekRef: current } = await kek.wrap(Buffer.alloc(32), { purpose: 'rewrap-version-check' });
  const prefix = current.slice(0, current.lastIndexOf('/') + 1);
  const version = BigInt(current.slice(prefix.length));
  let changed = 0;
  async function swap(row: Wrap, context: WrapContext, save: (wrapped: Buffer, ref: string) => Promise<pg.QueryResult>) {
    const suffix = row.kek_ref.slice(prefix.length);
    if (!row.kek_ref.startsWith(prefix) || !/^\d+$/.test(suffix) || BigInt(suffix) >= version) return;
    try {
      const { wrapped, kekRef } = await rewrapKey(kek, row.wrapped, row.kek_ref, context);
      const result = await save(wrapped, kekRef);
      changed += result.rowCount ?? 0;
    } catch (error) {
      log.warn('KEK re-wrap failed for a row', errorFields(error));
    }
  }

  let cursor: [string, string, number] = ['', '', 0];
  while (true) {
    const { rows } = await pool.query<Wrap & { credential_id: string; field: string; version: number }>(
      `SELECT s.credential_id, s.field, s.version, s.wrapped_dek AS wrapped, s.kek_ref
       FROM credential_secret_versions s JOIN credentials c ON c.id = s.credential_id
       WHERE c.org_id = $1 AND s.kek_ref <> $2 AND s.wrapped_dek IS NOT NULL AND s.destroyed_at IS NULL
         AND (s.credential_id, s.field, s.version) > ($3,$4,$5)
       ORDER BY s.credential_id, s.field, s.version LIMIT $6`, [orgId, current, ...cursor, BATCH],
    );
    for (const row of rows) {
      cursor = [row.credential_id, row.field, row.version];
      await swap(row, { org_id: orgId, credential_id: row.credential_id }, (wrapped, ref) => pool.query(
        `UPDATE credential_secret_versions SET wrapped_dek = $4, kek_ref = $5
         WHERE credential_id = $1 AND field = $2 AND version = $3 AND wrapped_dek = $6 AND kek_ref = $7 AND destroyed_at IS NULL`,
        [row.credential_id, row.field, row.version, wrapped, ref, row.wrapped, row.kek_ref],
      ));
    }
    if (rows.length < BATCH) break;
  }

  const ingest = await pool.query<Wrap & { id: string }>(
    'SELECT id, wrapped_private_key AS wrapped, kek_ref FROM ingest_keys WHERE kek_ref <> $1', [current],
  );
  for (const row of ingest.rows) await swap(row, { purpose: 'ingest-key', ingest_key_id: row.id }, (wrapped, ref) => pool.query(
    'UPDATE ingest_keys SET wrapped_private_key = $2, kek_ref = $3 WHERE id = $1 AND wrapped_private_key = $4 AND kek_ref = $5',
    [row.id, wrapped, ref, row.wrapped, row.kek_ref],
  ));
  const held = await pool.query<Wrap & { purpose: string }>(
    'SELECT purpose, wrapped, kek_ref FROM vault_keys WHERE kek_ref <> $1', [current],
  );
  for (const row of held.rows) await swap(row, { purpose: `vault-key:${row.purpose}` }, (wrapped, ref) => pool.query(
    'UPDATE vault_keys SET wrapped = $2, kek_ref = $3 WHERE purpose = $1 AND wrapped = $4 AND kek_ref = $5',
    [row.purpose, wrapped, ref, row.wrapped, row.kek_ref],
  ));
  if (changed) log.info('vault keys re-wrapped', { count: changed });
  return changed;
}
