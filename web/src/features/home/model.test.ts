import { describe, expect, it } from 'vitest';
import type { RunbookRow } from '../runbooks/catalog';
import {
  dailyRuns,
  elapsed,
  fireTime,
  quietFailedNote,
  todayRuns,
  failingStep,
  firstName,
  greeting,
  needsYou,
  ago,
  principalActor,
  progressOf,
  runActivity,
  runCounts,
  shortAge,
  startOfDay,
  waitingOn,
  type InboxTask,
  type RunDocument,
  type RunEntry,
  type StepView,
} from './model';

const names = {
  nameOf: (id: string) => ({ usr_alice: 'Alice', usr_bob: 'Bob' })[id] ?? id,
  teamName: (ref: string) => (ref === 'team_1' ? 'oncall' : ref),
};

function step(over: Partial<StepView> & Pick<StepView, 'id' | 'path' | 'status'>): StepView {
  return {
    schema_revision: 1,
    verdict: null,
    outputs: {},
    summary: null,
    actor: null,
    claim: null,
    attempt: 1,
    satisfied: false,
    exhausted: false,
    checklist: [],
    amended: false,
    amendments: [],
    stale: false,
    eligible_recipients: [],
    ...over,
  } as StepView;
}

function doc(status: string, steps: StepView[], titles: Record<string, string> = {}): RunDocument {
  return {
    header: {
      schema_revision: 1,
      id: 'exe_1',
      runbook_version_id: 'rbv_1',
      runbook_version: 1,
      title: 'Run',
      status,
      phase: 'ordinary',
      result: null,
      pending_terminal_status: null,
      inputs: {},
      fields: {},
      tags: [],
      steps,
      amended: false,
      amendments: [],
      stale_step_ids: [],
      rerun_of: null,
      resume_brief: null,
      progress: { done: 2, total: 7 },
    },
    sections: steps.map((s) => ({
      step_run_id: s.id,
      path: s.path,
      title: titles[s.id] ?? '',
      status: s.status,
      kind: s.kind,
      summary: s.summary,
      verdict: s.verdict,
    })),
  } as unknown as RunDocument;
}

describe('the greeting strip', () => {
  it('greets by the time of day and the first name', () => {
    expect(greeting(new Date(2026, 10, 14, 8))).toBe('Good morning');
    expect(greeting(new Date(2026, 10, 14, 13))).toBe('Good afternoon');
    expect(greeting(new Date(2026, 10, 14, 20))).toBe('Good evening');
    expect(greeting(new Date(2026, 10, 14, 2))).toBe('Good evening');
    expect(firstName('  Ada Lovelace ')).toBe('Ada');
    expect(firstName(undefined)).toBe('');
  });

  it('writes ages and elapsed times compactly', () => {
    expect(shortAge(30_000)).toBe('now');
    expect(shortAge(4 * 60_000)).toBe('4m');
    expect(shortAge(3 * 3_600_000)).toBe('3h');
    expect(shortAge(3 * 86_400_000)).toBe('3d');
    expect(ago(10_000)).toBe('just now');
    expect(ago(5 * 60_000)).toBe('5m ago');
    expect(elapsed(45_000)).toBe('45s');
    expect(elapsed(65 * 60_000)).toBe('1h 05m');
    expect(elapsed(50 * 3_600_000)).toBe('2d 2h');
  });
});

describe('needs you', () => {
  const task = (id: string, project: string, since?: number): InboxTask =>
    ({
      project_id: project,
      execution_id: `exe_${id}`,
      step_run_id: `sr_${id}`,
      path: id,
      title: id,
      kind: 'human_task',
      ...(since === undefined ? {} : { since_ms: since }),
    }) as InboxTask;

  it("keeps this project's tasks, oldest first, top five", () => {
    const tasks = [
      task('a', 'prj_1', 30),
      task('b', 'prj_2', 1),
      task('c', 'prj_1', 10),
      task('d', 'prj_1'),
      ...['e', 'f', 'g'].map((id, i) => task(id, 'prj_1', 40 + i)),
    ];
    const { items, total } = needsYou(tasks, 'prj_1');
    expect(total).toBe(6);
    expect(items.map((t) => t.path)).toEqual(['c', 'a', 'e', 'f', 'g']);
  });
});

describe('what a run waits on', () => {
  it('names approvals, signals, timers, child runs, agents and people', () => {
    const steps = [
      step({ id: 'sr_g', path: 'preflight', status: 'claimed', kind: 'group' }),
      step({
        id: 'sr_a',
        path: 'cab_review',
        status: 'waiting',
        kind: 'approval',
        eligible_recipients: [{ kind: 'team', reference: 'team_1' }],
      }),
      step({
        id: 'sr_s',
        path: 'customer_go',
        status: 'waiting',
        kind: 'wait',
        wait: { signal: 'release_email_sent' },
      }),
      step({
        id: 'sr_h',
        path: 'status_page',
        status: 'ready',
        kind: 'task',
        eligible_recipients: [
          { kind: 'user', reference: 'usr_alice' },
          { kind: 'user', reference: 'usr_bob' },
        ],
      }),
      step({ id: 'sr_done', path: 'build', status: 'succeeded', kind: 'task' }),
    ];
    const items = waitingOn(doc('waiting', steps, { sr_s: 'Customer confirms' }), names, 5);
    expect(items.map((i) => [i.kind, i.label])).toEqual([
      ['approval', 'Approval · team oncall'],
      ['signal', 'Signal · release_email_sent'],
      ['person', 'Alice +1'],
    ]);
    expect(items[1]?.step).toBe('Customer confirms');
  });

  it('names a claimed agent or person, an unassigned task, a child and a timer', () => {
    const agent = step({
      id: 'sr_1',
      path: 'canary_us',
      status: 'claimed',
      kind: 'task',
      actor: { kind: 'agent', name: 'deploy-bot', principal_id: 'p', session_id: 's' },
    });
    const person = step({
      id: 'sr_2',
      path: 'notes',
      status: 'claimed',
      kind: 'task',
      actor: { kind: 'human', user_id: 'usr_bob' },
    });
    const free = step({ id: 'sr_3', path: 'check', status: 'ready', kind: 'task' });
    const child = step({ id: 'sr_4', path: 'verify', status: 'waiting', kind: 'call' });
    const timer = step({
      id: 'sr_5',
      path: 'pause',
      status: 'waiting',
      kind: 'wait',
      wait: { until_ms: 0 },
    });
    const items = waitingOn(doc('running', [agent, person, free, child, timer]), names, 5);
    expect(items.map((i) => i.kind)).toEqual(['agent', 'person', 'agent', 'child', 'timer']);
    expect(items[0]?.label).toBe('Agent · deploy-bot');
    expect(items[1]?.label).toBe('Bob');
    expect(items[2]?.label).toBe('Agent');
  });

  it("names a ready task's assignee, or a person or anyone when it names none", () => {
    const team = step({
      id: 'sr_1',
      path: 'sso',
      status: 'ready',
      kind: 'task',
      actor_kind: 'human',
      assignees: [{ kind: 'team', reference: 'team_1' }],
    });
    const person = step({
      id: 'sr_2',
      path: 'p',
      status: 'ready',
      kind: 'task',
      actor_kind: 'human',
    });
    const anyone = step({
      id: 'sr_3',
      path: 'a',
      status: 'ready',
      kind: 'task',
      actor_kind: 'any',
    });
    const bot = step({ id: 'sr_4', path: 'b', status: 'ready', kind: 'task', actor_kind: 'agent' });
    const items = waitingOn(doc('waiting', [team, person, anyone, bot]), names, 5);
    expect(items.map((i) => [i.kind, i.label])).toEqual([
      ['person', 'team oncall'],
      ['person', 'A person'],
      ['person', 'Anyone'],
      ['agent', 'Agent'],
    ]);
  });

  it('says paused or queued for the whole run, and caps the list', () => {
    expect(waitingOn(doc('paused', []), names)).toEqual([{ kind: 'paused', label: 'Paused' }]);
    expect(waitingOn(doc('queued', []), names)).toEqual([{ kind: 'queued', label: 'Queued' }]);
    const many = ['a', 'b', 'c', 'd'].map((id) =>
      step({ id, path: id, status: 'waiting', kind: 'wait', wait: { signal: id } }),
    );
    expect(waitingOn(doc('waiting', many), names)).toHaveLength(3);
  });

  it('reads progress from the view, and none before the run starts', () => {
    expect(progressOf(doc('running', []).header)).toEqual({ done: 2, total: 7 });
    const queued = { ...doc('queued', []).header, progress: undefined };
    expect(progressOf(queued)).toBeNull();
  });
});

describe('the failing step', () => {
  it('is the first failed leaf, with its summary', () => {
    const document = doc(
      'failed',
      [
        step({ id: 'sr_g', path: 'rollout', status: 'failed', kind: 'foreach' }),
        step({
          id: 'sr_1',
          path: 'region_health',
          status: 'timed_out',
          kind: 'task',
          summary: '5xx at 4%',
        }),
        step({ id: 'sr_2', path: 'later', status: 'failed', kind: 'task' }),
      ],
      { sr_1: 'Error rate in eu-west' },
    );
    expect(failingStep(document)).toEqual({
      title: 'Error rate in eu-west',
      path: 'region_health',
      status: 'timed_out',
      summary: '5xx at 4%',
    });
  });

  it('falls back to a check whose verdict failed the run', () => {
    const document = doc(
      'failed',
      [
        step({ id: 'sr_1', path: 'health', status: 'succeeded', kind: 'task', verdict: 'pass' }),
        step({
          id: 'sr_g',
          path: 'error_rates',
          status: 'succeeded',
          kind: 'foreach',
          verdict: 'fail',
        }),
        step({
          id: 'sr_2',
          path: 'rate',
          status: 'succeeded',
          kind: 'task',
          verdict: 'fail',
          summary: 'eu-west 2.41%',
        }),
        step({ id: 'sr_e', path: 'checks_failed', status: 'succeeded', kind: 'end' }),
      ],
      { sr_2: 'Error rate in eu-west' },
    );
    expect(failingStep(document)).toMatchObject({
      title: 'Error rate in eu-west',
      summary: 'eu-west 2.41%',
    });
  });

  it('falls back to a failed container, or nothing', () => {
    const only = doc('failed', [
      step({ id: 'sr_g', path: 'rollout', status: 'failed', kind: 'foreach' }),
    ]);
    expect(failingStep(only)?.path).toBe('rollout');
    expect(failingStep(doc('failed', []))).toBeNull();
  });
});

describe('counts and activity', () => {
  const now = new Date(2026, 10, 14, 15).getTime();
  const today = startOfDay(now);
  const run = (id: string, status: string, created: number, completed?: number): RunEntry =>
    ({
      id,
      url: `/v1/executions/${id}`,
      web_url: `/x/${id}`,
      runbook_id: 'rb_1',
      status,
      title: id,
      tags: [],
      fields: {},
      created_by: 'user:usr_alice',
      created_at_ms: created,
      completed_at_ms: completed ?? null,
    }) as RunEntry;
  const entries = [
    run('a', 'succeeded', today + 1000, today + 5000),
    run('b', 'failed', today + 2000, today + 3000),
    run('c', 'waiting', today + 4000),
    run('d', 'cancelled', today + 6000, today + 7000),
    run('e', 'succeeded', today - 86_400_000, today - 80_000_000),
    run('f', 'succeeded', today - 30 * 86_400_000, today - 30 * 86_400_000 + 1),
  ];

  it("counts today's runs by status", () => {
    expect(runCounts(entries, today)).toEqual({
      started: 4,
      succeeded: 1,
      failed: 1,
      open: 1,
      other: 1,
    });
  });

  it('draws a daily series only from a complete read', () => {
    expect(dailyRuns(entries, now, 7, true)).toEqual([0, 0, 0, 0, 0, 1, 4]);
    expect(dailyRuns(entries, now, 7, false)).toBeNull();
  });

  it('lists starts and finishes newest first', () => {
    const items = runActivity(entries, 4);
    expect(items.map((i) => i.key)).toEqual(['d:finished', 'd:started', 'a:finished', 'c:started']);
  });

  it('names who started a run', () => {
    expect(principalActor('user:usr_alice', names.nameOf)).toEqual({
      kind: 'person',
      label: 'Alice',
    });
    expect(principalActor('human:usr_alice', names.nameOf)).toEqual({
      kind: 'person',
      label: 'Alice',
    });
    expect(principalActor('agent:deploy-bot', names.nameOf)).toEqual({
      kind: 'agent',
      label: 'deploy-bot',
    });
    expect(principalActor('service:tok_1', names.nameOf).kind).toBe('service');
    expect(principalActor('execution:fam_1', names.nameOf).kind).toBe('run');
    expect(principalActor('weird', names.nameOf)).toEqual({ kind: 'system', label: 'weird' });
  });
});

// UI-2 review round 1.
describe('review round 1', () => {
  const at = (day: number, hour: number, minute = 0) =>
    new Date(2026, 10, day, hour, minute).getTime();
  const now = at(13, 20); // Friday 13 November 2026, 20:00 local
  const time = (ms: number) =>
    new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

  it("names a timer's day when it is not today", () => {
    expect(fireTime(at(13, 21, 30), now)).toBe(time(at(13, 21, 30)));
    const monday = fireTime(at(16, 9), now);
    expect(monday).toBe(
      `${new Date(at(16, 9)).toLocaleDateString([], { weekday: 'short' })} ${time(at(16, 9))}`,
    );
    const far = fireTime(at(30, 9), now);
    expect(far).toBe(
      `${new Date(at(30, 9)).toLocaleDateString([], { month: 'short', day: 'numeric' })} ${time(at(30, 9))}`,
    );
    // A timer due yesterday that has not fired yet is not "today".
    expect(fireTime(at(12, 9), now)).not.toBe(time(at(12, 9)));

    const timer = step({
      id: 'sr_t',
      path: 'cool_off',
      status: 'waiting',
      kind: 'wait',
      wait: { until_ms: at(16, 9) },
    });
    expect(waitingOn(doc('waiting', [timer]), names, 3, now)[0]?.label).toBe(`Timer · ${monday}`);
  });

  const entry = (id: string, created: number) =>
    ({
      id,
      url: `/v1/executions/${id}`,
      web_url: `/x/${id}`,
      runbook_id: 'rb_1',
      status: 'succeeded',
      title: id,
      tags: [],
      fields: {},
      created_by: 'user:usr_alice',
      created_at_ms: created,
      completed_at_ms: created + 1,
    }) as RunEntry;

  it("marks today's count as a lower bound when the capped read stops inside today", () => {
    const busy = Array.from({ length: 300 }, (_, i) => entry(`r${i}`, now - i * 1000));
    expect(todayRuns({ entries: busy, complete: false }, now)).toMatchObject({
      started: 300,
      capped: true,
    });
    // The cap was reached, but the oldest run read is from before today:
    // every one of today's runs was read.
    const spread = [entry('t', now - 1000), entry('y', at(12, 9))];
    expect(todayRuns({ entries: spread, complete: false }, now)).toMatchObject({
      started: 1,
      capped: false,
    });
    expect(todayRuns({ entries: busy, complete: true }, now).capped).toBe(false);
  });

  const row = (status?: string) =>
    ({
      runbook_id: `rb_${status ?? 'never'}`,
      slug: 's',
      origin: 'ui',
      archived: false,
      tags: [],
      draft_ahead: false,
      active_runs: 0,
      ...(status ? { last_run: { id: 'exe', status, created_at_ms: now } } : {}),
    }) as RunbookRow;

  it('says every last run passed only when it knows so', () => {
    expect(
      quietFailedNote({ rows: [row('succeeded'), row('running'), row()], isError: false }),
    ).toBe("Every runbook's last run passed or is still going.");
    expect(quietFailedNote({ rows: undefined, isError: true })).toMatch(/could not be checked/i);
    expect(quietFailedNote({ rows: undefined, isError: false })).toBeNull();
    const withheld = { ...row('succeeded'), active_runs: null, last_run: null } as RunbookRow;
    expect(quietFailedNote({ rows: [withheld], isError: false })).toMatch(/your access/i);
    expect(quietFailedNote({ rows: [row('succeeded'), row('cancelled')], isError: false })).toBe(
      "1 runbook's last run was cancelled.",
    );
    expect(
      quietFailedNote({
        rows: [row('cancelled'), { ...row('cancelled'), runbook_id: 'x' }],
        isError: false,
      }),
    ).toBe("2 runbooks' last runs were cancelled.");
  });
});
