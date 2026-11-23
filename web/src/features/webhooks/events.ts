//! The webhook event hierarchy shown by the create/edit forms — the
//! same catalog the server fans out, grouped the way people think about
//! it. `webhook.test` is deliberately absent: it is the test ping, not a
//! subscribable event.

export interface EventGroup {
  label: string;
  events: { kind: string; label: string }[];
}

export const EVENT_GROUPS: EventGroup[] = [
  {
    label: 'Runs',
    events: [
      { kind: 'execution.queued', label: 'Run queued' },
      { kind: 'execution.started', label: 'Run started' },
      { kind: 'execution.completed', label: 'Run completed' },
    ],
  },
  {
    label: 'Steps',
    events: [
      { kind: 'step.blocked', label: 'Step blocked or handed off' },
      { kind: 'step.completed', label: 'Step completed' },
      { kind: 'step.reminder', label: 'Step reminder' },
      { kind: 'step.overdue', label: 'Step overdue' },
      { kind: 'step.escalated', label: 'Step escalated' },
    ],
  },
  {
    label: 'Approvals',
    events: [
      { kind: 'approval.requested', label: 'Approval requested' },
      { kind: 'approval.escalated', label: 'Approval escalated' },
      { kind: 'approval.expired', label: 'Approval expired' },
      { kind: 'approval.decided', label: 'Approval decided' },
    ],
  },
  {
    label: 'Runbooks',
    events: [{ kind: 'runbook.published', label: 'Runbook published' }],
  },
  {
    label: 'Memory',
    events: [
      { kind: 'memory.proposed', label: 'Memory proposed' },
      { kind: 'memory.saved', label: 'Memory saved' },
      { kind: 'memory.edited', label: 'Memory edited' },
    ],
  },
  {
    label: 'Agents',
    events: [{ kind: 'agent.work_available', label: 'Agent work available' }],
  },
  {
    label: 'Runner credentials',
    events: [
      { kind: 'credential.reconnect_required', label: 'OAuth credential needs reconnection' },
      { kind: 'credential.oauth_refresh_failing', label: 'OAuth credential refresh keeps failing' },
      { kind: 'credential.oauth_expiring', label: 'OAuth credential expires soon' },
    ],
  },
];

export const ALL_EVENT_KINDS: string[] = EVENT_GROUPS.flatMap((group) =>
  group.events.map((event) => event.kind),
);

/** A stored `events` list as a selection: `*` means every event. */
export function selectionOf(events: string[]): Set<string> {
  if (events.includes('*')) return new Set(ALL_EVENT_KINDS);
  return new Set(events.filter((kind) => ALL_EVENT_KINDS.includes(kind)));
}

/** The stored form of a selection: everything checked is `["*"]`, so a
 * subscription meant as "all events" also gets future event types. */
export function eventsOfSelection(selected: ReadonlySet<string>): string[] {
  if (ALL_EVENT_KINDS.every((kind) => selected.has(kind))) return ['*'];
  return ALL_EVENT_KINDS.filter((kind) => selected.has(kind));
}
