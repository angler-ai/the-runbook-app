import type { components } from '@runbook/runner-shared/core-client';
import { expect, it } from 'vitest';
import { stepPrompt } from '../src/loop/prompt.ts';

const step = { template: { instructions: 'Count errors.', values: { threshold: 4 } }, checklist: [] } as unknown as components['schemas']['ClaimedStepRead'];
const overview = {
  title: 'Tuesday rollout', project: 'operations', runbook_slug: 'deploy-check',
  step: { id: 'verify', title: 'Verify deployment' },
};

it('puts a stable orientation before instructions and the run identity in untrusted input', () => {
  const prompt = stepPrompt(step, 'Use metrics.', overview);
  expect(prompt.system).toContain('You are working a step in The Runbook');
  expect(prompt.system).toContain('You cannot approve anything');
  expect(prompt.system).toContain('untrusted data');
  expect(prompt.system.indexOf('You are working')).toBeLessThan(prompt.system.indexOf('Use metrics.'));
  expect(prompt.system.indexOf('Use metrics.')).toBeLessThan(prompt.system.indexOf('Count errors.'));
  expect(prompt.system).toBe(stepPrompt(step, 'Use metrics.', { ...overview, title: 'Wednesday rollout' }).system);
  expect(JSON.parse(prompt.input)).toMatchObject({
    run_context: { step_id: 'verify', step_title: 'Verify deployment', run_title: 'Tuesday rollout',
      runbook: 'deploy-check', project: 'operations' },
    values: { threshold: 4 },
  });
});

it('loads agent skill bodies after step skills and deduplicates by hash', () => {
  const prompt = stepPrompt({
    ...step,
    skills: [{ hash: 'shared', body: 'Step body', files: [] }],
  }, 'Use metrics.', overview, [
    { hash: 'shared', body: 'Duplicate body' },
    { hash: 'agent', body: 'Agent skill body' },
  ]);
  expect(prompt.system).toContain('Agent skill body');
  expect(prompt.system.indexOf('Step body')).toBeLessThan(prompt.system.indexOf('Agent skill body'));
  expect(prompt.system).not.toContain('Duplicate body');
});
