import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const doc = (name) => readFileSync(new URL(`../../site/src/content/docs/runner/${name}.mdx`, import.meta.url), 'utf8');

test('the public runner guide describes decision leases and the evidence people can review', () => {
  const decision = doc('agents').split('## Decide approvals')[1]?.split('## Follow runs')[0] ?? '';
  assert.match(decision, /first decision wins/i);
  assert.match(decision, /lease is revoked/);
  assert.match(decision, /labeled `read`/);
  assert.match(decision, /references of the tool calls/);
  assert.match(decision, /link to the transcript/);
});

test('the public runner guide names the fields and destination checks for sealed entry', () => {
  const credentials = doc('credentials');
  assert.match(credentials, /\| `api-key` \| `key` \|/);
  assert.match(credentials, /\| `model-provider` \| `api_key` \|/);
  assert.match(credentials, /\| `mcp-bearer` \| `token` \|/);
  assert.match(credentials, /--expected-config/);
  assert.match(credentials, /--binding-pin/);
});
