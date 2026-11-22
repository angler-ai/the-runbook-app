import { useState, type ReactNode } from 'react';
import { Button } from '../../ui/primitives/button';
import { Input, fieldClasses } from '../../ui/primitives/input';
import {
  runnerError,
  useRunnerMutation,
  useRunnerQuery,
  type Items,
  type Row,
  type RunnerStatus,
} from './api';
import { Field, list, panel, Rows, sharing } from './presentation';

type Kind = 'providers' | 'profiles' | 'mcp-servers' | 'policy';
type Spec = {
  key: string;
  label: string;
  kind?: 'number' | 'json' | 'sharing' | 'list' | 'url';
  choices?: string[];
  initial?: string;
  required?: boolean;
};
const sharingField: Spec = {
  key: 'sharing',
  label: 'Projects (none, all, or comma-separated IDs)',
  kind: 'sharing',
  initial: 'none',
};
const name: Spec = { key: 'name', label: 'Name', required: true };
const config: Record<Kind, { title: string; fields: Spec[] }> = {
  providers: {
    title: 'Model providers',
    fields: [
      name,
      { key: 'type', label: 'Type', choices: ['anthropic', 'openai-compatible'], required: true },
      { key: 'credential_id', label: 'Model credential ID', required: true },
      { key: 'endpoint', label: 'Endpoint', kind: 'url', required: true },
      {
        key: 'models',
        label: 'Models and prices (JSON)',
        kind: 'json',
        required: true,
        initial: '[{"id":"","input_usd_per_million":0,"output_usd_per_million":0}]',
      },
      sharingField,
    ],
  },
  profiles: {
    title: 'Model profiles',
    fields: [
      name,
      { key: 'provider_id', label: 'Provider ID', required: true },
      { key: 'model', label: 'Model', required: true },
      { key: 'options', label: 'Provider options (JSON)', kind: 'json', initial: '{}' },
      sharingField,
    ],
  },
  'mcp-servers': {
    title: 'MCP servers',
    fields: [
      name,
      { key: 'url', label: 'Server URL', kind: 'url', required: true },
      { key: 'transport', label: 'Transport', choices: ['streamable-http', 'sse'], required: true },
      { key: 'credential_id', label: 'MCP credential ID' },
      { key: 'tool_labels', label: 'Tool labels (JSON)', kind: 'json', initial: '{}' },
      { key: 'never_allowed', label: 'Never allowed tools (comma-separated names)', kind: 'list' },
      sharingField,
    ],
  },
  policy: {
    title: 'Policy & budgets',
    fields: [
      { key: 'org_concurrency', label: 'Organization concurrency', kind: 'number', required: true },
      { key: 'org_cost_per_day_usd', label: 'Daily budget (USD)', kind: 'number', required: true },
      { key: 'org_cost_per_month_usd', label: 'Monthly budget (USD)', kind: 'number' },
      {
        key: 'agent_concurrency',
        label: 'Default agent concurrency',
        kind: 'number',
        required: true,
      },
      {
        key: 'agent_max_cost_usd',
        label: 'Default step budget (USD)',
        kind: 'number',
        required: true,
      },
      { key: 'run_max_cost_usd', label: 'Run budget (USD)', kind: 'number' },
      { key: 'credential_max_age_days', label: 'Maximum credential age (days)', kind: 'number' },
      { key: 'time_zone', label: 'Budget time zone', required: true },
      { key: 'default_egress', label: 'Default egress (JSON)', kind: 'json' },
      { key: 'project_limits', label: 'Project limits (JSON)', kind: 'json' },
    ],
  },
};

type CatalogRow = Row & { id: string; name: string };
type Selected = { id?: string; initial: Row };

function fieldValue(spec: Spec, initial: Row) {
  const value = initial[spec.key];
  if (value == null) return spec.initial ?? '';
  if (spec.kind === 'sharing') return String((value as { projects: string | string[] }).projects);
  if (spec.kind === 'json') return JSON.stringify(value, null, 2);
  if (spec.kind === 'list' && Array.isArray(value)) return value.join(', ');
  return String(value);
}

function readFields(specs: Spec[], fields: FormData) {
  return Object.fromEntries(
    specs.map((spec) => {
      const value = String(fields.get(spec.key) ?? '').trim();
      switch (spec.kind) {
        case 'json':
          return [spec.key, JSON.parse(value || '{}')];
        case 'sharing':
          return [spec.key, sharing(value)];
        case 'list':
          return [spec.key, list(value)];
        case 'number':
          return [spec.key, value ? Number(value) : null];
        default:
          return [spec.key, value || null];
      }
    }),
  );
}

function CatalogForm({
  kind,
  selected,
  status,
  editable,
  onSaved,
}: {
  kind: Kind;
  selected: Selected;
  status: RunnerStatus;
  editable: boolean;
  onSaved?: (item: CatalogRow) => void;
}) {
  const mutation = useRunnerMutation(status);
  const [error, setError] = useState('');
  const [saved, setSaved] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const { fields } = config[kind];
  const disabled = !editable || status.status !== 'online' || mutation.isPending;
  const path = `/${kind}${selected.id ? `/${selected.id}` : ''}`;
  return (
    <form
      className={panel}
      onSubmit={async (event) => {
        event.preventDefault();
        if (disabled) return;
        setError('');
        setSaved(false);
        try {
          const body = readFields(fields, new FormData(event.currentTarget));
          if (kind === 'providers') body.auth = 'stored';
          const result = await mutation.mutateAsync({
            path,
            method: kind === 'policy' ? 'PUT' : selected.id ? 'PATCH' : 'POST',
            body,
          });
          setSaved(true);
          onSaved?.(result as CatalogRow);
        } catch (error) {
          setError(runnerError(error));
        }
      }}
    >
      <fieldset className="grid gap-3" disabled={disabled}>
        {fields.map((spec) => (
          <Field key={spec.key} label={spec.label}>
            {spec.choices ? (
              <select
                className={fieldClasses + ' h-10'}
                name={spec.key}
                defaultValue={fieldValue(spec, selected.initial)}
                required={spec.required}
              >
                {spec.choices.map((choice) => (
                  <option key={choice}>{choice}</option>
                ))}
              </select>
            ) : spec.kind === 'json' ? (
              <textarea
                className={fieldClasses + ' min-h-20 py-2 font-mono'}
                name={spec.key}
                defaultValue={fieldValue(spec, selected.initial)}
                required={spec.required}
              />
            ) : (
              <Input
                name={spec.key}
                type={spec.kind === 'number' ? 'number' : spec.kind === 'url' ? 'url' : 'text'}
                step={spec.kind === 'number' ? 'any' : undefined}
                min={spec.kind === 'number' ? 0 : undefined}
                defaultValue={fieldValue(spec, selected.initial)}
                required={spec.required}
              />
            )}
          </Field>
        ))}
        {kind === 'mcp-servers' && (
          <p className="text-sm text-fg-muted">
            Label tools read, write, or destructive. Unlabeled tools are destructive.
          </p>
        )}
        {editable && (
          <div className="flex flex-wrap gap-2">
            <Button type="submit">Save {kind === 'policy' ? 'policy' : 'changes'}</Button>
            {selected.id && !confirmDelete && (
              <Button variant="secondary" onClick={() => setConfirmDelete(true)}>
                Delete
              </Button>
            )}
            {confirmDelete && (
              <>
                <Button
                  variant="destructive"
                  onClick={async () => {
                    setError('');
                    try {
                      await mutation.mutateAsync({ path, method: 'DELETE' });
                      setConfirmDelete(false);
                      onSaved?.({ id: '', name: '' });
                    } catch (error) {
                      setError(runnerError(error));
                    }
                  }}
                >
                  Confirm delete
                </Button>
                <Button variant="secondary" onClick={() => setConfirmDelete(false)}>
                  Cancel
                </Button>
              </>
            )}
          </div>
        )}
      </fieldset>
      {saved && (
        <p role="status" className="mt-2 text-sm">
          Changes saved.
        </p>
      )}
      {error && (
        <p role="alert" className="mt-2 text-sm text-danger">
          {error}
        </p>
      )}
    </form>
  );
}

function Discover({
  row,
  status,
  editable,
}: {
  row: CatalogRow;
  status: RunnerStatus;
  editable: boolean;
}) {
  const [snapshot, setSnapshot] = useState<{
    snapshot_id: string;
    hash: string;
    tools: { name: string; description?: string }[];
  }>();
  const [message, setMessage] = useState('');
  const mutation = useRunnerMutation(status);
  const path = `/mcp-servers/${row.id}`;
  const disabled = !editable || status.status !== 'online' || mutation.isPending;
  return (
    <section className={panel}>
      <h3 className="font-semibold">Tool snapshot</h3>
      <p className="mt-1 text-sm text-fg-muted">
        {row.pinned_snapshot_id
          ? `Pinned snapshot ${String(row.pinned_snapshot_id)}`
          : 'No snapshot pinned.'}
      </p>
      {editable && (
        <Button
          className="mt-2"
          variant="secondary"
          disabled={disabled}
          onClick={async () => {
            setMessage('');
            setSnapshot(undefined);
            try {
              setSnapshot(
                (await mutation.mutateAsync({
                  path: `${path}/discover`,
                  method: 'POST',
                })) as typeof snapshot,
              );
            } catch (error) {
              setMessage(runnerError(error));
            }
          }}
        >
          Discover tools
        </Button>
      )}
      {snapshot && (
        <div className="mt-3 grid gap-2">
          <p className="break-all text-sm text-fg-muted">Snapshot {snapshot.hash}</p>
          <Rows
            label="Discovered tools"
            columns={['Tool', 'Description']}
            rows={snapshot.tools.map((tool) => ({
              id: tool.name,
              cells: [tool.name, tool.description ?? '—'],
            }))}
          />
          <Button
            className="w-fit"
            disabled={disabled}
            onClick={async () => {
              setMessage('');
              try {
                await mutation.mutateAsync({
                  path: `${path}/pin`,
                  method: 'POST',
                  body: { snapshot_id: snapshot.snapshot_id },
                });
                setSnapshot(undefined);
                setMessage('Tool snapshot pinned.');
              } catch (error) {
                setMessage(runnerError(error));
              }
            }}
          >
            Pin snapshot
          </Button>
        </div>
      )}
      {message && (
        <p role="status" className="mt-2 text-sm">
          {message}
        </p>
      )}
    </section>
  );
}

export function Catalog({
  kind,
  status,
  editable,
}: {
  kind: Exclude<Kind, 'policy'>;
  status: RunnerStatus;
  editable: boolean;
}) {
  const rows = useRunnerQuery<Items<CatalogRow>>(status, `/${kind}`);
  const presets = useRunnerQuery<Items<CatalogRow>>(
    status,
    '/mcp-servers?presets=true',
    kind === 'mcp-servers',
  );
  const [selected, setSelected] = useState<Selected>();
  const current = rows.data?.items.find((row) => row.id === selected?.id);
  const type = kind === 'providers' ? 'provider' : kind === 'profiles' ? 'profile' : 'MCP server';
  const secondary = kind === 'providers' ? 'type' : kind === 'profiles' ? 'model' : 'url';
  let detail: ReactNode;
  if (selected) {
    detail = (
      <div className="grid gap-3">
        <h3 className="font-semibold">{String(selected.initial.name ?? `Add ${type}`)}</h3>
        <CatalogForm
          kind={kind}
          status={status}
          editable={editable}
          selected={selected}
          key={`${selected.id ?? 'new'}:${String(selected.initial.name ?? '')}`}
          onSaved={(item) => setSelected(item.id ? { id: item.id, initial: item } : undefined)}
        />
        {kind === 'mcp-servers' && current && (
          <Discover key={current.id} row={current} status={status} editable={editable} />
        )}
      </div>
    );
  }
  return (
    <section className="grid gap-3">
      <h2 className="font-semibold">{config[kind].title}</h2>
      {kind === 'profiles' && (
        <p className="text-sm text-fg-muted">
          Choose a provider and model, then share the profile with the projects that will use it.
        </p>
      )}
      {rows.error && (
        <p role="alert" className="text-danger">
          {runnerError(rows.error)}
        </p>
      )}
      <Rows
        label={config[kind].title}
        columns={[
          'Name',
          secondary === 'url' ? 'URL' : secondary === 'type' ? 'Type' : 'Model',
          'ID',
        ]}
        rows={(rows.data?.items ?? []).map((row) => ({
          id: row.id,
          cells: [
            <Button
              variant="link"
              size="sm"
              onClick={() => setSelected({ id: row.id, initial: row })}
            >
              {row.name}
            </Button>,
            String(row[secondary] ?? '—'),
            <code>{row.id}</code>,
          ],
        }))}
      />
      {editable && (
        <div className="flex flex-wrap gap-2">
          <Button
            className="w-fit"
            disabled={status.status !== 'online'}
            onClick={() => setSelected({ initial: {} })}
          >
            Add {type}
          </Button>
          {kind === 'mcp-servers' &&
            presets.data?.items.map((preset) => (
              <Button
                key={preset.name}
                variant="secondary"
                disabled={status.status !== 'online'}
                onClick={() => setSelected({ initial: preset })}
              >
                Use {preset.name} preset
              </Button>
            ))}
        </div>
      )}
      {kind === 'mcp-servers' && selected?.initial.credential != null && (
        <p className="text-sm text-fg-muted">
          For authenticated discovery, create an MCP bearer token for this URL and enter its
          credential ID.
        </p>
      )}
      {detail}
    </section>
  );
}

export function PolicyBudgets({ status, editable }: { status: RunnerStatus; editable: boolean }) {
  const policy = useRunnerQuery<Row>(status, '/policy');
  if (policy.error)
    return (
      <p role="alert" className="text-danger">
        {runnerError(policy.error)}
      </p>
    );
  if (!policy.data)
    return (
      <p className="text-fg-muted">
        {status.status === 'online' ? 'Loading policy…' : 'Policy is unavailable.'}
      </p>
    );
  return (
    <CatalogForm
      kind="policy"
      status={status}
      selected={{ initial: policy.data }}
      editable={editable}
    />
  );
}
