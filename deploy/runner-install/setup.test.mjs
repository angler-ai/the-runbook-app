import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { cp, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { loadConfig, loadInstallId, RUNNER_ROLES } from '../../runner/packages/shared/src/config.ts';

test('setup and upgrade produce valid process environments without rotating credentials', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'runbook-setup-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const config = join(root, 'deploy/runner-config');
  await mkdir(config, { recursive: true });
  await symlink(fileURLToPath(new URL('../../runner', import.meta.url)), join(root, 'runner'), 'dir');
  const installer = join(root, 'deploy/runner-install');
  await cp(new URL('.', import.meta.url), installer, { recursive: true });
  const core = 'https://core.example';
  const runnerUrl = 'https://runner.example';
  const org = 'org_' + '0'.repeat(26);
  const runner = 'rn_' + '1'.repeat(26);
  const install = 'ri_' + '2'.repeat(26);
  await writeFile(join(config, 'registration.json'), JSON.stringify({
    core, runnerUrl, org, group: 'rg_' + '3'.repeat(26), install_id: install, runner_id: runner,
    directory_token: 'd'.repeat(43), claim_token: 'c'.repeat(43), release_token: 'r'.repeat(43),
  }));
  const run = (...extra) => {
    const result = spawnSync(process.execPath, [join(installer, 'setup.mjs'), ...extra, core, runnerUrl, org, '1.0.0'], {
      encoding: 'utf8', env: { PATH: process.env.PATH },
    });
    return result;
  };
  const read = (role) => readFile(join(config, role + '.env'), 'utf8');
  async function validate() {
    for (const role of RUNNER_ROLES) {
      const env = Object.fromEntries((await read(role)).trimEnd().split('\n').map((line) => {
        const index = line.indexOf('=');
        return [line.slice(0, index), line.slice(index + 1)];
      }));
      const parsed = loadConfig(role, env);
      if (role === 'api') assert.equal(parsed.internalListen.port, 8094);
      if (role === 'vault') {
        assert.equal(loadInstallId(env), install);
        assert.deepEqual(parsed.runnerIds, [runner]);
      }
      if (['worker', 'gateway', 'sandbox-ctl'].includes(role)) assert.equal(parsed.urls.api, 'http://runner-api:8094');
    }
  }
  const fresh = run();
  assert.equal(fresh.status, 0, fresh.stderr);
  assert.match(fresh.stdout, /No vault identity enrollment is recorded.*runbook runner vault-identity/);
  await validate();
  const postgres = await read('postgres');
  const kek = await readFile(join(config, 'kek.json'), 'utf8');
  await writeFile(join(config, 'vault.env'), (await read('vault')).replace(/^RUNNER_(INSTALL_ID|ALLOWED_RUNNER_IDS)=.*\n/gm, ''));
  for (const role of ['worker', 'gateway', 'sandbox-ctl']) {
    await writeFile(join(config, role + '.env'), (await read(role)).replace('runner-api:8094', 'runner-api:8090'));
  }
  const upgraded = run('--upgrade');
  assert.equal(upgraded.status, 0, upgraded.stderr);
  assert.match(upgraded.stdout, /No vault identity enrollment is recorded.*runbook runner vault-identity/);
  await validate();
  const migrate = await read('migrate');
  const api = await read('api');
  await writeFile(join(config, 'migrate.env'), migrate.replace(/^RUNNER_DB_API_PASSWORD=.*$/m, 'RUNNER_DB_API_PASSWORD=wrong'));
  const refused = run('--upgrade');
  assert.equal(refused.status, 1);
  assert.match(refused.stderr, /database login configuration/);
  assert.equal(await read('api'), api);
  assert.equal(await read('postgres'), postgres);
  assert.equal(await readFile(join(config, 'kek.json'), 'utf8'), kek);
});

test('fresh enrollment resumes a malformed join and prints its fingerprint', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'runbook-identity-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const installer = join(root, 'deploy/runner-install');
  await cp(new URL('.', import.meta.url), installer, { recursive: true });
  await symlink(fileURLToPath(new URL('../../runner', import.meta.url)), join(root, 'runner'), 'dir');
  const capture = join(root, 'enrolled.json');
  const preload = join(root, 'fake-core.mjs');
  await writeFile(preload, `
    import { writeFile } from 'node:fs/promises';
    globalThis.fetch = async (url, init) => {
      if (url.endsWith('/meta')) return Response.json({ runner_contract_version: 4 });
      if (url.endsWith('/register')) {
        await writeFile(${JSON.stringify(capture)}, init.body, { flag: 'wx' });
        return Response.json({ install_id: 'ri_' + '2'.repeat(26), directory_token: 'd'.repeat(43),
          release_token: 'r'.repeat(43), join_token: 'j'.repeat(43) });
      }
      if (url.endsWith('/runners')) {
        try {
          await writeFile(${JSON.stringify(join(root, 'join-attempt'))}, '', { flag: 'wx' });
          return new Response('private-join-bytes');
        } catch (error) {
          if (error.code !== 'EEXIST') throw error;
          return Response.json({ runner_id: 'rn_' + '1'.repeat(26), claim_token: 'c'.repeat(43) });
        }
      }
      throw new Error('unexpected request');
    };
  `);
  const run = (input) => spawnSync(process.execPath, ['--import', preload, join(installer, 'setup.mjs'),
    'https://core.example', 'https://runner.example', 'org_' + '0'.repeat(26), '1.0.0'], {
    input, encoding: 'utf8', env: { PATH: process.env.PATH },
  });
  const failedJoin = run('trb_rbs_' + 't'.repeat(43));
  assert.equal(failedJoin.status, 1);
  assert.match(failedJoin.stderr, /Invalid core runner join reply/);
  assert.doesNotMatch(failedJoin.stderr, /private-join/);
  const result = run('');
  assert.equal(result.status, 0, result.stderr);
  const { vault_identity_key: key } = JSON.parse(await readFile(capture, 'utf8'));
  assert.match(key ?? '', /^[A-Za-z0-9_-]{43}$/);
  const { createHash } = await import('node:crypto');
  assert.ok(result.stdout.includes(createHash('sha256').update(Buffer.from(key, 'base64url')).digest('hex')));
});

test('installer JSON errors redact input and explain how to recover an orphaned registration', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'runbook-recovery-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const installer = join(root, 'deploy/runner-install');
  await cp(new URL('.', import.meta.url), installer, { recursive: true });
  await symlink(fileURLToPath(new URL('../../runner', import.meta.url)), join(root, 'runner'), 'dir');
  const preload = join(root, 'fake-core.mjs');
  await writeFile(preload, `
    globalThis.fetch = async (url) => url.endsWith('/meta')
      ? Response.json({ runner_contract_version: 4 })
      : new Response('trb_rjn_SECRET-fixture');
  `);
  const run = () => spawnSync(process.execPath, ['--import', preload, join(installer, 'setup.mjs'),
    'https://core.example', 'https://runner.example', 'org_' + '0'.repeat(26), '1.0.0'], {
    input: 'trb_rbs_' + 't'.repeat(43), encoding: 'utf8', env: { PATH: process.env.PATH },
  });
  const malformed = run();
  assert.equal(malformed.status, 1);
  assert.doesNotMatch(malformed.stderr, /trb_rjn/);
  assert.match(malformed.stderr, /runbook runner status.*remove the orphaned registration.*runbook runner bootstrap/);
  await writeFile(join(root, 'deploy/runner-config/registration.json'), 'trb_rjn_SECRET-fixture');
  const corrupt = run();
  assert.equal(corrupt.status, 1);
  assert.doesNotMatch(corrupt.stderr, /trb_rjn/);
  assert.match(corrupt.stderr, /Invalid registration.json/);
});
