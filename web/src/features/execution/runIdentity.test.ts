import { describe, expect, it } from 'vitest';
import type { Schemas } from '../../api/client';
import { assignedLabel, dataSummary } from './runIdentity';

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
