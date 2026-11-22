import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { expectNoAxeViolations } from '../../test/axe';
import { Button } from '../primitives/button';
import { agentReviewText, ApprovalPanel } from './ApprovalPanel';
import { ChecklistForm } from './ChecklistForm';
import { ConfirmDialog } from './ConfirmDialog';
import { OutputsForm, type JsonSchema } from './OutputsForm';
import { ReportBar } from './ReportBar';

describe('ChecklistForm', () => {
  it('renders items with 44px touch targets, handles onToggle, and is axe-clean', async () => {
    const user = userEvent.setup();
    let toggledIndex: number | null = null;
    let toggledValue: boolean | null = null;

    const items = [
      { title: 'Check metrics endpoint', completed: false },
      { title: 'Verify database cluster health', completed: true },
    ];

    const { container } = render(
      <ChecklistForm
        items={items}
        onToggle={(idx, val) => {
          toggledIndex = idx;
          toggledValue = val;
        }}
      />,
    );

    const checkboxes = screen.getAllByRole('checkbox');
    expect(checkboxes[0]).not.toBeChecked();
    expect(checkboxes[1]).toBeChecked();

    // Verify touch target class
    const firstRow = checkboxes[0]?.closest('.min-h-\\[44px\\]');
    expect(firstRow).toBeInTheDocument();

    // Toggle by clicking label
    await user.click(screen.getByText('Check metrics endpoint'));
    expect(toggledIndex).toBe(0);
    expect(toggledValue).toBe(true);

    await expectNoAxeViolations(container);
  });
});

describe('OutputsForm', () => {
  const schema: JsonSchema = {
    type: 'object',
    required: ['req_string', 'int_val'],
    properties: {
      req_string: { type: 'string', title: 'Required string', default: 'default-text' },
      int_val: { type: 'integer', title: 'Integer value' },
      num_val: { type: 'number', title: 'Number value' },
      bool_val: { type: 'boolean', title: 'Boolean flag', default: true },
      enum_val: { type: 'string', title: 'Environment', enum: ['prod', 'stage'] },
      unsupported: { type: 'custom_array', title: 'Unsupported field' },
    },
  };

  it('populates defaults, renders all supported types and unsupported note, and is axe-clean', async () => {
    const { container } = render(<OutputsForm schema={schema} />);

    // Defaults populated
    expect(screen.getByLabelText(/Required string/)).toHaveValue('default-text');
    expect(screen.getByRole('checkbox', { name: /Boolean flag/ })).toBeChecked();

    // Unsupported field note
    expect(
      screen.getByText(/Type "custom_array" is not supported in this form/),
    ).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('rejects 1.5 for integer field with "Must be an integer"', async () => {
    const user = userEvent.setup();
    let submitted = false;

    render(
      <OutputsForm
        schema={schema}
        onSubmit={() => {
          submitted = true;
        }}
      />,
    );

    const intInput = screen.getByLabelText(/Integer value/);
    await user.type(intInput, '1.5');
    await user.click(screen.getByRole('button', { name: 'Submit outputs' }));

    expect(screen.getByText('Must be an integer')).toBeInTheDocument();
    expect(submitted).toBe(false);
  });

  it.each(['1e3', '9007199254740993'])(
    'rejects %s for an integer field instead of submitting a different value',
    async (typed) => {
      const user = userEvent.setup();
      let submitted: Record<string, unknown> | null = null;
      render(
        <OutputsForm
          schema={schema}
          onSubmit={(values) => {
            submitted = values;
          }}
        />,
      );
      await user.type(screen.getByLabelText(/Integer value/), typed);
      await user.click(screen.getByRole('button', { name: 'Submit outputs' }));
      expect(screen.getByText('Must be an integer')).toBeInTheDocument();
      expect(submitted).toBeNull();
    },
  );

  it('submits exactly the validated integer and number values', async () => {
    const user = userEvent.setup();
    let submitted: Record<string, unknown> | null = null;
    render(
      <OutputsForm
        schema={schema}
        onSubmit={(values) => {
          submitted = values;
        }}
      />,
    );
    await user.type(screen.getByLabelText(/Integer value/), '-42');
    await user.type(screen.getByLabelText(/Number value/), '2.5e2');
    await user.click(screen.getByRole('button', { name: 'Submit outputs' }));
    expect(submitted).toMatchObject({ int_val: -42, num_val: 250 });
  });

  it('validates required fields', async () => {
    const user = userEvent.setup();
    let submitted = false;

    render(
      <OutputsForm
        schema={schema}
        initialValues={{ req_string: '', int_val: '' }}
        onSubmit={() => {
          submitted = true;
        }}
      />,
    );

    // Clear default text and submit
    await user.click(screen.getByRole('button', { name: 'Submit outputs' }));
    expect(screen.getAllByText('This field is required').length).toBeGreaterThanOrEqual(1);
    expect(submitted).toBe(false);
  });

  it('submits typed values when valid', async () => {
    const user = userEvent.setup();
    let submittedValues: Record<string, unknown> | null = null;

    render(
      <OutputsForm
        schema={schema}
        initialValues={{ req_string: 'hello', int_val: '42', num_val: '3.14' }}
        onSubmit={(vals) => {
          submittedValues = vals;
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Submit outputs' }));
    expect(submittedValues).toEqual({
      req_string: 'hello',
      int_val: 42,
      num_val: 3.14,
      bool_val: true,
    });
  });

  it('maps 422 problem errors to fields', () => {
    render(
      <OutputsForm
        schema={schema}
        errors={[{ field: 'int_val', message: 'Value out of permitted range (1..100)' }]}
      />,
    );
    expect(screen.getByText('Value out of permitted range (1..100)')).toBeInTheDocument();
  });
});

describe('ApprovalPanel', () => {
  const approvalData = {
    schema_revision: 1,
    step_run_id: 'sr_gate',
    status: 'waiting' as const,
    prompt: 'Canary check failed. Choose action:',
    options: [
      { id: 'promote', title: 'Promote anyway', auto: false },
      { id: 'rollback', title: 'Roll back', auto: false, destructive: true },
    ],
    eligible_recipients: [{ kind: 'team' as const, reference: 'platform-oncall' }],
    escalation_recipients: [{ kind: 'team' as const, reference: 'leads' }],
    escalated: false,
    quorum: 1,
    decisions: [
      {
        by: 'human' as const,
        user_id: 'usr_alice',
        option: 'promote',
        comment: 'Known test flake',
        membership_revision: 1,
      },
      {
        by: 'agent' as const,
        agent_name: 'triage-bot',
        model: 'gemini-1.5-pro',
        session_id: 'ses_123',
        option: 'promote',
        comment: 'Diagnostics clear',
      },
      {
        by: 'system' as const,
        option: 'rollback',
        policy: 'policy:unhealthy_threshold',
      },
    ],
  };

  it('renders prompt, eligibility line, decisions, and is axe-clean', async () => {
    const { container } = render(<ApprovalPanel approval={approvalData} />);

    expect(screen.getByText('Canary check failed. Choose action:')).toBeInTheDocument();
    expect(screen.getByText(/Eligible: @platform-oncall/)).toBeInTheDocument();
    expect(screen.getByText(/Escalation: @leads/)).toBeInTheDocument();
    expect(screen.getAllByText('chose promote')).toHaveLength(2);
    expect(screen.getByText('"Known test flake"')).toBeInTheDocument();
    expect(screen.getByText('triage-bot')).toBeInTheDocument();
    expect(screen.getByText(/\(gemini-1\.5-pro\)/)).toBeInTheDocument();
    expect(screen.getByText('"Diagnostics clear"')).toBeInTheDocument();
    expect(screen.getByText('(policy:unhealthy_threshold)')).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('confirms every vote before calling onDecide (a vote cannot be undone)', async () => {
    const user = userEvent.setup();
    let decidedOption: string | null = null;
    let decidedComment: string | undefined = undefined;

    render(
      <ApprovalPanel
        approval={approvalData}
        onDecide={(opt, comment) => {
          decidedOption = opt;
          decidedComment = comment;
        }}
      />,
    );

    const commentInput = screen.getByLabelText(/Decision comment/);
    await user.type(commentInput, 'LGTM');

    await user.click(screen.getByRole('button', { name: 'Promote anyway' }));
    expect(decidedOption).toBeNull();
    const dialog = screen.getByRole('alertdialog', { name: 'Confirm: Promote anyway' });
    expect(dialog).toHaveTextContent(/cannot be changed/i);
    await user.click(within(dialog).getByRole('button', { name: 'Promote anyway' }));
    expect(decidedOption).toBe('promote');
    expect(decidedComment).toBe('LGTM');
  });

  it('gives each panel its own comment field id', async () => {
    const user = userEvent.setup();
    render(
      <>
        <ApprovalPanel approval={approvalData} />
        <ApprovalPanel approval={{ ...approvalData, step_run_id: 'sr_gate_2' }} />
      </>,
    );
    const fields = screen.getAllByLabelText(/Decision comment/);
    expect(fields).toHaveLength(2);
    expect(fields[0]?.id).not.toBe(fields[1]?.id);
    // The second label focuses the second field.
    await user.click(screen.getAllByText(/Decision comment/)[1] as HTMLElement);
    expect(document.activeElement).toBe(fields[1]);
  });

  it('offers no options or comment once the approval is no longer open', () => {
    render(
      <ApprovalPanel approval={{ ...approvalData, status: 'succeeded' }} onDecide={() => {}} />,
    );
    expect(screen.queryByRole('button', { name: 'Promote anyway' })).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/Decision comment/)).not.toBeInTheDocument();
    // The outcome (walkthrough #5): the last option chosen, and who chose it.
    expect(screen.getByText('Decided: Roll back by System policy')).toBeInTheDocument();
    // The recorded decisions still show.
    expect(screen.getAllByText('chose promote')).toHaveLength(2);
  });

  it('opens ConfirmDialog for destructive rollback option', async () => {
    const user = userEvent.setup();
    let decidedOption: string | null = null;

    render(
      <ApprovalPanel
        approval={approvalData}
        onDecide={(opt) => {
          decidedOption = opt;
        }}
      />,
    );

    // Destructive option is solid red (destructive button)
    const rollbackBtn = screen.getByRole('button', { name: 'Roll back' });
    await user.click(rollbackBtn);

    // Dialog appears
    expect(screen.getByRole('alertdialog')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Confirm: Roll back' })).toBeInTheDocument();

    // Confirm in dialog
    const confirmBtn = screen.getAllByRole('button', { name: 'Roll back' }).at(-1)!;
    await user.click(confirmBtn);
    expect(decidedOption).toBe('rollback');
  });
});

describe('ApprovalPanel with a runner agent (R1-D2)', () => {
  const open = {
    schema_revision: 1,
    step_run_id: 'sr_release_gate',
    status: 'waiting' as const,
    prompt: 'Ship 2.4.0?',
    options: [
      { id: 'ship', title: 'Ship it', auto: false },
      { id: 'hold', title: 'Hold the release', auto: false },
    ],
    eligible_recipients: [{ kind: 'team' as const, reference: 'release-managers' }],
    escalation_recipients: [],
    escalated: false,
    quorum: 1,
    decisions: [],
  };
  const agent = { name: 'release-reviewer', readyAtMs: 1_000_000 + 15 * 60_000 };
  const transcriptHref = '/p/web/agents/transcripts/sr_release_gate';

  it('says when the agent may decide, and that people can decide first', () => {
    render(<ApprovalPanel approval={open} agent={agent} now={1_000_000} onDecide={() => {}} />);
    expect(
      screen.getByText(
        'Agent release-reviewer may decide in 15m if no one has. People can decide first.',
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Ship it' })).toBeEnabled();
  });

  it('shows the agent reviewing while it holds the decision lease; people can still decide', async () => {
    const decide = vi.fn();
    const user = userEvent.setup();
    const { container } = render(
      <ApprovalPanel
        approval={{
          ...open,
          decision_lease: { lease_id: 'clm_1', agent_name: 'release-reviewer' },
        }}
        agent={agent}
        now={2_000_000}
        onDecide={decide}
      />,
    );
    const status = screen.getByRole('status');
    expect(status).toHaveTextContent('Agent release-reviewer is reviewing.');
    expect(status).toHaveTextContent('You can still decide; the first decision wins.');
    expect(screen.queryByText(/may decide/)).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Hold the release' }));
    await user.click(
      within(screen.getByRole('alertdialog')).getByRole('button', { name: 'Hold the release' }),
    );
    expect(decide).toHaveBeenCalledWith('hold', undefined);
    await expectNoAxeViolations(container);
  });

  it('shows an abstention with its reason, and leaves the decision to people', () => {
    render(
      <ApprovalPanel
        approval={{
          ...open,
          agent_notes: [
            {
              lease_id: 'clm_1',
              principal: 'runner:rin_1',
              agent_name: 'release-reviewer',
              model: 'claude-sonnet',
              reason: 'No canary data for this version yet.',
              at_ms: 1,
            },
          ],
        }}
        agent={agent}
        now={2_000_000}
        onDecide={() => {}}
      />,
    );
    const review = document.querySelector('[data-slot="agent-review"]') as HTMLElement;
    expect(review).toHaveTextContent(
      'Agent release-reviewer (claude-sonnet) abstained: No canary data for this version yet.',
    );
    expect(review).toHaveTextContent('People decide this approval.');
    expect(screen.queryByText(/may decide/)).not.toBeInTheDocument();
  });

  it('shows the agent decision with model, reason, evidence and the runner transcript', async () => {
    const { container } = render(
      <ApprovalPanel
        approval={{
          ...open,
          status: 'succeeded',
          decisions: [
            {
              by: 'agent' as const,
              principal: 'runner:rin_1',
              agent_name: 'release-reviewer',
              model: 'claude-sonnet',
              session_id: 'clm_1',
              option: 'ship',
              comment: 'Canary error rate 0.1% is under the 1% threshold [call 2].',
              evidence: ['[call 2]', '[call 3]'],
            },
          ],
        }}
        agent={agent}
        transcriptHref={transcriptHref}
        now={2_000_000}
      />,
    );
    expect(screen.getByText('Decided: Ship it by agent release-reviewer')).toBeInTheDocument();
    const decision = document.querySelector('[data-slot="agent-decision"]') as HTMLElement;
    expect(decision).toHaveTextContent('Decided by agent release-reviewer (claude-sonnet)');
    expect(decision).toHaveTextContent('chose ship');
    expect(decision).toHaveTextContent(
      '"Canary error rate 0.1% is under the 1% threshold [call 2]."',
    );
    expect(decision).toHaveTextContent('Runner-reported evidence: [call 2], [call 3]');
    expect(within(decision).getByRole('link', { name: 'Runner transcript' })).toHaveAttribute(
      'href',
      `${transcriptHref}?claim_id=clm_1`,
    );
    // Closed: nothing about when the agent may decide.
    expect(screen.queryByText(/may decide/)).not.toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('links no runner transcript for an external agent decision', () => {
    render(
      <ApprovalPanel
        approval={{
          ...open,
          status: 'succeeded',
          decisions: [
            {
              by: 'agent' as const,
              agent_name: 'triage-bot',
              model: null,
              session_id: 'ses_1',
              option: 'hold',
              comment: null,
            },
          ],
        }}
        transcriptHref={transcriptHref}
      />,
    );
    expect(screen.getByText(/Decided by agent/)).toHaveTextContent('Decided by agent triage-bot');
    expect(screen.queryByRole('link', { name: 'Runner transcript' })).not.toBeInTheDocument();
  });
});

describe('agentReviewText', () => {
  const approval = { status: 'waiting' as const };
  it('words each state, and nothing once closed', () => {
    const agent = { name: 'reviewer', readyAtMs: 10_000 };
    expect(agentReviewText(approval, agent, 0)).toBe(
      'Agent reviewer may decide in 10s if no one has',
    );
    expect(agentReviewText(approval, agent, 20_000)).toBe('Agent reviewer may decide now');
    expect(
      agentReviewText(
        { ...approval, decision_lease: { lease_id: 'clm_1', agent_name: 'reviewer' } },
        agent,
        0,
      ),
    ).toBe('Agent reviewer is reviewing');
    expect(agentReviewText(approval, undefined, 0)).toBeNull();
    expect(agentReviewText({ status: 'succeeded' }, agent, 0)).toBeNull();
  });
});

describe('ReportBar', () => {
  it('handles Done and Skip callbacks, and is axe-clean', async () => {
    const user = userEvent.setup();
    const reports: string[] = [];

    const { container, rerender } = render(
      <ReportBar allowSkip={false} onReport={({ action }) => reports.push(action)} />,
    );

    expect(screen.queryByRole('button', { name: 'Skip' })).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Done' }));
    expect(reports).toContain('done');

    rerender(<ReportBar allowSkip={true} onReport={({ action }) => reports.push(action)} />);

    // Skip needs a reason, like Failed and Blocked (step.skip_reason_required).
    await user.click(screen.getByRole('button', { name: 'Skip' }));
    expect(reports).not.toContain('skip');
    const dialog = await screen.findByRole('dialog');
    await user.type(within(dialog).getByRole('textbox'), 'Not applicable');
    await user.click(within(dialog).getByRole('button', { name: 'Submit report' }));
    expect(reports).toContain('skip');

    await expectNoAxeViolations(container);
  });

  it('sends a written summary with Done, prefills the reason dialog with it, and can hide Done', async () => {
    const user = userEvent.setup();
    const payloads: { action: string; summary?: string }[] = [];
    const { rerender } = render(
      <ReportBar summary=" Dashboard green " onReport={(p) => payloads.push(p)} />,
    );
    await user.click(screen.getByRole('button', { name: 'Done' }));
    expect(payloads.at(-1)).toEqual({ action: 'done', summary: 'Dashboard green' });

    await user.click(screen.getByRole('button', { name: 'Failed' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByRole('textbox')).toHaveValue(' Dashboard green ');
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));

    rerender(<ReportBar showDone={false} onReport={(p) => payloads.push(p)} />);
    expect(screen.queryByRole('button', { name: 'Done' })).not.toBeInTheDocument();
  });

  it('requires summary for Failed and Blocked', async () => {
    const user = userEvent.setup();
    let reportedSummary: string | undefined = undefined;
    let reportedAction: string | undefined = undefined;

    render(
      <ReportBar
        onReport={({ action, summary }) => {
          reportedAction = action;
          reportedSummary = summary;
        }}
      />,
    );

    // Click Failed
    await user.click(screen.getByRole('button', { name: 'Failed' }));
    expect(screen.getByRole('dialog', { name: 'Report Step as Failed' })).toBeInTheDocument();

    // Submit button is disabled when empty
    const submitBtn = screen.getByRole('button', { name: 'Submit report' });
    expect(submitBtn).toBeDisabled();

    // Type summary and submit
    await user.type(screen.getByLabelText(/Summary/), 'Service crashed on boot');
    expect(submitBtn).not.toBeDisabled();
    await user.click(submitBtn);

    expect(reportedAction).toBe('failed');
    expect(reportedSummary).toBe('Service crashed on boot');
  });
});

describe('ConfirmDialog', () => {
  it('opens and confirms destructive action, and is axe-clean', async () => {
    const user = userEvent.setup();
    let confirmed = false;

    render(
      <ConfirmDialog
        trigger={<Button variant="destructive">Delete database</Button>}
        title="Delete database?"
        description="This action cannot be undone."
        confirmLabel="Delete"
        destructive={true}
        onConfirm={() => {
          confirmed = true;
        }}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Delete database' }));
    expect(screen.getByRole('alertdialog', { name: 'Delete database?' })).toBeInTheDocument();
    await expectNoAxeViolations(document.body);

    const confirmBtn = screen.getAllByRole('button', { name: 'Delete' }).at(-1)!;
    await user.click(confirmBtn);
    expect(confirmed).toBe(true);
  });
});

describe('ApprovalPanel names (walkthrough #1)', () => {
  const ALICE = 'usr_01M76R2QSVVFRN94EPY96GDTND';
  const BOB = 'usr_01M76R2QZ86FKFYJG2JX2PWR4P';
  const approval = {
    schema_revision: 1,
    step_run_id: 'sr_gate',
    status: 'waiting' as const,
    prompt: 'Checks failed. Choose an action.',
    options: [{ id: 'keep', title: 'keep', auto: false }],
    eligible_recipients: [
      { kind: 'user' as const, reference: ALICE },
      { kind: 'user' as const, reference: BOB },
      { kind: 'team' as const, reference: 'team_01M76R2QZ86FKFYJG2JX2PWR4P' },
    ],
    escalation_recipients: [],
    escalated: false,
    quorum: 1,
    decisions: [{ by: 'human' as const, user_id: ALICE, option: 'keep', membership_revision: 1 }],
  };
  const nameOf = (id: string) => (id === ALICE ? 'Alice Example' : `short:${id.slice(-4)}`);
  const teamName = (id: string) => (id.startsWith('team_') ? 'oncall' : id);

  it('lists eligible people and teams by name, and decisions by name', () => {
    render(<ApprovalPanel approval={approval} nameOf={nameOf} teamName={teamName} />);
    expect(screen.getByText(/Eligible: Alice Example, short:WR4P, @oncall/)).toBeInTheDocument();
    expect(screen.queryByText(new RegExp(ALICE))).not.toBeInTheDocument();
    const decisions = screen.getByText(/Recorded decisions/i).parentElement as HTMLElement;
    expect(within(decisions).getByText('Alice Example')).toBeInTheDocument();
  });
});

describe('ApprovalPanel once decided (walkthrough #5)', () => {
  const BOB = 'usr_01M76R2QZ86FKFYJG2JX2PWR4P';
  const decided = {
    schema_revision: 1,
    step_run_id: 'sr_gate',
    status: 'succeeded' as const,
    prompt: 'Checks failed for v2.0.0. Choose an action.',
    options: [
      { id: 'rollback', title: 'rollback', auto: false },
      { id: 'keep', title: 'keep', auto: false },
    ],
    eligible_recipients: [{ kind: 'user' as const, reference: BOB }],
    escalation_recipients: [],
    escalated: false,
    quorum: 1,
    decisions: [
      {
        by: 'human' as const,
        user_id: BOB,
        option: 'keep',
        comment: 'Recovered after the re-check',
        membership_revision: 1,
      },
    ],
  };
  const nameOf = (id: string) => (id === BOB ? 'Bob Example' : id);

  it('says what was decided and by whom, with no call to action', () => {
    render(<ApprovalPanel approval={decided} nameOf={nameOf} onDecide={() => {}} />);
    expect(screen.getByText('Decided: keep by Bob Example')).toBeInTheDocument();
    expect(screen.queryByText(/approval required/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/choose an action/i)).not.toBeInTheDocument();
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
  });

  it('prefers the step outcome when it is given (quorum, system policy)', () => {
    render(
      <ApprovalPanel
        approval={decided}
        nameOf={nameOf}
        outcome={{ option: 'rollback', by: 'System policy' }}
      />,
    );
    expect(screen.getByText('Decided: rollback by System policy')).toBeInTheDocument();
  });

  it('says how an approval closed without a decision', () => {
    render(<ApprovalPanel approval={{ ...decided, status: 'expired', decisions: [] }} />);
    expect(screen.getByText('Closed without a decision (expired).')).toBeInTheDocument();
    expect(screen.queryByText(/choose an action/i)).not.toBeInTheDocument();
  });
});
