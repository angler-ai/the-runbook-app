import { useState } from 'react';
import { defineGallery } from '../../gallery/registry';
import { RunbookGraph } from './RunbookGraph';
import type { Outline, OutlineNode } from '../types';
import postDeployFixture from '../outline/fixtures/post-deploy.outline.json';
// The compiler's outline of scripts/dev/review-runbooks/release-train.yaml
// (crates/runbook-web-wasm/tests/outline_details.rs keeps it current).
import releaseTrainFixture from './fixtures/release-train.outline.json';

const flagshipOutline = postDeployFixture as Outline;
const releaseTrainOutline = releaseTrainFixture as Outline;
const callHref = (slug: string) => `#/runbooks/${slug}`;

function SelectedExample() {
  const [selected, setSelected] = useState<string | undefined>('cab_review');
  return (
    <RunbookGraph
      outline={releaseTrainOutline}
      callHref={callHref}
      selectedId={selected}
      onSelect={setSelected}
    />
  );
}

function generateLargeOutline(): Outline {
  const steps: OutlineNode[] = [];
  for (let i = 1; i <= 501; i++) {
    steps.push({
      id: `task_${i}`,
      title: `Automated migration step ${i}`,
      path: `task_${i}`,
      kind: 'task',
    });
  }

  return {
    id: 'large-scale-migration',
    title: 'Large Scale Cluster Migration',
    stepBudget: 1000,
    steps,
    finally: [],
  };
}

export default [
  defineGallery({
    id: 'runbook-graph',
    group: 'Graph',
    title: 'RunbookGraph',
    examples: [
      {
        name: 'Release train (complex: catch, decision, join any, nested loop, call, signal)',
        render: () => (
          <div className="w-full h-[720px]">
            <RunbookGraph outline={releaseTrainOutline} callHref={callHref} />
          </div>
        ),
      },
      {
        name: 'Release train, a step selected (details panel)',
        render: () => (
          <div className="w-full h-[720px]">
            <SelectedExample />
          </div>
        ),
      },
      {
        name: 'Release train, containers collapsed',
        render: () => (
          <div className="w-full h-[520px]">
            <RunbookGraph
              outline={releaseTrainOutline}
              callHref={callHref}
              initialCollapsed={['preflight', 'risk_gate', 'rollout']}
            />
          </div>
        ),
      },
      {
        name: 'Flagship Post-Deploy Verification Graph',
        render: () => (
          <div className="w-full h-[520px]">
            <RunbookGraph outline={flagshipOutline} />
          </div>
        ),
      },
      {
        name: 'Over-budget large runbook (>500 steps)',
        render: () => (
          <div className="w-full h-[400px]">
            <RunbookGraph outline={generateLargeOutline()} />
          </div>
        ),
      },
    ],
  }),
];
