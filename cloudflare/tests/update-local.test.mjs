// UPD1: cloudflare/scripts/update.mjs from release N to N+1 against the local
// stack (`wrangler dev`, Miniflare's D1, R2 and Durable Objects). LOCAL ONLY.
//
// Release N is the test build reporting an older release
// (RUNBOOK_TEST_RELEASE, a test seam); N+1 is the same build reporting this
// checkout's release, with an extra additive D1 migration. The update runs
// with its real D1 steps against the local database (`--remote` becomes
// `--local --persist-to`): reading both migration histories, recording the
// files the Worker's runner applied in wrangler's table, and `wrangler d1
// migrations apply`, which must apply only the new file. The calls that need
// an account are mocked: `whoami`, `deployments status`, `d1 time-travel
// info`, and `deploy` / `rollback`, which restart the local stack as N+1 / N.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { cp, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { root, startStack } from '../scripts/dev-stack.mjs';
import { EXIT, cargoVersion, main, realDeps, wranglerConfigReader } from '../scripts/update.mjs';

const SYNTHETIC = '1999_upd1_synthetic.sql';

function older(version) {
  const [a, b, c] = version.split('.').map(Number);
  if (c > 0) return `${a}.${b}.${c - 1}`;
  if (b > 0) return `${a}.${b - 1}.999`;
  return `${a - 1}.999.999`;
}
const rowsOf = (stdout) => JSON.parse(stdout).flatMap((r) => r.results ?? []);

test('update.mjs takes a local deployment from N to N+1 with an additive D1 migration, and rolls back a failed one', async () => {
  const target = cargoVersion(await readFile(join(root, '..', 'Cargo.toml'), 'utf8'));
  const n = older(target);
  const state = await mkdtemp(join(tmpdir(), 'runbook-upd1-local-'));
  let stack = await startStack({ state, auth: false, vars: { RUNBOOK_TEST_RELEASE: n } });
  const restart = async (release) => {
    await stack.stop();
    stack = await startStack({ state, port: stack.port, auth: false, vars: release ? { RUNBOOK_TEST_RELEASE: release } : {} });
  };
  try {
    // Release N with data: the instance bootstrapped (the Worker's runner
    // migrates D1 on first use).
    const boot = await fetch(`${stack.origin}/internal/bootstrap`, {
      method: 'POST',
      headers: { 'x-bootstrap-secret': stack.bootstrapSecret, 'content-type': 'application/json' },
      body: JSON.stringify({ organization: 'Update test', user_name: 'Owner', project_slug: 'default' }),
    });
    assert.equal(boot.status, 201, await boot.clone().text());
    const { personal_token: pat } = await boot.json();
    const me = async () => (await fetch(`${stack.origin}/v1/me`, { headers: { authorization: `Bearer ${pat}` } })).status;
    assert.equal(await me(), 200);
    assert.equal((await (await fetch(`${stack.origin}/v1/meta`)).json()).version, n);

    // N+1's D1 migrations: this checkout's plus one additive file.
    const migrations = join(state, 'migrations-next');
    await cp(join(root, 'migrations', 'd1'), migrations, { recursive: true });
    await writeFile(join(migrations, SYNTHETIC), '-- UPD1 local test: additive.\nCREATE TABLE upd1_synthetic (id TEXT PRIMARY KEY);\n');
    const config = JSON.parse(await readFile(stack.files.api, 'utf8'));
    config.d1_databases[0].migrations_dir = migrations;
    const configPath = join(state, 'update-api.json');
    await writeFile(configPath, JSON.stringify(config, null, 2));

    const real = realDeps(await wranglerConfigReader());
    const calls = [];
    let deployWorks = true;
    const deps = {
      ...real,
      isTTY: false,
      log: (m) => calls.push(`log ${m}`),
      run: (cmd) => {
        calls.push(`run ${cmd.slice(1).join(' ')}`); // npm test built build-test already
        return 0;
      },
      wrangler: async (args, options = {}) => {
        calls.push(args.filter((a) => !a.startsWith('/')).join(' '));
        const ok = (stdout = '{}') => ({ status: 0, stdout, stderr: '' });
        if (args[0] === 'whoami') return ok();
        if (args[0] === 'deployments') return ok(JSON.stringify({ versions: [{ version_id: 'api-n', percentage: 100 }] }));
        if (args[0] === 'd1' && args[1] === 'time-travel') return ok(JSON.stringify({ bookmark: 'local-bookmark' }));
        if (args[0] === 'deploy') {
          await restart(deployWorks ? null : n);
          return ok();
        }
        if (args[0] === 'rollback') {
          assert.equal(args[1], 'api-n');
          await restart(n);
          return ok();
        }
        assert.equal(args[0], 'd1');
        const local = args.flatMap((a) => (a === '--remote' ? ['--local', '--persist-to', stack.persist] : [a]));
        return real.wrangler(local, { ...options, capture: true });
      },
    };
    const update = () =>
      main(['--source', 'checkout', '--config', configPath, '--no-auth', '--url', stack.origin, '--yes', '--verify-timeout', '60', '--state-dir', join(state, 'update')], deps);

    assert.equal(await update(), EXIT.OK, calls.join('\n'));
    const meta = await (await fetch(`${stack.origin}/v1/meta`)).json();
    assert.equal(meta.version, target);
    assert.ok(meta.schema.d1 >= 1014 && meta.schema.execution_do >= 5, JSON.stringify(meta.schema));
    // wrangler applied only the new file; the runner's files were recorded.
    const files = (await readdir(migrations)).filter((f) => f.endsWith('.sql')).sort();
    const recorded = rowsOf(stack.d1('SELECT name FROM d1_migrations ORDER BY name')).map((r) => r.name);
    assert.deepEqual(recorded, files);
    assert.equal(rowsOf(stack.d1("SELECT count(*) AS n FROM sqlite_master WHERE name = 'upd1_synthetic'"))[0].n, 1);
    assert.ok(calls.some((c) => c.startsWith('d1 execute DB --remote --command CREATE TABLE IF NOT EXISTS d1_migrations')));
    assert.ok(calls.some((c) => c.includes(`1 pending (${SYNTHETIC})`)), calls.join('\n'));
    // The data survives, and the Worker serves on the newer schema.
    assert.equal(await me(), 200);

    // A failed update (the new release never answers) rolls back.
    await restart(n);
    deployWorks = false;
    calls.length = 0;
    assert.equal(await update(), EXIT.ROLLED_BACK, calls.join('\n'));
    assert.ok(calls.includes('rollback api-n -c --message update.mjs: ' + target + ' failed --yes'), calls.join('\n'));
    assert.ok(calls.some((c) => c.includes('0 pending')), 'nothing left to migrate');
    assert.equal((await (await fetch(`${stack.origin}/v1/meta`)).json()).version, n);
    assert.equal(await me(), 200);
  } finally {
    await stack.stop();
    await rm(state, { recursive: true, force: true });
  }
});
