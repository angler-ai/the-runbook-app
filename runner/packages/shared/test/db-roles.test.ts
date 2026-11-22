// The runner database's per-process roles (design §7.9, plan "Runner
// database"). These grants are security requirements: the vault is the only
// reader and writer of secret tables, and nobody can rewrite the audit log.
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { type DbRole, DB_ROLES } from '../src/db.ts';
import { createTestDatabase, hasTestDatabase, type TestDatabase } from '../test-utils/pg.ts';

const SECRET_TABLES = ['credential_secret_versions', 'ingest_keys', 'vault_keys', 'seal_replay'];

describe.skipIf(!hasTestDatabase)('runner database roles', () => {
  let db: TestDatabase;
  beforeAll(async () => {
    db = await createTestDatabase();
    await db.admin().query(
      `INSERT INTO credentials (id, org_id, name, type, config, binding)
       VALUES ('cr_1', 'org_1', 'datadog', 'api-key', '{}', '{}')`,
    );
  });
  afterAll(async () => db?.drop());

  /** Run SQL as a role; resolve to the Postgres error code, or 'ok'. */
  async function attempt(role: DbRole, sql: string, params: unknown[] = []): Promise<string> {
    try {
      await db.pool(role).query(sql, params);
      return 'ok';
    } catch (e) {
      return (e as { code?: string }).code ?? 'error';
    }
  }
  const DENIED = '42501'; // insufficient_privilege

  it('api_role_cannot_read_secret_versions', async () => {
    for (const table of SECRET_TABLES) {
      expect(await attempt('api', `SELECT * FROM ${table}`), table).toBe(DENIED);
    }
    // It can read and write credential metadata (configuration).
    expect(await attempt('api', `SELECT id, binding_hash FROM credentials`)).toBe('ok');
    expect(await attempt('api', `UPDATE credentials SET name = 'dd' WHERE id = 'cr_1'`)).toBe('ok');
  });

  it('vault_is_sole_writer_of_secret_versions', async () => {
    const insert = `INSERT INTO credential_secret_versions
      (credential_id, field, version, ciphertext, nonce, wrapped_dek, kek_ref, binding_hash, fingerprint, created_by)
      VALUES ('cr_1', 'key', $1, '\\x00', '\\x00', '\\x00', 'local/test/v1', '\\x00', 'abcd1234', 'test')`;
    for (const role of DB_ROLES.filter((r) => r !== 'vault')) {
      expect(await attempt(role, insert, [1]), role).toBe(DENIED);
      expect(await attempt(role, `UPDATE credential_secret_versions SET destroyed_at = now()`), role).toBe(DENIED);
      expect(await attempt(role, `DELETE FROM credential_secret_versions`), role).toBe(DENIED);
      expect(await attempt(role, `SELECT 1 FROM credential_secret_versions`), role).toBe(DENIED);
    }
    expect(await attempt('vault', insert, [1])).toBe('ok');
    expect(await attempt('vault', `SELECT ciphertext FROM credential_secret_versions`)).toBe('ok');
    // Catalog check too, so a later migration can't widen it unnoticed.
    const { rows } = await db.admin().query<{ grantee: string; table_name: string }>(
      `SELECT DISTINCT grantee, table_name FROM information_schema.role_table_grants
       WHERE table_name = ANY($1) AND grantee NOT IN ('postgres', 'PUBLIC')`,
      [SECRET_TABLES],
    );
    expect(new Set(rows.map((r) => r.grantee))).toEqual(new Set(['rb_vault']));
  });

  it('api cannot delete a credential that still has secret versions', async () => {
    // A cascade would run as the table owner and let runner-api shred secrets.
    expect(await attempt('api', `DELETE FROM credentials WHERE id = 'cr_1'`)).toBe(DENIED);
  });

  it('runner-api cannot relabel or restore a revoked credential in Postgres', async () => {
    const admin = db.admin();
    await admin.query("UPDATE credentials SET status = 'revoked', revoked_at = now() WHERE id = 'cr_1'");
    expect(await attempt('api', "UPDATE credentials SET label = 'read' WHERE id = 'cr_1'")).toBe(DENIED);
    expect(await attempt('api', "UPDATE credentials SET status = 'active', revoked_at = NULL WHERE id = 'cr_1'")).toBe(DENIED);
    expect(await attempt('api', "DELETE FROM credentials WHERE id = 'cr_1'")).toBe(DENIED);
  });

  it('worker_role_cannot_update_audit', async () => {
    await db.admin().query(
      `INSERT INTO audit_log (id, at, actor, action, resource_type, detail, prev_hash, hash)
       VALUES ('au_1', now(), '{}', 'test', 'test', '{}', '\\x00', '\\x00')`,
    );
    for (const role of DB_ROLES) {
      expect(await attempt(role, `UPDATE audit_log SET action = 'x'`), role).toBe(DENIED);
      expect(await attempt(role, `DELETE FROM audit_log`), role).toBe(DENIED);
      expect(await attempt(role, `TRUNCATE audit_log`), role).toBe(DENIED);
    }
    // Writers are INSERT-only (the trigger extends the chain); only runner-api reads entries (audit API).
    for (const role of DB_ROLES.filter((r) => r !== 'api')) {
      expect(await attempt(role, `SELECT seq, hash FROM audit_log`), role).toBe(DENIED);
      expect(await attempt(role, `SELECT detail FROM audit_log`), role).toBe(DENIED);
      expect(await attempt(role, `SELECT * FROM audit_chain_head`), role).toBe(DENIED);
      expect(await attempt(role, `UPDATE audit_chain_head SET seq = 0`), role).toBe(DENIED);
    }
    expect(await attempt('api', `SELECT detail FROM audit_log`)).toBe('ok');
  });

  it('workers cannot write transcripts or usage on behalf of another runner', async () => {
    expect(await attempt('worker', `SELECT * FROM agents`)).toBe('ok');
    expect(await attempt('worker', `SELECT * FROM credentials`)).toBe(DENIED);
    expect(await attempt('worker', `UPDATE agents SET status = 'active'`)).toBe(DENIED);
    expect(
      await attempt(
        'worker',
        `INSERT INTO transcripts (step_run_id, group_id, runner_id, project_id, agent_id, seq, entry)
         VALUES ('sr_1', 'rg_1', 'rn_1', 'pr_1', 'ag_1', 1, '{}')`,
      ),
    ).toBe(DENIED);
    expect(await attempt('worker', `INSERT INTO usage_events (step_run_id, group_id, runner_id, kind)
      VALUES ('sr_1', 'rg_1', 'rn_1', 'model')`)).toBe(DENIED);

    expect(await attempt('worker', `SELECT * FROM transcripts`)).toBe(DENIED);
    const toolCall = `INSERT INTO tool_calls (step_run_id, group_id, runner_id, project_id, agent_id, tool)
      VALUES ('sr_1', 'rg_1', 'rn_1', 'pr_1', 'ag_1', 'read')`;
    expect(await attempt('worker', toolCall)).toBe(DENIED);
    expect(await attempt('api', toolCall)).toBe('ok');
  });

  it('agent versions are immutable', async () => {
    await db.admin().query(
      `INSERT INTO agents (id, project_id, group_id, name) VALUES ('ag_1', 'pr_1', 'rg_1', 'checker')`,
    );
    expect(
      await attempt(
        'api',
        `INSERT INTO agent_versions (id, agent_id, version, config, ceiling, created_by)
         VALUES ('av_1', 'ag_1', 1, '{}', '{}', 'us_1')`,
      ),
    ).toBe('ok');
    expect(await attempt('api', `UPDATE agent_versions SET config = '{"x":1}'`)).toBe(DENIED);
    expect(await attempt('api', `DELETE FROM agent_versions`)).toBe(DENIED);
  });

  it('every execution table carries group_id and runner_id (design §6.4)', async () => {
    const { rows } = await db.admin().query<{ table_name: string; column_name: string }>(
      `SELECT table_name, column_name FROM information_schema.columns WHERE table_schema = 'public'`,
    );
    const columns = new Map<string, Set<string>>();
    for (const r of rows) {
      if (!columns.has(r.table_name)) columns.set(r.table_name, new Set());
      columns.get(r.table_name)?.add(r.column_name);
    }
    const planTables = [
      'groups', 'runners', 'providers', 'profiles', 'mcp_servers', 'mcp_tool_snapshots', 'credentials',
      'credential_secret_versions', 'ingest_keys', 'agents', 'agent_versions', 'proposals', 'policy',
      'ledger_buckets', 'ledger_reservations', 'usage_events', 'slots', 'transcripts', 'tool_calls',
      'releases', 'audit_log',
    ];
    for (const t of planTables) expect(columns.has(t), t).toBe(true);
    const both = [
      'runners', 'slots', 'ledger_buckets', 'ledger_reservations', 'usage_events', 'transcripts',
      'tool_calls', 'releases', 'audit_log',
    ];
    for (const t of both) {
      expect(columns.get(t)?.has('group_id'), `${t}.group_id`).toBe(true);
      expect(columns.get(t)?.has('runner_id'), `${t}.runner_id`).toBe(true);
    }
    for (const t of ['groups', 'agents']) expect(columns.get(t)?.has('group_id'), `${t}.group_id`).toBe(true);
    // Credentials name the groups they may be released to (part of the binding).
    expect(columns.get('credentials')?.has('groups')).toBe(true);
  });

  it('migrations are idempotent and recorded', async () => {
    const { migrate } = await import('../src/db.ts');
    await migrate(db.adminUrl);
    const { rows } = await db.admin().query(`SELECT version FROM runner_schema_migrations ORDER BY version`);
    expect(rows.map((r) => r.version)).toEqual(['0001_init', '0002_assertion_replays', '0003_budget_policy', '0004_slot_claim', '0005_gateway_tool_records', '0006_credential_deletion', '0007_catalog_snapshots', '0008_scheduler_events', '0009_sandbox_metering', '0010_worker_reports', '0011_vault_model_profile_policy', '0012_agent_observations', '0013_cache_write_usage', '0014_vertex_gemini', '0015_vault_assertion_replays', '0016_transcript_writer', '0017_atomic_seal_metadata', '0018_vault_credential_policy', '0019_reservation_capability', '0020_usage_reconciliation', '0021_atomic_credential_policy', '0022_vault_proposal_policy', '0023_pending_usage_writer', '0024_ambiguous_legacy_usage', '0025_transcript_claim', '0026_decision_ledger', '0027_task_claim_ledger']);
  });

  it('openPool refuses a superuser or a role outside its group', async () => {
    const { openPool } = await import('../src/db.ts');
    const p = openPool('vault', db.adminUrl, { max: 1 });
    await expect(p.query('SELECT 1')).rejects.toThrow(/rb_vault/);
    await p.end();
    const wrong = openPool('vault', db.urlFor('api'), { max: 1 });
    await expect(wrong.query('SELECT 1')).rejects.toThrow(/rb_vault/);
    await wrong.end();
  });

  it('openPool refuses a login in another process role or owning runner tables', async () => {
    const { openPool } = await import('../src/db.ts');
    const suffix = Math.random().toString(36).slice(2, 10);
    const twoRoles = `rb_test_two_${suffix}`;
    const owner = `rb_test_owner_${suffix}`;
    const admin = db.admin();
    const urlAs = (login: string) => {
      const u = new URL(db.adminUrl);
      u.username = login;
      u.password = 'fake-test-only-password';
      return u.toString();
    };
    try {
      await admin.query(`CREATE ROLE ${twoRoles} LOGIN PASSWORD 'fake-test-only-password' IN ROLE rb_vault, rb_api`);
      await admin.query(`CREATE ROLE ${owner} LOGIN PASSWORD 'fake-test-only-password' IN ROLE rb_vault`);
      await admin.query(`CREATE TABLE owned_${suffix} (x int)`);
      await admin.query(`ALTER TABLE owned_${suffix} OWNER TO ${owner}`);
      const two = openPool('vault', urlAs(twoRoles), { max: 1 });
      await expect(two.query('SELECT 1')).rejects.toThrow(/rb_api/);
      await two.end();
      const owns = openPool('vault', urlAs(owner), { max: 1 });
      await expect(owns.query('SELECT 1')).rejects.toThrow(/owns/);
      await owns.end();
      // The ordinary process login still connects.
      expect((await db.pool('vault').query('SELECT 1 AS one')).rows[0].one).toBe(1);
    } finally {
      await admin.query(`DROP TABLE IF EXISTS owned_${suffix}`);
      await admin.query(`DROP ROLE IF EXISTS ${twoRoles}`);
      await admin.query(`DROP ROLE IF EXISTS ${owner}`);
    }
  });

  it('process roles cannot create tables in the runner schema', async () => {
    for (const role of DB_ROLES) {
      expect(await attempt(role, `CREATE TABLE sneaky_${role} (x int)`), role).toBe(DENIED);
    }
    // Also where PUBLIC still has CREATE on public (the default before Postgres 15).
    const old = await createTestDatabase({ beforeMigrate: (c) => c.query('GRANT CREATE ON SCHEMA public TO PUBLIC') });
    try {
      for (const role of DB_ROLES) {
        const result = await old
          .pool(role)
          .query(`CREATE TABLE sneaky_${role} (x int)`)
          .then(() => 'ok', (e: { code?: string }) => e.code ?? 'error');
        expect(result, role).toBe(DENIED);
      }
    } finally {
      await old.drop();
    }
  });
});
