export type {
  Tool,
  ToolSnapshot as Snapshot,
  PinnedTools,
  PinToolsInput,
  ToolDrift,
  ToolConnection,
} from '../../../../runner/packages/shared/src/contracts/catalog.ts';
import type { Budget } from '../../../../runner/packages/shared/src/contracts/budgets.ts';
import type {
  Environment as SharedEnvironment,
  EnvironmentBudgets,
} from '../../../../runner/packages/shared/src/contracts/environments.ts';

export type ToolLabel = 'read' | 'write' | 'destructive';
export type EgressRule = { host: string; label: 'read' | 'write' };
export type Share = { projects: 'all' | 'none' | string[] };
export type Named = { id: string; name: string };
export type Project = { id: string; title: string; slug: string };
export type Connection = Named & {
  url: string;
  transport: 'streamable-http' | 'sse';
  credential_id?: string | null;
  sharing: Share;
  pinned_snapshot_id?: string | null;
  pinned_tool_count?: number;
  tool_labels?: Record<string, ToolLabel>;
  never_allowed?: string[];
  drift?: boolean;
};
export type { Budget };
export type Environment = Omit<SharedEnvironment, 'egress'> & { egress: EgressRule[] | null };
export type Limits = EnvironmentBudgets;
export type EnvironmentInput = { name: string; description?: string } & Partial<
  Pick<
    Environment,
    | 'image'
    | 'setup'
    | 'setup_timeout_s'
    | 'env'
    | 'credential_env'
    | 'size'
    | 'timeout_s'
    | 'egress'
    | 'budgets'
  >
>;
export type BudgetOverview = { time_zone: string; items: Budget[]; next_cursor?: string | null };

export function sharedWith(share: Share | undefined, project: string) {
  return (
    share?.projects === 'all' ||
    (Array.isArray(share?.projects) && share.projects.includes(project))
  );
}
