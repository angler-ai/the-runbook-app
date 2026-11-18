import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expectNoAxeViolations } from '../../test/axe';
import { SkillList, type SkillVersionView } from './SkillList';
import { SkillDetail } from './SkillDetail';

const mockSkills: SkillVersionView[] = [
  {
    name: 'gh-stack',
    description: 'Manage stacked branches and pull requests.',
    hash: 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
    reference: 'github.com/runbook/skills@v1.2.0',
    files: [
      { path: 'SKILL.md', sha256: 'a1b2c3d4e5f6', size: 4096 },
      { path: 'scripts/sync.sh', sha256: 'f0e1d2c3b4a5', size: 1240 },
    ],
    latest: true,
    usages: [
      {
        runbook_id: 'rb_deploy',
        version_id: 'rbv_01HXYZ',
        step_ids: ['submit_pr', 'sync_stack'],
      },
    ],
  },
  {
    name: 'aws-ecs-canary',
    description: 'Shift traffic progressively across ECS blue/green targets.',
    hash: '8f434346648f6b96df89dda901c5176b10a6d83961dd3c1ac88b59b2dc327aa4',
    reference: 'github.com/runbook/skills@v0.8.4',
    files: [{ path: 'SKILL.md', sha256: 'b2c3d4e5f607', size: 3200 }],
    latest: true,
    usages: [],
  },
];

describe('SkillList', () => {
  it('renders skill items with details and responds to selection', async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();

    const { container } = render(
      <SkillList skills={mockSkills} selectedName="gh-stack" onSelect={onSelect} />,
    );

    // A row: the name, how many runbooks use it, one line of description
    // (the source reference is in the detail panel).
    expect(screen.getByText('gh-stack')).toBeInTheDocument();
    expect(screen.getByText('1 runbook')).toBeInTheDocument();
    expect(screen.getByText('aws-ecs-canary')).toBeInTheDocument();
    expect(screen.getByText('0 runbooks')).toBeInTheDocument();

    expect(screen.getByRole('button', { name: /^gh-stack/ })).toHaveAttribute(
      'aria-current',
      'true',
    );

    // Clicking anywhere on another row selects it.
    await user.click(screen.getByText('0 runbooks'));
    expect(onSelect).toHaveBeenCalledWith(mockSkills[1]);

    await expectNoAxeViolations(container);
  });

  it('renders empty state when no skills are provided', async () => {
    const { container } = render(<SkillList skills={[]} />);
    expect(screen.getByText('No skills found')).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });
});

describe('SkillDetail', () => {
  it('renders skill details, files, history, and usages', async () => {
    const user = userEvent.setup();
    const onOpenFile = vi.fn();

    const { container } = render(
      <SkillDetail
        skill={mockSkills[0]!}
        skillMarkdown="# gh-stack guide"
        onOpenFile={onOpenFile}
      />,
    );

    // Title and metadata
    expect(screen.getByText('gh-stack')).toBeInTheDocument();
    expect(screen.getAllByText('github.com/runbook/skills@v1.2.0').length).toBeGreaterThan(0);

    // Files
    expect(screen.getByText('SKILL.md')).toBeInTheDocument();
    expect(screen.getByText('scripts/sync.sh')).toBeInTheDocument();
    expect(screen.getByText('4.0 KB')).toBeInTheDocument();

    // Click file
    const fileRow = screen.getByText('scripts/sync.sh');
    await user.click(fileRow);
    expect(onOpenFile).toHaveBeenCalledWith('scripts/sync.sh');

    // Usages
    expect(screen.getByText('rb_deploy')).toBeInTheDocument();
    expect(screen.getByText('submit_pr')).toBeInTheDocument();
    expect(screen.getByText('sync_stack')).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('renders SKILL.md as literal plain text without HTML evaluation', async () => {
    const maliciousMarkdown = `<script>alert("xss")</script><img src="x" onerror="alert(1)" />`;

    const { container } = render(
      <SkillDetail skill={mockSkills[0]!} skillMarkdown={maliciousMarkdown} />,
    );

    // Pre tag holds the plain text
    const pre = container.querySelector('[data-slot="skill-markdown"]');
    expect(pre).toBeInTheDocument();
    expect(pre?.textContent).toBe(maliciousMarkdown);

    // No <script> element or <img> element should exist in the DOM
    expect(container.querySelector('script')).toBeNull();
    expect(container.querySelector('img[onerror]')).toBeNull();
    expect(container.querySelector('img')).toBeNull();

    await expectNoAxeViolations(container);
  });
});

describe('SkillDetail usages and history', () => {
  const [ghStack, canary] = mockSkills as [SkillVersionView, SkillVersionView];

  it('names a usage by its runbook and version number when it can', () => {
    render(
      <SkillDetail
        skill={ghStack}
        describeUsage={() => ({ runbook: 'Deploy service', version: 'v3' })}
      />,
    );
    expect(screen.getByText('Deploy service')).toBeInTheDocument();
    expect(screen.getByText('v3')).toBeInTheDocument();
    expect(screen.queryByText('rb_deploy')).not.toBeInTheDocument();
    expect(screen.queryByText('rbv_01HXYZ')).not.toBeInTheDocument();
  });

  it('falls back to the IDs without a description', () => {
    render(<SkillDetail skill={ghStack} />);
    expect(screen.getByText('rb_deploy')).toBeInTheDocument();
  });

  it('marks the latest version and invents no upload time', () => {
    render(
      <SkillDetail
        skill={ghStack}
        history={[
          { version: 'e3b0c442', hash: ghStack.hash, latest: true },
          { version: '8f434346', hash: canary.hash, latest: false },
        ]}
      />,
    );
    expect(screen.getAllByText('Latest')).toHaveLength(1);
    expect(screen.queryByText(/just now|ago/)).not.toBeInTheDocument();
  });
});
