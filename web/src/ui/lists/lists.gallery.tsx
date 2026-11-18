import { useState } from 'react';
import { defineGallery } from '../../gallery/registry';
import { ExecutionTable } from './ExecutionTable';
import { FilterBar, type ExecutionStatus } from './FilterBar';
import { InboxList } from './InboxList';
import { VersionList } from './VersionList';
import { DiffView } from './DiffView';

const sampleExecutions = [
  {
    id: 'exe_01JABCDEFGHJKMNPQRSTVWXYZ0',
    schema_revision: 1,
    title: 'Deploy API service v2.4.1',
    status: 'running' as const,
    phase: 'ordinary' as const,
    runbook_id: 'rbk_post_deploy',
    runbook_slug: 'post-deploy',
    runbook_version: 7,
    runbook_version_id: 'rbv_01JABCDEFGHJKMNPQRSTVWXYZ0',
    started_at_ms: Date.now() - 1000 * 60 * 12,
    created_by: 'usr_alice',
    tags: ['prod', 'deploy'],
    fields: {},
    inputs: {},
    steps: [],
    stale_step_ids: [],
    amended: false,
    amendments: [],
  },
  {
    id: 'exe_01JABCDEFGHJKMNPQRSTVWXYZ1',
    schema_revision: 1,
    title: 'Smoke test staging cluster',
    status: 'succeeded' as const,
    phase: 'completed' as const,
    runbook_id: 'rbk_post_deploy',
    runbook_slug: 'post-deploy',
    runbook_version: 6,
    runbook_version_id: 'rbv_01JABCDEFGHJKMNPQRSTVWXYZ1',
    started_at_ms: Date.now() - 1000 * 60 * 75,
    completed_at_ms: Date.now() - 1000 * 60 * 70,
    created_by: 'agt_triage_bot',
    tags: ['staging'],
    fields: {},
    inputs: {},
    steps: [],
    stale_step_ids: [],
    amended: false,
    amendments: [],
  },
  {
    id: 'exe_01JABCDEFGHJKMNPQRSTVWXYZ2',
    schema_revision: 1,
    title: 'Emergency rollback eu-west-1',
    status: 'failed' as const,
    phase: 'completed' as const,
    runbook_id: 'rbk_incident',
    runbook_slug: 'incident-mitigation',
    runbook_version: 3,
    runbook_version_id: 'rbv_01JABCDEFGHJKMNPQRSTVWXYZ2',
    started_at_ms: Date.now() - 1000 * 60 * 120,
    completed_at_ms: Date.now() - 1000 * 60 * 118,
    created_by: 'system',
    tags: ['incident'],
    fields: {},
    inputs: {},
    steps: [],
    stale_step_ids: [],
    amended: false,
    amendments: [],
  },
];

const sampleInbox = [
  {
    project_id: 'prj_01JABCDEFGHJKMNPQRSTVWXYZ0',
    execution_id: 'exe_01JABCDEFGHJKMNPQRSTVWXYZ0',
    execution_title: 'Deploy API service v2.4.1',
    step_run_id: 'sr_01JABCDEFGHJKMNPQRSTVWXYZ0',
    path: 'gate.canary_signoff',
    title: 'Canary health checks inconclusive. Sign off to promote?',
    kind: 'approval' as const,
  },
  {
    project_id: 'prj_01JABCDEFGHJKMNPQRSTVWXYZ0',
    execution_id: 'exe_01JABCDEFGHJKMNPQRSTVWXYZ1',
    execution_title: 'Database schema migration v44',
    step_run_id: 'sr_01JABCDEFGHJKMNPQRSTVWXYZ1',
    path: 'verify.data_integrity',
    title: 'Verify replicated table row counts match legacy MySQL',
    kind: 'human_task' as const,
  },
];

const sampleVersions = [
  {
    runbook_id: 'rbk_post_deploy',
    runbook_version: 7,
    runbook_version_id: 'rbv_01JABCDEFGHJKMNPQRSTVWXYZ7',
    branch: 'main',
    commit_sha: 'aedcf62a91e7d2f1552dd26430949eef4fb9712f',
    created_at_ms: Date.now() - 1000 * 60 * 60 * 2,
  },
  {
    runbook_id: 'rbk_post_deploy',
    runbook_version: 6,
    runbook_version_id: 'rbv_01JABCDEFGHJKMNPQRSTVWXYZ6',
    branch: 'main',
    commit_sha: '87f7da2c9527a57788803d956a5f2c8aa108df08',
    created_at_ms: Date.now() - 1000 * 60 * 60 * 48,
  },
  {
    runbook_id: 'rbk_post_deploy',
    runbook_version: null,
    runbook_version_id: 'rbv_01JABCDEFGHJKMNPQRSTVWXYZ_prev',
    branch: 'feature/canary-check',
    commit_sha: '5738302f87f7da2c9527a57788803d956a5f2c8a',
    created_at_ms: Date.now() - 1000 * 60 * 20,
  },
];

const sampleDiff = {
  added_steps: ['canary.verify_metrics', 'notify.oncall'],
  changed_steps: ['deploy.apply_helm'],
  removed_steps: ['legacy.healthcheck'],
  unified_diff: `@@ -12,6 +12,12 @@
 steps:
   - id: deploy.apply_helm
     title: Deploy Helm chart to cluster
-    timeout: 300s
+    timeout: 600s
+    retry: 2
+
+  - id: canary.verify_metrics
+    title: Verify Prometheus latency histogram
+    threshold_ms: 250
+
   - id: gate.approval
     title: Sign off on production rollout`,
};

function FilterBarInteractive() {
  const [statuses, setStatuses] = useState<ExecutionStatus[]>(['running', 'waiting']);
  const [runbook, setRunbook] = useState('');
  const [tags, setTags] = useState(['prod', 'deploy']);

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
      }}
    />
  );
}

export default [
  defineGallery({
    id: 'execution-table',
    group: 'Lists',
    title: 'ExecutionTable',
    examples: [
      {
        name: 'Populated execution runs',
        render: () => (
          <ExecutionTable
            executions={sampleExecutions}
            hasMore={true}
            onLoadMore={() => alert('Load more executions')}
            onSelectExecution={(exe) => alert(`Selected execution ${exe.id}`)}
          />
        ),
      },
      {
        name: 'Empty state',
        render: () => <ExecutionTable executions={[]} />,
      },
    ],
  }),

  defineGallery({
    id: 'filter-bar',
    group: 'Lists',
    title: 'FilterBar',
    examples: [
      {
        name: 'Interactive filters',
        render: () => <FilterBarInteractive />,
      },
    ],
  }),

  defineGallery({
    id: 'inbox-list',
    group: 'Lists',
    title: 'InboxList',
    examples: [
      {
        name: 'Grouped approvals and tasks',
        render: () => (
          <InboxList
            items={sampleInbox}
            onSelectTask={(task) => alert(`Selected task ${task.path}`)}
          />
        ),
      },
      {
        name: 'Empty inbox',
        render: () => <InboxList items={[]} />,
      },
    ],
  }),

  defineGallery({
    id: 'version-list',
    group: 'Lists',
    title: 'VersionList',
    examples: [
      {
        name: 'Versions with git refs and compare',
        render: () => (
          <VersionList
            versions={sampleVersions}
            selectedVersionId="rbv_01JABCDEFGHJKMNPQRSTVWXYZ7"
            onSelectVersion={(v) => alert(`Selected version v${v.runbook_version}`)}
            onCompare={(v1, v2) =>
              alert(`Compare v${v1.runbook_version} vs v${v2.runbook_version}`)
            }
          />
        ),
      },
      {
        name: 'Empty version list',
        render: () => <VersionList versions={[]} />,
      },
    ],
  }),

  defineGallery({
    id: 'diff-view',
    group: 'Lists',
    title: 'DiffView',
    examples: [
      {
        name: 'VersionDiffView with step changes and unified diff',
        render: () => <DiffView diff={sampleDiff} />,
      },
      {
        name: 'Computed diff from raw before and after strings',
        render: () => (
          <DiffView
            before={`title: Deploy Service\nsteps:\n  - id: step_one\n    timeout: 30s\n  - id: step_two\n`}
            after={`title: Deploy Service v2\nsteps:\n  - id: step_one\n    timeout: 60s\n  - id: step_three\n`}
          />
        ),
      },
      {
        name: 'Empty / identical diff',
        render: () => <DiffView before="same content" after="same content" />,
      },
    ],
  }),
];
