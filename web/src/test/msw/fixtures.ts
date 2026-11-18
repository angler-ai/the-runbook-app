// The checked-in API examples (../api/examples/*.json), typed against the generated
// schema. The `as` casts are checked: TypeScript rejects them when a fixture's shape
// is incompatible with the schema type (it only widens enum strings).
import type { components } from '../../api/schema';
import amended from '../../../../api/examples/amended.json';
import approvalWait from '../../../../api/examples/approval-wait.json';
import checkFinding from '../../../../api/examples/check-finding.json';
import executionDocument from '../../../../api/examples/execution-document.json';
import loopRecovered from '../../../../api/examples/loop-recovered.json';
import memoryEntry from '../../../../api/examples/memory-entry.json';
import memoryProposal from '../../../../api/examples/memory-proposal.json';
import myTasks from '../../../../api/examples/my-tasks.json';
import postDeployCompleted from '../../../../api/examples/post-deploy-completed.json';
import previewRun from '../../../../api/examples/preview-run.json';
import rerunInputsInvalid from '../../../../api/examples/rerun-inputs-invalid.json';
import resumeBrief from '../../../../api/examples/resume-brief.json';

type Schemas = components['schemas'];

export const fixtures = {
  /** ExecutionView, completed with a rollback result and an amendment. */
  amended: amended as Schemas['ExecutionView'],
  approvalWait: approvalWait as Schemas['ApprovalView'],
  /** StepView with a failing check verdict. */
  checkFinding: checkFinding as Schemas['StepView'],
  executionDocument: executionDocument as Schemas['ExecutionDocumentView'],
  loopRecovered: loopRecovered as Schemas['StepView'],
  memoryEntry: memoryEntry as Schemas['MemoryEntryView'],
  memoryProposal: memoryProposal as Schemas['MemoryEntryView'],
  myTasks: myTasks as Schemas['InboxPage'],
  postDeployCompleted: postDeployCompleted as Schemas['ExecutionView'],
  previewRun: previewRun as Schemas['ExecutionView'],
  /** Problem (422 execution.inputs_invalid) with field errors. */
  rerunInputsInvalid: rerunInputsInvalid as Schemas['Problem'],
  resumeBrief: resumeBrief as Schemas['ResumeBrief'],
};

export type Fixtures = typeof fixtures;
