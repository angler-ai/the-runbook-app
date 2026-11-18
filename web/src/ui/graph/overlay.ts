// UI-3: a run's state laid over the runbook graph. The graph stays the
// runbook's (UI-2's layout, never a second graph); each card reads its
// step run's state from this map, so a live update re-renders cards
// without laying the graph out again.

/** A step run's state as a card shows it (claimed reads as running). */
export type RunState =
  | 'pending'
  | 'ready'
  | 'running'
  | 'waiting'
  | 'succeeded'
  | 'failed'
  | 'skipped'
  | 'cancelled'
  | 'timed_out'
  | 'expired';

export type OverlayWaitKind =
  'agent' | 'person' | 'approval' | 'signal' | 'timer' | 'child' | 'queued' | 'paused';

export interface NodeIteration {
  /** "region 2 of 3", "attempt 2 of 4". */
  label: string;
  /** The picked iteration's own name ("us-west"), when it has one. */
  detail?: string;
  /** The picked iteration, from 0. */
  index: number;
  /** Iteration keys in order (one per started iteration). */
  keys: string[];
}

export interface NodeOverlay {
  /** The step run's rendered title, when the run has one (`${{ … }}` resolved). */
  title?: string;
  state: RunState;
  /** Words for the state ("Running", "Waiting", "Not taken"). */
  stateLabel: string;
  verdict?: 'pass' | 'warn' | 'fail';
  /** Skipped, an untaken branch, a catch or finally that did not run. */
  dimmed?: boolean;
  /** Why it is dimmed, or another short fact ("Skipped: condition false"). */
  note?: string;
  /** What an open step waits on (UI-2's badges). */
  badge?: { kind: OverlayWaitKind; label: string; title?: string };
  /** Who worked on it: "deploy-checker · claude-sonnet", a person's name. */
  actor?: { kind: 'agent' | 'human' | 'system'; label: string };
  /** A foreach or loop's iterations and the one the cards below show. */
  iteration?: NodeIteration;
  /** A call step's child run. */
  child?: { href: string; label: string };
  /** LANG7: the person who took this agent step over. */
  takenOverBy?: string;
  /** A retried task's attempt (2 and up). */
  attempt?: number;
  /** LANG9 follow-ups that already happened on the shown step run. */
  followup?: {
    remindersSent: number;
    escalated: boolean;
    /** Resolved recipient names, for the badge's accessible name and title. */
    escalatedTo?: string;
  };
  /** It ran (or is running): its incoming edge reads as travelled. */
  ran: boolean;
  /** The run's current step: the first open step in runbook order. */
  current?: boolean;
}

export interface GraphOverlay {
  nodes: ReadonlyMap<string, NodeOverlay>;
  /** Show another iteration of a foreach or loop (by key). */
  pickIteration?: (containerId: string, key: string) => void;
}
