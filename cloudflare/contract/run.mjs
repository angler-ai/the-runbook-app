// CF9: the edition contract suite (PRD §16 item 5). One scripted pass
// (scenario.mjs) runs against the native server on Postgres and against the
// local Workers stack; then:
// 1. coverage: every public route in CF0's manifest
//    (crates/runbook-api/src/routes.rs, equal to api/openapi.draft.json by
//    its own test) was called on both builds;
// 2. parity: every call answered with the same status and the same
//    normalized body (normalize.mjs), apart from the recorded divergences
//    (allowlist.mjs), each of which must still occur;
// 3. editions: both builds are the open source edition: `/v1/meta` and
//    discovery report `self_hosted`, and every cloud-only route answers 404
//    and is absent from the manifest and the contract.
//
//   DATABASE_URL=postgres://… node contract/run.mjs     (from cloudflare/)
//
// Needs the built api Worker (`npm run build`) and `runbook-server`
// (`cargo build -p runbook-server`); creates and drops one
// `rb_test_contract_*` database, so run it under the pg lock. LOCAL ONLY.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { join } from 'node:path';
import { root } from '../scripts/dev-stack.mjs';
import { ALLOWED } from './allowlist.mjs';
import { normalize } from './normalize.mjs';
import { scenario } from './scenario.mjs';
import { startNative, startWorkers } from './targets.mjs';

const repo = join(root, '..');
const log = (m) => process.stdout.write(`[contract] ${m}\n`);

/** Routes the private cloud build registers (PRD §16 matrix). The open
 * source builds must not know them. */
export const CLOUD_ONLY = [
  ['POST', '/v1/orgs'],
  ['POST', '/v1/orgs/{org}/transfer'],
  ['GET', '/v1/orgs/{org}/billing'],
  ['POST', '/v1/orgs/{org}/billing/checkout'],
  ['GET', '/v1/orgs/{org}/usage'],
  ['GET', '/v1/orgs/{org}/audit-log'],
  ['GET', '/v1/orgs/{org}/audit-log/export'],
  ['GET', '/v1/orgs/{org}/roles'],
  ['POST', '/v1/executions/{exe}/public-links'],
  ['GET', '/v1/public/reports/{link}'],
  ['POST', '/v1/github/webhooks'],
  ['GET', '/v1/projects/{p}/github'],
  ['POST', '/v1/gallery/templates'],
  ['GET', '/v1/gallery/templates'],
  ['POST', '/v1/projects/{p}/runners'],
];

/** CF0's public routes, read from the manifest source. */
async function manifest() {
  const source = await readFile(join(repo, 'crates', 'runbook-api', 'src', 'routes.rs'), 'utf8');
  const routes = new Set();
  for (const m of source.matchAll(/(?:api|host)\(\s*"(\w+)",\s*"([^"]+)",\s*(?:Area|Host)::\w+,\s*Kind::Public\s*,\s*Mfa::\w+\("[^"]+"\)\s*,?\s*\)/g)) {
    routes.add(`${m[1]} ${m[2]}`);
  }
  assert.ok(routes.size > 80, `only ${routes.size} public routes parsed from routes.rs`);
  return routes;
}

function fill(template, params) {
  return template.replace(/\{(\*?)(\w+)\}/g, (_, wildcard, name) => {
    if (params[name] === undefined) throw new Error(`no ${name} for ${template}`);
    return wildcard ? params[name].split('/').map(encodeURIComponent).join('/') : encodeURIComponent(params[name]);
  });
}

/** One build's client: every call is recorded. */
function client(target, receiver) {
  const records = [];
  const origins = [
    [target.origin, '<origin>'],
    [target.usercontent, '<usercontent>'],
    [receiver, '<receiver>'],
  ];
  const keys = new Map();
  async function send(method, template, params = {}, opts = {}) {
    const url = new URL(fill(template, params), target.origin);
    for (const [k, v] of Object.entries(opts.query ?? {})) url.searchParams.set(k, v);
    const headers = {};
    const token = opts.token === undefined ? target.pat : opts.token;
    if (token) headers.authorization = `Bearer ${token}`;
    if (opts.ifMatch) headers['if-match'] = opts.ifMatch;
    if (method !== 'GET' && method !== 'HEAD') {
      const key = opts.key ?? opts.label;
      // One key per label, the same on both builds.
      headers['idempotency-key'] = keys.get(key) ?? key.replace(/[^a-z0-9]+/gi, '-');
    }
    if (opts.body !== undefined) headers['content-type'] = 'application/json';
    const response = await fetch(url, {
      method,
      headers,
      body: opts.body === undefined ? undefined : JSON.stringify(opts.body),
      redirect: 'manual',
    });
    const type = response.headers.get('content-type') ?? '';
    const text = await response.text();
    let body = null;
    if (type.includes('json')) {
      try {
        body = JSON.parse(text);
      } catch {}
    }
    return { status: response.status, body, text, type, headers: response.headers };
  }
  function record(label, method, template, r, opts = {}) {
    const kind = r.type.split(';')[0].trim();
    let body =
      r.body !== null
        ? normalize(r.body, origins)
        : kind.startsWith('text/') || kind.includes('json')
          ? normalize(r.text, origins)
          : `<${kind || 'empty'}: ${r.text.length > 0 ? 'bytes' : 'none'}>`;
    if (template === '/.well-known/jwks.json' && Array.isArray(body?.keys)) {
      body = { ...body, keys: body.keys.map(({ kid, x, ...key }) => ({
        ...key,
        kid: /^((grant|user)-)[A-Za-z0-9_-]{16}$/.test(kid) ? kid.split('-')[0] : kid,
        x: /^[A-Za-z0-9_-]{43}$/.test(x) ? '<public key>' : x,
      })) };
    }
    if (opts.unordered && Array.isArray(body?.items)) {
      body.items.sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b)));
    }
    records.push({
      label,
      op: `${method} ${template}`,
      status: r.status,
      type: kind,
      body,
      // MCP2: response headers a call asks to compare (`www-authenticate`).
      ...(opts.recordHeaders
        ? { headers: Object.fromEntries(opts.recordHeaders.map((h) => [h, normalize(r.headers.get(h), origins)])) }
        : {}),
    });
  }
  return {
    org: target.org,
    service: target.service,
    project: target.project,
    user: target.user,
    receiver,
    records,
    async call(label, method, template, params = {}, opts = {}) {
      const r = await send(method, template, params, { ...opts, label });
      record(label, method, template, r, opts);
      return r;
    },
    /** Retry a read until `ready` holds (D56's bounded delay), then record
     * the settled answer. */
    async until(label, method, template, params, ready) {
      const deadline = Date.now() + 20_000;
      for (;;) {
        await target.tick?.();
        const r = await send(method, template, params, { label });
        if (ready(r) || Date.now() > deadline) {
          record(label, method, template, r);
          return r;
        }
        await new Promise((done) => setTimeout(done, 200));
      }
    },
    /** The SSE stream: its status, type and first event's name. */
    async events(label, exe) {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), 5_000);
      const url = new URL(fill('/v1/executions/{exe}/events', { exe }), target.origin);
      let status = 0;
      let first = '<none>';
      let type = '';
      try {
        const response = await fetch(url, {
          headers: { authorization: `Bearer ${target.pat}`, accept: 'text/event-stream' },
          signal: controller.signal,
        });
        status = response.status;
        type = (response.headers.get('content-type') ?? '').split(';')[0];
        const reader = response.body.getReader();
        let buffered = '';
        while (!buffered.includes('\n\n')) {
          const { value, done } = await reader.read();
          if (done) break;
          buffered += new TextDecoder().decode(value);
        }
        first = /^event: *(.+)$/m.exec(buffered)?.[1] ?? '<data>';
        controller.abort();
      } catch {}
      clearTimeout(timer);
      records.push({ label, op: 'GET /v1/executions/{exe}/events', status, type, body: { first_event: first } });
    },
    note(label, status) {
      records.push({ label, op: 'PUT <usercontent>/u/*', status, type: '', body: null });
    },
  };
}

/** Every JSON path where `a` and `b` differ. */
function differences(a, b, path = '') {
  if (JSON.stringify(a) === JSON.stringify(b)) return [];
  const objects = a && b && typeof a === 'object' && typeof b === 'object' && Array.isArray(a) === Array.isArray(b);
  if (!objects) return [path || '.'];
  const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
  return [...keys].flatMap((k) => differences(a[k], b[k], path ? `${path}.${k}` : k));
}

async function runOn(start, name) {
  const received = [];
  const server = createServer((req, res) => {
    req.resume();
    req.on('end', () => {
      received.push(req.url);
      res.writeHead(200, { 'content-type': 'text/plain' });
      res.end('ok');
    });
  });
  await new Promise((done) => server.listen(0, '127.0.0.1', done));
  const receiver = `http://127.0.0.1:${server.address().port}/hook`;
  log(`starting ${name}`);
  const target = await start();
  try {
    const t = client(target, receiver);
    try {
      await scenario(t);
      if (/synthetic-core-oauth-code|synthetic_CORE_OAUTH_STATE/.test(target.logs())) {
        throw new Error('runner OAuth callback query leaked to the server log');
      }
    } catch (error) {
      await mkdir(join(root, 'contract', '.out'), { recursive: true });
      await writeFile(join(root, 'contract', '.out', `${name}.partial.json`), JSON.stringify(t.records, null, 2));
      throw new Error(`${name}: the scenario stopped after ${t.records.length} calls (${name}.partial.json): ${error.message}`);
    }
    // Cloud-only routes: absent, as any unknown path.
    for (const [method, template] of CLOUD_ONLY) {
      const path = template.replace(/\{(\w+)\}/g, (_, n) => `${n}_00000000000000000000000000`);
      const r = await fetch(new URL(path, target.origin), {
        method,
        headers: { authorization: `Bearer ${target.pat}`, 'content-type': 'application/json', 'idempotency-key': 'cloud' },
        body: method === 'GET' ? undefined : '{}',
      });
      t.records.push({
        label: `cloud-only ${method} ${template}`,
        op: `cloud ${method} ${template}`,
        status: r.status,
        type: '',
        body: normalize(await r.json().catch(() => null), []),
      });
    }
    return { records: t.records, logs: target.logs() };
  } finally {
    await target.stop();
    await new Promise((done) => server.close(done));
  }
}

async function main() {
  const databaseUrl = process.env.DATABASE_URL;
  if (!databaseUrl) throw new Error('set DATABASE_URL (the native build runs on Postgres)');
  const routes = await manifest();
  const contract = JSON.parse(await readFile(join(repo, 'api', 'openapi.draft.json'), 'utf8'));
  const out = join(root, 'contract', '.out');
  await mkdir(out, { recursive: true });
  const native = await runOn(() => startNative({ databaseUrl, log }), 'native');
  await writeFile(join(out, 'native.json'), JSON.stringify(native.records, null, 2));
  await writeFile(join(out, 'native.log'), native.logs);
  const workers = await runOn(() => startWorkers({ log }), 'workers');
  await writeFile(join(out, 'workers.json'), JSON.stringify(workers.records, null, 2));
  await writeFile(join(out, 'workers.log'), workers.logs);

  const failures = [];
  // 1. Coverage.
  for (const [name, run] of [['native', native], ['workers', workers]]) {
    const called = new Set(run.records.map((r) => r.op));
    for (const route of routes) if (!called.has(route)) failures.push(`${name}: never called ${route}`);
  }
  // 2. Parity, through the allowlist.
  const used = new Set();
  if (native.records.length !== workers.records.length) {
    failures.push(`native made ${native.records.length} calls, workers ${workers.records.length}`);
  }
  for (let i = 0; i < Math.min(native.records.length, workers.records.length); i += 1) {
    const [n, w] = [native.records[i], workers.records[i]];
    if (n.label !== w.label) {
      failures.push(`call ${i}: ${n.label} vs ${w.label}`);
      break;
    }
    const diffs = differences({ status: n.status, type: n.type, body: n.body }, { status: w.status, type: w.type, body: w.body });
    for (const diff of diffs) {
      const entry = ALLOWED.find(
        (a) => a.exercised && (a.label === '*' || a.label === n.label) && (diff === a.path || diff.startsWith(`${a.path}.`)),
      );
      if (entry) {
        used.add(entry);
        continue;
      }
      const at = (rec) => diff.split('.').reduce((v, k) => (v === undefined || v === null ? v : v[k]), rec);
      failures.push(
        `${n.label} (${n.op}) differs at ${diff}: native ${JSON.stringify(at(n))?.slice(0, 300)} vs workers ${JSON.stringify(at(w))?.slice(0, 300)}`,
      );
    }
  }
  for (const entry of ALLOWED) {
    if (entry.exercised && !used.has(entry)) {
      failures.push(`allowlist entry ${entry.label} ${entry.path} never matched: remove it if the builds now agree`);
    }
  }
  // 3. Editions.
  for (const [name, run] of [['native', native], ['workers', workers]]) {
    const meta = run.records.find((r) => r.label === 'meta');
    const discovery = run.records.find((r) => r.label === 'discovery');
    if (meta?.body?.edition !== 'self_hosted') failures.push(`${name}: /v1/meta edition is ${meta?.body?.edition}`);
    if (discovery?.body?.edition !== 'self_hosted') failures.push(`${name}: discovery edition is ${discovery?.body?.edition}`);
    for (const r of run.records.filter((x) => x.op.startsWith('cloud '))) {
      // 404, or 405 where the path serves other methods in the open
      // source build (POST /v1/orgs beside GET /v1/orgs).
      const path = r.op.split(' ')[2];
      const shared = [...routes].some((route) => route.split(' ')[1] === path);
      const absent = r.status === 404 || (shared && r.status === 405);
      if (!absent) failures.push(`${name}: ${r.label} answered ${r.status}, not 404 (or 405 on a shared path)`);
    }
  }
  const documented = new Set(
    Object.entries(contract.paths).flatMap(([path, ops]) => Object.keys(ops).map((m) => `${m.toUpperCase()} ${path}`)),
  );
  for (const [method, template] of CLOUD_ONLY) {
    if (routes.has(`${method} ${template}`) || documented.has(`${method} ${template}`)) {
      failures.push(`cloud-only ${method} ${template} is in the open source manifest or contract`);
    }
  }

  log(`${native.records.length} calls per build, ${routes.size} public routes covered, ${CLOUD_ONLY.length} cloud-only routes absent`);
  for (const entry of ALLOWED) {
    log(`allowed (${entry.exercised ? (used.has(entry) ? 'seen' : 'NOT SEEN') : 'not reachable'}): ${entry.label} ${entry.path}`);
  }
  if (failures.length > 0) {
    process.stderr.write(`[contract] ${failures.length} failure(s):\n${failures.join('\n')}\n`);
    process.exit(1);
  }
  log('the native and Workers builds answer the contract alike');
}

main().catch((error) => {
  process.stderr.write(`[contract] ${error.stack ?? error}\n`);
  process.exit(1);
});
