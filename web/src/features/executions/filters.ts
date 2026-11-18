// Execution list filters, carried in the URL search params so a filtered
// list is a shareable link: ?status=waiting,running&runbook=post-deploy&tags=prod
import type { Schemas } from '../../api/client';

export type ExecutionStatus = Schemas['ExecutionStatus'];

export interface ExecutionFilters {
  statuses: ExecutionStatus[];
  runbook: string;
  tags: string[];
}

export interface ExecutionSearch {
  status?: string;
  runbook?: string;
  tags?: string;
}

const STATUSES = new Set<string>([
  'queued',
  'running',
  'waiting',
  'paused',
  'succeeded',
  'failed',
  'cancelled',
]);

export function parseExecutionSearch(search: ExecutionSearch): ExecutionFilters {
  return {
    statuses: (search.status ?? '')
      .split(',')
      .map((s) => s.trim())
      .filter((s): s is ExecutionStatus => STATUSES.has(s)),
    runbook: search.runbook ?? '',
    tags: (search.tags ?? '')
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean),
  };
}

export function toExecutionSearch(filters: ExecutionFilters): ExecutionSearch {
  return {
    ...(filters.statuses.length ? { status: filters.statuses.join(',') } : {}),
    ...(filters.runbook ? { runbook: filters.runbook } : {}),
    ...(filters.tags.length ? { tags: filters.tags.join(',') } : {}),
  };
}

export function validateExecutionSearch(search: Record<string, unknown>): ExecutionSearch {
  return {
    ...(typeof search.status === 'string' ? { status: search.status } : {}),
    ...(typeof search.runbook === 'string' ? { runbook: search.runbook } : {}),
    ...(typeof search.tags === 'string' ? { tags: search.tags } : {}),
  };
}
