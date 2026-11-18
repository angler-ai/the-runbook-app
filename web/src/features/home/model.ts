// UI-2: the Home page's model. Pure functions over what the existing /v1
// reads return (run search entries, run documents, the runbook catalog and
// the inbox), so each section can be tested on its own states and the page
// needs no aggregate endpoint (UI-2).
import type { Schemas } from '../../api/client';
import type { NameOf } from '../../ui/execution/names';
import { hasRunData, type RunbookRow } from '../runbooks/catalog';

export type RunEntry = Schemas['RunSearchEntry'];
export type RunDocument = Schemas['ExecutionDocumentView'];
export type ExecutionView = Schemas['ExecutionView'];
export type StepView = Schemas['StepView'];
export type InboxTask = Schemas['InboxTaskView'];
export type Recipient = Schemas['RecipientRef'];

export const OPEN_STATUSES = ['queued', 'running', 'waiting', 'paused'] as const;
/** A run in one of these is finished: its document no longer changes. */
export const TERMINAL_STATUSES: ReadonlySet<string> = new Set(['succeeded', 'failed', 'cancelled']);

/** "Good morning" before noon, "Good afternoon" until 6 pm, then "Good evening". */
export function greeting(now: Date): string {
  const hour = now.getHours();
  if (hour < 5) return 'Good evening';
  if (hour < 12) return 'Good morning';
  if (hour < 18) return 'Good afternoon';
  return 'Good evening';
}

/** The first word of a display name; the whole name when it has one word. */
export function firstName(name: string | undefined): string {
  const trimmed = (name ?? '').trim();
  return trimmed.split(/\s+/)[0] ?? '';
}

/** A compact age: `now`, `4m`, `3h`, `2d`. */
export function shortAge(ms: number): string {
  if (!Number.isFinite(ms) || ms < 60_000) return 'now';
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

/** Elapsed time with two units: `45s`, `12m`, `1h 05m`, `2d 3h`. */
export function elapsed(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${String(minutes % 60).padStart(2, '0')}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

// ---- Needs you

/** This project's open tasks, oldest first (the longest wait leads), at most `limit`. */
export function needsYou(
  tasks: readonly InboxTask[],
  projectId: string,
  limit = 5,
): { items: InboxTask[]; total: number } {
  const mine = tasks.filter((task) => task.project_id === projectId);
  const sorted = [...mine].sort(
    (a, b) => (a.since_ms ?? Number.MAX_SAFE_INTEGER) - (b.since_ms ?? Number.MAX_SAFE_INTEGER),
  );
  return { items: sorted.slice(0, limit), total: mine.length };
}

// ---- Running now: what a run waits on

import type { WaitKind } from '../../ui/status/WaitBadge';
export type { WaitKind };

export interface WaitingOn {
  kind: WaitKind;
  /** Short badge text, e.g. "Signal · release_email_sent". */
  label: string;
  /** The step's rendered title, when known. */
  step?: string;
}

const CONTAINERS = new Set(['group', 'parallel', 'foreach', 'loop', 'switch']);
const OPEN_STEP = new Set(['ready', 'claimed', 'waiting']);

/** People and teams as names: "Alice", "team oncall", "Alice +2". */
export function recipientsLabel(
  recipients: readonly Recipient[],
  nameOf: NameOf,
  teamName: (reference: string) => string,
): string {
  const names = recipients.map((r) =>
    r.kind === 'user' ? nameOf(r.reference) : `team ${teamName(r.reference)}`,
  );
  if (names.length === 0) return '';
  if (names.length === 1) return names[0] ?? '';
  return `${names[0]} +${names.length - 1}`;
}

function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

/**
 * When a timer fires, as read on `now`'s local day: the time alone today,
 * with the weekday within six days either way ("Mon 09:00"), else with the
 * date ("Nov 30 09:00").
 */
export function fireTime(ms: number, now: number): string {
  const days = Math.round((startOfDay(ms) - startOfDay(now)) / 86_400_000);
  if (days === 0) return clock(ms);
  const day =
    Math.abs(days) < 7
      ? new Date(ms).toLocaleDateString([], { weekday: 'short' })
      : new Date(ms).toLocaleDateString([], { month: 'short', day: 'numeric' });
  return `${day} ${clock(ms)}`;
}

/**
 * What an open run waits on now, from its view's open leaf steps in
 * runbook order (at most `limit`, one per kind and label): an approval and
 * who decides, a signal by name, a timer and when it fires, a child run, a
 * claimed task's agent or person, or a ready task's recipients (a person)
 * or, with none named, an agent to pick it up.
 */
export function waitingOn(
  document: RunDocument,
  names: { nameOf: NameOf; teamName: (reference: string) => string },
  limit = 3,
  now = Date.now(),
): WaitingOn[] {
  const view = document.header;
  if (view.status === 'paused') return [{ kind: 'paused', label: 'Paused' }];
  if (view.status === 'queued') return [{ kind: 'queued', label: 'Queued' }];
  const sections = new Map(document.sections.map((s) => [s.step_run_id, s]));
  const out: WaitingOn[] = [];
  const seen = new Set<string>();
  for (const step of view.steps) {
    if (!OPEN_STEP.has(step.status) || CONTAINERS.has(step.kind ?? '')) continue;
    const section = sections.get(step.id);
    const title = section?.title || step.path;
    const item = stepWaitingOn(step, section, title, names, now);
    if (!item) continue;
    const key = `${item.kind}:${item.label}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(item);
    if (out.length >= limit) break;
  }
  return out;
}

/** UI-3: what one open leaf step waits on (the run graph's badge). */
export function stepWaitingOn(
  step: StepView,
  section: RunDocument['sections'][number] | undefined,
  title: string,
  names: { nameOf: NameOf; teamName: (reference: string) => string },
  now: number,
): WaitingOn | null {
  const who = (recipients: readonly Recipient[]) =>
    recipientsLabel(recipients, names.nameOf, names.teamName);
  switch (step.kind) {
    case 'approval': {
      const approvers = who(section?.approval?.eligible_recipients ?? step.eligible_recipients);
      return {
        kind: 'approval',
        label: approvers ? `Approval · ${approvers}` : 'Approval',
        step: title,
      };
    }
    case 'wait': {
      if (step.wait?.signal)
        return { kind: 'signal', label: `Signal · ${step.wait.signal}`, step: title };
      if (step.wait?.until_ms != null)
        return {
          kind: 'timer',
          label: `Timer · ${fireTime(step.wait.until_ms, now)}`,
          step: title,
        };
      return { kind: 'timer', label: 'Timer', step: title };
    }
    case 'call':
      return { kind: 'child', label: 'Child run', step: title };
    default: {
      // A task (or an unlabelled step from an older server).
      const actor = step.actor;
      if (step.status === 'claimed' && actor?.kind === 'agent')
        return { kind: 'agent', label: `Agent · ${actor.name}`, step: title };
      if (step.status === 'claimed' && actor?.kind === 'human')
        return { kind: 'person', label: names.nameOf(actor.user_id), step: title };
      // A handoff names its recipients; otherwise the task's own assignee.
      const recipients = who(
        step.eligible_recipients.length > 0 ? step.eligible_recipients : (step.assignees ?? []),
      );
      if (recipients) return { kind: 'person', label: recipients, step: title };
      if (step.actor_kind === 'human') return { kind: 'person', label: 'A person', step: title };
      if (step.actor_kind === 'any') return { kind: 'person', label: 'Anyone', step: title };
      if (step.status === 'claimed') return { kind: 'agent', label: 'Agent working', step: title };
      return { kind: 'agent', label: 'Agent', step: title };
    }
  }
}

/** Steps done of total, from the run's progress (absent before it starts). */
export function progressOf(view: ExecutionView): { done: number; total: number } | null {
  const progress = view.progress;
  if (!progress || progress.total <= 0) return null;
  return { done: Math.min(progress.done, progress.total), total: progress.total };
}

// ---- Failed: the step that failed the run

const FAILED_STEP = new Set(['failed', 'timed_out', 'expired']);

export interface FailedStep {
  title: string;
  path: string;
  status: string;
  summary?: string;
}

/**
 * The step that failed a run: the first leaf step (runbook order) that
 * failed, timed out or expired, else the first such container, else the
 * first leaf check whose verdict is `fail` (a check can fail the run
 * through an `end: failed` while its own status is succeeded).
 */
export function failingStep(document: RunDocument): FailedStep | null {
  const failed = document.sections.filter((s) => FAILED_STEP.has(s.status));
  const leaf =
    failed.find((s) => !CONTAINERS.has(s.kind ?? '')) ??
    failed[0] ??
    document.sections.find((s) => s.verdict === 'fail' && !CONTAINERS.has(s.kind ?? ''));
  if (!leaf) return null;
  return {
    title: leaf.title || leaf.path,
    path: leaf.path,
    status: leaf.status,
    ...(leaf.summary ? { summary: leaf.summary } : {}),
  };
}

// ---- Health and today's runs

/** Local midnight of `now`'s day (epoch ms). */
export function startOfDay(now: number): number {
  const day = new Date(now);
  day.setHours(0, 0, 0, 0);
  return day.getTime();
}

/** Local midnight `days - 1` days before `now`'s day: the start of a `days`-day window. */
export function windowStart(now: number, days: number): number {
  const day = new Date(startOfDay(now));
  day.setDate(day.getDate() - (days - 1));
  return day.getTime();
}

export interface RunCounts {
  started: number;
  succeeded: number;
  failed: number;
  open: number;
  other: number;
}

/** Runs created at or after `fromMs`, by current status. */
export function runCounts(entries: readonly RunEntry[], fromMs: number): RunCounts {
  const counts: RunCounts = { started: 0, succeeded: 0, failed: 0, open: 0, other: 0 };
  for (const entry of entries) {
    if (entry.created_at_ms < fromMs) continue;
    counts.started += 1;
    if (entry.status === 'succeeded') counts.succeeded += 1;
    else if (entry.status === 'failed') counts.failed += 1;
    else if ((OPEN_STATUSES as readonly string[]).includes(entry.status)) counts.open += 1;
    else counts.other += 1;
  }
  return counts;
}

export interface TodayRuns extends RunCounts {
  /** The read hit its page cap inside today: the counts are lower bounds. */
  capped: boolean;
}

/**
 * Today's runs (from local midnight of `now`) in a read of recent runs. A
 * capped read (`complete` false) still counted every run of today when the
 * oldest run it reached started before today; otherwise the counts are
 * lower bounds and `capped` says so.
 */
export function todayRuns(
  recent: { entries: readonly RunEntry[]; complete: boolean },
  now: number,
): TodayRuns {
  const from = startOfDay(now);
  const oldest = recent.entries.reduce(
    (min, entry) => Math.min(min, entry.created_at_ms),
    Number.POSITIVE_INFINITY,
  );
  const capped = !recent.complete && recent.entries.length > 0 && oldest >= from;
  return { ...runCounts(recent.entries, from), capped };
}

export const ALL_PASSED_NOTE = "Every runbook's last run passed or is still going.";

/**
 * The second line of an empty "Recently failed": what the catalog says
 * about each runbook's last run, only as far as it is known. Null while the
 * catalog loads (or when it has no runbooks).
 */
export function quietFailedNote(catalog: {
  rows: readonly RunbookRow[] | undefined;
  isError: boolean;
}): string | null {
  if (catalog.isError)
    return "Each runbook's last run could not be checked: the runbook list did not load.";
  const rows = catalog.rows;
  if (!rows || rows.length === 0) return null;
  if (!hasRunData(rows)) return "Your access does not show each runbook's last run.";
  const cancelled = rows.filter((row) => row.last_run?.status === 'cancelled').length;
  if (cancelled === 1) return "1 runbook's last run was cancelled.";
  if (cancelled > 1) return `${cancelled} runbooks' last runs were cancelled.`;
  return ALL_PASSED_NOTE;
}

/**
 * Runs started per local day for the `days` days ending today (oldest
 * first), or null when the read did not reach back that far (`complete`
 * false), so a sparkline never draws a partial week as a real one.
 */
export function dailyRuns(
  entries: readonly RunEntry[],
  now: number,
  days: number,
  complete: boolean,
): number[] | null {
  if (!complete) return null;
  const today = startOfDay(now);
  const starts = Array.from({ length: days }, (_, i) => {
    const day = new Date(today);
    day.setDate(day.getDate() - (days - 1 - i));
    return day.getTime();
  });
  const series = starts.map(() => 0);
  for (const entry of entries) {
    for (let i = days - 1; i >= 0; i -= 1) {
      if (entry.created_at_ms >= (starts[i] ?? 0)) {
        series[i] = (series[i] ?? 0) + 1;
        break;
      }
    }
  }
  // Runs older than the window fall through every day and are not counted.
  return series;
}

// ---- Recent activity

export type ActorKind = 'person' | 'agent' | 'service' | 'run' | 'system';

export interface Actor {
  kind: ActorKind;
  label: string;
}

/**
 * Who a principal names: `human:`/`user:`/a bare `usr_…` is a person,
 * `agent:` an agent, `service:` a service token, `execution:` a parent run.
 */
export function principalActor(principal: string, nameOf: NameOf): Actor {
  const at = principal.indexOf(':');
  const prefix = at < 0 ? '' : principal.slice(0, at);
  const rest = at < 0 ? principal : principal.slice(at + 1);
  if (prefix === 'user' || prefix === 'human' || (!prefix && /^usr_/.test(principal)))
    return { kind: 'person', label: nameOf(rest) };
  if (prefix === 'agent') return { kind: 'agent', label: rest || 'An agent' };
  if (prefix === 'service') return { kind: 'service', label: 'A service token' };
  if (prefix === 'execution') return { kind: 'run', label: 'A parent run' };
  // LANG9b: started by the runbook's own schedule or webhook trigger.
  if (prefix === 'schedule') return { kind: 'system', label: `Schedule ${rest}` };
  if (prefix === 'webhook') return { kind: 'system', label: `Webhook ${rest}` };
  return { kind: 'system', label: principal || 'The system' };
}

/** `just now`, `4m ago`, `3h ago`, `2d ago`. */
export function ago(ms: number): string {
  const age = shortAge(ms);
  return age === 'now' ? 'just now' : `${age} ago`;
}

export interface ActivityItem {
  key: string;
  kind: 'started' | 'finished';
  atMs: number;
  run: RunEntry;
}

/** Runs started and finished, newest first, at most `limit`. */
export function runActivity(entries: readonly RunEntry[], limit = 8): ActivityItem[] {
  const items: ActivityItem[] = [];
  for (const run of entries) {
    items.push({ key: `${run.id}:started`, kind: 'started', atMs: run.created_at_ms, run });
    if (run.completed_at_ms != null)
      items.push({ key: `${run.id}:finished`, kind: 'finished', atMs: run.completed_at_ms, run });
  }
  return items.sort((a, b) => b.atMs - a.atMs || b.key.localeCompare(a.key)).slice(0, limit);
}
