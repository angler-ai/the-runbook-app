// Shared UI types (A12). The outline contract is fixed by the plan (§2): it is the
// wasm compiler's `outline` output and the input to the Graph (A7) and Outline (A8)
// views. Kinds mirror runbook_spec::ir::NodeKind (crates/runbook-spec/src/ir.rs).
// Change it together with the compiler's outline output.

export type OutlineNodeKind =
  | 'task'
  | 'group'
  | 'parallel'
  | 'foreach'
  | 'loop'
  | 'switch'
  | 'wait'
  | 'approval'
  | 'set'
  | 'end'
  | 'call';

export type OutlineActor = 'human' | 'agent' | 'any';

// UI-2: optional detail for the graph's node cards and side panel. Every
// field below is additive and optional; older outlines omit them.
export type OutlineBranchKind = 'case' | 'default' | 'catch' | 'finally';

export type OutlineBranch = {
  /** A switch case's condition text (`inputs.risk == 'high'`), or the kind. */
  label: string;
  kind?: OutlineBranchKind;
  /** A switch case's condition as written (`${{ … }}`). */
  when?: string;
  children: OutlineNode[];
};

export type OutlineOutput = { name: string; type: string; required?: boolean };

export type OutlineApproval = {
  prompt: string;
  options: { id: string; title: string; auto?: boolean }[];
  /** As written: `team:oncall`, `user:Alice`. */
  approvers: string[];
  min: number;
  timeout?: string;
  onTimeout?: 'escalate' | 'decide';
  timeoutDecision?: string;
  escalateTo?: string[];
  escalationTimeout?: string;
  allowAgents?: boolean;
};

export type OutlineWait =
  | { kind: 'signal'; signal: string; timeout?: string }
  | { kind: 'for'; duration: string; timeout?: string }
  | { kind: 'until'; until: string; timeout?: string };

export type OutlineJoin = { kind: 'all' | 'any' } | { kind: 'at_least'; count: number };

export type OutlineForeach = {
  items: string;
  as: string;
  concurrency: number;
  maxItems: number;
  /** UI-3: the `key:` expression as written; absent when iterations are keyed by index. */
  key?: string;
};

export type OutlineLoop = {
  until?: string;
  while?: string;
  maxIterations: number;
  delay?: string;
};

export type OutlineCall = { runbook: string; version?: number; with?: string[] };

export type OutlineSet = { fields?: string[]; vars?: string[] };

export type OutlineEnd = { status: 'succeeded' | 'failed' | 'cancelled'; message?: string };

export type OutlineNode = {
  id: string;
  title?: string;
  path: string;
  kind: OutlineNodeKind;
  actor?: OutlineActor;
  check?: boolean;
  destructive?: boolean;
  hint?: string;
  children?: OutlineNode[];
  branches?: OutlineBranch[];
  /** The step's `if`, as written. */
  guard?: string;
  /** Omitted for the default `fail`. */
  onFailure?: 'continue' | 'handoff';
  /** Compact duration (`20m`), task defaults applied. */
  timeout?: string;
  instructions?: string;
  /** As written: `team:oncall`, `user:Alice` or an expression. */
  assignee?: string;
  skill?: string;
  skills?: string[];
  checklist?: string[];
  outputs?: OutlineOutput[];
  approval?: OutlineApproval;
  wait?: OutlineWait;
  join?: OutlineJoin;
  foreach?: OutlineForeach;
  loop?: OutlineLoop;
  call?: OutlineCall;
  set?: OutlineSet;
  end?: OutlineEnd;
};

export type Outline = {
  id: string;
  title: string;
  description?: string;
  steps: OutlineNode[];
  finally: OutlineNode[];
  stepBudget: number | null;
};
