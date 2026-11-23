import { describe, expect, it } from 'vitest';
import type { Schemas } from '../../api/client';
import { approvalAgentOf, assignedLabel, dataSummary } from './runIdentity';

const names = {
  nameOf: (id: string) => (id === 'usr_1' ? 'Alice' : id),
  teamName: (ref: string) => ref.replace(/^team_/, ''),
};

describe('dataSummary', () => {
  it('shows up to two scalar entries as key=value', () => {
    expect(dataSummary({ customer: 'Northwind Goods', tier: 2, region: 'eu' })).toBe(
      'customer=Northwind Goods · tier=2',
    );
  });

  it('skips objects, lists and empty strings, and clips long values', () => {
    expect(dataSummary({ meta: { a: 1 }, list: [1], blank: '', ok: true })).toBe('ok=true');
    expect(dataSummary({ note: 'x'.repeat(40) })).toBe(`note=${'x'.repeat(31)}…`);
    expect(dataSummary({})).toBeUndefined();
    expect(dataSummary(null)).toBeUndefined();
  });
});

describe('assignedLabel', () => {
  const view = (step: Partial<Schemas['ExecutionView']['steps'][number]>) =>
    ({
      steps: [{ id: 'sr_1', eligible_recipients: [], ...step }],
    }) as unknown as Schemas['ExecutionView'];

  it('names the assignees, people and teams', () => {
    expect(
      assignedLabel(
        view({
          assignees: [
            { kind: 'user', reference: 'usr_1' },
            { kind: 'team', reference: 'team_oncall' },
          ],
        }),
        'sr_1',
        names,
      ),
    ).toBe('assigned to Alice, team oncall');
  });

  it('falls back to who may take it, and to nothing for an unknown step', () => {
    expect(
      assignedLabel(
        view({ eligible_recipients: [{ kind: 'team', reference: 'team_ops' }] }),
        'sr_1',
        names,
      ),
    ).toBe('assigned to team ops');
    expect(assignedLabel(view({}), 'sr_9', names)).toBeUndefined();
  });
});

describe('approvalAgentOf (R1-D2)', () => {
  const view = (step: Record<string, unknown>) =>
    ({
      steps: [{ id: 'sr_1', eligible_recipients: [], ...step }],
    }) as unknown as Schemas['ExecutionView'];
  const runner = { agent_id: 'agt_1', agent_name: 'release-reviewer' };

  it('reads the runner agent, its ready time, lease and abstentions off an approval step', () => {
    expect(
      approvalAgentOf(
        view({ kind: 'approval', runner: { ...runner, decision_ready_at_ms: 42 } }),
        'sr_1',
      ),
    ).toEqual({ name: 'release-reviewer', readyAtMs: 42, reviewing: false, abstained: false });
    expect(
      approvalAgentOf(
        view({
          kind: 'approval',
          runner: {
            ...runner,
            decision_lease: { lease_id: 'clm_1', agent_name: 'release-reviewer' },
            agent_notes: [
              { lease_id: 'clm_0', principal: 'runner:r', agent_name: 'a', reason: 'x', at_ms: 1 },
            ],
          },
        }),
        'sr_1',
      ),
    ).toMatchObject({ reviewing: true, abstained: true });
  });

  it('is undefined for tasks, approvals without a runner agent and unknown steps', () => {
    expect(approvalAgentOf(view({ kind: 'task', runner }), 'sr_1')).toBeUndefined();
    expect(approvalAgentOf(view({ kind: 'approval' }), 'sr_1')).toBeUndefined();
    expect(approvalAgentOf(view({ kind: 'approval', runner }), 'sr_9')).toBeUndefined();
    expect(approvalAgentOf(undefined, 'sr_1')).toBeUndefined();
  });
});
