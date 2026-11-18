// Query keys per resource (A5). Later phases invalidate by prefix:
// mutations on one execution invalidate ['executions', exe] and its list.

export const queryKeys = {
  me: ['me'] as const,
  orgs: ['orgs'] as const,
  projects: (orgId: string) => ['orgs', orgId, 'projects'] as const,
  directoryUsers: (orgId: string) => ['orgs', orgId, 'users'] as const,
  directoryTeams: (orgId: string) => ['orgs', orgId, 'teams'] as const,
  myTasks: (filters?: { kind?: string }) => ['me', 'tasks', filters ?? {}] as const,
  runbooks: (projectId: string) => ['projects', projectId, 'runbooks'] as const,
  runbook: (runbookId: string) => ['runbooks', runbookId] as const,
  runbookDraft: (runbookId: string) => ['runbooks', runbookId, 'draft'] as const,
  runbookVersions: (runbookId: string) => ['runbooks', runbookId, 'versions'] as const,
  versionForm: (runbookId: string, version: string) =>
    ['runbooks', runbookId, 'versions', version, 'form'] as const,
  executions: (projectId: string, search?: string) =>
    ['projects', projectId, 'executions', search ?? ''] as const,
  execution: (executionId: string) => ['executions', executionId] as const,
  executionDocument: (executionId: string) => ['executions', executionId, 'report'] as const,
  executionResumeBrief: (executionId: string) => ['executions', executionId, 'next'] as const,
  executionEvidence: (executionId: string) => ['executions', executionId, 'evidence'] as const,
  meta: ['meta'] as const,
};
