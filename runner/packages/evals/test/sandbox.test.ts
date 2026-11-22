import { expect, it } from 'vitest';
import { runnerSubject } from '../src/baselines.ts';
import { grade, selectTasks } from '../src/tasks.ts';
import { nativeReply, reply, scripted } from './scripted.ts';

const available = Boolean(process.env.DOCKER_HOST || process.env.RUNNER_TEST_DOCKER_SOCKET);
it.skipIf(!available).each(['current', 'bash', 'new-loop'] as const)('runs %s in the hardened Docker sandbox on fake responses', async (subject) => {
  const task = selectTasks('config/timeout')[0]!;
  const command = 'sed -i "/^\\[prod\\]/,$ s/timeout = 1/timeout = 2/" service.toml';
  const action = subject === 'current' ? { tool: 'shell', arguments: { command } } : { bash: command };
  const final = { outputs: task.expected.outputs, checklist: task.step.checklist, summary: 'Updated only the prod timeout.' };
  const model = subject === 'new-loop' ? scripted(nativeReply('bash', { command }, 'edit'),
    nativeReply('submit_result', { ...final, evidence: [{ claim: 'Edited prod', tool_call_ids: ['edit'] }], unverified: [] }, 'finish')) :
    scripted(reply(action), reply(final));
  const result = await runnerSubject(subject, () => ({ model, cost: () => 0 })).run(task, new AbortController().signal);
  expect(result.usage).toMatchObject({ turns: 2, toolCalls: 1, toolErrors: 0 });
  expect(grade(task, result.answer, result.files, result.observed, result.toolNames)).toMatchObject({ success: true });
});

it.skipIf(!available)('preserves the contents of a clipped-size fixture through sandbox provisioning and snapshots', async () => {
  const task = selectTasks('clipping/delivery')[0]!;
  const command = 'grep "^ERROR" large.log';
  const fake = scripted(reply({ tool: 'shell', arguments: { command: 'cat large.log' } }),
    reply({ tool: 'shell', arguments: { command } }),
    reply({ outputs: task.expected.outputs, checklist: task.step.checklist, summary: 'Found the expired delivery lease.' }));
  let calls = 0;
  const model = { ...fake, async doStream(request: Parameters<typeof fake.doStream>[0]) {
    if (++calls === 2) {
      const prompt = JSON.stringify(request.prompt);
      expect(prompt).toContain('[TRUNCATED]');
      expect(prompt).not.toContain('reason=expired_lease');
    }
    return fake.doStream(request);
  } };
  const result = await runnerSubject('current', () => ({ model, cost: () => 0 })).run(task, new AbortController().signal);
  expect(result.files).toEqual(task.files);
  expect(result.answer.status).toBe('submitted');
  expect(result.usage).toMatchObject({ turns: 3, toolErrors: 0 });
});

it.skipIf(!available).each(['current', 'bash', 'new-loop'] as const)('replays a recorded %s run against a fresh Docker fixture', async (subject) => {
  const { hashValue } = await import('../src/replay.ts');
  const { replayTrial } = await import('../src/replay-run.ts');
  const task = selectTasks('logs/disk')[0]!;
  const command = 'cat service.log';
  const action = subject === 'current' ? { tool: 'shell', arguments: { command } } : { bash: command };
  const final = { outputs: task.expected.outputs, checklist: task.step.checklist, summary: 'The disk error preceded the retries.' };
  const model = subject === 'new-loop' ? scripted(nativeReply('bash', { command }, 'read'),
    nativeReply('submit_result', { ...final, evidence: [{ claim: 'Found the disk error', tool_call_ids: ['read'] }], unverified: [] }, 'finish')) :
    scripted(reply(action), reply(final));
  const signal = new AbortController().signal;
  const result = await runnerSubject(subject, () => ({ model, cost: () => 0 })).run(task, signal);
  const trace = { task: task.id, subject, modelId: model.modelId, calls: result.calls!, answer: result.answer,
    toolNames: result.toolNames!, toolHashes: result.toolHashes!, workspaceHash: hashValue(result.files) };
  await expect(replayTrial(trace, signal)).resolves.toBeUndefined();
  await expect(replayTrial({ ...trace, workspaceHash: 'changed' }, signal)).rejects.toThrow('workspace changed');
});

it.skipIf(!available)('persists Bash state, manages background commands and applies guarded native file edits', async () => {
  const { openSandbox } = await import('../src/workspace.ts');
  const task = selectTasks('logs/disk')[0]!;
  const signal = new AbortController().signal;
  const sandbox = await openSandbox(task, signal);
  async function call(name: string, args: Record<string, unknown>) {
    return await sandbox.native.tools.find((tool) => tool.name === name)!.call(args, signal) as Record<string, unknown>;
  }
  try {
    expect(await call('bash', { command: 'mkdir scratch; cd scratch; export RUNBOOK_FIXTURE=kept' })).toMatchObject({ exit_code: 0 });
    expect(await call('bash', { command: 'printf "%s %s" "${PWD##*/}" "$RUNBOOK_FIXTURE"' }))
      .toMatchObject({ stdout: 'scratch kept', exit_code: 0 });
    expect(await call('bash', { command: 'printf partial; sleep 10', timeout_s: 1 }))
      .toMatchObject({ stdout: 'partial', timed_out: true, warning: expect.stringContaining('Outcome unknown') });
    expect(await call('bash', { command: 'rmdir /workspace/scratch' })).toMatchObject({ exit_code: 0 });
    expect(await call('bash', { command: 'pwd' })).toMatchObject({ stdout: '/workspace\n', exit_code: 0 });
    expect(await call('create', { path: 'nested/deeper/edit.txt', content: 'initial' })).toMatchObject({ ok: true });
    expect(await call('bash', { command: 'stat -c %a nested/deeper/edit.txt' })).toMatchObject({ stdout: '644\n' });
    expect(await call('bash', { command: 'truncate -s 131073 nested/deeper/edit.txt' })).toMatchObject({ exit_code: 0 });
    expect(await call('str_replace', { path: 'nested/deeper/edit.txt', old_str: 'initial', new_str: 'edited' }))
      .toMatchObject({ error: 'This file exceeds the 128 KiB edit limit.' });
    expect(await call('bash', { command: 'rm nested/deeper/edit.txt' })).toMatchObject({ exit_code: 0 });
    expect(await call('str_replace', { path: 'nested/deeper/edit.txt', old_str: 'initial', new_str: 'edited' }))
      .toMatchObject({ error: expect.stringContaining('edit.txt') });
    expect(await call('bash', { command: 'git init -q; printf "ignored.txt\\n" > .gitignore; touch ignored.txt' })).toMatchObject({ exit_code: 0 });
    expect(await call('glob', { pattern: '*.txt' })).toMatchObject({ paths: ['ignored.txt'] });
    expect(await call('glob', { pattern: '*.txt', respect_ignore: true })).toMatchObject({ paths: [] });
    expect(await call('grep', { pattern: 'disk', ignore_case: true, context: 1 })).toMatchObject({ matches: expect.stringContaining('09:00') });
    expect(await call('str_replace', { path: 'service.log', old_str: 'cache cold', new_str: 'cache warm' }))
      .toMatchObject({ error: expect.stringContaining('View or create') });
    expect(await call('view', { path: 'service.log', start_line: 2, end_line: 2 }))
      .toMatchObject({ content: expect.stringMatching(/^2\t09:01 ERROR/) });
    expect(await call('bash', { command: 'printf "09:03 INFO later\\n" >> /workspace/service.log' })).toMatchObject({ exit_code: 0 });
    expect(await call('str_replace', { path: 'service.log', old_str: 'cache cold', new_str: 'cache warm' }))
      .toMatchObject({ ok: true, file_changed_since_view: true });
    expect(await call('glob', { pattern: '*.log' })).toMatchObject({ paths: ['service.log'] });
    expect(await call('grep', { pattern: 'INFO later' })).toMatchObject({ matches: expect.stringContaining(':4:09:03 INFO later') });
    const artifact = await sandbox.native.storeResult('fixture', 'sample result data', signal);
    expect(await call('view', { path: artifact })).toMatchObject({ content: '1\tsample result data' });
    expect(await call('grep', { pattern: 'sample result data' })).toMatchObject({ matches: '' });
    expect(await call('grep', { pattern: 'sample result data', path: artifact }))
      .toMatchObject({ matches: expect.stringContaining('sample result data') });
    expect(await call('bash', { command: 'seq 2500 > many.txt' })).toMatchObject({ exit_code: 0 });
    const whole = await call('view', { path: 'many.txt' });
    expect(String(whole.content).split('\n')).toHaveLength(2000);
    expect(String(whole.content).endsWith('2000\t2000')).toBe(true);
    const longLine = 'é'.repeat(39_999) + 'Z';
    expect(await call('create', { path: 'long.txt', content: longLine })).toMatchObject({ ok: true });
    const firstPage = await call('view', { path: 'long.txt', start_line: 1, end_line: 1 });
    expect(firstPage).toMatchObject({ content: '1\t' + longLine.slice(0, 8000), next_column: 8001 });
    const lastPage = await call('view', { path: 'long.txt', start_line: 1, end_line: 1, start_column: 32001 });
    expect(lastPage).toMatchObject({ content: '1\t' + longLine.slice(32000) });
    expect(lastPage.next_column).toBeUndefined();
    expect(await call('bash_background', { command: 'printf ready; sleep 30' })).toMatchObject({ job_id: '1' });
    let output: Record<string, unknown> = {};
    for (let i = 0; i < 5 && output.output !== 'ready'; i++) {
      output = await call('bash_output', { job_id: '1' });
    }
    expect(output).toMatchObject({ running: true, output: 'ready' });
    expect(await call('bash_output', { job_id: '1' })).toMatchObject({ output: '' });
    expect(await call('bash_kill', { job_id: '1' })).toMatchObject({ stopped: true });
  } finally {
    await sandbox.close();
  }
}, 15_000);

it.skipIf(!available)('caps background logs while letting the job complete and stores the same Bash text that was clipped', async () => {
  const { openSandbox } = await import('../src/workspace.ts');
  const { runNativeAgent } = await import('../../worker/src/loop/native.ts');
  const signal = new AbortController().signal;
  const sandbox = await openSandbox(selectTasks('logs/disk')[0]!, signal);
  const tools = sandbox.native.tools;
  const call = (name: string, args: Record<string, unknown>) =>
    tools.find((tool) => tool.name === name)!.call(args, signal) as Promise<Record<string, unknown>>;
  try {
    expect(await call('bash_background', { command: 'head -c 1048576 /dev/zero | tr "\\000" x; touch done' }))
      .toMatchObject({ job_id: '1' });
    let size = 0;
    let tail = '';
    let job: Record<string, unknown>;
    for (let i = 0; i < 16; i++) {
      job = await call('bash_output', { job_id: '1' });
      size += Buffer.byteLength(String(job.output));
      tail = String(job.output) || tail;
      if (!job.running && job.output === '') break;
    }
    expect(size).toBeLessThan(525_000);
    expect(tail).toContain('[Background log limit reached]');
    expect(await call('bash', { command: 'test -f done' })).toMatchObject({ exit_code: 0 });

    expect(await call('bash', { command: 'printf HEAD; yes x | head -c 300000; printf TAIL' }))
      .toMatchObject({ stdout: expect.stringMatching(/^HEAD[x\n]+\n\[TRUNCATED\]\n[x\n]+TAIL$/), truncated: true });

    const fixture = scripted(nativeReply('bash', { command: 'printf "HEAD\\n"; yes middle | head -n 6000; printf "TAIL\\n"' }, 'large'),
      nativeReply('submit_result', { outputs: {}, summary: 'done', evidence: [], unverified: [] }, 'finish'));
    let path: string | undefined;
    const model = { ...fixture, async doStream(request: Parameters<typeof fixture.doStream>[0]) {
      const latest = request.prompt.at(-1);
      if (latest?.role === 'tool') {
        const part = latest.content[0];
        if (part?.type === 'tool-result' && part.output.type === 'text') {
          expect(part.output.value).toMatch(/HEAD\n[\s\S]*\[Clipped\. Full result: [^\]]+\][\s\S]*TAIL\n/);
          path = /Full result: ([^\]]+)/.exec(part.output.value)?.[1];
        }
      }
      return fixture.doStream(request);
    } };
    await expect(runNativeAgent({ model, tools, signal, system: 'Fixture', input: 'Run the fixture', outputSchema: undefined,
      storeResult: sandbox.native.storeResult,
      limits: { maxTurns: 5, maxTokens: 1000, maxOutputTokens: 1000, timeoutMs: 5000, idleMs: 4000, toolMs: 1000, stallMs: 1000 },
    })).resolves.toMatchObject({ status: 'succeeded' });
    expect(path).toMatch(/^\/runbook-results\/result-/);
    expect(await call('view', { path, start_line: 1, end_line: 2 })).toMatchObject({ content: '1\tstdout:\n2\tHEAD' });
    expect(await call('view', { path, start_line: 6003, end_line: 6003 })).toMatchObject({ content: '6003\tTAIL' });
  } finally {
    await sandbox.close();
  }
}, 20_000);

it.skipIf(!available)('requires an actual successful test command after both fixture edits', async () => {
  const challenge = selectTasks('tested-edit/webhook')[0]!;
  const command = challenge.expected.verification!.command;
  const final = { outputs: challenge.expected.outputs, checklist: challenge.step.checklist, summary: 'Both prod limits verified.' };
  const model = scripted(
    reply({ bash: command }),
    reply({ bash: 'cat request.txt' }),
    reply({ bash: "sed -i 's/^prod_limit=7$/prod_limit=20/' service.conf" }),
    reply({ bash: "sed -i 's/| prod | 7 |/| prod | 20 |/' operations.md" }),
    reply({ bash: command }),
    reply(final));
  const result = await runnerSubject('bash', () => ({ model, cost: () => 0 })).run(challenge, new AbortController().signal);
  expect(result.toolNames?.filter((name) => name === 'verified_command')).toEqual(['verified_command']);
  expect(result.usage).toMatchObject({ toolCalls: 5, toolErrors: 1 });
  expect(grade(challenge, result.answer, result.files, result.observed, result.toolNames).success).toBe(true);
}, 15_000);
