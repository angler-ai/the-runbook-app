import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { expectNoAxeViolations } from '../../test/axe';
import { DocumentHeader } from './DocumentHeader';
import { StepRail, type StepRailItem } from './StepRail';
import { sectionDisplayStatus, StepSection, type StepSectionType } from './StepSection';
import { UndeliveredNotice } from './UndeliveredNotice';
import { shortId } from './names';

describe('DocumentHeader', () => {
  it('renders title, status, result, version, inputs, and amended tag, and is axe-clean', async () => {
    const { container } = render(
      <DocumentHeader
        title="Production Deployment"
        status="succeeded"
        result="promoted"
        runbookSlug="deploy-service"
        runbookVersion={4}
        id="exe_01HX4A"
        amended={true}
        inputs={{ service: 'payments-api', region: 'us-west' }}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 1, name: 'Production Deployment' }),
    ).toBeInTheDocument();
    expect(screen.getByText('deploy-service@4')).toBeInTheDocument();
    expect(screen.getByText('exe_01HX4A')).toBeInTheDocument();
    expect(screen.getByText('promoted')).toBeInTheDocument();
    expect(screen.getByText('Amended')).toBeInTheDocument();
    expect(screen.getByText(/payments-api/)).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });
});

describe('StepRail', () => {
  const steps: StepRailItem[] = [
    {
      step_run_id: 'sr_1',
      title: 'Health probe',
      path: 'health',
      status: 'succeeded',
      verdict: 'pass',
    },
    { step_run_id: 'sr_2', title: 'Canary rollout', path: 'canary', status: 'running' },
    { step_run_id: 'sr_3', title: 'Approval gate', path: 'gate', status: 'waiting', verdict: null },
  ];

  it('renders steps with current highlight and handles keyboard navigation', async () => {
    const user = userEvent.setup();
    let selectedId: string | null = 'sr_1';

    const { container, rerender } = render(
      <StepRail
        steps={steps}
        selectedStepRunId={selectedId}
        onSelect={(id) => {
          selectedId = id;
        }}
      />,
    );

    const firstButton = screen.getByRole('button', { name: /Health probe/ });
    expect(firstButton).toHaveAttribute('aria-current', 'step');

    // Arrow down moves focus and selects next
    firstButton.focus();
    await user.keyboard('{ArrowDown}');
    expect(selectedId).toBe('sr_2');

    rerender(
      <StepRail
        steps={steps}
        selectedStepRunId="sr_2"
        onSelect={(id) => {
          selectedId = id;
        }}
      />,
    );

    const secondButton = screen.getByRole('button', { name: /Canary rollout/ });
    expect(secondButton).toHaveAttribute('aria-current', 'step');

    await expectNoAxeViolations(container);
  });
});

describe('StepSection', () => {
  const mockSection: StepSectionType = {
    step_run_id: 'sr_01HX4A',
    title: 'Verify error rate',
    path: 'metrics.error_rate',
    status: 'failed',
    verdict: 'fail',
    actor: {
      kind: 'agent',
      name: 'metrics-watcher',
      model: 'sonnet',
      principal_id: 'prn_1',
      session_id: 'ses_1',
    },
    summary: 'Error rate 3.4% exceeds threshold 1.0%.\nManual intervention recommended.',
    outputs: {
      rate: 0.034,
      threshold: 0.01,
      details: { region: 'us-east', p99: 120 },
    },
    evidence: ['https://metrics.example.test/dashboards/123'],
    memory_read: [{ memory_id: 'mem_baseline_123', revision: 2 }],
    amended: false,
    amendments: [],
    stale: false,
    logs: { href: '/v1/logs' },
  };

  it('renders title, actor badge, status, verdict, outputs, evidence, and is axe-clean', async () => {
    const user = userEvent.setup();
    const { container } = render(
      <StepSection
        section={mockSection}
        stepNumber={1}
        checklist={[
          { title: 'Check metrics endpoint', completed: true },
          { title: 'Review alarms', completed: false },
        ]}
        logLines={['[info] probe started', '[warn] high latency detected']}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 2, name: '1. Verify error rate' }),
    ).toBeInTheDocument();
    expect(screen.getByText('metrics-watcher · sonnet')).toBeInTheDocument();
    expect(screen.getByText('Verdict fail')).toBeInTheDocument();
    expect(screen.getByText(/Error rate 3.4% exceeds threshold/)).toBeInTheDocument();
    expect(screen.getByText('0.034')).toBeInTheDocument();
    expect(screen.getByText('Check metrics endpoint')).toBeInTheDocument();
    expect(screen.getByText('mem_baseline_123')).toBeInTheDocument();

    // Evidence link is rendered with target="_blank" and rel="noopener noreferrer"
    const link = screen.getByRole('link', { name: 'https://metrics.example.test/dashboards/123' });
    expect(link).toHaveAttribute('href', 'https://metrics.example.test/dashboards/123');
    expect(link).toHaveAttribute('rel', 'noopener noreferrer');

    // Toggle logs
    const logToggle = screen.getByRole('button', { name: /Show log/ });
    await user.click(logToggle);
    expect(screen.getByText('[info] probe started')).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('shows reminders and an escalation with named recipients', async () => {
    const { container } = render(
      <StepSection
        section={{
          ...mockSection,
          followup: {
            reminders_sent: 2,
            escalated: true,
            escalated_to: [
              { kind: 'user', reference: 'usr_00000000000000000000000003' },
              { kind: 'team', reference: 'team_00000000000000000000000001' },
            ],
          },
        }}
        nameOf={(id) => (id.endsWith('03') ? 'Carol Chen' : id)}
        teamNameOf={() => 'oncall'}
      />,
    );

    const reminded = container.querySelector('[data-slot="reminded-tag"]');
    const escalated = container.querySelector('[data-slot="escalated-tag"]');
    expect(reminded).toHaveTextContent('Reminded ×2');
    expect(escalated).toHaveTextContent('Escalated');
    expect(escalated).toHaveAccessibleName('Escalated to Carol Chen, team oncall');
    await expectNoAxeViolations(container);
  });
});

describe('UndeliveredNotice', () => {
  it('renders notifications and is axe-clean', async () => {
    const { container } = render(
      <UndeliveredNotice
        notifications={[
          {
            event_id: 'evt_01HX4A',
            event_type: 'step.failed',
            reason: 'no_subscriber',
            step_run_id: 'sr_01HX4A',
          },
        ]}
      />,
    );

    expect(screen.getByRole('status')).toBeInTheDocument();
    expect(screen.getByText('step.failed')).toBeInTheDocument();
    expect(screen.getByText('(no_subscriber)')).toBeInTheDocument();
    expect(screen.getByText('evt_01HX4A')).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('renders nothing when empty', () => {
    const { container } = render(<UndeliveredNotice notifications={[]} />);
    expect(container.firstChild).toBeNull();
  });

  it('keeps a dismissal to its own run (UI-3 review)', async () => {
    sessionStorage.clear();
    const one = [{ event_id: 'evt_1', event_type: 'step.failed', reason: 'no_subscriber' }];
    const { rerender } = render(
      <UndeliveredNotice notifications={one} compact dismissKey="exe_A" />,
    );
    await userEvent
      .setup()
      .click(screen.getByRole('button', { name: 'Dismiss the undelivered notifications notice' }));
    expect(screen.queryByText(/not delivered/)).not.toBeInTheDocument();
    // The next run (same page, new ID) shows its own notice.
    rerender(<UndeliveredNotice notifications={one} compact dismissKey="exe_B" />);
    expect(screen.getByText('1 notification not delivered')).toBeInTheDocument();
    // Back on the first run, its dismissal still holds.
    rerender(<UndeliveredNotice notifications={one} compact dismissKey="exe_A" />);
    expect(screen.queryByText(/not delivered/)).not.toBeInTheDocument();
  });
});

describe('Untrusted content security (A10)', () => {
  it('renders script tags, javascript urls, and img tags as literal text without execution elements', () => {
    const maliciousSection: StepSectionType = {
      step_run_id: 'sr_xss',
      title: 'XSS Probe',
      path: 'xss',
      status: 'succeeded',
      summary: '<script>alert(1)</script>',
      outputs: {
        payload: '<img src="x" onerror="alert(1)">',
      },
      evidence: [
        'javascript:alert(1)',
        'data:text/html,<script>alert(2)</script>',
        'https://safe.example.test/report',
      ],
      logs: { href: '/logs' },
      memory_read: [],
      amended: false,
      amendments: [],
      stale: false,
    };

    const { container } = render(
      <StepSection
        section={maliciousSection}
        isLogsExpanded={true}
        logLines={['<script>alert("log")</script>', '<img src="x" onerror="alert(2)">']}
      />,
    );

    // 1. Literal text appears on screen
    expect(screen.getByText('<script>alert(1)</script>')).toBeInTheDocument();
    expect(screen.getByText(/<img src="x" onerror="alert\(1\)">/)).toBeInTheDocument();
    expect(screen.getByText('javascript:alert(1)')).toBeInTheDocument();
    expect(screen.getByText('<script>alert("log")</script>')).toBeInTheDocument();

    // 2. No <script> tags exist in DOM
    expect(container.querySelectorAll('script').length).toBe(0);

    // 3. No <img> tags exist in DOM
    expect(container.querySelectorAll('img').length).toBe(0);

    // 4. No javascript: or data: link was rendered as an anchor tag
    expect(container.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(container.querySelector('a[href^="data:"]')).toBeNull();

    // 5. The valid https link IS rendered as an anchor tag
    const safeLink = container.querySelector('a[href="https://safe.example.test/report"]');
    expect(safeLink).not.toBeNull();
    expect(safeLink).toHaveAttribute('rel', 'noopener noreferrer');
  });
});

describe('Names, not raw user ids (walkthrough #1)', () => {
  const ALICE = 'usr_01M76R2QSVVFRN94EPY96GDTND';
  const STRANGER = 'usr_01M76R2QZ86FKFYJG2JX2PWR4P';
  const names: Record<string, string> = { [ALICE]: 'Alice Example' };
  const nameOf = (id: string) => names[id] ?? shortId(id);
  const decided: StepSectionType = {
    step_run_id: 'sr_signoff',
    title: 'Roll back or keep?',
    path: 'signoff',
    status: 'succeeded',
    actor: { kind: 'human', user_id: ALICE },
    outputs: { decision: 'keep', decided_by: ALICE, comment: 'usr_ is just text here' },
    evidence: [],
    memory_read: [],
    amended: false,
    amendments: [],
    stale: false,
    logs: { href: '/logs' },
  };

  it('shows the actor and decided_by by name', () => {
    render(<StepSection section={decided} nameOf={nameOf} />);
    expect(screen.getAllByText('Alice Example')).toHaveLength(2);
    expect(screen.queryByText(ALICE)).not.toBeInTheDocument();
    expect(screen.getByText('usr_ is just text here')).toBeInTheDocument();
  });

  it('falls back to a short id for someone not in the directory', () => {
    render(
      <StepSection
        section={{ ...decided, actor: { kind: 'human', user_id: STRANGER } }}
        nameOf={nameOf}
      />,
    );
    expect(screen.getByText(shortId(STRANGER))).toBeInTheDocument();
    expect(screen.queryByText(STRANGER)).not.toBeInTheDocument();
    expect(shortId(STRANGER)).toBe('usr_…PWR4P');
  });

  it('never shows a raw user id without a resolver either', () => {
    render(<StepSection section={decided} />);
    expect(screen.queryByText(ALICE)).not.toBeInTheDocument();
    expect(screen.getAllByText(shortId(ALICE)).length).toBeGreaterThan(0);
  });
});

describe('Control nodes (walkthrough #4)', () => {
  const container: StepSectionType = {
    step_run_id: 'sr_decide',
    title: 'Keep or roll back',
    path: 'decide',
    kind: 'switch',
    status: 'ready',
    outputs: {},
    evidence: [],
    memory_read: [],
    amended: false,
    amendments: [],
    stale: false,
    logs: { href: '/logs' },
  };

  it('shows a container whose children are open as Running, not Ready', () => {
    render(<StepSection section={container} />);
    expect(screen.getByText('Running')).toBeInTheDocument();
    expect(screen.queryByText('Ready')).not.toBeInTheDocument();
  });

  it('keeps a task that is ready as Ready, and a finished container as it is', () => {
    expect(sectionDisplayStatus({ ...container, kind: 'task' })).toBe('ready');
    expect(sectionDisplayStatus({ ...container, kind: null })).toBe('ready');
    expect(sectionDisplayStatus({ ...container, status: 'succeeded' })).toBe('succeeded');
    for (const kind of ['group', 'parallel', 'foreach', 'loop', 'switch']) {
      expect(sectionDisplayStatus({ ...container, kind })).toBe('running');
    }
  });
});

describe('StepRail follows the current step (walkthrough #3)', () => {
  const many: StepRailItem[] = Array.from({ length: 12 }, (_, i) => ({
    step_run_id: `sr_${i}`,
    title: `Step ${i}`,
    status: 'pending' as const,
  }));

  it('scrolls its own list, never the page, to keep the selected step in view', () => {
    const { rerender, container } = render(<StepRail steps={many} selectedStepRunId="sr_0" />);
    const list = container.querySelector('[data-slot="step-rail-list"]') as HTMLElement;
    expect(list).not.toBeNull();
    // A 100px-high list scrolled to the top; step 11 sits at 250-280px.
    Object.defineProperty(list, 'clientHeight', { configurable: true, value: 100 });
    Object.defineProperty(list, 'scrollHeight', { configurable: true, value: 400 });
    list.getBoundingClientRect = () => ({ top: 0, bottom: 100 }) as DOMRect;
    const last = screen.getByRole('button', { name: /Step 11/ });
    last.getBoundingClientRect = () => ({ top: 250, bottom: 280 }) as DOMRect;
    const pageScroll = vi.spyOn(window, 'scrollTo').mockImplementation(() => {});
    rerender(<StepRail steps={many} selectedStepRunId="sr_11" />);
    expect(list.scrollTop).toBe(180);
    expect(pageScroll).not.toHaveBeenCalled();
    pageScroll.mockRestore();
  });
});
