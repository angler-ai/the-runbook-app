import { useState } from 'react';
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
  const [after, setAfter] = useState('0');
  const query = useRunnerQuery<{ chain?: { ok?: boolean }; items: AuditEntry[] }>(
    status,
    `/audit?after=${after}`,
  );
  const entries = query.data?.items ?? [];
  return (
    <div className="grid gap-3">
      {query.error && (
        <p role="alert" className="text-danger">
          {runnerError(query.error)}
        </p>
      )}
      {query.data?.chain?.ok != null && (
        <p className="text-sm">
          Audit chain {query.data.chain.ok ? 'verified' : 'failed verification'}
        </p>
      )}
      <Rows
        label="Runner audit"
        columns={['Time', 'Actor', 'Action', 'Resource']}
        rows={entries.map((entry) => ({
          id: entry.seq,
          cells: [
            new Date(entry.at).toLocaleString(),
            entry.actor?.id ?? entry.actor?.kind ?? '—',
            entry.action,
            `${entry.resource_type} ${entry.resource_id ?? ''}`,
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
