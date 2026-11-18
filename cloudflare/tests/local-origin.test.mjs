// CF7: the local multi-Worker `wrangler dev` (scripts/dev-stack.mjs) as
// one origin, routed like a deployed site: the web UI at `/` and client
// routes, `/v1`, `/.well-known`, `/api/auth` (the auth Worker behind the
// api Worker's AUTH binding) and `/mcp`; the user-content origin (another
// hostname on the same port) serves only `/u/*`. The web UI here is a small
// fixture, so `npm test` needs no web build. LOCAL ONLY.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { contentSecurityPolicy, startStack } from '../scripts/dev-stack.mjs';

let stack;
let fixture;
let csp;
before(async () => {
  fixture = await mkdtemp(join(tmpdir(), 'runbook-web-fixture-'));
  await mkdir(join(fixture, 'assets'));
  await mkdir(join(fixture, 'icons'));
  await writeFile(join(fixture, 'index.html'), '<!doctype html><title>Runbook fixture</title><script src="/assets/app-abc123.js"></script>');
  await writeFile(join(fixture, 'assets', 'app-abc123.js'), 'console.log("app");');
  await writeFile(join(fixture, 'icons', 'icon.svg'), '<svg xmlns="http://www.w3.org/2000/svg"/>');
  csp = await contentSecurityPolicy();
  stack = await startStack({ assets: fixture });
});
after(async () => {
  await stack?.stop();
  if (fixture) await rm(fixture, { recursive: true, force: true });
});

const get = (url, init) => fetch(url, { redirect: 'manual', ...init });

test('the web UI: index and client routes are no-store HTML with the CSP', async () => {
  for (const path of ['/', '/index.html', '/runs/exe_01ABCDEFGHJKMNPQRSTVWXYZ00', '/icons/', '/v1x']) {
    const response = await get(`${stack.origin}${path}`);
    assert.equal(response.status, 200, path);
    assert.match(await response.text(), /Runbook fixture/, path);
    assert.equal(response.headers.get('cache-control'), 'no-store', path);
    assert.equal(response.headers.get('content-security-policy'), csp, path);
    assert.equal(response.headers.get('referrer-policy'), 'no-referrer', path);
    assert.equal(response.headers.get('x-content-type-options'), 'nosniff', path);
  }
});

test('hashed bundles are immutable; a missing one is 404, never HTML', async () => {
  const bundle = await get(`${stack.origin}/assets/app-abc123.js`);
  assert.equal(bundle.status, 200);
  assert.equal(bundle.headers.get('cache-control'), 'public, max-age=31536000, immutable');
  assert.equal(bundle.headers.get('content-security-policy'), csp);
  const missing = await get(`${stack.origin}/assets/gone-000.js`);
  assert.equal(missing.status, 404);
  assert.doesNotMatch(await missing.text(), /Runbook fixture/);
  const icon = await get(`${stack.origin}/icons/icon.svg`);
  assert.equal(icon.status, 200);
  assert.equal(icon.headers.get('cache-control'), 'no-cache');
});

test('/v1, /.well-known, /api/auth and /mcp reach their Workers', async () => {
  const meta = await get(`${stack.origin}/v1/meta`);
  assert.equal(meta.status, 200);
  assert.equal(meta.headers.get('content-type'), 'application/json');
  const body = await meta.json();
  assert.equal(body.capabilities.uploads, true, JSON.stringify(body));
  const discovery = await (await get(`${stack.origin}/.well-known/runbook`)).json();
  assert.equal(discovery.api_base_url, `${stack.origin}/v1`);
  assert.ok(JSON.stringify(discovery).includes('"email"'), JSON.stringify(discovery));
  const auth = await get(`${stack.origin}/api/auth/ok`);
  assert.equal(auth.status, 200);
  assert.deepEqual(await auth.json(), { ok: true });
  const mcp = await get(`${stack.origin}/mcp`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' });
  assert.equal(mcp.status, 401);
  const unknown = await get(`${stack.origin}/v1/nope`);
  assert.equal(unknown.status, 404);
  assert.equal((await unknown.json()).code, 'resource.not_found');
});

test('the user-content origin serves only /u/*, never the app', async () => {
  assert.notEqual(new URL(stack.usercontent).origin, new URL(stack.origin).origin);
  for (const path of ['/', '/runs/x', '/v1/meta', '/api/auth/ok']) {
    const response = await get(`${stack.usercontent}${path}`);
    assert.equal(response.status, 404, path);
    assert.equal(response.headers.get('content-security-policy'), "sandbox; default-src 'none'; frame-ancestors 'none'", path);
  }
  const unsigned = await get(`${stack.usercontent}/u/exe_x/evd_y`);
  assert.equal(unsigned.status, 403);
});

test('the PILOT_PSQL shim seeds a first token into the running stack\'s D1', async () => {
  const boot = await get(`${stack.origin}/internal/bootstrap`, {
    method: 'POST',
    headers: { 'x-bootstrap-secret': stack.bootstrapSecret, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Pilot', user_name: 'Owner', project_slug: 'pilot' }),
  });
  assert.equal(boot.status, 201, await boot.clone().text());
  const { org_id, user_id, project_id } = await boot.json();
  const secret = `trb_pat_${randomBytes(32).toString('base64url')}`;
  const vars = {
    token_id: 'tok_01JABCDEFGHJKMNPQRSTVWXYZ9',
    org_id,
    user_id,
    project_id,
    hash_hex: createHash('sha256').update(secret).digest('hex'),
  };
  const args = [new URL('../scripts/d1-psql.mjs', import.meta.url).pathname, stack.state, '-X', '-q', '-t', '-A'];
  for (const [k, v] of Object.entries(vars)) args.push('-v', `${k}=${v}`);
  const sql = await readFile(new URL('../../examples/pilot/seed-first-token.sql', import.meta.url), 'utf8');
  const out = spawnSync(process.execPath, args, { input: sql, encoding: 'utf8' });
  assert.equal(out.status, 0, out.stderr);
  assert.equal(out.stdout.trim(), vars.token_id);
  const me = await get(`${stack.origin}/v1/me`, { headers: { authorization: `Bearer ${secret}` } });
  assert.equal(me.status, 200, await me.clone().text());
  const again = spawnSync(process.execPath, args.map((a) => a.replace(user_id, 'usr_01JABCDEFGHJKMNPQRSTVWXYZ8')), { input: sql, encoding: 'utf8' });
  assert.equal(again.status, 0, again.stderr);
  assert.equal(again.stdout.trim(), '', 'no seed for a user who is not a member');
});
