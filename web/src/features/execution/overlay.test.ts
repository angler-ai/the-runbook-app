import { describe, expect, it } from 'vitest';
import type { Outline } from '../../ui/types';
import releaseTrainJson from '../../ui/graph/fixtures/release-train.outline.json';
import {
  buildOverlay,
  defaultIteration,
  documentItems,
  duration,
  indexRecords,
  iterationKeys,
  iterationName,
  keyLabel,
  outcomeOf,
  parseStepPath,
  participants,
  type DocItem,
} from './overlay';
import { failedInPreflight, midRollout, RIVER, waitingOnApproval } from './fixtures/runs';

const outline = releaseTrainJson as Outline;
const names = {
  nameOf: (id: string) => (id === RIVER ? 'Riley Chen' : id),
  teamName: (ref: string) => (ref === 'team_oncall' ? 'oncall' : ref),
};

describe('step paths', () => {
  it('splits iteration scopes from the step id', () => {
    expect(parseStepPath('build')).toEqual([{ id: 'build' }]);
    expect(parseStepPath('rollout[0].settle#2.region_health')).toEqual([
      { id: 'rollout', key: '0' },
      { id: 'settle', key: '#2' },
      { id: 'region_health' },
    ]);
  });

  it('names an iteration by its item when the key is its index', () => {
    expect(iterationName('1', ['us-east', 'us-west'], false)).toBe('us-west');
    expect(iterationName('4', ['us-east'], false)).toBeUndefined();
    expect(iterationName('1', undefined, false)).toBeUndefined();
    expect(iterationName('eu%2Ewest', undefined, false)).toBe('eu.west');
  });

  it('reads an escaped foreach key and a loop key', () => {
    expect(keyLabel('eu%2Ewest')).toBe('eu.west');
    expect(keyLabel('#3')).toBe('3');
    expect(keyLabel('%E0%A4%A')).toBe('%E0%A4%A');
  });

  it('indexes each section with its node and scope', () => {
    const records = indexRecords(midRollout());
    const health = records.find((r) => r.path === 'rollout[0].settle#1.region_health');
    expect(health?.nodeId).toBe('region_health');
    expect(health?.scope).toEqual([
      { container: 'rollout', key: '0' },
      { container: 'settle', key: '#1' },
    ]);
    expect(health?.step?.id).toBe(health?.section.step_run_id);
    // Each iteration it runs in, by name.
    expect(health?.scopeNames).toEqual(['us-east', 'attempt 1']);
  });

  it('lists iteration keys in order and defaults to the open one', () => {
    const records = indexRecords(midRollout());
    expect(iterationKeys(records, 'rollout', [])).toEqual(['0', '1']);
    expect(defaultIteration(records, 'rollout', [])).toBe('1');
    // us-east has no open step: its latest iteration.
    expect(defaultIteration(records, 'settle', [{ container: 'rollout', key: '0' }])).toBe('#2');
  });
});

describe('the graph overlay', () => {
  it('shows each step run’s state, verdict and actor', () => {
    const { nodes } = buildOverlay(outline, waitingOnApproval(), { names });
    expect(nodes.get('changelog')).toMatchObject({
      state: 'succeeded',
      verdict: 'pass',
      actor: { kind: 'agent', label: 'release-agent · claude-sonnet' },
      ran: true,
    });
    expect(nodes.get('canary')).toMatchObject({ state: 'pending', ran: false });
    expect(nodes.get('canary')?.dimmed).toBeUndefined();
  });

  it('names what an open step waits on with Home’s badge', () => {
    const overlay = buildOverlay(outline, waitingOnApproval(), { names });
    expect(overlay.nodes.get('cab_review')).toMatchObject({
      state: 'waiting',
      badge: { kind: 'approval', label: 'Approval · team oncall' },
      current: true,
    });
    expect(overlay.currentId).toBe('cab_review');
    // A container running its children reads as running.
    expect(overlay.nodes.get('risk_gate')?.state).toBe('running');
  });

  it('dims the branches a switch did not take, and keeps the taken one', () => {
    const { nodes } = buildOverlay(outline, waitingOnApproval(), { names });
    expect(nodes.get('lead_signoff')).toMatchObject({ dimmed: true, stateLabel: 'Not taken' });
    expect(nodes.get('auto_ok')).toMatchObject({ dimmed: true });
    expect(nodes.get('hold_if_rejected')?.dimmed).toBeUndefined();
  });

  it('infers the taken branch from what ran when the view has no branch', () => {
    const doc = midRollout();
    for (const step of doc.header.steps) delete step.branch;
    const { nodes } = buildOverlay(outline, doc, { names });
    expect(nodes.get('cab_review')?.dimmed).toBe(true);
    expect(nodes.get('auto_ok')).toMatchObject({ state: 'succeeded' });
  });

  it('lights catch and finally lanes only when they ran', () => {
    const open = buildOverlay(outline, waitingOnApproval(), { names }).nodes;
    expect(open.get('preflight_failed')).toMatchObject({
      dimmed: true,
      note: 'Runs if something fails',
    });
    expect(open.get('retro_notes')).toMatchObject({ dimmed: true, note: 'Runs at the end' });
    const failed = buildOverlay(outline, failedInPreflight(), { names }).nodes;
    expect(failed.get('preflight_failed')).toMatchObject({ state: 'succeeded', ran: true });
    expect(failed.get('preflight_failed')?.dimmed).toBeUndefined();
    expect(failed.get('retro_notes')?.dimmed).toBeUndefined();
    // Never reached in a finished run.
    expect(failed.get('canary')).toMatchObject({ dimmed: true, stateLabel: 'Not reached' });
  });

  it('marks a step skipped by its condition', () => {
    const { nodes } = buildOverlay(outline, failedInPreflight(), { names });
    expect(nodes.get('build')).toMatchObject({
      state: 'skipped',
      dimmed: true,
      note: 'Skipped: condition false',
    });
  });

  it('counts iterations and shows the open one by default', () => {
    const { nodes } = buildOverlay(outline, midRollout(), { names });
    expect(nodes.get('rollout')?.iteration).toEqual({
      label: 'region 2 of 3',
      detail: 'us-west',
      index: 1,
      keys: ['0', '1'],
    });
    expect(nodes.get('settle')?.iteration).toMatchObject({ label: 'attempt 1 of 4', index: 0 });
    expect(nodes.get('deploy_region')).toMatchObject({ state: 'succeeded' });
    expect(nodes.get('region_health')).toMatchObject({
      state: 'running',
      badge: { kind: 'agent', label: 'Agent · release-agent' },
    });
  });

  it('shows the steps of a picked iteration', () => {
    const east = buildOverlay(outline, midRollout(), {
      names,
      selection: { rollout: '0' },
    }).nodes;
    expect(east.get('rollout')?.iteration?.label).toBe('region 1 of 3');
    expect(east.get('settle')?.iteration?.label).toBe('attempt 2 of 4');
    expect(east.get('region_health')).toMatchObject({ state: 'succeeded', verdict: 'pass' });
    const first = buildOverlay(outline, midRollout(), {
      names,
      selection: { rollout: '0', settle: '#1' },
    }).nodes;
    expect(first.get('region_health')).toMatchObject({ verdict: 'fail' });
    // An unknown pick falls back to the default.
    const stale = buildOverlay(outline, midRollout(), {
      names,
      selection: { rollout: '7' },
    }).nodes;
    expect(stale.get('rollout')?.iteration?.detail).toBe('us-west');
  });

  it('titles each card with the picked iteration’s rendered title', () => {
    const { nodes } = buildOverlay(outline, midRollout(), { names });
    expect(nodes.get('deploy_region')?.title).toBe('Deploy to us-west');
    expect(
      buildOverlay(outline, waitingOnApproval(), { names }).nodes.get('region_health')?.title,
    ).toBeUndefined();
  });

  it('leaves an unstarted iteration’s steps pending', () => {
    const { nodes } = buildOverlay(outline, waitingOnApproval(), { names });
    expect(nodes.get('rollout')?.iteration).toBeUndefined();
    expect(nodes.get('region_health')).toMatchObject({ state: 'pending' });
  });

  it('links a started call step to its child run', () => {
    const doc = midRollout();
    const verify = doc.header.steps.find((s) => s.path === 'verify')!;
    verify.call = { child_execution_id: 'exe_CHILD', started: true };
    const { nodes } = buildOverlay(outline, doc, { names });
    expect(nodes.get('verify')?.child).toEqual({ href: '/x/exe_CHILD', label: 'Child run' });
  });

  it('shows who took an agent step over, from the document’s section (LANG7)', () => {
    const doc = midRollout();
    const health = doc.sections.find((s) => s.path === 'rollout[1].settle#1.region_health')!;
    health.taken_over = {
      by: { kind: 'human', user_id: RIVER },
      at_ms: 1_794_500_100_000,
      previous_actor_kind: 'agent',
    };
    const { nodes } = buildOverlay(outline, doc, { names });
    expect(nodes.get('region_health')?.takenOverBy).toBe('Riley Chen');
  });

  it('carries reminders and escalation recipients onto the shown step run', () => {
    const doc = midRollout();
    const health = doc.sections.find((s) => s.path === 'rollout[1].settle#1.region_health')!;
    health.followup = {
      reminders_sent: 2,
      escalated: true,
      escalated_to: [
        { kind: 'user', reference: RIVER },
        { kind: 'team', reference: 'team_oncall' },
      ],
    };
    const { nodes } = buildOverlay(outline, doc, { names });
    expect(nodes.get('region_health')?.followup).toEqual({
      remindersSent: 2,
      escalated: true,
      escalatedTo: 'Riley Chen, team oncall',
    });
  });
});

function shape(items: DocItem[]): unknown[] {
  return items.map((item) =>
    item.kind === 'step'
      ? item.children.length
        ? [item.record.nodeId, shape(item.children)]
        : item.record.nodeId
      : item.kind === 'iteration'
        ? [item.label, shape(item.children)]
        : `(${item.reason}: ${item.titles.join(', ')})`,
  );
}

describe('the document', () => {
  it('nests sections as the runbook does and folds what did not run', () => {
    expect(shape(documentItems(outline, midRollout()))).toEqual([
      [
        'preflight',
        [
          ['checks', ['changelog', 'migrations', 'freeze']],
          'build',
          '(unreached: Tell the release channel preflight failed, halt_preflight, stop_after_preflight)',
        ],
      ],
      [
        'risk_gate',
        [
          '(untaken: Change advisory review, hold_if_rejected)',
          '(untaken: Release lead signs off)',
          'auto_ok',
        ],
      ],
      ['canary', ['canary_us', 'canary_eu']],
      [
        'rollout',
        [
          [
            'Region us-east (1 of 3)',
            [
              'deploy_region',
              [
                'settle',
                [
                  ['Attempt 1 of 4', ['region_health']],
                  ['Attempt 2 of 4', ['region_health']],
                ],
              ],
            ],
          ],
          [
            'Region us-west (2 of 3)',
            ['deploy_region', ['settle', [['Attempt 1 of 4', ['region_health']]]]],
          ],
        ],
      ],
      '(upcoming: Post-deploy checks, Announce, shipped)',
      '(unreached: Write the release notes summary)',
    ]);
  });

  it('keeps the condition of an untaken case', () => {
    const items = documentItems(outline, midRollout());
    const gate = items.find((i) => i.kind === 'step' && i.record.nodeId === 'risk_gate');
    const folded = gate?.kind === 'step' ? gate.children.filter((c) => c.kind === 'folded') : [];
    expect(folded.map((f) => (f.kind === 'folded' ? f.branch : null))).toEqual([
      "inputs.risk == 'high'",
      "inputs.risk == 'medium'",
    ]);
  });

  it('folds skipped steps and lists every section without an outline', () => {
    const doc = failedInPreflight();
    const items = documentItems(outline, doc);
    const preflight = items[0];
    expect(preflight?.kind === 'step' && shape(preflight.children)).toEqual([
      ['checks', ['changelog', 'migrations', 'freeze']],
      '(skipped: Build and sign the release artifacts)',
      'preflight_failed',
      'halt_preflight',
      'stop_after_preflight',
    ]);
    const flat = documentItems(null, doc);
    expect(flat).toHaveLength(doc.sections.length);
  });

  it('says how the run went in one line', () => {
    const failed = outcomeOf(failedInPreflight(), 0);
    expect(failed).toEqual({
      headline: 'Failed in 4m 12s',
      checks: '2 of 3 checks passed · 1 failed',
    });
    const open = outcomeOf(midRollout(), 1_794_499_900_000 + 95_000);
    expect(open.headline).toBe('Running · 1m 35s so far');
    expect(duration(3_725_000)).toBe('1h 02m');
  });

  it('names who worked on the run', () => {
    const doc = midRollout();
    doc.sections[3]!.actor = { kind: 'human', user_id: RIVER };
    expect(participants(doc, names.nameOf)).toEqual([
      'release-agent · claude-sonnet',
      'Riley Chen',
    ]);
  });
});

// ---- UI-3 review round 1

/** The release train with its rollout keyed by `key:` (the outline says so). */
function keyedOutline(): Outline {
  const copy = structuredClone(outline);
  const rollout = copy.steps.find((n) => n.id === 'rollout')!;
  rollout.foreach = { ...rollout.foreach!, key: '${{ region.id }}' };
  return copy;
}

/** midRollout, with the rollout's iterations keyed by all-digit ids. */
function keyedRollout() {
  const doc = midRollout();
  for (const section of doc.sections) {
    section.path = section.path
      .replace('rollout[0]', 'rollout[7]')
      .replace('rollout[1]', 'rollout[3]');
  }
  for (const step of doc.header.steps) {
    step.path = step.path.replace('rollout[0]', 'rollout[7]').replace('rollout[1]', 'rollout[3]');
  }
  return doc;
}

/** midRollout, with the canary skipped by its `if` (the engine skips only the guard). */
function canarySkipped() {
  const doc = midRollout();
  for (const path of ['canary', 'canary_us', 'canary_eu']) {
    const section = doc.sections.find((s) => s.path === path)!;
    const step = doc.header.steps.find((s) => s.id === section.step_run_id)!;
    delete section.actor;
    delete section.verdict;
    delete section.summary;
    step.actor = null;
    step.verdict = null;
    if (path === 'canary') {
      section.status = 'skipped';
      step.status = 'skipped';
      step.skip_reason = 'condition';
    } else {
      section.status = 'pending';
      step.status = 'pending';
    }
  }
  return doc;
}

/** midRollout, with the rollout over zero regions (done, nothing ran inside). */
function emptyRollout() {
  const doc = midRollout();
  doc.sections = doc.sections.filter((s) => !s.path.startsWith('rollout['));
  doc.header.steps = doc.header.steps.filter((s) => !s.path.startsWith('rollout['));
  const section = doc.sections.find((s) => s.path === 'rollout')!;
  const step = doc.header.steps.find((s) => s.id === section.step_run_id)!;
  section.status = 'succeeded';
  step.status = 'succeeded';
  step.iterations = { started: 0, total: 0, labels: [] };
  return doc;
}

/** failedInPreflight, with the build skipped by a person, saying why. */
function actorSkipped() {
  const doc = midRollout();
  const section = doc.sections.find((s) => s.path === 'build')!;
  const step = doc.header.steps.find((s) => s.id === section.step_run_id)!;
  section.status = 'skipped';
  step.status = 'skipped';
  section.actor = { kind: 'human', user_id: RIVER };
  step.actor = section.actor;
  section.summary = 'Reusing yesterday’s signed artifacts.';
  delete step.skip_reason;
  return doc;
}

describe('review round 1: the graph overlay', () => {
  it('dims the steps inside a container skipped by its condition as skipped', () => {
    const { nodes } = buildOverlay(outline, canarySkipped(), { names });
    expect(nodes.get('canary')).toMatchObject({ state: 'skipped', dimmed: true });
    for (const id of ['canary_us', 'canary_eu']) {
      expect(nodes.get(id)).toMatchObject({
        state: 'skipped',
        stateLabel: 'Skipped',
        dimmed: true,
        ran: false,
        note: 'Skipped with Canary',
      });
    }
  });

  it('names a keyed foreach’s iteration by its key, even when the key is all digits', () => {
    const { nodes, records } = buildOverlay(keyedOutline(), keyedRollout(), { names });
    expect(nodes.get('rollout')?.iteration).toMatchObject({
      label: 'region 2 of 3',
      detail: '3',
      keys: ['7', '3'],
    });
    expect(records.get('deploy_region')?.scopeNames).toEqual(['3']);
    // Without `key:`, a digit key is the index into the items.
    expect(iterationName('1', ['us-east', 'us-west'], false)).toBe('us-west');
    expect(iterationName('1', ['us-east', 'us-west'], true)).toBe('1');
  });

  it('marks an empty foreach’s body as having no items', () => {
    const { nodes } = buildOverlay(outline, emptyRollout(), { names });
    expect(nodes.get('rollout')).toMatchObject({ state: 'succeeded', note: 'No items' });
    for (const id of ['deploy_region', 'settle', 'region_health']) {
      expect(nodes.get(id)).toMatchObject({ dimmed: true, stateLabel: 'No items', ran: false });
    }
  });

  it('says who skipped a step an actor skipped', () => {
    const { nodes } = buildOverlay(outline, actorSkipped(), { names });
    expect(nodes.get('build')).toMatchObject({
      state: 'skipped',
      dimmed: true,
      note: 'Skipped by Riley Chen',
    });
  });
});

describe('review round 1: the document', () => {
  it('folds only condition skips and shows a step an actor skipped', () => {
    const items = documentItems(outline, actorSkipped());
    const preflight = items[0];
    expect(preflight?.kind === 'step' && shape(preflight.children)).toEqual([
      ['checks', ['changelog', 'migrations', 'freeze']],
      'build',
      '(unreached: Tell the release channel preflight failed, halt_preflight, stop_after_preflight)',
    ]);
  });

  it('folds the steps inside a skipped container as skipped', () => {
    const items = documentItems(outline, canarySkipped());
    expect(shape(items)).toContain('(skipped: Canary)');
    expect(JSON.stringify(shape(items))).not.toContain('Canary in us-east');
  });

  it('names a keyed foreach’s iterations by their keys', () => {
    const items = documentItems(keyedOutline(), keyedRollout());
    const rollout = items.find((i) => i.kind === 'step' && i.record.nodeId === 'rollout');
    const labels =
      rollout?.kind === 'step'
        ? rollout.children.map((c) => (c.kind === 'iteration' ? c.label : null))
        : [];
    expect(labels).toEqual(['Region 7 (1 of 3)', 'Region 3 (2 of 3)']);
  });

  it('says an empty foreach had no items', () => {
    const items = documentItems(outline, emptyRollout());
    const rollout = items.find((i) => i.kind === 'step' && i.record.nodeId === 'rollout');
    expect(rollout?.kind === 'step' && shape(rollout.children)).toEqual([
      '(empty: Deploy to ${{ region }}, Wait for ${{ region }} to settle)',
    ]);
  });
});
