// Gate fix B2: the AUTH3 rate limits trip on Workers. Each request builds
// its own backends, so per-request windows never filled; the limits are now
// a Durable Object per key (LimitsDO), shared by every isolate. Also the
// native per-IP limit on POST /v1/device/code (20 an hour) now has its
// Worker counterpart, keyed by cf-connecting-ip. LOCAL ONLY.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { startSignInStack } from './harness.mjs';

const BOOTSTRAP_SECRET = randomBytes(24).toString('hex');
const OWNER = 'owner@example.test';
let stack;
let cookie;
before(async () => {
  stack = await startSignInStack({ vars: { BOOTSTRAP_SECRET } });
  const boot = await stack.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Acme', user_name: 'Owner', project_slug: 'ops', owner_email: OWNER }),
  });
  assert.equal(boot.status, 201, await boot.clone().text());
  // Device approval is a signed-in person in a browser (D47).
  const auth = (path, body) =>
    stack.fetch(path, {
      method: 'POST',
      headers: { 'content-type': 'application/json', origin: stack.origin },
      body: JSON.stringify(body),
    });
  const sent = await auth('/api/auth/email-otp/send-verification-otp', { email: OWNER, type: 'sign-in' });
  assert.equal(sent.status, 200, await sent.clone().text());
  const otp = (await stack.mail()).filter((m) => m.to === OWNER).at(-1)?.code;
  const signed = await auth('/api/auth/sign-in/email-otp', { email: OWNER, otp });
  const set = signed.headers.getSetCookie?.() ?? [signed.headers.get('set-cookie')];
  cookie = set.find((c) => c && c.startsWith('__Host-runbook.session_token='))?.split(';')[0];
  assert.ok(cookie, 'signed in');
});
after(async () => {
  await stack?.dispose();
});

test('guessing device codes trips the per-user miss limit across requests', async () => {
  const statuses = [];
  for (let i = 0; i < 7; i += 1) {
    const response = await stack.fetch('/v1/device/approve', {
      method: 'POST',
      headers: {
        cookie,
        origin: stack.origin,
        'x-runbook-csrf': '1',
        'idempotency-key': `guess-${i}`,
        'content-type': 'application/json',
      },
      body: JSON.stringify({ user_code: `BCDF-GH${i}J`, approve: true }),
    });
    const body = await response.json();
    statuses.push([response.status, body.code]);
  }
  assert.deepEqual(statuses.slice(0, 5).map(([s]) => s), [404, 404, 404, 404, 404], JSON.stringify(statuses));
  assert.deepEqual(statuses[5], [429, 'limit.rate'], JSON.stringify(statuses));
});

test('device codes are limited per client IP, 20 an hour', async () => {
  const ask = (ip) =>
    stack.fetch('/v1/device/code', {
      method: 'POST',
      headers: { 'cf-connecting-ip': ip, 'content-type': 'application/json' },
      body: JSON.stringify({ client_name: 'limit test' }),
    });
  for (let i = 0; i < 20; i += 1) {
    const response = await ask('203.0.113.7');
    assert.ok(response.status < 300, `${i}: ${response.status} ${await response.text()}`);
  }
  const limited = await ask('203.0.113.7');
  assert.equal(limited.status, 429);
  const body = await limited.json();
  assert.equal(body.code, 'limit.rate');
  assert.ok(body.retry_after > 0, JSON.stringify(body));
  const other = await ask('198.51.100.9');
  assert.ok(other.status < 300, `another client: ${other.status}`);
});
