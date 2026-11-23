import { randomBytes } from 'node:crypto';
import pg from 'pg';
import { describe, expect, it, vi } from 'vitest';
import { installRoleNames, migrate, openPool } from '@runbook/runner-shared/db';
import { TEST_DATABASE_URL } from '../../shared/test-utils/pg.ts';

describe.skipIf(!TEST_DATABASE_URL)('GKE database role separation', () => {
  it('isolates API, scheduler, and vault effective roles and denies runner logins access to core', async () => {
    const prefix = 'gke_' + randomBytes(6).toString('hex');
    const names = installRoleNames(prefix);
    const login = `${prefix}_login`;
    const vaultLogin = `${prefix}_vault_login`;
    const bootstrap = `${prefix}_bootstrap`;
    const runner = `${prefix}_runner`;
    const core = `${prefix}_core`;
    const password = 'test-only-password';
    const admin = new pg.Client({ connectionString: TEST_DATABASE_URL });
    const url = (database: string, user: string) => {
      const result = new URL(TEST_DATABASE_URL!);
      result.pathname = '/' + database;
      result.username = user;
      result.password = password;
      return result.toString();
    };
    const pools: pg.Pool[] = [];
    await admin.connect();
    try {
      await admin.query(`CREATE ROLE ${login} LOGIN PASSWORD '${password}'`);
      await admin.query(`CREATE ROLE ${vaultLogin} LOGIN PASSWORD '${password}'`);
      await admin.query(`CREATE ROLE ${bootstrap} LOGIN CREATEROLE PASSWORD '${password}'`);
      await admin.query(`CREATE DATABASE ${runner} OWNER ${bootstrap}`);
      await admin.query(`CREATE DATABASE ${core}`);
      await admin.query(`REVOKE CONNECT ON DATABASE ${core} FROM PUBLIC`);
      const owner = new pg.Client({ connectionString: url(runner, bootstrap) });
      await owner.connect();
      try {
        await owner.query(`REVOKE CONNECT ON DATABASE ${runner} FROM PUBLIC`);
        expect(await migrate(url(runner, bootstrap), undefined, prefix)).toContain('0001_init');
        expect(await migrate(url(runner, bootstrap), undefined, prefix)).toEqual([]);
        await owner.query(`GRANT CONNECT ON DATABASE ${runner} TO ${login}, ${vaultLogin}`);
        await owner.query(`GRANT ${names.api}, ${names.scheduler} TO ${login} WITH INHERIT FALSE`);
        await owner.query(`GRANT ${names.vault} TO ${vaultLogin} WITH INHERIT FALSE`);
      } finally {
        await owner.end();
      }
      vi.stubEnv('RUNNER_DB_ROLE_PREFIX', prefix);
      const api = openPool('api', url(runner, login));
      const scheduler = openPool('scheduler', url(runner, login));
      const vault = openPool('vault', url(runner, vaultLogin));
      pools.push(api, scheduler, vault);
      expect((await api.query('SELECT current_user AS role')).rows[0]?.role).toBe(names.api);
      expect((await scheduler.query('SELECT current_user AS role')).rows[0]?.role).toBe(names.scheduler);
      await expect(api.query('SELECT * FROM credential_secret_versions')).rejects.toMatchObject({ code: '42501' });
      await expect(scheduler.query('SELECT * FROM credential_secret_versions')).rejects.toMatchObject({ code: '42501' });
      await expect(scheduler.query('UPDATE agents SET name = name')).rejects.toMatchObject({ code: '42501' });
      await expect(vault.query('SELECT * FROM credential_secret_versions')).resolves.toMatchObject({ rowCount: 0 });
      for (const identity of [login, vaultLogin]) {
        const client = new pg.Client({ connectionString: url(core, identity) });
        try {
          await expect(client.connect()).rejects.toMatchObject({ code: '42501' });
        } finally {
          await client.end().catch(() => undefined);
        }
      }
    } finally {
      vi.unstubAllEnvs();
      await Promise.all(pools.map((pool) => pool.end()));
      await admin.query(`DROP DATABASE IF EXISTS ${runner} WITH (FORCE)`);
      await admin.query(`DROP DATABASE IF EXISTS ${core} WITH (FORCE)`);
      await admin.query(`DROP ROLE IF EXISTS ${login}, ${vaultLogin}, ${Object.values(names).join(', ')}, ${bootstrap}`);
      await admin.end();
    }
  });
});
