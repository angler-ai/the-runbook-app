import { useState } from 'react';
import { Input } from '../../ui/primitives/input';
import { StatusPill } from '../../ui/status/StatusPill';
import { Field, resourceName } from './presentation';
import { Button } from '../../ui/primitives/button';
import { runnerError, useRunnerQuery, type RunnerStatus } from './api';
import { Rows } from './presentation';

interface AuditEntry {
  seq: string;
  at: string;
  actor: { kind?: string; id?: string };
  action: string;
  resource_type: string;
  resource_id?: string;
}
export function Audit({ status }: { status: RunnerStatus }) {
  const [filter, setFilter] = useState('');
  const [after, setAfter] = useState('0');
  const query = useRunnerQuery<{ chain?: { ok?: boolean }; items: AuditEntry[] }>(
    status,
    `/audit?after=${after}`,
  );
  const entries = query.data?.items ?? [];
  const search = filter.trim().toLowerCase();
  const visible = entries.filter((entry) =>
    [
      eventName(entry.action),
      resourceName(entry.resource_type),
      entry.resource_type,
      entry.actor?.kind,
      entry.actor?.id,
    ].some((value) => value?.toLowerCase().includes(search)),
  );
  return (
    <div className="grid gap-3">
      {query.error && (
        <p role="alert" className="text-danger">
          {runnerError(query.error)}
        </p>
      )}
      <div className="grid justify-items-start gap-3 sm:flex sm:items-end sm:justify-between">
        <div className="w-full sm:max-w-xs">
          <Field label="Filter audit events">
            <Input
              type="search"
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
              placeholder="Event, actor or resource"
            />
          </Field>
        </div>
        {query.data?.chain?.ok != null && (
          <StatusPill
            status={query.data.chain.ok ? 'succeeded' : 'failed'}
            label={query.data.chain.ok ? 'Audit chain verified' : 'Audit chain failed verification'}
          />
        )}
      </div>
      {filter && !visible.length && (
        <p className="text-sm text-fg-muted">No events match this filter on this page.</p>
      )}
      <Rows
        label="Runner audit"
        stackOnMobile
        columns={['Time', 'Actor', 'Action', 'Resource']}
        rows={visible.map((entry) => ({
          id: entry.seq,
          cells: [
            new Date(entry.at).toLocaleString(),
            entry.actor?.id ?? entry.actor?.kind ?? '—',
            eventName(entry.action),
            resourceName(entry.resource_type),
          ],
        }))}
      />
      <div className="flex gap-2">
        {after !== '0' && (
          <Button variant="secondary" onClick={() => setAfter('0')}>
            First page
          </Button>
        )}
        {entries.length === 1000 && (
          <Button variant="secondary" onClick={() => setAfter(entries.at(-1)!.seq)}>
            Next page
          </Button>
        )}
      </div>
    </div>
  );
}

export function eventName(action: string) {
  const labels: Record<string, string> = {
    'catalog.discovered': 'Discovered connection tools',
    'catalog.pinned': 'Pinned connection tools',
    'catalog.created': 'Created catalog entry',
    'catalog.updated': 'Updated catalog entry',
    'catalog.deleted': 'Deleted catalog entry',
  };
  const words = action.replaceAll(/[._-]/g, ' ');
  return labels[action] ?? words.charAt(0).toUpperCase() + words.slice(1);
}
