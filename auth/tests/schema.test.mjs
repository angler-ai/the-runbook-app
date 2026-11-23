import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const read = (dir, file) => JSON.parse(readFileSync(new URL(`../drizzle/${dir}/meta/${file}`, import.meta.url), 'utf8'));
const expected = {
  user: ['two_factor_enabled', 'mfa_generation'],
  session: ['mfa_verified_at', 'mfa_method', 'mfa_generation'],
  two_factor: ['id', 'secret', 'backup_codes', 'user_id', 'verified', 'failed_verification_count', 'locked_until'],
  passkey: ['id', 'name', 'public_key', 'user_id', 'credential_id', 'counter', 'device_type', 'backed_up', 'transports', 'created_at', 'last_used_at', 'aaguid', 'mfa_generation'],
  mfa_audit: ['id', 'user_id', 'action', 'actor', 'occurred_at', 'notification', 'notify_attempted_at', 'notified_at'],
  oauth_access_token: ['mfa_verified_at', 'mfa_method'],
  oauth_refresh_token: ['mfa_verified_at', 'mfa_method'],
};

for (const [dir, prefix] of [['pg', 'auth.'], ['sqlite', 'auth_']]) {
  test(`${dir}: both factors, verified session and OAuth timestamps, and MFA audit data are migrated`, () => {
    const { entries } = read(dir, '_journal.json');
    const { tables } = read(dir, `${String(entries.at(-1).idx).padStart(4, '0')}_snapshot.json`);
    for (const [table, columns] of Object.entries(expected)) {
      const actual = tables[`${prefix}${table}`]?.columns;
      assert.ok(actual, `${table} is present`);
      for (const column of columns) assert.ok(actual[column], `${table}.${column} is present`);
    }
    assert.equal(tables[`${prefix}session`].columns.mfa_verified_at.notNull, false);
    assert.equal(tables[`${prefix}session`].columns.mfa_method.notNull, false);
    for (const table of ['two_factor', 'passkey']) {
      const fk = Object.values(tables[`${prefix}${table}`].foreignKeys).find((key) => key.columnsFrom[0] === 'user_id');
      assert.equal(fk?.onDelete, 'cascade');
    }
  });
}
