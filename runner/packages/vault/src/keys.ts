// Vault-held keys other than ingest keys, stored KEK-wrapped in vault_keys:
// the fingerprint HMAC key ("vault display key", design §7.3). Created on
// first start; replicas starting together agree on one row.
import { randomBytes } from 'node:crypto';
import type pg from 'pg';
import type { Kek } from './kek/index.ts';

export async function loadOrCreateKey(pool: pg.Pool | pg.PoolClient, kek: Kek, purpose: string,
  initialKey?: () => Buffer | undefined, onCreated?: (key: Buffer) => Promise<void>): Promise<Buffer> {
  const ctx = { purpose: `vault-key:${purpose}` };
  const existing = await pool.query<{ wrapped: Buffer; kek_ref: string }>(
    'SELECT wrapped, kek_ref FROM vault_keys WHERE purpose = $1',
    [purpose],
  );
  if (existing.rows[0]) return kek.unwrap(existing.rows[0].wrapped, existing.rows[0].kek_ref, ctx);
  const key = initialKey?.() ?? randomBytes(32);
  try {
    const { wrapped, kekRef } = await kek.wrap(key, ctx);
    const inserted = await pool.query(
      'INSERT INTO vault_keys (purpose, wrapped, kek_ref) VALUES ($1, $2, $3) ON CONFLICT (purpose) DO NOTHING',
      [purpose, wrapped, kekRef],
    );
    if (inserted.rowCount === 1) {
      await onCreated?.(key);
      return key;
    }
  } catch (error) {
    key.fill(0);
    throw error;
  }
  key.fill(0);
  return loadOrCreateKey(pool, kek, purpose, initialKey, onCreated);
}
