import { useState } from 'react';
import { Input } from '../../ui/primitives/input';
import { Button } from '../../ui/primitives/button';
import { runnerError, useRunnerQuery, type Items, type RunnerStatus } from '../runner/api';
import { Field, panel, sentenceCase } from '../runner/presentation';
import { Problem } from '../runner/runnerForms';
import type { ToolConnection } from '../runner/resources';

export type ToolBinding = { server_id: string; tool: string };

export function AgentToolPicker({
  projectId,
  selected,
  onChange,
  status,
  guidedConnection,
}: {
  projectId: string;
  selected: ToolBinding[];
  onChange(value: ToolBinding[]): void;
  status: RunnerStatus;
  guidedConnection?: string;
}) {
  const query = useRunnerQuery<Items<ToolConnection>>(status, `/projects/${projectId}/mcp-tools`);
  const connections = query.data?.items ?? [];
  const [search, setSearch] = useState('');
  const [expanded, setExpanded] = useState(() => selected.map((tool) => tool.server_id));

  if (query.error) return <Problem>{runnerError(query.error)}</Problem>;
  if (query.isPending)
    return (
      <p role="status" className="text-sm text-fg-muted">
        Loading project tools…
      </p>
    );
  const byId = new Map(connections.map((connection) => [connection.id, connection]));
  const unavailable = selected.filter(
    ({ server_id, tool }) => !byId.get(server_id)?.tools.some(({ name }) => name === tool),
  );
  const shown = guidedConnection
    ? connections.filter(({ id }) => id === guidedConnection)
    : connections;
  return (
    <div className="grid gap-2">
      {shown.length > 0 && (
        <Field label="Search agent tools">
          <Input type="search" value={search} onChange={(event) => setSearch(event.target.value)} />
        </Field>
      )}
      {!connections.length && (
        <p className="text-sm text-fg-muted">No connections are shared with this project.</p>
      )}
      {unavailable.length > 0 && (
        <section className={panel + ' grid gap-2'}>
          <h4 className="text-sm font-medium">Unavailable saved tools</h4>
          <ul className="grid gap-2">
            {unavailable.map((binding) => (
              <li
                key={JSON.stringify([binding.server_id, binding.tool])}
                className="flex flex-wrap items-center justify-between gap-2"
              >
                <span className="text-sm break-all">
                  {binding.tool}
                  {byId.has(binding.server_id) && (
                    <span className="text-fg-muted"> · {byId.get(binding.server_id)?.name}</span>
                  )}
                </span>
                <Button
                  type="button"
                  size="sm"
                  variant="secondary"
                  aria-label={`Remove ${binding.tool}`}
                  onClick={() => onChange(selected.filter((other) => other !== binding))}
                >
                  Remove
                </Button>
              </li>
            ))}
          </ul>
        </section>
      )}
      {shown.map((connection) => {
        const enabled = guidedConnection === connection.id || expanded.includes(connection.id);
        const matching = connection.tools.filter(({ name }) =>
          name.toLowerCase().includes(search.trim().toLowerCase()),
        );
        const count = selected.filter(({ server_id }) => server_id === connection.id).length;
        return (
          <div className={panel + ' grid gap-2'} key={connection.id}>
            <label className="flex items-center gap-2 font-medium">
              {guidedConnection !== connection.id && (
                <input
                  type="checkbox"
                  name="connection-tools"
                  className="accent-accent"
                  checked={enabled}
                  onChange={(event) => {
                    if (event.target.checked) setExpanded([...expanded, connection.id]);
                    else {
                      setExpanded(expanded.filter((id) => id !== connection.id));
                      onChange(selected.filter((binding) => binding.server_id !== connection.id));
                    }
                  }}
                />
              )}
              {connection.name}
              <span className="text-xs font-normal text-fg-muted" aria-hidden="true">
                · {count} selected
              </span>
            </label>
            {enabled &&
              (matching.length > 0 ? (
                <ul className="ml-2 grid gap-2 border-l border-border pl-4">
                  {matching.map((tool) => (
                    <li key={tool.name}>
                      <label className="flex flex-wrap items-center gap-2 text-sm">
                        <input
                          type="checkbox"
                          name="connection-tools"
                          className="accent-accent"
                          checked={selected.some(
                            (binding) =>
                              binding.server_id === connection.id && binding.tool === tool.name,
                          )}
                          onChange={(event) =>
                            onChange(
                              event.target.checked
                                ? [...selected, { server_id: connection.id, tool: tool.name }]
                                : selected.filter(
                                    (binding) =>
                                      binding.server_id !== connection.id ||
                                      binding.tool !== tool.name,
                                  ),
                            )
                          }
                        />
                        <span className="break-all">{tool.name}</span>
                        <span className="rounded border border-border px-1.5 py-0.5 text-xs text-fg-muted">
                          {sentenceCase(tool.label)}
                        </span>
                      </label>
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="text-sm text-fg-muted">
                  {search && connection.tools.length
                    ? 'No matching tools.'
                    : connection.pinned_snapshot_id
                      ? 'No allowed tools are available from the current pin.'
                      : 'Ask a runner admin to pin tools for this connection.'}
                </p>
              ))}
          </div>
        );
      })}
    </div>
  );
}
