import { useState } from 'react';
import { defineGallery } from '../../gallery/registry';
import { Button } from '../primitives/button';
import { ApprovalPanel } from './ApprovalPanel';
import { ChecklistForm, type ChecklistItem } from './ChecklistForm';
import { ConfirmDialog } from './ConfirmDialog';
import { OutputsForm, type JsonSchema } from './OutputsForm';
import { ReportBar, type ReportPayload } from './ReportBar';

function ChecklistDemo() {
  const [items, setItems] = useState<ChecklistItem[]>([
    { title: 'Check production dashboard', completed: true },
    { title: 'Verify read-replica latency is below 50ms', completed: false },
    { title: 'Confirm DNS propagation', completed: false },
  ]);

  const handleToggle = (index: number, completed: boolean) => {
    setItems((prev) =>
      prev.map((item, i) => (i === index ? { ...item, completed } : item)),
    );
  };

  return <ChecklistForm items={items} onToggle={handleToggle} />;
}

const sampleSchema: JsonSchema = {
  type: 'object',
  required: ['service_name', 'latency_ms'],
  properties: {
    service_name: {
      type: 'string',
      title: 'Service Name',
      description: 'The deployed service identifier.',
      default: 'api-gateway',
    },
    status_code: {
      type: 'integer',
      title: 'HTTP Status Code',
      description: 'Recorded HTTP response status.',
      default: 200,
    },
    latency_ms: {
      type: 'number',
      title: 'Latency (ms)',
      description: 'Round-trip probe duration.',
    },
    healthy: {
      type: 'boolean',
      title: 'Service is healthy',
      default: true,
    },
    environment: {
      type: 'string',
      title: 'Target Environment',
      enum: ['production', 'staging', 'canary'],
      default: 'production',
    },
    raw_payload: {
      type: 'object',
      title: 'Complex Payload',
      description: 'An unsupported complex object in this flat form.',
    },
  },
};

function OutputsDemo() {
  const [lastSubmission, setLastSubmission] = useState<Record<string, unknown> | null>(null);

  return (
    <div className="max-w-md flex flex-col gap-4">
      <OutputsForm
        schema={sampleSchema}
        onSubmit={(vals) => setLastSubmission(vals)}
        errors={[{ field: 'latency_ms', message: 'Latency exceeded SLA threshold' }]}
      />
      {lastSubmission && (
        <pre className="p-3 bg-surface-raised rounded border border-border text-2xs font-mono">
          {JSON.stringify(lastSubmission, null, 2)}
        </pre>
      )}
    </div>
  );
}

function ApprovalDemo() {
  const [decided, setDecided] = useState<string | null>(null);

  return (
    <div className="max-w-lg">
      <ApprovalPanel
        approval={{
          schema_revision: 1,
          step_run_id: 'sr_gate_1',
          status: 'waiting',
          prompt: 'Smoke checks failed in eu-west. Decide rollout plan:',
          options: [
            { id: 'keep', title: 'Keep deployment', auto: false },
            { id: 'rollback', title: 'Roll back', auto: false, destructive: true },
          ],
          eligible_recipients: [{ kind: 'team', reference: 'oncall' }],
          escalation_recipients: [{ kind: 'team', reference: 'eng-leads' }],
          escalated: false,
          quorum: 1,
          decisions: [
            {
              by: 'human',
              user_id: 'usr_alice',
              option: 'keep',
              comment: 'Investigated and root-caused to planned restart.',
              membership_revision: 1,
            },
            {
              by: 'agent',
              agent_name: 'rollback-assistant',
              model: 'gemini-1.5-pro',
              session_id: 'ses_auto_9',
              option: 'rollback',
              comment: 'Error budget exceeded.',
            },
            {
              by: 'system',
              option: 'rollback',
              policy: 'policy:max_failed_regions_exceeded',
            },
          ],
        }}
        onDecide={(opt, comment) => {
          setDecided(`Decided ${opt}${comment ? ` with comment "${comment}"` : ''}`);
        }}
      />
      {decided && <p className="mt-2 text-xs font-mono text-fg">{decided}</p>}
    </div>
  );
}

function ReportBarDemo() {
  const [lastReport, setLastReport] = useState<ReportPayload | null>(null);

  return (
    <div className="flex flex-col gap-4 max-w-lg">
      <ReportBar
        allowSkip={true}
        onReport={(payload) => setLastReport(payload)}
      />
      {lastReport && (
        <div className="text-xs font-mono text-fg">
          Reported: action={lastReport.action}
          {lastReport.summary && ` (summary: "${lastReport.summary}")`}
        </div>
      )}
    </div>
  );
}

function ConfirmDialogDemo() {
  const [open, setOpen] = useState(false);
  const [confirmed, setConfirmed] = useState(false);

  return (
    <div>
      <ConfirmDialog
        open={open}
        onOpenChange={setOpen}
        trigger={<Button variant="destructive">Roll back deployment</Button>}
        title="Roll back deployment?"
        description="This will terminate canary pods and revert routing to the previous release."
        confirmLabel="Roll back"
        destructive={true}
        onConfirm={() => setConfirmed(true)}
      />
      {confirmed && <p className="mt-2 text-xs text-danger font-medium">Rollback confirmed!</p>}
    </div>
  );
}

export default [
  defineGallery({
    id: 'actions-checklist-form',
    group: 'Actions',
    title: 'ChecklistForm',
    examples: [{ name: 'Interactive checklist', render: () => <ChecklistDemo /> }],
  }),
  defineGallery({
    id: 'actions-outputs-form',
    group: 'Actions',
    title: 'OutputsForm',
    examples: [{ name: 'JSON schema outputs form with validation', render: () => <OutputsDemo /> }],
  }),
  defineGallery({
    id: 'actions-approval-panel',
    group: 'Actions',
    title: 'ApprovalPanel',
    examples: [{ name: 'Approval panel with options & decisions', render: () => <ApprovalDemo /> }],
  }),
  defineGallery({
    id: 'actions-report-bar',
    group: 'Actions',
    title: 'ReportBar',
    examples: [{ name: 'ReportBar with Done, Failed, Blocked, Skip', render: () => <ReportBarDemo /> }],
  }),
  defineGallery({
    id: 'actions-confirm-dialog',
    group: 'Actions',
    title: 'ConfirmDialog',
    examples: [{ name: 'Destructive confirmation', render: () => <ConfirmDialogDemo /> }],
  }),
];
