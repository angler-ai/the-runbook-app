// UI-2: the selected step's details, beside the graph (over it on narrow
// panes). Everything here comes from the outline; nothing is fetched.
import type { ReactNode } from 'react';
import { ExternalLink, TriangleAlert, X } from 'lucide-react';
import type { OutlineBranch, OutlineNode } from '../types';
import { cn } from '../primitives/cn';
import { TemplateText } from './TemplateText';
import { actorLabel, branchLabel, joinLabel, joinMeaning, nodeTitle, waitLabel } from './labels';
import { KindIcon, kindWord } from './RunbookNodeComponent';
import { onLinkClick, useGraph } from './context';

export interface NodeDetailsProps {
  node: OutlineNode;
  onClose: () => void;
  callHref?: (slug: string) => string | undefined;
  className?: string;
  /**
   * UI-3: a run's details for this step, shown first; the step's
   * definition then folds under "Definition".
   */
  children?: ReactNode;
  /**
   * UI-3: the heading's id (the run page focuses it after an action). The
   * heading always takes focus: the graph moves focus to it when the panel
   * opens, which announces the panel and its step.
   */
  headingId?: string;
  /** UI-3: words beside the kind, such as the run state. */
  badge?: ReactNode;
  /** UI-3: the title to show (a run's rendered title) instead of the node's. */
  title?: string;
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[96px_1fr] gap-x-3 gap-y-0.5 py-1.5 text-sm">
      <dt className="text-fg-muted">{label}</dt>
      <dd className="min-w-0 break-words text-fg">{children}</dd>
    </div>
  );
}

function Mono({ children }: { children: ReactNode }) {
  return <code className="font-mono text-[0.92em] text-fg">{children}</code>;
}

function Expr({ text }: { text: string }) {
  return <TemplateText text={text} className="font-mono text-[0.92em]" />;
}

function caseRows(branches: OutlineBranch[]) {
  return branches.map((b, i) => (
    <li key={`${b.kind ?? b.label}-${i}`} className="flex items-baseline gap-2">
      <span className="shrink-0 text-fg-muted">{b.kind === 'case' ? `${i + 1}.` : '–'}</span>
      <span className="min-w-0">
        {b.kind === 'case' && b.when ? <Expr text={b.when} /> : branchLabel(b)}
        <span className="text-fg-muted">
          {' '}
          → {b.children.length === 0 ? 'nothing' : b.children.map((c) => c.id).join(', ')}
        </span>
      </span>
    </li>
  ));
}

/** The step's definition: inline on the runbook page, folded under a run's details. */
function Definition({ folded, children }: { folded: boolean; children: ReactNode }) {
  if (!folded) return <>{children}</>;
  return (
    <details data-slot="step-definition" className="mt-4 border-t border-border pt-3">
      <summary className="cursor-pointer select-none text-xs font-medium uppercase tracking-wider text-fg-muted hover:text-fg">
        Definition
      </summary>
      <div className="mt-2">{children}</div>
    </details>
  );
}

export function NodeDetails({
  node,
  onClose,
  callHref,
  className,
  children,
  headingId,
  badge,
  title: titleOverride,
}: NodeDetailsProps) {
  const title = titleOverride ?? nodeTitle(node);
  const href = node.call ? callHref?.(node.call.runbook) : undefined;
  const { navigate } = useGraph();
  return (
    <aside
      data-slot="node-details"
      aria-label="Step details"
      className={cn(
        'flex h-full w-full flex-col overflow-hidden border-l border-border bg-surface text-fg shadow-overlay sm:w-[340px] md:shadow-none',
        className,
      )}
    >
      <header className="flex items-start gap-2 border-b border-border px-4 py-3">
        <KindIcon node={node} className="mt-0.5 size-4 shrink-0 text-fg-muted" />
        <div className="min-w-0 flex-1">
          <p className="flex items-center gap-2 text-2xs font-medium uppercase tracking-wider text-fg-muted">
            {kindWord(node)}
            {badge}
          </p>
          <h2
            id={headingId}
            data-slot="node-details-heading"
            tabIndex={-1}
            className="rounded-sm text-md font-semibold leading-snug focus-visible:outline-2 focus-visible:outline-focus"
          >
            <TemplateText text={title} />
          </h2>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label="Close step details"
          className="-mr-1 rounded-md p-1 text-fg-muted hover:bg-hover hover:text-fg"
        >
          <X className="size-4" aria-hidden="true" />
        </button>
      </header>
      <div className="flex-1 overflow-y-auto px-4 py-3">
        {children}
        <Definition folded={children !== undefined}>
          {node.destructive && (
            <p className="mb-3 flex items-center gap-2 rounded-md bg-status-failed-bg px-3 py-2 text-sm font-medium text-status-failed-fg">
              <TriangleAlert className="size-4 shrink-0" aria-hidden="true" />
              Destructive: changes production state and needs care to undo.
            </p>
          )}
          <dl className="divide-y divide-border">
            <Fact label="Step id">
              <Mono>{node.id}</Mono>
            </Fact>
            <Fact label="Source">
              <Mono>{node.path}</Mono>
            </Fact>
            {node.actor && <Fact label="Actor">{actorLabel(node.actor)}</Fact>}
            {node.assignee && (
              <Fact label="Assignee">
                <TemplateText text={node.assignee} />
              </Fact>
            )}
            {node.guard && (
              <Fact label="Runs if">
                <Expr text={node.guard} />
              </Fact>
            )}
            {node.timeout && <Fact label="Timeout">{node.timeout}</Fact>}
            {node.onFailure && <Fact label="On failure">{node.onFailure}</Fact>}
            {node.skills && node.skills.length > 0 ? (
              <Fact label={node.skills.length > 1 ? 'Skills' : 'Skill'}>
                <div className="flex flex-wrap gap-1">
                  {node.skills.map((s) => (
                    <Mono key={s}>{s}</Mono>
                  ))}
                </div>
              </Fact>
            ) : node.skill ? (
              <Fact label="Skill">
                <Mono>{node.skill}</Mono>
              </Fact>
            ) : null}

            {node.wait && (
              <Fact label="Waits">
                {node.wait.kind === 'signal' ? (
                  <>
                    for signal <Mono>{node.wait.signal}</Mono>
                    <span className="block text-xs text-fg-muted">
                      Sent by a person, another system or the API when it happens.
                    </span>
                  </>
                ) : node.wait.kind === 'until' ? (
                  <>
                    until <Expr text={node.wait.until} />
                  </>
                ) : (
                  waitLabel(node.wait).value
                )}
              </Fact>
            )}
            {node.wait?.timeout && <Fact label="Gives up after">{node.wait.timeout}</Fact>}

            {node.join && (
              <Fact label="Join">
                {joinLabel(node.join).replace('Join: ', '')}
                <span className="block text-xs text-fg-muted">{joinMeaning(node.join)}</span>
              </Fact>
            )}
            {node.foreach && (
              <>
                <Fact label="Items">
                  <Expr text={node.foreach.items} />
                </Fact>
                <Fact label="Each as">
                  <Mono>{node.foreach.as}</Mono>
                </Fact>
                <Fact label="Concurrency">
                  {node.foreach.concurrency} at a time, at most {node.foreach.maxItems} items
                </Fact>
              </>
            )}
            {node.loop && (
              <>
                {node.loop.until && (
                  <Fact label="Until">
                    <Expr text={node.loop.until} />
                  </Fact>
                )}
                {node.loop.while && (
                  <Fact label="While">
                    <Expr text={node.loop.while} />
                  </Fact>
                )}
                <Fact label="At most">
                  {node.loop.maxIterations} {node.loop.maxIterations === 1 ? 'time' : 'times'}
                  {node.loop.delay ? `, ${node.loop.delay} apart` : ''}
                </Fact>
              </>
            )}
            {node.kind === 'switch' && node.branches && (
              <Fact label="Cases">
                <ol className="space-y-1">{caseRows(node.branches)}</ol>
              </Fact>
            )}
            {node.kind === 'group' && node.branches && node.branches.length > 0 && (
              <Fact label="Handlers">
                <ul className="space-y-1">{caseRows(node.branches)}</ul>
              </Fact>
            )}

            {node.approval && (
              <>
                <Fact label="Prompt">
                  <TemplateText text={node.approval.prompt} />
                </Fact>
                <Fact label="Options">{node.approval.options.map((o) => o.title).join(' / ')}</Fact>
                <Fact label="Approvers">
                  {node.approval.approvers.length > 0
                    ? node.approval.approvers.join(', ')
                    : 'Anyone'}
                  {node.approval.min > 1 ? ` (${node.approval.min} needed)` : ''}
                  {node.approval.allowAgents ? ' · agents may decide' : ''}
                </Fact>
                {node.approval.timeout && (
                  <Fact label="Timeout">
                    {node.approval.timeout}
                    {node.approval.onTimeout === 'decide' && node.approval.timeoutDecision
                      ? `, then decides ${node.approval.timeoutDecision}`
                      : node.approval.escalateTo
                        ? `, then escalates to ${node.approval.escalateTo.join(', ')}${node.approval.escalationTimeout ? ` for ${node.approval.escalationTimeout}` : ''}`
                        : ''}
                  </Fact>
                )}
              </>
            )}

            {node.call && (
              <>
                <Fact label="Runs">
                  {href ? (
                    <a
                      href={href}
                      data-slot="call-link"
                      onClick={(e) => onLinkClick(e, navigate)}
                      className="inline-flex items-center gap-1 font-medium text-accent underline-offset-2 hover:underline"
                    >
                      {node.call.runbook}
                      <ExternalLink className="size-3.5" aria-hidden="true" />
                    </a>
                  ) : (
                    <Mono>{node.call.runbook}</Mono>
                  )}
                  <span className="block text-xs text-fg-muted">
                    {node.call.version
                      ? `Pinned to version ${node.call.version}.`
                      : 'The latest published version when this runbook is published.'}
                  </span>
                </Fact>
                {node.call.with && node.call.with.length > 0 && (
                  <Fact label="Inputs">
                    <Mono>{node.call.with.join(', ')}</Mono>
                  </Fact>
                )}
              </>
            )}
            {node.set && (
              <>
                {node.set.fields && node.set.fields.length > 0 && (
                  <Fact label="Fields">
                    <Mono>{node.set.fields.join(', ')}</Mono>
                  </Fact>
                )}
                {node.set.vars && node.set.vars.length > 0 && (
                  <Fact label="Vars">
                    <Mono>{node.set.vars.join(', ')}</Mono>
                  </Fact>
                )}
              </>
            )}
            {node.end && (
              <Fact label="Ends run">
                {node.end.status}
                {node.end.message ? ` — ${node.end.message}` : ''}
              </Fact>
            )}

            {node.outputs && node.outputs.length > 0 && (
              <Fact label="Outputs">
                <ul className="space-y-0.5">
                  {node.outputs.map((o) => (
                    <li key={o.name}>
                      <Mono>{o.name}</Mono>{' '}
                      <span className="text-fg-muted">
                        {o.type}
                        {o.required ? ', required' : ''}
                      </span>
                    </li>
                  ))}
                </ul>
              </Fact>
            )}
            {node.checklist && node.checklist.length > 0 && (
              <Fact label="Checklist">
                <ul className="list-disc space-y-0.5 pl-4">
                  {node.checklist.map((item) => (
                    <li key={item}>
                      <TemplateText text={item} />
                    </li>
                  ))}
                </ul>
              </Fact>
            )}
          </dl>
          {node.instructions && (
            <section className="mt-3">
              <h3 className="mb-1 text-xs font-medium uppercase tracking-wider text-fg-muted">
                Instructions
              </h3>
              <p className="whitespace-pre-wrap text-sm leading-relaxed">
                <TemplateText text={node.instructions} />
              </p>
            </section>
          )}
        </Definition>
      </div>
    </aside>
  );
}
