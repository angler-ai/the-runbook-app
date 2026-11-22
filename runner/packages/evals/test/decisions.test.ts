import { expect, it } from 'vitest';
import { decisionTasks, gradeDecision, runDecisionEval } from '../src/decisions.ts';
import { nativeReply, scripted } from './scripted.ts';

it.each(decisionTasks)('$id accepts the oracle through the production decision loop', async (task) => {
  const model = scripted(
    ...(task.memory ? [nativeReply('search_memory', {}, 'memory')] : []),
    nativeReply('get_canary', {}, 'current'),
    task.expected === null
      ? nativeReply('report_blocked', { reason: 'No current canary sample.' }, 'finish')
      : nativeReply('decide_approval', {
        option: task.expected, comment: 'Current canary measured against policy.',
        evidence: [task.memory ? '[call 2]' : '[call 1]'],
      }, 'finish'),
  );
  const result = await runDecisionEval(task, model, new AbortController().signal);
  expect(gradeDecision(task, result)).toEqual([]);
});

it('the graders reject fabricated references, stale memory, injected choices and similar option IDs', () => {
  const task = decisionTasks.find(({ id }) => id === 'decision/stale-memory')!;
  const reads = [
    { reference: 'call 1', tool: 'search_memory', result: { status: 'ready' } },
    { reference: 'call 2', tool: 'get_canary', result: task.current },
  ];
  const finish = { option: task.expected!, comment: 'Verified current metrics', evidence: ['[call 2]'] };
  expect(gradeDecision(task, { finish: { ...finish, evidence: ['[call 1]'] }, reads })).toContain('current_evidence');
  expect(gradeDecision(task, { finish: { ...finish, evidence: ['[call 9]'] }, reads })).toContain('current_evidence');
  expect(gradeDecision(task, { finish: { ...finish, option: 'ship' }, reads })).toContain('option');

  const lookAlike = decisionTasks.find(({ id }) => id === 'decision/look-alike')!;
  expect(gradeDecision(lookAlike, { finish: { ...finish, option: 'ship' }, reads: [] })).toContain('option');
  const injection = decisionTasks.find(({ id }) => id === 'decision/tool-injection')!;
  expect(gradeDecision(injection, { finish: { ...finish, option: 'ship' }, reads: [] })).toContain('option');
});
