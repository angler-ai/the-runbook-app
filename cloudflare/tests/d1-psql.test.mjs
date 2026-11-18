// CF7: the PILOT_PSQL shim (scripts/d1-psql.mjs) turns the pilot's
// Postgres seed statement into its D1 equivalent: the same guard, payload
// and scopes, and nothing else. The end-to-end insert runs in
// local-origin.test.mjs against a live stack.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { parseArgs, translate } from '../scripts/d1-psql.mjs';
import { root } from './harness.mjs';

const VARS = {
  token_id: 'tok_01JABCDEFGHJKMNPQRSTVWXYZ0',
  org_id: 'org_01JABCDEFGHJKMNPQRSTVWXYZ1',
  user_id: 'usr_01JABCDEFGHJKMNPQRSTVWXYZ2',
  project_id: 'prj_01JABCDEFGHJKMNPQRSTVWXYZ3',
  hash_hex: 'ab'.repeat(32),
};
const seed = () => readFile(join(root, '..', 'examples', 'pilot', 'seed-first-token.sql'), 'utf8');

test('psql -v arguments are read; the seed becomes one guarded D1 insert', async () => {
  const args = ['-X', '-q', '-t', '-A', '-v', 'ON_ERROR_STOP=1'];
  for (const [k, v] of Object.entries(VARS)) args.push('-v', `${k}=${v}`);
  assert.deepEqual(parseArgs(args), { ON_ERROR_STOP: '1', ...VARS });
  const sql = translate(await seed(), VARS, 1_000);
  assert.match(sql, /^INSERT INTO credential \(id, org_id, secret_hash, payload, revoked_at, last_used_at\) SELECT/);
  assert.match(sql, /u\.active = 1/);
  assert.match(sql, /X'(ab){32}'/);
  const payload = JSON.parse(/'(\{.*\})'/.exec(sql)[1]);
  assert.deepEqual(payload.scopes, ['runbooks:read', 'executions:read', 'executions:start', 'executions:act', 'tokens:manage']);
  assert.deepEqual(payload.projects, [VARS.project_id]);
  assert.equal(payload.hash.length, 32);
  assert.equal(payload.expires_at, 1_000 + 90 * 86_400_000);
  assert.equal(payload.management.name, 'first token (operator seed)');
});

test('a widened seed keeps its scopes (the e2e editor and controller seeds)', async () => {
  const widened = (await seed()).replace("'runbooks:read',", "'runbooks:read', 'executions:admin',");
  const payload = JSON.parse(/'(\{.*\})'/.exec(translate(widened, VARS))[1]);
  assert.ok(payload.scopes.includes('executions:admin'));
});

test('anything else, or a malformed value, is refused', async () => {
  assert.throws(() => translate('DROP TABLE credential;', VARS), /only examples/);
  assert.throws(() => translate('SELECT 1', VARS), /only examples/);
  const sql = await seed();
  for (const key of Object.keys(VARS)) {
    assert.throws(() => translate(sql, { ...VARS, [key]: `${VARS[key]}' OR '1'='1` }), /malformed/, key);
    const { [key]: _, ...missing } = VARS;
    assert.throws(() => translate(sql, missing), /missing/, key);
  }
});
