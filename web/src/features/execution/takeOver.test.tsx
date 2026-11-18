import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import type { Schemas } from '../../api/client';
import type { MeView } from '../../api/hooks';
import { fixtures } from '../../test/msw/fixtures';
import { StepAssignment } from './StepAssignment';
import { canTakeOver, strictAssigneeAllows } from './takeOver';

type Section = Schemas['StepSection'];

const me = {
  user: { id: 'usr_1', name: 'Alice' },
  teams: [{ id: 'team_1', slug: 'oncall' }],
} as unknown as MeView;

function section(patch: Partial<Section>): Section {
  const base = structuredClone(fixtures.executionDocument.sections[0]) as Section;
  return { ...base, status: 'ready', task_actor: 'agent', assignee: undefined, ...patch };
}

afterEach(cleanup);

describe('canTakeOver (LANG7)', () => {
  const act = { canAct: true };

  it('offers a ready agent task to a person who may act', () => {
    expect(canTakeOver(section({}), me, act)).toBe(true);
  });

  it('never offers a live claim, another task, or without run access', () => {
    expect(canTakeOver(section({ status: 'claimed' }), me, act)).toBe(false);
    expect(canTakeOver(section({ task_actor: 'any' }), me, act)).toBe(false);
    expect(canTakeOver(section({ task_actor: undefined }), me, act)).toBe(false);
    expect(canTakeOver(section({}), me, { canAct: false })).toBe(false);
    expect(canTakeOver(section({}), undefined, act)).toBe(false);
  });

  it('is hidden where a plain claim is the action, and on a paused run', () => {
    // A handed-off agent step is claimed, not taken over.
    expect(canTakeOver(section({}), me, act, { canClaim: true })).toBe(false);
    expect(canTakeOver(section({}), me, act, { paused: true })).toBe(false);
    expect(canTakeOver(section({}), me, act, { paused: false, canClaim: false })).toBe(true);
  });

  it('follows a strict assignee', () => {
    const strict = (recipients: Schemas['RecipientRef'][], to = 'team:oncall') =>
      section({ assignee: { to, strict: true, recipients } });
    expect(canTakeOver(strict([{ kind: 'team', reference: 'team_1' }]), me, act)).toBe(true);
    expect(canTakeOver(strict([{ kind: 'user', reference: 'usr_2' }]), me, act)).toBe(false);
    // Unresolved: the server resolves it again at claim time, so the
    // assignee as written decides.
    expect(canTakeOver(strict([]), me, act)).toBe(true);
    expect(canTakeOver(strict([], 'team:leads'), me, act)).toBe(false);
    // A guidance assignee does not limit who may take over.
    const guidance = section({ assignee: { to: 'user:bob', strict: false } });
    expect(canTakeOver(guidance, me, act)).toBe(true);
  });
});

describe('strictAssigneeAllows (LANG7 review)', () => {
  const strict = (recipients: Schemas['RecipientRef'][], to = 'team:oncall') =>
    section({ task_actor: 'any', assignee: { to, strict: true, recipients } });

  it('lets anyone through a guidance assignee or none', () => {
    expect(strictAssigneeAllows(section({ assignee: undefined }), me)).toBe(true);
    expect(strictAssigneeAllows(section({ assignee: { to: 'user:bob', strict: false } }), me)).toBe(
      true,
    );
  });

  it('allows only a resolved recipient', () => {
    expect(strictAssigneeAllows(strict([{ kind: 'team', reference: 'team_1' }]), me)).toBe(true);
    expect(strictAssigneeAllows(strict([{ kind: 'user', reference: 'usr_2' }]), me)).toBe(false);
    expect(strictAssigneeAllows(strict([]), undefined)).toBe(false);
  });

  it('matches the written assignee while it resolves to nobody', () => {
    // The server resolves it again at claim time against the live directory.
    expect(strictAssigneeAllows(strict([], 'team:oncall'), me)).toBe(true);
    expect(strictAssigneeAllows(strict([], 'user:usr_1'), me)).toBe(true);
    expect(strictAssigneeAllows(strict([], 'user:Alice'), me)).toBe(true);
    expect(strictAssigneeAllows(strict([], 'team:leads'), me)).toBe(false);
    expect(strictAssigneeAllows(strict([], 'user:bob'), me)).toBe(false);
    const unresolved = section({ assignee: { strict: true, recipients: [] } });
    expect(strictAssigneeAllows(unresolved, me)).toBe(false);
  });
});

describe('StepAssignment (LANG7)', () => {
  const nameOf = (id: string) => (id === 'usr_1' ? 'Alice' : id);

  it('shows who took over and a strict assignee', () => {
    render(
      <StepAssignment
        section={section({
          taken_over: {
            by: { kind: 'human', user_id: 'usr_1' },
            at_ms: 1,
            previous_actor_kind: 'agent',
          },
          assignee: { to: 'team:oncall', strict: true, routing_error: 'handoff.no_recipient' },
        })}
        nameOf={nameOf}
      />,
    );
    expect(screen.getByText(/taken over by alice from an agent/i)).toBeInTheDocument();
    expect(screen.getByText(/strict assignee/i)).toHaveTextContent(
      'Strict assignee: team:oncall (only they can claim it) · no one can claim it (handoff.no_recipient)',
    );
  });

  it('renders nothing for a guidance assignee or an untouched step', () => {
    const { container } = render(
      <StepAssignment
        section={section({ assignee: { to: 'team:oncall', strict: false } })}
        nameOf={nameOf}
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});
