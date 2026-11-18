// CF12: the api Worker's request-body cap is native's JSON cap (2 MiB +
// 64 KiB, `runbook_api::http::JSON_BODY_BYTES`) on every route, now that the
// edition requires Workers Paid. It was 1 MiB, which refused a runbook
// source near the compiler's 1 MiB ceiling once JSON-encoded. A declared
// length over the cap is refused before the body is read, with native's
// code (`request.body_too_large`). LOCAL ONLY.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { chunked, startWorker } from './harness.mjs';

const JSON_BODY_BYTES = 2 * 1_048_576 + 65_536;
const SOURCE_BYTES = 1_048_576;
// CF12b: what an over-cap chunked body sends before it waits for the
// answer: past the cap by two chunks (see `chunked` in the harness).
const OVER_CAP = JSON_BODY_BYTES + 2 * 65_536;
const BOOTSTRAP_SECRET = randomBytes(24).toString('hex');
let worker;
let owner;
before(async () => {
  worker = await startWorker({ faults: false, vars: { BOOTSTRAP_SECRET, ORIGIN: 'https://runbook.test' } });
  const boot = await worker.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: JSON.stringify({ organization: 'Limits Org', user_name: 'Owner', project_slug: 'ops' }),
  });
  assert.equal(boot.status, 201, await boot.clone().text());
  owner = await boot.json();
});
after(async () => {
  await worker?.dispose();
});

const post = (path, body, key) =>
  worker.fetch(path, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${owner.personal_token}`,
      'content-type': 'application/json',
      'idempotency-key': key,
    },
    body,
  });

test('discovery reports the cap', async () => {
  const meta = await (await worker.fetch('/v1/meta')).json();
  assert.equal(meta.limits.max_request_bytes, JSON_BODY_BYTES);
});

test('a body of exactly the cap is read; one byte more is refused before it is', async () => {
  const path = `/v1/projects/${owner.project_id}/runbooks`;
  // Valid JSON padded with spaces to the exact size; its source does not
  // compile, so an answer other than 413 shows the body was read.
  const body = (size) => {
    const json = JSON.stringify({ source: 'not a runbook' });
    return json.slice(0, -1) + ' '.repeat(size - json.length) + '}';
  };
  const exact = await post(path, body(JSON_BODY_BYTES), 'exact');
  assert.notEqual(exact.status, 413, await exact.clone().text());
  assert.ok(exact.status >= 400 && exact.status < 500, String(exact.status));
  const over = await post(path, body(JSON_BODY_BYTES + 1), 'over');
  assert.equal(over.status, 413, await over.clone().text());
  assert.equal((await over.json()).code, 'request.body_too_large');
});

test('a runbook source at the compiler ceiling saves, even with every byte escaped', async () => {
  const head = [
    'apiVersion: therunbook.app/v1',
    'kind: Runbook',
    'metadata: {id: ceiling, title: Ceiling}',
    `spec: {steps: [{id: check, title: Check, assignee: "user:${owner.user_id}"}]}`,
    '',
  ].join('\n');
  // A comment of quotes, each of which JSON escapes to two bytes.
  const source = `${head}#${'"'.repeat(SOURCE_BYTES - head.length - 2)}\n`;
  assert.equal(Buffer.byteLength(source), SOURCE_BYTES);
  const body = JSON.stringify({ source });
  assert.ok(body.length > 2_000_000, `${body.length} B`);
  const created = await post(`/v1/projects/${owner.project_id}/runbooks`, body, 'ceiling');
  assert.equal(created.status, 201, (await created.clone().text()).slice(0, 500));
  const first = await created.json();
  // Its replay row keeps the 2 MB payload as a digest (D1 rows hold at most
  // 2,000,000 bytes): a retry answers the recorded response, and the key
  // with another body is refused.
  const retried = await post(`/v1/projects/${owner.project_id}/runbooks`, body, 'ceiling');
  assert.equal(retried.status, 201, (await retried.clone().text()).slice(0, 500));
  assert.equal((await retried.json()).runbook_id, first.runbook_id);
  const other = await post(`/v1/projects/${owner.project_id}/runbooks`, body.replace('Ceiling', 'Other'), 'ceiling');
  assert.equal(other.status, 409, (await other.clone().text()).slice(0, 500));
  assert.equal((await other.json()).code, 'request.idempotency_key_reused');
  const draft = await worker.fetch(`/v1/runbooks/${first.runbook_id}/draft`, {
    headers: { authorization: `Bearer ${owner.personal_token}` },
  });
  assert.equal((await draft.json()).source, source);
});

// CF12 review round 1: a body without Content-Length (chunked) is read with
// a running cap and refused at cap + 1, on /v1 and /mcp alike, without
// reading the rest. `pulled` reports how much the server asked for.
test('a chunked body over the cap is refused on /v1, unread past it', async () => {
  const { stream, pulled, release } = chunked(64 * 1_048_576, 65_536, OVER_CAP);
  const response = await worker.fetch(`/v1/projects/${owner.project_id}/runbooks`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${owner.personal_token}`,
      'content-type': 'application/json',
      'idempotency-key': 'chunked-over',
    },
    body: stream,
    duplex: 'half',
  });
  assert.equal(response.status, 413, (await response.clone().text()).slice(0, 300));
  assert.equal((await response.json()).code, 'request.body_too_large');
  assert.ok(pulled() < 32 * 1_048_576, `${pulled()} B pulled`);
  release();
});

test('a chunked body at the cap is read on /v1', async () => {
  const { stream } = chunked(JSON_BODY_BYTES);
  const response = await worker.fetch(`/v1/projects/${owner.project_id}/runbooks`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${owner.personal_token}`,
      'content-type': 'application/json',
      'idempotency-key': 'chunked-exact',
    },
    body: stream,
    duplex: 'half',
  });
  // Spaces are not JSON: read, then refused as invalid, not as too large.
  assert.notEqual(response.status, 413, await response.clone().text());
});

test('a chunked body over the cap is refused on /mcp, unread past it', async () => {
  const { stream, pulled, release } = chunked(64 * 1_048_576, 65_536, OVER_CAP);
  const response = await worker.fetch('/mcp', {
    method: 'POST',
    headers: {
      authorization: `Bearer ${owner.personal_token}`,
      'content-type': 'application/json',
      accept: 'application/json, text/event-stream',
    },
    body: stream,
    duplex: 'half',
  });
  assert.equal(response.status, 413, (await response.clone().text()).slice(0, 300));
  assert.equal((await response.json()).error.data.code, 'request.body_too_large');
  assert.ok(pulled() < 32 * 1_048_576, `${pulled()} B pulled`);
  release();
});

// CF12b: the operator's bootstrap read its body whole; it now reads with the
// same running cap as every other JSON route.
test('a chunked body over the cap is refused on /internal/bootstrap, unread past it', async () => {
  const { stream, pulled, release } = chunked(64 * 1_048_576, 65_536, OVER_CAP);
  const response = await worker.fetch('/internal/bootstrap', {
    method: 'POST',
    headers: { 'x-bootstrap-secret': BOOTSTRAP_SECRET, 'content-type': 'application/json' },
    body: stream,
    duplex: 'half',
  });
  assert.equal(response.status, 413, (await response.clone().text()).slice(0, 300));
  assert.equal((await response.json()).error, 'request.body_too_large');
  assert.ok(pulled() < 32 * 1_048_576, `${pulled()} B pulled`);
  release();
});
