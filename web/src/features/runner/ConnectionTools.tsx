import { useState } from 'react';
import { GitCompare, RefreshCw, Wrench } from 'lucide-react';
import { EmptyState } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import { StatusPill } from '../../ui/status/StatusPill';
import { runnerError, useRunnerMutation, useRunnerQuery, type RunnerStatus } from './api';
import { Input } from '../../ui/primitives/input';
import { Field, Rows } from './presentation';
import { FormSection, Problem, ReviewChanges, selectClasses } from './runnerForms';
import type {
  Connection,
  PinnedTools,
  PinToolsInput,
  Snapshot,
  Tool,
  ToolDrift,
  ToolLabel,
} from './resources';

function toolWords(name: string) {
  return name
    .replace(/([a-z])([A-Z])/g, '$1_$2')
    .toLowerCase()
    .split(/[^a-z0-9]+/);
}

const writeVerbs = new Set([
  'write',
  'update',
  'create',
  'delete',
  'send',
  'remove',
  'execute',
  'mutate',
  'archive',
  'insert',
  'replace',
  'merge',
  'publish',
  'set',
  'edit',
]);

function looksReadOnly(name: string) {
  const words = toolWords(name);
  return (
    ['get', 'list', 'search', 'fetch', 'read', 'describe', 'retrieve', 'lookup'].includes(
      words[0] ?? '',
    ) && !words.some((word) => writeVerbs.has(word))
  );
}

function schemaSummary(before?: Tool, after?: Tool) {
  const previous = before?.inputSchema ?? {};
  const next = after?.inputSchema ?? {};
  const oldFields = Object.keys(previous.properties ?? {});
  const newFields = Object.keys(next.properties ?? {});
  const added = newFields.filter((field) => !oldFields.includes(field));
  const removed = oldFields.filter((field) => !newFields.includes(field));
  return (
    [
      added.length ? `Added inputs: ${added.join(', ')}` : '',
      removed.length ? `Removed inputs: ${removed.join(', ')}` : '',
      JSON.stringify(previous.required) !== JSON.stringify(next.required)
        ? 'Required inputs changed'
        : '',
    ]
      .filter(Boolean)
      .join(' · ') || 'Input types or constraints changed'
  );
}

function ToolSchema({ tool }: { tool: Tool }) {
  return (
    <pre className="max-h-52 overflow-auto text-xs">
      {JSON.stringify(tool.inputSchema ?? {}, null, 2)}
    </pre>
  );
}

function ToolDiff({ before, after }: { before?: Tool; after?: Tool }) {
  return (
    <div className="grid min-w-0 gap-2 sm:grid-cols-2">
      {(
        [
          ['Pinned', before],
          ['Discovered', after],
        ] as const
      ).map(([label, tool]) => (
        <div key={label} className="min-w-0 rounded-md border border-border bg-bg p-2">
          <p className="mb-1 text-xs font-medium text-fg-muted">{label}</p>
          {tool ? (
            <>
              {tool.description && <p className="mb-1 text-xs">{tool.description}</p>}
              <ToolSchema tool={tool} />
            </>
          ) : (
            <p className="text-xs text-fg-muted">Tool absent</p>
          )}
        </div>
      ))}
    </div>
  );
}

export function ConnectionTools({
  connection,
  status,
  editable,
  onPinned,
}: {
  connection: Connection;
  status: RunnerStatus;
  editable: boolean;
  onPinned?: () => void;
}) {
  const mutation = useRunnerMutation(status);
  const path = `/mcp-servers/${connection.id}`;
  const pinnedQuery = useRunnerQuery<PinnedTools>(status, path + '/tools');
  const driftQuery = useRunnerQuery<ToolDrift>(status, path + '/drift');
  const pinned = pinnedQuery.data?.snapshot;
  const latest = driftQuery.data?.latest;
  const [reviewId, setReviewId] = useState<string>();
  const candidate =
    latest && latest.snapshot_id === reviewId && latest.snapshot_id !== pinned?.snapshot_id
      ? latest
      : undefined;
  const [labelEdits, setLabelEdits] = useState<Record<string, ToolLabel>>({});
  const [accessEdits, setAccessEdits] = useState<Record<string, boolean>>({});
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const [review, setReview] = useState(false);
  const [search, setSearch] = useState('');
  const [filter, setFilter] = useState('all');
  const tools = candidate?.tools ?? pinned?.tools ?? [];
  const diff = driftQuery.data?.changes;
  const currentTools = new Map(latest?.tools.map((tool) => [tool.name, tool]));
  const changes = diff
    ? [
        ...diff.added.map((name) => ({
          name,
          kind: 'Added' as const,
          after: currentTools.get(name),
        })),
        ...diff.changed.map(({ name, before, schema_changed }) => ({
          name,
          kind: 'Changed' as const,
          before,
          after: currentTools.get(name),
          schema_changed,
        })),
        ...diff.removed.map((before) => ({ name: before.name, kind: 'Removed' as const, before })),
      ]
    : [];
  const loading = pinnedQuery.isPending || driftQuery.isPending;
  const disabled = !editable || mutation.isPending || loading;
  const labels = { ...connection.tool_labels, ...labelEdits };
  const blocked = new Set(connection.never_allowed);
  if (candidate) for (const name of diff?.added ?? []) blocked.add(name);
  for (const [name, allowed] of Object.entries(accessEdits)) {
    if (allowed) blocked.delete(name);
    else blocked.add(name);
  }
  const labelFor = (name: string) => (Object.hasOwn(labels, name) ? labels[name]! : 'destructive');

  const savedBlocked = new Set(connection.never_allowed);
  const pinnedNames = new Set(pinned?.tools.map((tool) => tool.name));
  const wasAllowed = (name: string) => pinnedNames.has(name) && !savedBlocked.has(name);
  const savedLabel = (name: string) =>
    Object.hasOwn(connection.tool_labels ?? {}, name)
      ? connection.tool_labels![name]!
      : 'destructive';
  const higherRisk = (name: string) =>
    labelFor(name) !== 'read' ||
    writeVerbs.has(toolWords(name)[0] ?? '') ||
    (pinnedNames.has(name) &&
      (wasAllowed(name) || Object.hasOwn(connection.tool_labels ?? {}, name)) &&
      savedLabel(name) !== 'read');
  const readCandidates = tools.filter(
    ({ name }) =>
      looksReadOnly(name) &&
      !(wasAllowed(name) && savedLabel(name) !== 'read') &&
      (blocked.has(name) || labelFor(name) === 'read') &&
      (!Object.hasOwn(connection.tool_labels ?? {}, name) || savedLabel(name) === 'read'),
  );
  const matchesName = (name: string) => name.toLowerCase().includes(search.trim().toLowerCase());
  const matchesFilter = (name: string) => {
    switch (filter) {
      case 'new':
        return diff?.added.includes(name);
      case 'changed':
        return changes.some((change) => change.name === name && change.kind !== 'Added');
      case 'allowed':
        return !blocked.has(name);
      case 'risk':
        return higherRisk(name);
      default:
        return true;
    }
  };
  const visibleTools = tools.filter(({ name }) => matchesName(name) && matchesFilter(name));
  const visibleChanges = changes.filter(({ name }) => matchesName(name) && matchesFilter(name));
  const permissionChanges = tools.flatMap(({ name }) => {
    const before = wasAllowed(name);
    const after = !blocked.has(name);
    const classification = labelFor(name);
    return before !== after || savedLabel(name) !== classification
      ? [
          `${name} · ${before ? 'Allowed' : 'Blocked'} → ${after ? 'Allowed' : 'Blocked'} · ${savedLabel(name)} → ${classification}`,
        ]
      : [];
  });
  if (candidate)
    for (const name of pinnedNames) {
      if (!tools.some((tool) => tool.name === name))
        permissionChanges.push(`${name} · Removed from pin`);
    }
  const allowedCount = tools.filter(({ name }) => !blocked.has(name)).length;
  const pinLabel = `Pin ${allowedCount} allowed ${allowedCount === 1 ? 'tool' : 'tools'} · keep ${tools.length - allowedCount} blocked`;

  async function discover() {
    setError('');
    setMessage('');
    try {
      const snapshot = (await mutation.mutateAsync({
        path: path + '/discover',
        method: 'POST',
        body: {},
      })) as Snapshot;
      setReviewId(snapshot.snapshot_id);
      setLabelEdits({});
      setAccessEdits({});
    } catch (problem) {
      setError(runnerError(problem));
    }
  }

  async function save() {
    setError('');
    setMessage('');
    try {
      const access = { tool_labels: labels, never_allowed: [...blocked] };
      if (candidate) {
        const body: PinToolsInput = { snapshot_id: candidate.snapshot_id, ...access };
        await mutation.mutateAsync({ path: path + '/pin', method: 'POST', body });
        setReviewId(undefined);
        onPinned?.();
      } else {
        await mutation.mutateAsync({ path, method: 'PATCH', body: access });
      }
      setLabelEdits({});
      setAccessEdits({});
      setMessage(candidate ? 'Tool snapshot pinned.' : 'Tool access saved.');
      setReview(false);
    } catch (problem) {
      setReview(false);
      setError(runnerError(problem));
    }
  }

  return (
    <div className="grid gap-4">
      <FormSection
        title="Tool access"
        description="New tools start blocked. Allow only the tools projects can grant to agents."
      >
        <div className="flex flex-wrap gap-2">
          {editable && (
            <Button
              variant={tools.length ? 'secondary' : 'primary'}
              disabled={disabled}
              onClick={() => void discover()}
            >
              <RefreshCw aria-hidden /> {mutation.isPending ? 'Working…' : 'Discover tools'}
            </Button>
          )}
          {editable && tools.length > 0 && (
            <>
              <Button
                variant="secondary"
                disabled={disabled}
                onClick={() =>
                  setLabelEdits({
                    ...labelEdits,
                    ...Object.fromEntries(
                      readCandidates.map((tool) => [tool.name, 'read'] as const),
                    ),
                  })
                }
              >
                Label all read
              </Button>
              <Button
                variant="secondary"
                disabled={disabled}
                onClick={() =>
                  setAccessEdits({
                    ...accessEdits,
                    ...Object.fromEntries(
                      tools
                        .filter((tool) => higherRisk(tool.name))
                        .map((tool) => [tool.name, false]),
                    ),
                  })
                }
              >
                Block all writes
              </Button>
            </>
          )}
        </div>
        <div className="flex flex-wrap items-end gap-3">
          <Field label="Search tools">
            <Input
              type="search"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
          </Field>
          <Field label="Show tools">
            <select
              className={selectClasses}
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
            >
              <option value="all">All tools</option>
              <option value="new">New</option>
              <option value="changed">Changed or removed</option>
              <option value="allowed">Allowed</option>
              <option value="risk">Higher risk</option>
            </select>
          </Field>
        </div>
        <div className="sticky top-0 z-10 flex flex-wrap items-center justify-between gap-2 border-b border-border bg-surface py-2">
          <p role="status" className="text-xs text-fg-muted">
            {allowedCount} allowed · {tools.length - allowedCount} blocked
          </p>
          {editable && !candidate && latest && latest.snapshot_id !== pinned?.snapshot_id && (
            <Button
              className="w-fit"
              variant="secondary"
              disabled={disabled}
              onClick={() => {
                setLabelEdits({});
                setAccessEdits({});
                setReviewId(latest.snapshot_id);
              }}
            >
              Review discovered tools
            </Button>
          )}
          {editable && candidate && (
            <Button type="button" disabled={disabled} onClick={() => setReview(true)}>
              <GitCompare aria-hidden /> Review and pin
            </Button>
          )}
          {editable && !candidate && permissionChanges.length > 0 && (
            <Button type="button" disabled={disabled} onClick={() => setReview(true)}>
              Save tool access
            </Button>
          )}
        </div>
        {review && (
          <ReviewChanges
            title="Review tool access"
            description="Review the tools and classifications projects will be able to grant to agents."
            confirm={candidate ? pinLabel : 'Confirm tool access'}
            close={() => setReview(false)}
            onConfirm={save}
            pending={mutation.isPending}
          >
            {permissionChanges.length ? (
              <ul className="grid gap-2 text-sm break-all">
                {permissionChanges.map((change) => (
                  <li key={change}>{change}</li>
                ))}
              </ul>
            ) : (
              <p className="text-sm">No permission changes.</p>
            )}
          </ReviewChanges>
        )}
        {pinnedQuery.error ? (
          <Problem>{runnerError(pinnedQuery.error)}</Problem>
        ) : pinnedQuery.isPending ? (
          <p role="status" className="text-sm text-fg-muted">
            Loading pinned tools…
          </p>
        ) : tools.length ? (
          <Rows
            label={candidate ? 'Discovered tools' : 'Pinned tools'}
            stackOnMobile
            columns={['Tool', 'Classification', 'Access']}
            rows={visibleTools.map((tool) => ({
              id: tool.name,
              cells: [
                <div className="min-w-36">
                  <p className="font-medium break-all">{tool.name}</p>
                  <p className="mt-1 text-xs break-words text-fg-muted">
                    {tool.description ?? 'No description'}
                  </p>
                  {tool.inputSchema && (
                    <details className="mt-2 rounded-md border border-border bg-bg px-2 py-1">
                      <summary className="cursor-pointer text-xs">Input schema</summary>
                      <div className="mt-2 min-w-0">
                        <ToolSchema tool={tool} />
                      </div>
                    </details>
                  )}
                </div>,
                <select
                  aria-label={`Classification for ${tool.name}`}
                  className={selectClasses}
                  disabled={disabled}
                  value={labelFor(tool.name)}
                  onChange={(event) =>
                    setLabelEdits({ ...labelEdits, [tool.name]: event.target.value as ToolLabel })
                  }
                >
                  <option value="read">Read</option>
                  <option value="write">Write</option>
                  <option value="destructive">Destructive</option>
                </select>,
                <label className="flex min-h-10 items-center gap-2 whitespace-nowrap">
                  <input
                    aria-label={`Allow ${tool.name}`}
                    type="checkbox"
                    className="accent-accent"
                    disabled={disabled}
                    checked={!blocked.has(tool.name)}
                    onChange={(event) =>
                      setAccessEdits({ ...accessEdits, [tool.name]: event.target.checked })
                    }
                  />
                  {!blocked.has(tool.name) ? 'Allowed' : 'Blocked'}
                </label>,
              ],
            }))}
          />
        ) : (
          <EmptyState
            icon={Wrench}
            title={
              candidate
                ? 'No tools returned'
                : pinned
                  ? 'No tools in the pinned snapshot'
                  : 'No tools pinned'
            }
            description={'Discover and review the tools this system makes available.'}
          />
        )}
      </FormSection>
      {driftQuery.error && <Problem>{runnerError(driftQuery.error)}</Problem>}
      {latest && latest.snapshot_id !== pinned?.snapshot_id && (
        <FormSection title="Changes since pin">
          {visibleChanges.length === 0 ? (
            <p className="text-sm text-fg-muted">No matching changes since the last pin.</p>
          ) : (
            <ul className="grid gap-2">
              {visibleChanges.map((change) => (
                <li key={change.name} className="rounded-md border border-border px-3 py-2">
                  <details>
                    <summary className="cursor-pointer text-sm">
                      <StatusPill
                        status={
                          change.kind === 'Removed'
                            ? 'failed'
                            : change.kind === 'Changed'
                              ? 'waiting'
                              : 'succeeded'
                        }
                        label={change.kind}
                      />{' '}
                      <span className="break-all font-medium">{change.name}</span>
                    </summary>
                    <div className="mt-2">
                      {change.kind === 'Changed' && (
                        <p className="mb-2 text-xs text-fg-muted">
                          {change.schema_changed ? (
                            <>
                              Input schema changed
                              <span className="block">
                                {schemaSummary(change.before, change.after)}
                              </span>
                            </>
                          ) : (
                            'Description changed'
                          )}
                        </p>
                      )}
                      <ToolDiff
                        before={'before' in change ? change.before : undefined}
                        after={'after' in change ? change.after : undefined}
                      />
                    </div>
                  </details>
                </li>
              ))}
            </ul>
          )}
        </FormSection>
      )}
      <Problem>{error}</Problem>
      {message && (
        <p role="status" className="text-sm text-success">
          {message}
        </p>
      )}
    </div>
  );
}
