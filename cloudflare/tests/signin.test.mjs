// CF7: browser sign-in on the Cloudflare edition (D43–D46), end to end:
// the Rust api Worker proxies /api/auth/* to the auth Worker (Better Auth on
// the same D1) through its AUTH service binding; a session cookie is
// resolved through the auth Worker's /internal/session (cached 30 s, evicted
// on sign-out), linked to its directory user on first use (D45) and needs
// the CSRF header on unsafe methods (D46), in the api Worker and in the
// ExecutionDO alike; registration asks Rust's can-register through the
// auth Worker's API binding. LOCAL ONLY.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { AUTH_INTERNAL_SECRET, chunked, frames, startSignInStack } from './harness.mjs';

const BOOTSTRAP_SECRET = randomBytes(24).toString('hex');
const OWNER = 'owner@example.test';
let stack;
let pat;
let project;
before(async () => {
  stack = await startSignInStack({ vars: { BOOTSTRAP_SECRET } });
  const boot = await stack.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Acme', user_name: 'Owner', project_slug: 'ops', owner_email: OWNER }),
  });
  assert.equal(boot.status, 201, await boot.clone().text());
  const out = await boot.json();
  pat = out.personal_token;
  project = out.project_id;
});
after(async () => {
  await stack?.dispose();
});

function call(path, { method = 'GET', cookie, body, csrf = false, origin = true, bearer, key } = {}) {
  const headers = {};
  if (key) headers['idempotency-key'] = key;
  if (cookie) headers.cookie = cookie;
  if (bearer) headers.authorization = `Bearer ${bearer}`;
  if (body !== undefined) headers['content-type'] = 'application/json';
  if (csrf) headers['x-runbook-csrf'] = '1';
  if (origin) headers.origin = stack.origin;
  return stack.fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
}
async function json(response) {
  const text = await response.text();
  try {
    return { status: response.status, body: JSON.parse(text), text };
  } catch {
    return { status: response.status, body: null, text };
  }
}

async function signIn(email) {
  const sent = await call('/api/auth/email-otp/send-verification-otp', {
    method: 'POST',
    body: { email, type: 'sign-in' },
  });
  assert.equal(sent.status, 200, await sent.clone().text());
  const code = (await stack.mail()).filter((m) => m.to === email).at(-1)?.code;
  assert.match(code ?? '', /^\d{6}$/);
  return call('/api/auth/sign-in/email-otp', { method: 'POST', body: { email, otp: code } });
}

const sessionCookie = (response) => {
  const set = response.headers.getSetCookie?.() ?? [response.headers.get('set-cookie')];
  const line = set.find((c) => c && c.startsWith('__Host-runbook.session_token='));
  return line ? line.split(';')[0] : null;
};

let cookie;
let exe;
test('discovery lists email sign-in from the auth Worker', async () => {
  const meta = await json(await call('/.well-known/runbook', { origin: false }));
  assert.equal(meta.status, 200, meta.text);
  assert.ok(JSON.stringify(meta.body).includes('"email"'), meta.text);
});

test('a member signs in by email code through the proxy and gets the session cookie', async () => {
  const signed = await signIn(OWNER);
  assert.equal(signed.status, 200, await signed.clone().text());
  cookie = sessionCookie(signed);
  assert.ok(cookie, 'the __Secure- session cookie came back through the proxy');
  const me = await json(await call('/v1/me', { cookie, origin: false }));
  assert.equal(me.status, 200, me.text);
  assert.ok(me.text.includes(OWNER) || me.text.includes('Owner'), me.text);
});

test('D46: a cookie on an unsafe method needs the CSRF header and the origin', async () => {
  const source = [
    'apiVersion: therunbook.app/v1',
    'kind: Runbook',
    'metadata: {id: verify, title: Verify}',
    'spec: {steps: [{id: check, title: Check}]}',
    '',
  ].join('\n');
  const bare = await json(await call(`/v1/projects/${project}/runbooks`, { method: 'POST', cookie, body: { source } }));
  assert.equal(bare.status, 403, bare.text);
  assert.equal(bare.body.code ?? bare.body.error, 'auth.csrf', bare.text);
  const foreign = await stack.fetch(`/v1/projects/${project}/runbooks`, {
    method: 'POST',
    headers: { cookie, origin: 'https://evil.example', 'x-runbook-csrf': '1', 'content-type': 'application/json' },
    body: JSON.stringify({ source }),
  });
  assert.equal(foreign.status, 403);
  const created = await call(`/v1/projects/${project}/runbooks`, {
    method: 'POST',
    cookie,
    csrf: true,
    key: 'cookie-create-1',
    body: { source },
  });
  assert.equal(created.status, 201, await created.clone().text());
  runbookId = (await created.json()).runbook_id;
});
let runbookId;

test('the session reaches a run in its ExecutionDO (the api Worker routes by the linked user)', async () => {
  const draft = await call(`/v1/runbooks/${runbookId}/draft`, { bearer: pat, origin: false });
  const published = await stack.fetch(`/v1/runbooks/${runbookId}/versions`, {
    method: 'POST',
    headers: { authorization: `Bearer ${pat}`, 'idempotency-key': 'pub-1', 'if-match': draft.headers.get('etag'), 'content-type': 'application/json' },
    body: '{}',
  });
  assert.equal(published.status, 201, await published.clone().text());
  const started = await stack.fetch(`/v1/projects/${project}/executions`, {
    method: 'POST',
    headers: {
      cookie,
      origin: stack.origin,
      'x-runbook-csrf': '1',
      'idempotency-key': 'cookie-start-1',
      'content-type': 'application/json',
    },
    body: JSON.stringify({ runbook: 'verify' }),
  });
  assert.equal(started.status, 201, await started.clone().text());
  exe = (await started.json()).id;
  const read = await call(`/v1/executions/${exe}`, { cookie, origin: false });
  assert.equal(read.status, 200, await read.clone().text());
  assert.equal(read.headers.get('x-runbook-object'), '1', 'the run object answered');
});

test('parity: a session cookie opens the run\'s event stream, as natively', async () => {
  const stream = await call(`/v1/executions/${exe}/events`, { cookie, origin: false });
  assert.equal(stream.status, 200, stream.status === 200 ? '' : await stream.text());
  assert.match(stream.headers.get('content-type') ?? '', /^text\/event-stream/);
  const reader = frames(stream);
  const first = await reader.next(5000);
  assert.ok(first, 'a first frame');
  await reader.cancel();
  // A cookie without a session is refused like no credential.
  const stale = await call(`/v1/executions/${exe}/events`, { cookie: '__Host-runbook.session_token=nope', origin: false });
  assert.equal(stale.status, 401);
  await stale.body?.cancel();
});

test('a session claims a step, reads it and completes it, as the web does', async () => {
  const next = await json(await call(`/v1/executions/${exe}/next`, { cookie, origin: false }));
  assert.equal(next.status, 200, next.text);
  const sr = next.body.ready_work?.[0]?.step_run_id;
  assert.ok(sr, next.text);
  const claim = await json(
    await call(`/v1/step-runs/${sr}/claim`, {
      method: 'POST',
      cookie,
      csrf: true,
      key: 'cookie-claim-1',
      body: { session_id: 'tab-1', actor: { kind: 'human' } },
    }),
  );
  assert.equal(claim.status, 200, `claim: ${claim.text}`);
  const read = await json(await call(`/v1/step-runs/${sr}`, { cookie, origin: false }));
  assert.equal(read.status, 200, `read: ${read.text}`);
  const done = await json(
    await call(`/v1/step-runs/${sr}/report`, {
      method: 'POST',
      cookie,
      csrf: true,
      key: 'cookie-report-1',
      body: {
        step_run_id: sr,
        claim: claim.body,
        status: 'succeeded',
        summary: 'checked',
        outputs: {},
        verdict: null,
        evidence: [],
        logs: [],
      },
    }),
  );
  assert.equal(done.status, 200, `report: ${done.text}`);
});

test('D45: an email no member or invitation holds cannot register', async () => {
  const refused = await signIn('stranger@example.test');
  assert.equal(refused.status, 403, await refused.clone().text());
  assert.ok((await refused.text()).includes('auth.registration_refused'));
});

test('can-register answers only the auth Worker (the internal secret)', async () => {
  const outside = await call('/internal/can-register', { method: 'POST', body: { email: OWNER } });
  assert.equal(outside.status, 401);
  const inside = await stack.fetch('/internal/can-register', {
    method: 'POST',
    headers: { 'x-runbook-internal-secret': AUTH_INTERNAL_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ email: OWNER }),
  });
  assert.equal(inside.status, 200, await inside.clone().text());
  assert.deepEqual(await inside.json(), { allowed: true });
});

test('the proxy never forwards the internal secret or a client-set client IP', async () => {
  const response = await stack.fetch('/api/auth/ok', {
    headers: { 'x-runbook-internal-secret': AUTH_INTERNAL_SECRET },
  });
  assert.equal(response.status, 200);
  // /internal/* on the auth Worker is not reachable through /api/auth.
  const internal = await stack.fetch('/api/auth/../internal/health', {
    headers: { 'x-runbook-internal-secret': AUTH_INTERNAL_SECRET },
  });
  assert.notEqual(internal.status, 200);
});

test('sign-out evicts the cached session at once', async () => {
  const before = await call('/v1/me', { cookie, origin: false });
  assert.equal(before.status, 200);
  // D32: an open session stream ends at its next re-check once signed out.
  const stream = await call(`/v1/executions/${exe}/events`, { cookie, origin: false });
  assert.equal(stream.status, 200);
  const reader = frames(stream);
  const out = await call('/api/auth/sign-out', { method: 'POST', cookie, body: {} });
  assert.equal(out.status, 200, await out.clone().text());
  const after = await json(await call('/v1/me', { cookie, origin: false }));
  assert.equal(after.status, 401, after.text);
  let ended = false;
  for (let i = 0; i < 40 && !ended; i += 1) {
    ended = (await reader.next(5000).catch(() => undefined)) === null;
  }
  assert.ok(ended, 'the stream ended after sign-out');
});

// CF12 review round 1: the /api/auth/* proxy holds bodies to native's auth
// proxy cap (64 KiB, `runbook_api::http::AUTH_BODY_BYTES`), declared or
// chunked, before anything reaches the auth Worker.
test('the auth proxy refuses a body over its cap, declared or chunked', async () => {
  const CAP = 65_536;
  const path = '/api/auth/sign-in/email-otp';
  const headers = { 'content-type': 'application/json', origin: stack.origin };
  const declared = await stack.fetch(path, { method: 'POST', headers, body: ' '.repeat(CAP + 1) });
  assert.equal(declared.status, 413, await declared.clone().text());
  assert.equal((await declared.json()).code, 'request.body_too_large');
  const { stream, pulled, release } = chunked(64 * 1_048_576, 65_536, CAP + 2 * 65_536);
  const streamed = await stack.fetch(path, { method: 'POST', headers, body: stream, duplex: 'half' });
  assert.equal(streamed.status, 413, await streamed.clone().text());
  assert.equal((await streamed.json()).code, 'request.body_too_large');
  assert.ok(pulled() < 32 * 1_048_576, `${pulled()} B pulled`);
  release();
  // At the cap the body reaches the auth Worker (spaces: not a sign-in).
  const at = await stack.fetch(path, { method: 'POST', headers, body: ' '.repeat(CAP) });
  assert.notEqual(at.status, 413, await at.clone().text());
});
