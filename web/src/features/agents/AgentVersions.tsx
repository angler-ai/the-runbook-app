import type { Row } from '../runner/api';
import { panel } from '../runner/presentation';

export interface AgentVersion {
  id: string;
  version: number;
  config: Row;
  created_at?: string;
  run_count?: number;
  runs?: { execution_id: string; title?: string }[];
}

export function Diff({ before = {}, after }: { before?: Row; after: Row }) {
  const lines = [...new Set([...Object.keys(before), ...Object.keys(after)])]
    .sort()
    .flatMap((key) => {
      const oldValue = JSON.stringify(before[key]);
      const newValue = JSON.stringify(after[key]);
      if (oldValue === newValue) return [];
      return [
        ...(oldValue === undefined ? [] : [{ sign: '-', text: `${key}: ${oldValue}` }]),
        ...(newValue === undefined ? [] : [{ sign: '+', text: `${key}: ${newValue}` }]),
      ];
    });
  if (!lines.length) return <p className="text-fg-muted">No configuration changes.</p>;
  return (
    <pre
      className="max-w-full overflow-auto whitespace-pre-wrap break-all rounded-md border border-border bg-surface-raised p-3 text-xs"
      aria-label="Configuration diff"
    >
      {lines.map((line) => (
        <span
          className={line.sign === '+' ? 'block text-success' : 'block text-danger'}
          key={line.sign + line.text}
        >
          {line.sign} {line.text}
        </span>
      ))}
    </pre>
  );
}

export function Versions({ versions }: { versions: AgentVersion[] }) {
  if (!versions.length) return <p className="text-fg-muted">No versions yet.</p>;
  return (
    <div className="grid gap-3">
      {versions.map((version, index) => (
        <section className={panel} key={version.id}>
          <h3 className="mb-2 font-semibold">Version {version.version}</h3>
          {version.created_at && (
            <p className="mb-2 text-sm text-fg-muted">
              {new Date(version.created_at).toLocaleString()}
            </p>
          )}
          <Diff before={versions[index + 1]?.config} after={version.config} />
          <h4 className="mt-3 text-sm font-medium">
            {version.run_count === undefined
              ? 'Runs using this version'
              : `${version.run_count} ${version.run_count === 1 ? 'run' : 'runs'} using this version`}
          </h4>
          {version.run_count != null && version.run_count > (version.runs?.length ?? 0) && (
            <p className="text-sm text-fg-muted">
              Showing the {version.runs?.length ?? 0} most recent.
            </p>
          )}
          {version.runs?.length ? (
            <ul>
              {version.runs.map((run) => (
                <li key={run.execution_id}>
                  <a
                    className="text-sm text-accent underline"
                    href={`/x/${encodeURIComponent(run.execution_id)}`}
                  >
                    {run.title ?? run.execution_id}
                  </a>
                </li>
              ))}
            </ul>
          ) : (
            <p className="text-sm text-fg-muted">
              {version.runs ? 'No runs yet.' : 'Run history is unavailable.'}
            </p>
          )}
        </section>
      ))}
    </div>
  );
}
