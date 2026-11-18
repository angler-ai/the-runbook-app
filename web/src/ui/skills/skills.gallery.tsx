import { useState } from 'react';
import { defineGallery } from '../../gallery/registry';
import { SkillList, type SkillVersionView } from './SkillList';
import { SkillDetail } from './SkillDetail';

const mockSkills: SkillVersionView[] = [
  {
    name: 'gh-stack',
    description:
      'Manage stacked branches and pull requests with the gh-stack GitHub CLI extension.',
    hash: 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
    reference: 'github.com/runbook/skills@v1.2.0',
    files: [
      {
        path: 'SKILL.md',
        sha256: 'a1b2c3d4e5f60718293a4b5c6d7e8f901234567890abcdef1234567890abcdef',
        size: 4096,
      },
      {
        path: 'scripts/sync.sh',
        sha256: 'f0e1d2c3b4a5968778695a4b3c2d1e0f0123456789abcdef0123456789abcdef',
        size: 1240,
      },
      {
        path: 'references/api.json',
        sha256: '1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef',
        size: 8192,
      },
    ],
    latest: true,
    usages: [
      {
        runbook_id: 'rb_deploy_pipeline',
        version_id: 'rbv_01HXYZ1234567890ABCDEF12',
        step_ids: ['submit_pr', 'sync_stack'],
      },
      {
        runbook_id: 'rb_release_cut',
        version_id: 'rbv_01HXYZ9876543210FEDCBA98',
        step_ids: ['publish_branch'],
      },
    ],
  },
  {
    name: 'aws-ecs-canary',
    description:
      'Shift traffic progressively across ECS blue/green targets with CloudWatch metric alarms.',
    hash: '8f434346648f6b96df89dda901c5176b10a6d83961dd3c1ac88b59b2dc327aa4',
    reference: 'github.com/runbook/skills@v0.8.4',
    files: [
      {
        path: 'SKILL.md',
        sha256: 'b2c3d4e5f60718293a4b5c6d7e8f901234567890abcdef1234567890abcdef12',
        size: 3200,
      },
      {
        path: 'canary.py',
        sha256: 'c3d4e5f60718293a4b5c6d7e8f901234567890abcdef1234567890abcdef1234',
        size: 6540,
      },
    ],
    latest: true,
    usages: [
      {
        runbook_id: 'rb_deploy_pipeline',
        version_id: 'rbv_01HXYZ1234567890ABCDEF12',
        step_ids: ['canary_eval'],
      },
    ],
  },
];

const mockSkillMd = `# gh-stack

Manage stacked branches and pull requests with the gh-stack CLI extension.

## Usage
\`\`\`bash
gh stack create feature-1
gh stack sync
\`\`\`
`;

function InteractiveSkillView() {
  const [selected, setSelected] = useState<SkillVersionView>(mockSkills[0]!);

  return (
    <div className="grid grid-cols-1 md:grid-cols-12 gap-6 max-w-5xl">
      <div className="md:col-span-5">
        <SkillList skills={mockSkills} selectedName={selected.name} onSelect={setSelected} />
      </div>
      <div className="md:col-span-7 bg-surface p-6 rounded-xl border border-border">
        <SkillDetail
          skill={selected}
          skillMarkdown={mockSkillMd}
          onOpenFile={(p) => alert(`Open file: ${p}`)}
        />
      </div>
    </div>
  );
}

export default [
  defineGallery({
    id: 'skills-catalog',
    group: 'Skills',
    title: 'Skills Catalog & Detail',
    examples: [
      {
        name: 'Interactive Skill Explorer',
        render: () => <InteractiveSkillView />,
      },
      {
        name: 'Empty Skill List',
        render: () => <SkillList skills={[]} />,
      },
    ],
  }),
];
