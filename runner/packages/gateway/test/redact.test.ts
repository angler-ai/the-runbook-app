import { loadRedaction } from '@runbook/runner-shared/redact';
import type { GrantClaims } from '@runbook/runner-shared/contracts/core';
import { expect, it } from 'vitest';
import { stepRedactionRoute } from '../src/redact-route.ts';
import { MaterialStore } from '../src/material.ts';

it('only accepts bounded transcript chunks for the authenticated worker’s live claim', async () => {
  const now = Date.now();
  const claims = { kind: 'capability', exp: Math.ceil(now / 1000) + 500,
    sub: 'step', runner_id: 'mine', claim_id: 'claim', generation: 1 } as GrantClaims;
  const store = new MaterialStore(() => now);
  try {
    const route = stepRedactionRoute({ verifyCapability: async () => claims }, store,
      await loadRedaction(), undefined, () => now);
    const chunk = { capability: 'capability', step_run_id: 'step', role: 'input', value: 'task' };
    const send = (body: unknown, runnerId = 'mine') => route.handle({ caller: 'worker', params: {}, body, runnerId });
    await expect(send(chunk, 'other')).rejects.toMatchObject({ status: 403 });
    await expect(send({ ...chunk, step_run_id: 'other' })).rejects.toMatchObject({ status: 403 });
    await expect(send({ ...chunk, role: 'guess' })).rejects.toMatchObject({ status: 422 });
    await expect(send(chunk)).resolves.toEqual({ body: { value: 'task' } });
    await expect(send(chunk)).rejects.toMatchObject({ status: 429 });
    const tool = { ...chunk, role: 'tool', value: { tool: 'read', result: 'result' } };
    for (let i = 0; i < 119; i++) await send(tool);
    await expect(send(tool)).rejects.toMatchObject({ status: 429 });
    store.endClaim(claims);
    await expect(send(tool)).rejects.toMatchObject({ status: 403 });
  } finally {
    store.clear();
  }
});
