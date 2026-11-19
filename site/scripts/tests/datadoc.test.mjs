// The data-file parser behind the generated key, diagnostics and webhook
// event references.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { inheritedFrom, loadDataDoc, parseDataDoc, wrapKeyExample } from '../lib/datadoc.mjs';

test('parses sections, entries, metadata, bodies and examples', () => {
  const doc = parseDataDoc(
    [
      '<!-- a comment with ## inside -->',
      '# First',
      'Intro text.',
      '',
      '## spec.example',
      'type: string',
      'as: step',
      '',
      'Body line one.',
      '',
      '```yaml',
      '- id: example',
      '# not a section, inside a fence',
      '```',
      '',
      'Body after.',
    ].join('\n'),
    { exampleLang: 'yaml' },
  );
  assert.equal(doc.length, 1);
  assert.equal(doc[0].title, 'First');
  assert.equal(doc[0].intro, 'Intro text.');
  const [entry] = doc[0].entries;
  assert.equal(entry.name, 'spec.example');
  assert.deepEqual(entry.meta, { type: 'string', as: 'step' });
  assert.equal(entry.example, '- id: example\n# not a section, inside a fence');
  assert.match(entry.body, /^Body line one\.\n\nBody after\.$/);
});

test('wraps key examples into complete runbooks', () => {
  const step = wrapKeyExample({ name: 'x', meta: { as: 'step' }, example: '- id: a\n  actor: agent' });
  assert.equal(step, 'metadata: { id: key-example }\nspec:\n  steps:\n    - id: a\n      actor: agent\n');
  const input = wrapKeyExample({ name: 'x', meta: { as: 'inputs' }, example: 'svc: { type: string }' });
  assert.match(input, /^ {2}inputs:\n {4}svc: \{ type: string \}\n {2}steps:/m);
  assert.throws(() => wrapKeyExample({ name: 'x', meta: { as: 'nope' }, example: '' }));
});

test('field declarations inherit the input declaration entries', () => {
  assert.equal(inheritedFrom('spec.fields.<name>.type'), 'spec.inputs.<name>.type');
  assert.equal(inheritedFrom('spec.steps[].id'), null);
});

test('every key entry has an example and a wrapping', () => {
  for (const section of loadDataDoc('keys.md', { exampleLang: 'yaml' })) {
    for (const e of section.entries) {
      assert.ok(e.example, `${e.name} has an example`);
      assert.ok(['doc', 'step', 'inputs'].includes(e.meta.as), `${e.name} has a known "as:"`);
    }
  }
});

test('every diagnostic entry says how to fix it', () => {
  for (const section of loadDataDoc('diagnostics.md')) {
    for (const e of section.entries) assert.match(e.body, /^Fix: /m, `${e.name} has a Fix: line`);
  }
});
