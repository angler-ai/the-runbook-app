import { expect, test } from 'vitest';
import { renderResult } from '../src/loop/native-results.ts';

test('str_replace context can be copied verbatim into old_str', () => {
  const context = 'first\n  second\n\tthird';
  const rendered = renderResult('str_replace', { context });
  expect(rendered).toBe('context:\nfirst\n  second\n\tthird');
  expect('before\n' + context + '\nafter').toContain(rendered.slice('context:\n'.length));
});

test('bash_output preserves output indentation', () => {
  expect(renderResult('bash_output', { output: 'first\n\tsecond', running: true }))
    .toBe('output:\nfirst\n\tsecond\nrunning: true');
});

test('bash reports the duration of a timeout', () => {
  expect(renderResult('bash', { stdout: '', stderr: '', exit_code: 124, timed_out: true, timeout_s: 7 }))
    .toContain('\nTimed out after 7 s');
});

test('a clipped view directs the model to the source range', async () => {
  const { presentResult } = await import('../src/loop/native-results.ts');
  const rendered = presentResult('x'.repeat(2000), 128, 'call 1', '/runbook-results/view.txt', undefined, '', 'view');
  expect(rendered).toContain('Read a smaller range');
  expect(rendered).not.toContain('/runbook-results/view.txt');
});
