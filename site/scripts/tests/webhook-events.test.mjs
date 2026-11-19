// Every example body in scripts/data/webhook-events.md is a valid envelope
// of its own event type.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loadDataDoc } from '../lib/datadoc.mjs';

const ENVELOPE = ['id', 'type', 'version', 'created_at', 'org', 'project', 'execution', 'sequence', 'data'];
const entries = loadDataDoc('webhook-events.md', { exampleLang: 'json' }).flatMap((s) => s.entries);

test('there is an example for every documented event', () => {
  assert.ok(entries.length >= 17);
  for (const e of entries) assert.ok(e.example, `${e.name} has an example`);
});

for (const e of entries) {
  test(`${e.name}: example is a valid envelope`, () => {
    const body = JSON.parse(e.example);
    assert.deepEqual(Object.keys(body), ENVELOPE);
    assert.equal(body.type, e.name);
    assert.equal(body.version, 1);
    assert.match(body.id, /^evt_[0-9A-HJKMNP-TV-Z]{26}$/);
    assert.match(body.created_at, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/);
    assert.match(body.org, /^org_/);
    assert.match(body.project, /^prj_/);
    if (body.execution === null) {
      assert.equal(body.sequence, 0, 'project events have sequence 0');
    } else {
      assert.deepEqual(Object.keys(body.execution), ['id', 'title', 'runbook', 'runbook_version_id', 'runbook_version', 'url']);
      assert.match(body.execution.id, /^exe_/);
    }
    assert.equal(typeof body.data, 'object');
  });
}
