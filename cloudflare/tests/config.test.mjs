// CF1 guardrails: the Workers config here is local-only and the toolchain
// is pinned exactly to the Shelf app's versions.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { experimental_readRawConfig } from 'wrangler';
import { root } from './harness.mjs';

const PINS = {
  wrangler: '4.135.0',
  miniflare: '5.20260918.0-alpha',
  workerd: '1.20260918.1',
  esbuild: '0.28.2',
};

test('wrangler.jsonc is local-only: header, placeholders, no routes or workers.dev', async () => {
  const text = await readFile(join(root, 'wrangler.jsonc'), 'utf8');
  assert.match(text.split('\n')[0], /^\/\/ LOCAL ONLY\. NEVER DEPLOY FROM HERE\./);
  const { rawConfig: config } = experimental_readRawConfig({ config: join(root, 'wrangler.jsonc') });
  assert.equal(config.workers_dev, false);
  assert.equal(config.preview_urls, false);
  for (const key of ['route', 'routes', 'account_id', 'env']) assert.equal(config[key], undefined, key);
  for (const db of config.d1_databases) assert.match(db.database_id, /^0{8}-0{4}-0{4}-0{4}-0{12}$/);
  assert.deepEqual(config.migrations, [
    { tag: 'v1', new_sqlite_classes: ['ExecutionDO'] },
    // CF6: one OrgDO per organization (quota counters).
    { tag: 'v2', new_sqlite_classes: ['OrgDO'] },
    // CF11: one WebhookDO per webhook subscription (its delivery log).
    { tag: 'v3', new_sqlite_classes: ['WebhookDO'] },
    // Gate fix B2: one LimitsDO per rate-limit key (AUTH3 windows).
    { tag: 'v4', new_sqlite_classes: ['LimitsDO'] },
  ]);
  assert.deepEqual(config.durable_objects.bindings, [
    { name: 'EXECUTIONS', class_name: 'ExecutionDO' },
    { name: 'ORGS', class_name: 'OrgDO' },
    { name: 'WEBHOOKS', class_name: 'WebhookDO' },
    { name: 'LIMITS', class_name: 'LimitsDO' },
  ]);
  // CF4: two local buckets (blobs, user content), the local notification
  // queue, and the two crons.
  assert.deepEqual(config.triggers, { crons: ['* * * * *', '17 0 * * *'] });
  assert.deepEqual(config.r2_buckets.map((b) => b.binding), ['BLOBS', 'USER_CONTENT']);
  for (const bucket of config.r2_buckets) assert.match(bucket.bucket_name, /^runbook-local-/);
  assert.deepEqual(config.queues.producers, [{ binding: 'NOTIFICATIONS', queue: 'runbook-local-notifications' }]);
  for (const consumer of config.queues.consumers) {
    assert.match(consumer.queue, /^runbook-local-/);
    assert.match(consumer.dead_letter_queue, /^runbook-local-/);
  }
});

test('wrangler.conformance.jsonc is local-only too: test Worker, its DO, two local buckets and a local D1', async () => {
  const text = await readFile(join(root, 'wrangler.conformance.jsonc'), 'utf8');
  assert.match(text.split('\n')[0], /^\/\/ LOCAL ONLY\. NEVER DEPLOY FROM HERE\./);
  const { rawConfig: config } = experimental_readRawConfig({ config: join(root, 'wrangler.conformance.jsonc') });
  assert.equal(config.workers_dev, false);
  assert.equal(config.preview_urls, false);
  for (const key of ['route', 'routes', 'account_id', 'env', 'queues']) {
    assert.equal(config[key], undefined, key);
  }
  assert.deepEqual(config.durable_objects.bindings, [{ name: 'CASES', class_name: 'ConformanceDO' }]);
  // CF3: the D1 cases run on a local D1 with a placeholder ID.
  assert.deepEqual(config.d1_databases.map((d) => d.binding), ['DB']);
  assert.match(config.d1_databases[0].database_id, /^0{8}-/);
  assert.deepEqual(
    config.r2_buckets.map((b) => b.binding),
    ['BLOBS', 'USER_CONTENT'],
  );
  for (const bucket of config.r2_buckets) assert.match(bucket.bucket_name, /^runbook-local-/);
});

test('wrangler, miniflare, workerd and esbuild are pinned exactly and installed at those versions', async () => {
  const pkg = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
  assert.deepEqual(pkg.devDependencies, PINS);
  assert.equal(pkg.dependencies, undefined);
  assert.ok(!JSON.stringify(pkg.scripts).match(/deploy|login|--remote/), 'no deploy, login or remote scripts');
  const require = createRequire(join(root, 'package.json'));
  for (const [name, version] of Object.entries(PINS)) {
    assert.equal(require(`${name}/package.json`).version, version, name);
  }
});

test('CF7: the dev stack configs are local-only, and the dev api Worker matches wrangler.jsonc', async () => {
  const read = (name) => experimental_readRawConfig({ config: join(root, name) }).rawConfig;
  for (const name of ['wrangler.dev.jsonc', 'wrangler.auth.jsonc', 'wrangler.mail.jsonc']) {
    const text = await readFile(join(root, name), 'utf8');
    assert.match(text.split('\n')[0], /^\/\/ LOCAL ONLY\. NEVER DEPLOY FROM HERE\./, name);
    const config = read(name);
    assert.equal(config.workers_dev, false, name);
    assert.equal(config.preview_urls, false, name);
    for (const key of ['route', 'routes', 'account_id', 'env']) assert.equal(config[key], undefined, `${name} ${key}`);
    for (const db of config.d1_databases ?? []) assert.match(db.database_id, /^0{8}-0{4}-0{4}-0{4}-0{12}$/, name);
  }
  const test = read('wrangler.jsonc');
  const dev = read('wrangler.dev.jsonc');
  for (const key of ['name', 'compatibility_date', 'durable_objects', 'migrations', 'd1_databases', 'r2_buckets', 'triggers', 'queues']) {
    assert.deepEqual(dev[key], test[key], key);
  }
  assert.deepEqual(dev.services, [{ binding: 'AUTH', service: 'runbook-auth-local' }]);
  assert.equal(dev.assets.binding, 'ASSETS');
  assert.deepEqual(dev.assets.run_worker_first, ['/*', '!/assets/*', '!/manifest.webmanifest']);
  const auth = read('wrangler.auth.jsonc');
  assert.equal(auth.d1_databases[0].database_id, test.d1_databases[0].database_id, 'the auth Worker shares the D1');
  assert.deepEqual(auth.services.map((s) => s.service), [test.name, 'runbook-mail-local']);
  assert.equal(read('wrangler.mail.jsonc').name, 'runbook-mail-local');
});

function parseJsonc(text) {
  let insideString = false;
  let stringChar = null;
  let escape = false;
  let insideLineComment = false;
  let insideBlockComment = false;
  let result = '';

  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    const next = text[i + 1];

    if (insideLineComment) {
      if (ch === '\n') {
        insideLineComment = false;
        result += ch;
      }
      continue;
    }
    if (insideBlockComment) {
      if (ch === '*' && next === '/') {
        insideBlockComment = false;
        i++;
      }
      continue;
    }
    if (insideString) {
      result += ch;
      if (escape) {
        escape = false;
      } else if (ch === '\\') {
        escape = true;
      } else if (ch === stringChar) {
        insideString = false;
      }
      continue;
    }
    if (ch === '"' || ch === "'") {
      insideString = true;
      stringChar = ch;
      result += ch;
      continue;
    }
    if (ch === '/' && next === '/') {
      insideLineComment = true;
      i++;
      continue;
    }
    if (ch === '/' && next === '*') {
      insideBlockComment = true;
      i++;
      continue;
    }
    result += ch;
  }
  const cleaned = result.replace(/,(\s*[}\]])/g, '$1');
  return JSON.parse(cleaned);
}

test('gate fix B5: the example deploys from an untracked, ignored file, never the local wrangler.jsonc', async () => {
  const { execFileSync } = await import('node:child_process');
  const example = await readFile(join(root, 'wrangler.jsonc.example'), 'utf8');
  const header = example.split('\n').slice(0, 6).join('\n');
  assert.match(header, /Copy to wrangler\.prod\.jsonc/);
  assert.doesNotMatch(header, /Copy to wrangler\.jsonc\b/);
  const ignored = (path) => {
    try {
      execFileSync('git', ['check-ignore', '-q', path], { cwd: root });
      return true;
    } catch {
      return false;
    }
  };
  for (const path of ['wrangler.prod.jsonc', 'wrangler.auth.prod.jsonc', '.dev.vars']) assert.ok(ignored(path), `${path} is git-ignored`);
  assert.equal(ignored('wrangler.jsonc'), false, 'the local config stays tracked');
  const devVars = await readFile(join(root, '.dev.vars.example'), 'utf8');
  assert.doesNotMatch(devVars, /^#?\s*WEBHOOKS_ALLOW_PRIVATE=/m, 'no test seam to copy');
});

test('the sign-in Worker example: Cloudflare Email Service through EMAIL, placeholders only', async () => {
  const example = parseJsonc(await readFile(join(root, 'wrangler.auth.jsonc.example'), 'utf8'));
  assert.deepEqual(example.send_email?.map((b) => b.name), ['EMAIL']);
  assert.equal(example.vars.RUNBOOK_AUTH_EMAIL_TRANSPORT, 'cloudflare');
  assert.ok(example.send_email[0].allowed_sender_addresses.every((a) => a.endsWith('@runbooks.example.com')));
  assert.match(example.vars.RUNBOOK_EMAIL_FROM, /<[^>]+@runbooks\.example\.com>$/);
  for (const db of example.d1_databases) assert.match(db.database_id, /^0{8}-0{4}-0{4}-0{4}-0{12}$/);
  assert.equal(example.account_id, undefined);
  assert.equal(example.d1_databases[0].migrations_dir, '../auth/drizzle/sqlite');
});

test('CF10: wrangler.jsonc.example matches wrangler.jsonc binding names exactly with placeholders only', async () => {
  const exampleText = await readFile(join(root, 'wrangler.jsonc.example'), 'utf8');
  const realText = await readFile(join(root, 'wrangler.jsonc'), 'utf8');
  const example = parseJsonc(exampleText);
  const real = parseJsonc(realText);

  // Compare binding names ignoring IDs, buckets and queue resource names
  assert.deepEqual(
    example.durable_objects?.bindings?.map((b) => b.name).sort(),
    real.durable_objects?.bindings?.map((b) => b.name).sort(),
    'durable object binding names match',
  );
  assert.deepEqual(
    example.d1_databases?.map((d) => d.binding).sort(),
    real.d1_databases?.map((d) => d.binding).sort(),
    'D1 database binding names match',
  );
  assert.deepEqual(
    example.r2_buckets?.map((b) => b.binding).sort(),
    real.r2_buckets?.map((b) => b.binding).sort(),
    'R2 bucket binding names match',
  );
  assert.deepEqual(
    example.queues?.producers?.map((q) => q.binding).sort(),
    real.queues?.producers?.map((q) => q.binding).sort(),
    'Queue producer binding names match',
  );

  // Placeholders only: no real account ID or database IDs
  assert.equal(example.account_id, undefined, 'no real account_id in example');
  for (const db of example.d1_databases ?? []) {
    assert.match(db.database_id, /^0{8}-0{4}-0{4}-0{4}-0{12}$/, 'database_id is placeholder');
  }
});

test('UI-1: wrangler.jsonc.example documents the theme vars, commented out', async () => {
  const text = await readFile(join(root, 'wrangler.jsonc.example'), 'utf8');
  for (const name of [
    'RUNBOOK_THEME_ACCENT',
    'RUNBOOK_THEME_ACCENT_SECONDARY',
    'RUNBOOK_THEME_DARK_ACCENT',
    'RUNBOOK_THEME_DARK_ACCENT_SECONDARY',
  ]) {
    assert.match(text, new RegExp(`^\\s*// "${name}": "#[0-9a-f]{6}",?$`, 'm'), name);
    assert.equal(parseJsonc(text).vars[name], undefined, `${name} is not set by default`);
  }
  assert.match(text, /config\.invalid_theme_color/);
  assert.match(text, /ignored/, 'an invalid colour is ignored on Workers');
});
