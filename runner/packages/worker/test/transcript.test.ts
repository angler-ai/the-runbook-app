import { createTestDatabase, hasTestDatabase } from '@runbook/runner-shared/test-utils/pg';
import { loadRedaction } from '@runbook/runner-shared/redact';
import { expect, it } from 'vitest';
import { transcriptWriter } from '../src/transcript.ts';
import { transcriptEvidence } from '../src/evidence.ts';
import { gatewayResources } from '../../api/src/resources/gateway.ts';
import { redactStepValue, stepRedactionRoute } from '../../gateway/src/redact-route.ts';
import { MaterialStore } from '../../gateway/src/material.ts';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';

const claims = {
  sub: 'step', group_id: 'group', runner_id: 'runner', project_id: 'project', agent_id: 'agent',
  claim_id: 'claim', generation: 1, kind: 'capability', exp: 10, model_profile: null, max_tokens: null, timeout_ms: null, step_deadline_ms: null, connections: ['credential'],
} as GrantClaims;

it.skipIf(!hasTestDatabase)('transcript_redacted_before_write', async () => {
  const db = await createTestDatabase();
  const secret = 'fixture literal with no token shape';
  const store = new MaterialStore(() => 0);
  store.put({ release_id: 'release', step_run_id: 'step', claim_id: 'claim', generation: 1, runner_id: 'runner',
    credential_id: 'credential', delivery: 'proxy', binding: {}, material: { fields: { key: secret } },
    expires_at: new Date(5000).toISOString() });
  try {
    const wasm = await loadRedaction();
    const verifier = { verifyCapability: async () => claims };
    const api = gatewayResources(db.pool('api'), verifier).find(({ path }) => path === '/internal/transcripts')!;
    const gateway = stepRedactionRoute(verifier, store, wasm, undefined, () => 0, () => true, async (body) => {
      await api.handle({ body, caller: 'gateway', params: {} });
    });
    const call = (runnerId: string, body: unknown) => gateway.handle({ body, runnerId, caller: 'worker', params: {} });
    await expect(call('other-runner', { capability: 'fixture', step_run_id: 'step', role: 'tool', value: secret }))
      .rejects.toMatchObject({ status: 403 });
    // A compromised worker has to go through gateway redaction and cannot choose attribution.
    await call('runner', { capability: 'fixture', step_run_id: 'step', role: 'tool', value: secret,
      project_id: 'other-project' });
    const writer = transcriptWriter(async (role, value) => {
      await call('runner', { capability: 'fixture', step_run_id: 'step', role, value });
    }, wasm, async (value) => redactStepValue(store, claims, wasm, value));
    const safe = await writer.record('tool', { result: secret, url: 'https://fixture/?token=abcdefghijklmnop' });
    expect(JSON.stringify(safe)).not.toContain(secret);
    const { rows } = await db.admin().query('SELECT entry, project_id, runner_id FROM transcripts ORDER BY seq');
    expect(rows).toHaveLength(2);
    expect(rows[0]).toMatchObject({ project_id: 'project', runner_id: 'runner' });
    expect(JSON.stringify(rows)).not.toContain(secret);
    expect(JSON.stringify(rows)).not.toContain('abcdefghijklmnop');
    expect(writer.excerpt()).not.toContain(secret);
  } finally {
    store.clear();
    await db.drop();
  }
});

it('excerpt_evidence_under_8kb_with_link', () => {
  const evidence = transcriptEvidence('https://core.example', 'install', 'step', '🦆'.repeat(3000), undefined);
  expect(evidence.evidence).toEqual(['https://core.example/v1/runners/install/proxy/runner/v1/transcripts/step']);
  expect(Buffer.byteLength(evidence.logs.join(''))).toBeLessThan(8192);
  expect(evidence.logs[0]).not.toContain('\uFFFD');
});

it('clips tool results and keeps the excerpt limited to final output', async () => {
  const wasm = await loadRedaction();
  const writer = transcriptWriter(async () => {}, wasm, async (value) => value);
  await writer.record('system', 'internal instructions');
  await writer.record('input', 'submission');
  expect(writer.excerpt()).toBe('');
  await expect(writer.record('input', 'x'.repeat(40_000))).rejects.toMatchObject({ reason: 'input_too_large' });
  await expect(writer.record('system', 'x'.repeat(40_000))).rejects.toMatchObject({ reason: 'redaction_failed' });
  const clipped = await writer.record('tool', { result: 'data '.repeat(8000) });
  expect(clipped).toEqual(expect.stringMatching(/^\{"result":"data .+\[TRUNCATED\]$/s));
  await writer.record('output', { summary: 'Finished', outputs: { ok: true } });
  expect(writer.excerpt()).toContain('Finished');
  expect(writer.excerpt()).toContain('"ok":true');
  expect(writer.excerpt()).not.toContain('internal instructions');
  expect(writer.excerpt()).not.toContain('data');
});

// Release gate 2026-11-21: the core accepts only https evidence links, so a
// core on plain http (RUNNER_INSECURE_CORE_HTTP) got a link it refused.
it('links the transcript only when the core is on https', () => {
  expect(transcriptEvidence('http://runbook.localhost:8080', 'install', 'step', 'excerpt', undefined))
    .toEqual({ logs: ['excerpt'], evidence: [] });
  expect(transcriptEvidence('https://core.example', 'install', 'step', '', undefined).evidence)
    .toEqual(['https://core.example/v1/runners/install/proxy/runner/v1/transcripts/step']);
});
