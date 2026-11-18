import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { expectNoAxeViolations } from '../../test/axe';
import { ActorBadge } from './ActorBadge';
import { AmendedTag } from './AmendedTag';
import { FollowupTags } from './FollowupTags';
import { MonoRef } from './MonoRef';
import { StatusPill } from './StatusPill';
import { StaleTag } from './StaleTag';
import { StepLabels } from './StepLabels';
import { VerdictTag } from './VerdictTag';
import type { AnyStatus } from './StatusPill';

describe('StatusPill', () => {
  const statuses: AnyStatus[] = [
    'queued',
    'running',
    'waiting',
    'paused',
    'succeeded',
    'failed',
    'cancelled',
    'pending',
    'ready',
    'claimed',
    'skipped',
    'timed_out',
    'expired',
  ];

  it.each(statuses)('renders status %s with an icon and label, and is axe-clean', async (st) => {
    const { container } = render(<StatusPill status={st} />);
    const pill = container.querySelector('[data-slot="status-pill"]');
    expect(pill).toBeInTheDocument();
    expect(pill).toHaveAttribute('data-status', st);
    expect(pill?.querySelector('svg')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('renders custom label override', () => {
    render(<StatusPill status="running" label="Processing deployment" />);
    expect(screen.getByText('Processing deployment')).toBeInTheDocument();
  });
});

describe('VerdictTag', () => {
  it('renders pass, warn, fail and none verdicts and is axe-clean', async () => {
    const { container } = render(
      <div>
        <VerdictTag verdict="pass" />
        <VerdictTag verdict="warn" />
        <VerdictTag verdict="fail" />
        <VerdictTag verdict={null} />
      </div>,
    );
    expect(screen.getByText('Verdict pass')).toBeInTheDocument();
    expect(screen.getByText('Verdict warn')).toBeInTheDocument();
    expect(screen.getByText('Verdict fail')).toBeInTheDocument();
    expect(screen.getByText('No verdict')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('keeps inline and content-sized classes to avoid stretching on phone/flex layouts', () => {
    const { container } = render(<VerdictTag verdict="pass" />);
    const tag = container.querySelector('[data-slot="verdict-tag"]');
    expect(tag).toHaveClass('inline-flex');
    expect(tag).toHaveClass('w-fit');
    expect(tag).toHaveClass('self-start');
  });
});

describe('AmendedTag and StaleTag', () => {
  it('renders tags and is axe-clean', async () => {
    const { container } = render(
      <div>
        <AmendedTag />
        <StaleTag />
      </div>,
    );
    expect(screen.getByText('Amended')).toBeInTheDocument();
    expect(screen.getByText('Stale')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });
});

describe('FollowupTags', () => {
  it('renders reminders and escalations independently', async () => {
    const { container, rerender } = render(
      <FollowupTags followup={{ reminders_sent: 1, escalated: false }} />,
    );
    expect(container.querySelector('[data-slot="reminded-tag"]')).toHaveTextContent('Reminded');
    expect(container.querySelector('[data-slot="escalated-tag"]')).toBeNull();

    rerender(<FollowupTags followup={{ reminders_sent: 0, escalated: true }} />);
    expect(container.querySelector('[data-slot="reminded-tag"]')).toBeNull();
    expect(container.querySelector('[data-slot="escalated-tag"]')).toHaveAccessibleName(
      'Escalated',
    );
    await expectNoAxeViolations(container);
  });

  it('renders nothing before any follow-up has happened', () => {
    const { container } = render(
      <FollowupTags followup={{ reminders_sent: 0, escalated: false }} />,
    );
    expect(container.firstChild).toBeNull();
  });
});

describe('ActorBadge', () => {
  it('renders human actor with avatar initial and name, and is axe-clean', async () => {
    const { container } = render(
      <ActorBadge actor={{ kind: 'human', user_id: 'usr_alice' }} name="Alice Smith" />,
    );
    // The person's photo, falling back to their initials when it can't load.
    const img = container.querySelector('img[data-slot="picture-avatar"]')!;
    expect(img).toHaveAttribute('src', '/v1/users/usr_alice/picture');
    fireEvent.error(img);
    expect(screen.getByText('AS')).toBeInTheDocument();
    expect(screen.getByText('Alice Smith')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('renders human actor falling back to user_id when name not given', () => {
    const { container } = render(<ActorBadge actor={{ kind: 'human', user_id: 'usr_bob' }} />);
    fireEvent.error(container.querySelector('img')!);
    expect(screen.getByText('UB')).toBeInTheDocument();
    expect(screen.getByText('usr_bob')).toBeInTheDocument();
  });

  it('renders agent actor with robot icon and mono name · model', async () => {
    const { container } = render(
      <ActorBadge
        actor={{
          kind: 'agent',
          name: 'claude-code',
          model: 'opus',
          principal_id: 'prn_1',
          session_id: 'ses_1',
        }}
      />,
    );
    expect(screen.getByText('claude-code · opus')).toBeInTheDocument();
    expect(screen.getByText('claude-code · opus')).toHaveClass('font-mono');
    await expectNoAxeViolations(container);
  });

  it('renders agent actor without model', () => {
    render(
      <ActorBadge
        actor={{
          kind: 'agent',
          name: 'ops-agent',
          model: null,
          principal_id: 'prn_2',
          session_id: 'ses_2',
        }}
      />,
    );
    expect(screen.getByText('ops-agent')).toBeInTheDocument();
  });

  it('renders system actor with System policy text and is axe-clean', async () => {
    const { container } = render(<ActorBadge actor={{ kind: 'system' }} />);
    expect(screen.getByText('System policy')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });
});

describe('MonoRef', () => {
  it('renders monospace code element and is axe-clean', async () => {
    const { container } = render(
      <div>
        <MonoRef>exe_01HX4A</MonoRef>
        <MonoRef as="span">post-deploy@3</MonoRef>
      </div>,
    );
    expect(screen.getByText('exe_01HX4A')).toHaveClass('font-mono');
    expect(screen.getByText('post-deploy@3')).toHaveClass('font-mono');
    await expectNoAxeViolations(container);
  });
});

describe('StepLabels (LANG9)', () => {
  it('renders each label as key and value, in a labelled list', () => {
    render(<StepLabels labels={{ audience: 'customer', surface: 'settings/sources' }} />);
    const list = screen.getByRole('list', { name: 'Labels' });
    expect(list).toHaveTextContent('audience:customer');
    expect(list).toHaveTextContent('surface:settings/sources');
    expect(screen.getAllByRole('listitem')).toHaveLength(2);
  });

  it('renders nothing for a step without labels', () => {
    const { container } = render(<StepLabels labels={undefined} />);
    expect(container).toBeEmptyDOMElement();
    const { container: empty } = render(<StepLabels labels={{}} />);
    expect(empty).toBeEmptyDOMElement();
  });
});
