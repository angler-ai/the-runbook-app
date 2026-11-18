import { useState } from 'react';
import { defineGallery } from '../../gallery/registry';
import { RunbookOutline } from './RunbookOutline';
import type { Outline } from '../types';
import postDeployFixture from './fixtures/post-deploy.outline.json';

const flagshipOutline = postDeployFixture as Outline;

function InteractiveOutlineDemo() {
  const [selectedId, setSelectedId] = useState<string>('smoke.api_health');

  return (
    <div className="flex flex-col gap-4 max-w-lg bg-surface p-4 border border-border rounded-xl">
      <div className="flex items-center justify-between text-2xs font-mono text-fg-muted pb-2 border-b border-border">
        <span>Selected Step ID:</span>
        <span className="font-semibold text-accent">{selectedId || 'None'}</span>
      </div>

      <RunbookOutline
        outline={flagshipOutline}
        selectedId={selectedId}
        onSelect={setSelectedId}
      />
    </div>
  );
}

export default [
  defineGallery({
    id: 'runbook-outline',
    group: 'Outline',
    title: 'RunbookOutline',
    examples: [
      {
        name: 'Flagship Post-Deploy Verification Outline',
        render: () => <InteractiveOutlineDemo />,
      },
      {
        name: 'Simple outline without finally',
        render: () => (
          <div className="max-w-md bg-surface p-4 border border-border rounded-xl">
            <RunbookOutline
              outline={{
                id: 'simple',
                title: 'Simple Linear Steps',
                stepBudget: 2,
                steps: [
                  {
                    id: 'step_1',
                    title: 'Check database status',
                    path: 'step_1',
                    kind: 'task',
                    check: true,
                    hint: 'Agent · check',
                  },
                  {
                    id: 'step_2',
                    title: 'Notify engineer',
                    path: 'step_2',
                    kind: 'task',
                    hint: 'Human',
                  },
                ],
                finally: [],
              }}
            />
          </div>
        ),
      },
    ],
  }),
];
