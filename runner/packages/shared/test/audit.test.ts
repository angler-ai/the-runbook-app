import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { appendAudit, verifyAuditChain } from '../src/audit.ts';
import { createTestDatabase, hasTestDatabase, loginRole, type TestDatabase } from '../test-utils/pg.ts';

describe.skipIf(!hasTestDatabase)('audit chain', () => {
  let db: TestDatabase;
  beforeAll(async () => {
    db = await createTestDatabase();
  });
  afterAll(async () => db?.drop());

  const entry = (action: string) => ({
    actor: { kind: 'user' as const, id: 'us_1' },
    action,
    resource_type: 'credential',
    resource_id: 'cr_1',
    group_id: 'rg_1',
    runner_id: null,
    detail: { field: 'key', version: 1 },
  });

  it('appends a verifiable chain from every process role', async () => {
    for (const role of ['api', 'scheduler', 'worker', 'vault'] as const) {
      await appendAudit(db.pool(role), entry(`test.${role}`));
    }
    expect(await verifyAuditChain(db.pool('api'))).toEqual({ ok: true });
  });

  it('concurrent appends keep one chain', async () => {
    await Promise.all(
      Array.from({ length: 20 }, (_, i) => appendAudit(db.pool(i % 2 ? 'vault' : 'api'), entry(`burst.${i}`))),
    );
    expect(await verifyAuditChain(db.pool('api'))).toEqual({ ok: true });
  });

  it('audit_chain_detects_edited_row', async () => {
    const { id } = await appendAudit(db.pool('api'), entry('credential.updated'));
    await appendAudit(db.pool('api'), entry('credential.tested'));
    // A superuser rewrites history.
    await db.admin().query(`UPDATE audit_log SET detail = '{"field":"key","version":2}' WHERE id = $1`, [id]);
    expect(await verifyAuditChain(db.pool('api'))).toEqual({ ok: false, broken_at: id });
  });

  it('detects a deleted row in the middle', async () => {
    const fresh = await createTestDatabase();
    try {
      await appendAudit(fresh.pool('api'), entry('a'));
      const { id } = await appendAudit(fresh.pool('api'), entry('b'));
      const { id: next } = await appendAudit(fresh.pool('api'), entry('c'));
      await fresh.admin().query(`DELETE FROM audit_log WHERE id = $1`, [id]);
      expect(await verifyAuditChain(fresh.pool('api'))).toEqual({ ok: false, broken_at: next });
    } finally {
      await fresh.drop();
    }
  });

  // These run on a fresh database: the tests above leave a broken chain.
  async function withFresh(fn: (fresh: TestDatabase) => Promise<void>) {
    const fresh = await createTestDatabase();
    try {
      await fn(fresh);
    } finally {
      await fresh.drop();
    }
  }

  it('a writer cannot forge chain values, the time or its database role', () =>
    withFresh(async (db) => {
      const before = await appendAudit(db.pool('api'), entry('before.forged'));
      await db.pool('worker').query(
        `INSERT INTO audit_log (seq, id, at, actor, action, resource_type, detail, prev_hash, hash, db_role)
         VALUES (999999, 'au_forged', '2000-01-01T00:00:00Z', '{"kind":"process","id":"worker"}', 'forged', 'test', '{}',
                 '\\x01', '\\x02', 'rb_api')`,
      );
      const { rows } = await db.admin().query(
        `SELECT f.seq, f.at, f.db_role, f.prev_hash, f.hash, b.hash AS before_hash, b.seq AS before_seq
         FROM audit_log f, audit_log b WHERE f.id = 'au_forged' AND b.id = $1`,
        [before.id],
      );
      const row = rows[0];
      expect(row.db_role).toBe(loginRole('worker'));
      expect(Number(row.seq)).toBe(Number(row.before_seq) + 1);
      expect(row.prev_hash.equals(row.before_hash)).toBe(true);
      expect(Math.abs(row.at.getTime() - Date.now())).toBeLessThan(60_000);
      expect(await verifyAuditChain(db.pool('api'))).toEqual({ ok: true });
      // Writers no longer read the chain columns at all.
      await expect(db.pool('worker').query('SELECT seq, hash FROM audit_log')).rejects.toMatchObject({ code: '42501' });
    }));

  it('appendAudit fails within about 2 s while another session holds the chain lock', () =>
    withFresh(async (db) => {
      const holder = await db.pool('worker').connect();
      try {
        await holder.query('BEGIN');
        await holder.query(
          `INSERT INTO audit_log (id, actor, action, resource_type) VALUES ('au_holder', '{}', 'holder', 'test')`,
        );
        const started = Date.now();
        await expect(appendAudit(db.pool('vault'), entry('blocked'))).rejects.toMatchObject({ code: 'audit.unavailable' });
        expect(Date.now() - started).toBeLessThan(4_000);
      } finally {
        await holder.query('ROLLBACK').catch(() => undefined);
        holder.release();
      }
      expect(await verifyAuditChain(db.pool('api'))).toEqual({ ok: true });
    }));
});
