import { describe, it, expect, vi } from 'vitest';
import { useState } from 'react';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expectNoAxeViolations } from '../../test/axe';
import { MAX_LCS_CELLS, computeLineOperations, computeUnifiedDiff } from './diff';
import { DiffView } from './DiffView';
import { ExecutionTable } from './ExecutionTable';
import { FilterBar, type ExecutionStatus } from './FilterBar';
import { InboxList, inboxAgentText } from './InboxList';
import { VersionList } from './VersionList';

describe('LCS line diff algorithm (diff.ts)', () => {
  it('returns empty string for identical inputs', () => {
    expect(computeUnifiedDiff('hello\nworld', 'hello\nworld')).toBe('');
    expect(computeUnifiedDiff('', '')).toBe('');
  });

  it('computes line additions', () => {
    const before = 'line 1\nline 2';
    const after = 'line 1\nline 1.5\nline 2';
    const diff = computeUnifiedDiff(before, after);

    expect(diff).toContain('@@ -1,2 +1,3 @@');
    expect(diff).toContain(' line 1');
    expect(diff).toContain('+line 1.5');
    expect(diff).toContain(' line 2');
  });

  it('computes line deletions', () => {
    const before = 'line 1\nline 2\nline 3';
    const after = 'line 1\nline 3';
    const diff = computeUnifiedDiff(before, after);

    expect(diff).toContain('@@ -1,3 +1,2 @@');
    expect(diff).toContain(' line 1');
    expect(diff).toContain('-line 2');
    expect(diff).toContain(' line 3');
  });

  it('handles completely replaced text and file headers', () => {
    const before = 'apple\nbanana';
    const after = 'cherry\ndate';
    const diff = computeUnifiedDiff(before, after, {
      oldHeader: 'a/fruits.yaml',
      newHeader: 'b/fruits.yaml',
    });

    expect(diff).toContain('--- a/fruits.yaml');
    expect(diff).toContain('+++ b/fruits.yaml');
    expect(diff).toContain('-apple');
    expect(diff).toContain('-banana');
    expect(diff).toContain('+cherry');
    expect(diff).toContain('+date');
  });

  it('computes low-level operations correctly', () => {
    const ops = computeLineOperations('a\nb', 'a\nc\nb');
    expect(ops).toEqual([
      { type: 'equal', line: 'a' },
      { type: 'insert', line: 'c' },
      { type: 'equal', line: 'b' },
    ]);
  });

  it('diffs a one-line change in a 20,000-line source quickly', () => {
    const lines = Array.from({ length: 20_000 }, (_, i) => `line ${i}`);
    const after = [...lines];
    after[10_000] = 'changed';
    const started = Date.now();
    const ops = computeLineOperations(lines.join('\n'), after.join('\n'));
    expect(Date.now() - started).toBeLessThan(1_000);
    expect(ops.filter((op) => op.type !== 'equal')).toEqual([
      { type: 'delete', line: 'line 10000' },
      { type: 'insert', line: 'changed' },
    ]);
    expect(ops).toHaveLength(20_001);
  });

  it('falls back to a block replace when the changed middle is too large', () => {
    const side = Math.ceil(Math.sqrt(MAX_LCS_CELLS)) + 10;
    const before = Array.from({ length: side }, (_, i) => `old ${i}`).join('\n');
    const after = Array.from({ length: side }, (_, i) => `new ${i}`).join('\n');
    const started = Date.now();
    const ops = computeLineOperations(before, after);
    expect(Date.now() - started).toBeLessThan(1_000);
    expect(ops.filter((op) => op.type === 'delete')).toHaveLength(side);
    expect(ops.filter((op) => op.type === 'insert')).toHaveLength(side);
  });
});

describe('DiffView', () => {
  const sampleDiff = {
    added_steps: ['step_new'],
    removed_steps: ['step_old'],
    changed_steps: ['step_mod'],
    unified_diff: `@@ -1,3 +1,3 @@\n context\n-step_old: true\n+step_new: true`,
  };

  it('renders step changes and unified diff lines, and is axe-clean', async () => {
    const { container } = render(<DiffView diff={sampleDiff} />);

    expect(screen.getByText('Step Changes')).toBeInTheDocument();
    expect(screen.getByText('step_new')).toBeInTheDocument();
    expect(screen.getByText('step_old')).toBeInTheDocument();
    expect(screen.getByText('step_mod')).toBeInTheDocument();

    expect(screen.getByText('context')).toBeInTheDocument();
    expect(screen.getByText('-step_old: true')).toBeInTheDocument();
    expect(screen.getByText('+step_new: true')).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('renders no changes empty state when diff is empty', async () => {
    const { container } = render(<DiffView before="abc" after="abc" />);
    expect(screen.getByText('No changes detected')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('preserves plain text and never evaluates HTML (A10 compliance)', () => {
    const malicious = '<script>alert(1)</script><img src=x onerror=alert(2)>';
    render(
      <DiffView
        diff={{
          added_steps: [],
          removed_steps: [],
          changed_steps: [],
          unified_diff: `+${malicious}`,
        }}
      />,
    );

    // Rendered strictly as text, not as DOM elements
    expect(document.querySelector('script')).toBeNull();
    expect(document.querySelector('img')).toBeNull();
    expect(screen.getByText(`+${malicious}`)).toBeInTheDocument();
  });

  it('applies overflow containment classes to prevent widening pages on mobile', () => {
    const { container } = render(
      <DiffView
        diff={{
          added_steps: ['step_new'],
          removed_steps: [],
          changed_steps: [],
          unified_diff: '+very long line that could widen the container on a 390px mobile viewport',
        }}
      />,
    );

    const diffView = container.querySelector('[data-slot="diff-view"]');
    expect(diffView).toHaveClass('w-full');
    expect(diffView).toHaveClass('max-w-full');
    expect(diffView).toHaveClass('min-w-0');

    const containerEl = container.querySelector('[data-slot="unified-diff-container"]');
    expect(containerEl).toHaveClass('max-w-full');
    expect(containerEl).toHaveClass('min-w-0');

    const pre = container.querySelector('[data-slot="diff-pre"]');
    expect(pre).toHaveClass('overflow-x-auto');
    expect(pre).toHaveClass('max-w-full');
  });
});

describe('ExecutionTable', () => {
  const executions = [
    {
      id: 'exe_1',
      url: '/v1/executions/exe_1',
      web_url: '/x/exe_1',
      schema_revision: 1,
      title: 'Production Deploy',
      status: 'running' as const,
      phase: 'ordinary' as const,
      runbook_slug: 'deploy-service',
      runbook_version: 4,
      runbook_version_id: 'rbv_4',
      started_at_ms: 1000000,
      created_by: 'usr_alice',
      tags: ['prod'],
      fields: {},
      inputs: {},
      steps: [],
      stale_step_ids: [],
      amended: false,
      amendments: [],
    },
    {
      id: 'exe_2',
      url: '/v1/executions/exe_2',
      web_url: '/x/exe_2',
      schema_revision: 1,
      title: 'Failed Staging Run',
      status: 'failed' as const,
      phase: 'completed' as const,
      runbook_slug: 'deploy-service',
      runbook_version: null,
      runbook_version_id: 'rbv_prev',
      started_at_ms: 1000000,
      completed_at_ms: 1072000,
      created_by: 'agt_triage_bot',
      tags: [],
      fields: {},
      inputs: {},
      steps: [],
      stale_step_ids: [],
      amended: false,
      amendments: [],
    },
  ];

  it('renders execution rows, status pills, durations, actor badges, and is axe-clean', async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();

    const { container } = render(
      <ExecutionTable executions={executions} now={1060000} onSelectExecution={onSelect} />,
    );

    expect(screen.getByText('Production Deploy')).toBeInTheDocument();
    expect(screen.getByText('exe_1')).toBeInTheDocument();
    expect(screen.getByText('Running')).toBeInTheDocument();
    // The runbook (its title when known, else its slug) and its version.
    // Both runs are of deploy-service; only the first knows its version.
    expect(screen.getAllByText('deploy-service')).toHaveLength(2);
    expect(screen.getByText('v4')).toBeInTheDocument();
    expect(screen.queryByText('draft')).not.toBeInTheDocument();
    expect(screen.getByText('1m')).toBeInTheDocument(); // 60,000ms duration
    expect(screen.getByText('usr_alice')).toBeInTheDocument();

    expect(screen.getByText('Failed Staging Run')).toBeInTheDocument();
    expect(screen.getByText('Failed')).toBeInTheDocument();
    expect(screen.getByText('1m 12s')).toBeInTheDocument(); // 72,000ms duration
    expect(screen.getByText('agt_triage_bot')).toBeInTheDocument();

    await user.click(screen.getByText('Production Deploy'));
    expect(onSelect).toHaveBeenCalledWith(executions[0]);

    await expectNoAxeViolations(container);
  });

  it('renders EmptyState when executions list is empty', async () => {
    const { container } = render(<ExecutionTable executions={[]} />);
    expect(screen.getByText('No executions found')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });
});

describe('FilterBar', () => {
  it('supports status toggling, runbook search, tag adding/removal, reset, and is axe-clean', async () => {
    const user = userEvent.setup();
    const onResetMock = vi.fn();

    function FilterBarTestHarness() {
      const [statuses, setStatuses] = useState<ExecutionStatus[]>(['running']);
      const [runbook, setRunbook] = useState('');
      const [tags, setTags] = useState(['prod']);

      return (
        <FilterBar
          statuses={statuses}
          onStatusChange={setStatuses}
          runbook={runbook}
          onRunbookChange={setRunbook}
          tags={tags}
          onTagsChange={setTags}
          onReset={() => {
            setStatuses([]);
            setRunbook('');
            setTags([]);
            onResetMock();
          }}
        />
      );
    }

    const { container } = render(<FilterBarTestHarness />);

    // Toggle status 'succeeded'
    const succeededBtn = screen.getByRole('checkbox', { name: 'succeeded' });
    expect(succeededBtn).not.toBeChecked();
    await user.click(succeededBtn);
    expect(succeededBtn).toBeChecked();

    // Filter by runbook
    const runbookInput = screen.getByLabelText('Filter by runbook');
    await user.type(runbookInput, 'deploy');
    // Clear runbook filter button hit target
    const clearRunbookBtn = screen.getByRole('button', { name: 'Clear runbook filter' });
    expect(clearRunbookBtn).toHaveClass('size-6');
    expect(clearRunbookBtn).toHaveClass('min-w-6');
    expect(clearRunbookBtn).toHaveClass('min-h-6');
    await user.click(clearRunbookBtn);
    expect(runbookInput).toHaveValue('');

    // Re-type runbook for reset test
    await user.type(runbookInput, 'deploy');

    // Add tag
    const tagInput = screen.getByLabelText('Filter by tag');
    await user.type(tagInput, 'staging{enter}');
    expect(screen.getByText('#staging')).toBeInTheDocument();

    // Remove tag
    const removeStagingBtn = screen.getByLabelText('Remove tag staging');
    await user.click(removeStagingBtn);
    expect(screen.queryByText('#staging')).toBeNull();

    // Reset button
    const resetBtn = screen.getByRole('button', { name: /Reset/ });
    await user.click(resetBtn);
    expect(onResetMock).toHaveBeenCalled();
    expect(runbookInput).toHaveValue('');

    await expectNoAxeViolations(container);
  });

  it('provides at least a 24x24px target size for the clear runbook button (WCAG 2.5.8)', () => {
    render(<FilterBar runbook="post-deploy" onRunbookChange={() => {}} />);
    const clearBtn = screen.getByRole('button', { name: 'Clear runbook filter' });
    expect(clearBtn).toHaveClass('size-6');
    expect(clearBtn).toHaveClass('min-w-6');
    expect(clearBtn).toHaveClass('min-h-6');
  });
});

describe('InboxList', () => {
  const inboxItems = [
    {
      project_id: 'prj_1',
      execution_id: 'exe_1',
      execution_title: 'Deploy to EU',
      step_run_id: 'sr_1',
      path: 'gate.signoff',
      title: 'Sign off rollout',
      kind: 'approval' as const,
    },
    {
      project_id: 'prj_1',
      execution_id: 'exe_2',
      execution_title: 'DB Migration',
      step_run_id: 'sr_2',
      path: 'verify.data',
      title: 'Manual data integrity check',
      kind: 'human_task' as const,
    },
  ];

  it('renders grouped Approvals and Tasks, row details, handles clicks, and is axe-clean', async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();

    const { container } = render(<InboxList items={inboxItems} onSelectTask={onSelect} />);

    expect(screen.getByText(/Approvals \(1\)/)).toBeInTheDocument();
    expect(screen.getByText('Sign off rollout')).toBeInTheDocument();
    expect(screen.getByText('gate.signoff')).toBeInTheDocument();
    expect(screen.getByText('Deploy to EU')).toBeInTheDocument();

    expect(screen.getByText(/Tasks \(1\)/)).toBeInTheDocument();
    expect(screen.getByText('Manual data integrity check')).toBeInTheDocument();
    expect(screen.getByText('verify.data')).toBeInTheDocument();
    expect(screen.getByText('DB Migration')).toBeInTheDocument();

    await user.click(screen.getByText('Sign off rollout'));
    expect(onSelect).toHaveBeenCalledWith(inboxItems[0]);

    await expectNoAxeViolations(container);
  });

  // U23 (WCAG 2.4.7/2.4.11): a keyboard-focused row shows the accent
  // outline the step rail uses, not just a background shift.
  it('gives a focused row the accent focus outline', () => {
    render(<InboxList items={inboxItems} onSelectTask={() => {}} />);
    for (const row of screen.getAllByRole('button')) {
      expect(row).toHaveClass('focus-visible:outline-2', 'focus-visible:outline-accent');
      expect(row).not.toHaveClass('focus-visible:outline-none');
    }
  });

  // R1-D2: an approval a runner agent may decide says so, and when.
  it('marks approvals an agent may decide, and when', async () => {
    const now = 1_000_000;
    const runInfo = new Map([
      [
        'exe_1',
        {
          agentDecides: {
            name: 'release-reviewer',
            readyAtMs: now + 15 * 60_000,
            reviewing: false,
            abstained: false,
          },
        },
      ],
      // A human task in a run whose approval agent data is present is not marked.
      ['exe_2', { agentDecides: { name: 'x', reviewing: true, abstained: false } }],
    ]);
    const { container, rerender } = render(
      <InboxList items={inboxItems} runInfo={runInfo} now={now} />,
    );
    const marks = container.querySelectorAll('[data-slot="inbox-agent-decides"]');
    expect(marks).toHaveLength(1);
    expect(marks[0]).toHaveTextContent('Agent release-reviewer may decide in 15m');
    await expectNoAxeViolations(container);

    rerender(<InboxList items={inboxItems} runInfo={runInfo} now={now + 20 * 60_000} />);
    expect(screen.getByText('Agent release-reviewer may decide now')).toBeInTheDocument();
  });

  it('words a reviewing or abstained agent', () => {
    const agent = { name: 'r', readyAtMs: 5, reviewing: false, abstained: false };
    expect(inboxAgentText({ ...agent, reviewing: true }, 0)).toBe('Agent r is reviewing');
    expect(inboxAgentText({ ...agent, abstained: true }, 0)).toBe('Agent r abstained');
    expect(inboxAgentText({ name: 'r', reviewing: false, abstained: false }, 0)).toBe(
      'Agent r may decide now',
    );
  });

  it('renders EmptyState when inbox is clear', async () => {
    const { container } = render(<InboxList items={[]} />);
    expect(screen.getByText('Inbox is clear')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });
});

describe('VersionList', () => {
  const versions = [
    {
      runbook_id: 'rbk_1',
      runbook_version: 2,
      runbook_version_id: 'rbv_2',
      branch: 'main',
      commit_sha: 'aedcf62a91e7d2f1552dd26430949eef4fb9712f',
      created_at_ms: Date.now() - 3600000,
    },
    {
      runbook_id: 'rbk_1',
      runbook_version: 1,
      runbook_version_id: 'rbv_1',
      branch: 'main',
      commit_sha: '87f7da2c9527a57788803d956a5f2c8aa108df08',
      created_at_ms: Date.now() - 86400000,
    },
  ];

  it('renders version list, git refs, compare action, and is axe-clean', async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    const onCompare = vi.fn();

    const { container } = render(
      <VersionList
        versions={versions}
        selectedVersionId="rbv_2"
        onSelectVersion={onSelect}
        onCompare={onCompare}
      />,
    );

    expect(screen.getByText('v2')).toBeInTheDocument();
    expect(screen.getByText('latest')).toBeInTheDocument();
    expect(screen.getByText('rbv_2')).toBeInTheDocument();
    expect(screen.getByText('aedcf62')).toBeInTheDocument();

    expect(screen.getByText('v1')).toBeInTheDocument();
    expect(screen.getByText('rbv_1')).toBeInTheDocument();
    expect(screen.getByText('87f7da2')).toBeInTheDocument();

    // Click compare
    const compareBtn = screen.getByRole('button', { name: /Compare/ });
    await user.click(compareBtn);
    expect(onCompare).toHaveBeenCalledWith(versions[0], versions[1]);

    await expectNoAxeViolations(container);
  });

  it('renders EmptyState when versions list is empty', async () => {
    const { container } = render(<VersionList versions={[]} />);
    expect(screen.getByText('No versions found')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });
});
