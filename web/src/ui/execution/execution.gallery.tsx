import { useState } from 'react';
import { defineGallery } from '../../gallery/registry';
import { fixtures } from '../../test/msw/fixtures';
import { DocumentHeader } from './DocumentHeader';
import { StepRail } from './StepRail';
import { StepSection } from './StepSection';
import { UndeliveredNotice } from './UndeliveredNotice';
import type { StepSectionType } from './StepSection';

function ExecutionDocumentDemo() {
  const doc = fixtures.executionDocument;
  const [selectedId, setSelectedId] = useState<string | null>(doc.sections[0]?.step_run_id ?? null);

  const railSteps = doc.sections.map((sec) => ({
    step_run_id: sec.step_run_id,
    title: sec.title,
    path: sec.path,
    status: sec.status,
    verdict: sec.verdict,
  }));

  return (
    <div className="flex flex-col lg:flex-row gap-6 p-4 bg-canvas rounded-lg border border-border">
      <aside className="w-full lg:w-64 shrink-0 border-b lg:border-b-0 lg:border-r border-border pb-4 lg:pb-0 lg:pr-4">
        <StepRail
          steps={railSteps}
          selectedStepRunId={selectedId}
          onSelect={(id) => setSelectedId(id)}
        />
      </aside>

      <div className="flex-1 max-w-[760px] flex flex-col gap-6 overflow-y-auto">
        <DocumentHeader header={doc.header} />
        {doc.header.notifications_not_delivered && (
          <UndeliveredNotice notifications={doc.header.notifications_not_delivered} />
        )}
        <div className="flex flex-col divide-y divide-border">
          {doc.sections.map((section, idx) => (
            <StepSection
              key={section.step_run_id}
              section={section}
              stepNumber={idx + 1}
              isCurrent={selectedId === section.step_run_id}
              logLines={[
                `[probe] initiating check at ${section.path}`,
                `[status] response code 200 OK`,
                `[done] check satisfied`,
              ]}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

function AmendedDocumentDemo() {
  const amendedDoc = fixtures.amended;
  const section: StepSectionType = {
    step_run_id: amendedDoc.steps[0]?.id ?? 'sr_amended',
    title: 'Health check (Amended)',
    path: amendedDoc.steps[0]?.path ?? 'health',
    status: amendedDoc.steps[0]?.status ?? 'succeeded',
    verdict: amendedDoc.steps[0]?.verdict ?? 'warn',
    actor: amendedDoc.steps[0]?.actor,
    summary: amendedDoc.steps[0]?.summary,
    outputs: amendedDoc.steps[0]?.outputs ?? {},
    amended: true,
    amendments: amendedDoc.steps[0]?.amendments ?? [],
    stale: false,
    evidence: ['https://status.example.test/incident/123'],
    logs: { href: '/logs' },
    memory_read: [],
  };

  return (
    <div className="max-w-[760px] p-4 bg-canvas rounded-lg border border-border flex flex-col gap-6">
      <DocumentHeader header={amendedDoc} />
      <StepSection section={section} stepNumber={1} />
    </div>
  );
}

function PostDeployCompletedDemo() {
  const completed = fixtures.postDeployCompleted;
  return (
    <div className="max-w-[760px] p-4 bg-canvas rounded-lg border border-border flex flex-col gap-6">
      <DocumentHeader header={completed} />
      <UndeliveredNotice
        notifications={[
          {
            event_id: 'evt_01HX4A',
            event_type: 'step.failed',
            reason: 'no_subscriber',
            step_run_id: 'sr_01HX4A',
          },
        ]}
      />
    </div>
  );
}

export default [
  defineGallery({
    id: 'execution-document',
    group: 'Execution',
    title: 'ExecutionDocument',
    examples: [
      {
        name: 'Full execution document (fixture)',
        render: () => <ExecutionDocumentDemo />,
      },
      {
        name: 'Amended document (fixture)',
        render: () => <AmendedDocumentDemo />,
      },
      {
        name: 'Completed execution with notice (fixture)',
        render: () => <PostDeployCompletedDemo />,
      },
    ],
  }),
];
