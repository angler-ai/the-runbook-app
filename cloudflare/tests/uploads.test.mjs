// CF7: evidence uploads on the Cloudflare edition (D49–D51), with the
// signing key and the user-content origin as local config. The api Worker
// answers both hostnames: the app origin issues and finalizes slots, the
// user-content origin serves only `/u/*` (signed PUT and GET) with the
// native safety headers. Images stay off until a Workers pipeline strips
// their metadata (D51). LOCAL ONLY.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { startWorker } from './harness.mjs';

const BOOTSTRAP_SECRET = randomBytes(24).toString('hex');
const APP = 'https://runbook.test';
const CONTENT = 'https://usercontent.runbook.test';

let worker;
let pat;
let project;
let user;
let sr;
let claim;
before(async () => {
  worker = await startWorker({
    faults: false,
    vars: {
      BOOTSTRAP_SECRET,
      ORIGIN: APP,
      RUNBOOK_USERCONTENT_ORIGIN: CONTENT,
      RUNBOOK_UPLOAD_SIGNING_KEY: randomBytes(32).toString('base64'),
    },
  });
  const boot = await worker.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Acme', user_name: 'Owner', project_slug: 'ops' }),
  });
  assert.equal(boot.status, 201, await boot.clone().text());
  ({ personal_token: pat, project_id: project, user_id: user } = await boot.json());
});
after(async () => {
  await worker?.dispose();
});

async function json(method, path, body, { key, ifMatch } = {}) {
  const headers = { authorization: `Bearer ${pat}` };
  if (key) headers['idempotency-key'] = key;
  if (ifMatch) headers['if-match'] = ifMatch;
  if (body !== undefined) headers['content-type'] = 'application/json';
  const response = await worker.fetch(path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let parsed = null;
  try {
    parsed = JSON.parse(text);
  } catch {}
  return { status: response.status, body: parsed, text, etag: response.headers.get('etag'), headers: response.headers };
}
const content = (url, init) => worker.mf.dispatchFetch(url, init);

function assertSafety(response) {
  assert.equal(response.headers.get('x-content-type-options'), 'nosniff');
  assert.equal(
    response.headers.get('content-security-policy'),
    "sandbox; default-src 'none'; frame-ancestors 'none'",
  );
  assert.equal(response.headers.get('x-frame-options'), 'DENY');
  assert.equal(response.headers.get('referrer-policy'), 'no-referrer');
  assert.ok(response.headers.get('content-disposition'), 'a disposition on every response');
  assert.equal(response.headers.get('set-cookie'), null);
}

test('discovery advertises uploads, with no image types until a pipeline strips metadata', async () => {
  const meta = await json('GET', '/v1/meta');
  assert.equal(meta.status, 200, meta.text);
  assert.equal(meta.body.capabilities.uploads, true, meta.text);
  assert.deepEqual(meta.body.limits?.uploads?.image_types ?? meta.body.uploads?.image_types ?? [], [], meta.text);
});

test('a claimed step gets a slot whose upload URL is on the user-content origin', async () => {
  const source = [
    'apiVersion: therunbook.app/v1',
    'kind: Runbook',
    'metadata: {id: verify, title: Verify}',
    `spec: {steps: [{id: check, title: Check the deploy, assignee: "user:${user}"}]}`,
    '',
  ].join('\n');
  const created = await json('POST', `/v1/projects/${project}/runbooks`, { source }, { key: 'create-1' });
  assert.equal(created.status, 201, created.text);
  const rb = created.body.runbook_id;
  const draft = await json('GET', `/v1/runbooks/${rb}/draft`);
  const published = await json('POST', `/v1/runbooks/${rb}/versions`, {}, { key: 'publish-1', ifMatch: draft.etag });
  assert.equal(published.status, 201, published.text);
  const started = await json('POST', `/v1/projects/${project}/executions`, { runbook: 'verify' }, { key: 'start-1' });
  assert.equal(started.status, 201, started.text);
  const exe = started.body.id;
  for (let i = 0; i < 100 && !sr; i += 1) {
    const read = await json('GET', `/v1/executions/${exe}`);
    sr = JSON.stringify(read.body).match(/"(sr_[0-9A-Z]{26})"/)?.[1];
  }
  assert.ok(sr, 'the step run exists');
  const claimed = await json('POST', `/v1/step-runs/${sr}/claim`, { session_id: 'web' }, { key: 'claim-1' });
  assert.equal(claimed.status, 200, claimed.text);
  claim = claimed.body;

  const image = await json('POST', `/v1/step-runs/${sr}/evidence`, { name: 'shot.png', type: 'image/png', size: 10 }, { key: 'slot-image' });
  assert.ok(image.status >= 400 && image.status < 500, image.text);
  assert.equal(image.body.code, 'upload.type_unsupported', image.text);
});

const LOG = 'deploy ok\nall green\n';
let slot;
test('the signed PUT works only on the user-content origin, with the native checks and headers', async () => {
  const made = await json('POST', `/v1/step-runs/${sr}/evidence`, { name: 'deploy log.txt', type: 'text/plain', size: LOG.length }, { key: 'slot-1' });
  assert.equal(made.status, 201, made.text);
  slot = made.body;
  const url = new URL(slot.upload_url);
  assert.equal(url.origin, CONTENT, slot.upload_url);

  // The app origin never serves /u/* (its router refuses the request).
  const onApp = await worker.fetch(`${url.pathname}${url.search}`, { method: 'PUT', headers: { 'content-type': 'text/plain' }, body: LOG });
  assert.ok([404, 415].includes(onApp.status), String(onApp.status));
  const getOnApp = await worker.fetch(`${url.pathname}${url.search}`);
  assert.equal(getOnApp.status, 404);
  // The user-content origin serves nothing else.
  const meta = await content(`${CONTENT}/v1/meta`);
  assert.equal(meta.status, 404);
  assertSafety(meta);

  const preflight = await content(slot.upload_url, {
    method: 'OPTIONS',
    headers: { origin: APP, 'access-control-request-method': 'PUT' },
  });
  assert.equal(preflight.status, 204);
  assert.equal(preflight.headers.get('access-control-allow-origin'), APP);
  assert.equal(preflight.headers.get('access-control-allow-methods'), 'PUT');
  const foreign = await content(slot.upload_url, {
    method: 'OPTIONS',
    headers: { origin: 'https://evil.example', 'access-control-request-method': 'PUT' },
  });
  assert.equal(foreign.status, 404);

  const wrongType = await content(slot.upload_url, { method: 'PUT', headers: { 'content-type': 'application/json' }, body: LOG });
  assert.equal(wrongType.status, 422);
  assert.equal((await wrongType.json()).code, 'upload.type_mismatch');
  const tampered = new URL(slot.upload_url);
  tampered.searchParams.set('sig', 'A'.repeat(tampered.searchParams.get('sig').length));
  const forged = await content(tampered.href, { method: 'PUT', headers: { 'content-type': 'text/plain' }, body: LOG });
  assert.equal(forged.status, 403);
  assert.equal((await forged.json()).code, 'upload.url_invalid');
  const short = await content(slot.upload_url, { method: 'PUT', headers: { 'content-type': 'text/plain' }, body: LOG.slice(1) });
  assert.equal(short.status, 422);

  const put = await content(slot.upload_url, {
    method: 'PUT',
    headers: { 'content-type': 'text/plain', origin: APP, cookie: 'runbook.session_token=ignored' },
    body: LOG,
  });
  assert.equal(put.status, 204, await put.clone().text());
  assert.equal(put.headers.get('access-control-allow-origin'), APP);
  assertSafety(put);
});

test('finalize gives a signed read URL; the file comes back with the safety headers, as a download', async () => {
  const done = await json('POST', `/v1/step-runs/${sr}/evidence/${slot.evidence_id}/finalize`, { claim }, { key: 'fin-1' });
  assert.equal(done.status, 200, done.text);
  const read = done.body.url ?? done.body.download_url ?? done.body.read_url;
  assert.ok(read, done.text);
  assert.equal(new URL(read).origin, CONTENT);
  const file = await content(read);
  assert.equal(file.status, 200);
  assert.equal(await file.clone().text(), LOG);
  assert.equal(file.headers.get('content-type'), 'text/plain; charset=utf-8');
  assert.match(file.headers.get('content-disposition'), /^attachment; filename="deploy log.txt"; filename\*=UTF-8''deploy%20log.txt$/);
  assert.match(file.headers.get('cache-control'), /^private, max-age=\d+$/);
  assertSafety(file);
  const head = await content(read, { method: 'HEAD' });
  assert.equal(head.status, 200);
  assert.equal(head.headers.get('content-length'), String(LOG.length));
  const forged = new URL(read);
  forged.searchParams.set('sig', 'A'.repeat(forged.searchParams.get('sig').length));
  assert.equal((await content(forged.href)).status, 403);
});
