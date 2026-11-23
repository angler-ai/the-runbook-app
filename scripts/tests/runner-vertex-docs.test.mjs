import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const read = (path) => readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8');
const runnerDoc = (name) => read(`site/src/content/docs/runner/${name}.mdx`);

test('Docker Vertex setup documents the separate inference account and vault allowlist', () => {
  const setup = runnerDoc('setup');
  assert.match(setup, /aiplatform\.endpoints\.predict/);
  assert.match(setup, /roles\/iam\.serviceAccountTokenCreator/);
  assert.match(setup, /RUNNER_VERTEX_TARGETS:.*service_account/);
  assert.doesNotMatch(runnerDoc('models'), /roles\/aiplatform\.user/);
});

test('Vertex rebind docs explain who confirms a proposal and what the confirmation checks', () => {
  const models = runnerDoc('models');
  assert.match(models, /confirmation checks the displayed destination/i);
  assert.match(models, /approval confirms the proposed destination/i);
});

test('the published security docs describe the released Vertex token and its expiry', () => {
  assert.match(runnerDoc('security'), /Vertex-only service account[\s\S]+15 minutes[\s\S]+Google expires it/);
  assert.match(runnerDoc('setup'), /RUNNER_VERTEX_TARGETS/);
});
