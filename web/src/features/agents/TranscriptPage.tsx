import { useState } from 'react';
import { Link, useParams, useSearch } from '@tanstack/react-router';
import { useMe } from '../../api/hooks';
import { PageHeader } from '../../ui/layout';
import { Button } from '../../ui/primitives/button';
import { useOrgContext } from '../context/OrgContext';
import { runnerError, useRunnerQuery, useRunnerStatus, type Items } from '../runner/api';
import { panel, projectAllowed } from '../runner/presentation';

type Entry = { seq: number | string; claim_id?: string | null; entry: unknown };

function transcriptEntries(data: Items<Entry> | string | undefined): Entry[] {
  if (!data) return [];
  if (typeof data !== 'string') return data.items;
  return data.split(/\r?\n\r?\n/).flatMap((event) => {
    const payload = event
      .split(/\r?\n/)
      .filter((line) => line.startsWith('data:'))
      .map((line) => line.slice(5).trimStart())
      .join('\n');
    if (!payload) return [];
    try {
      const parsed = JSON.parse(payload) as Entry | Items<Entry>;
      return 'items' in parsed ? parsed.items : [parsed];
    } catch {
      return [];
    }
  });
}

export function TranscriptPage() {
  const { project, step } = useParams({ strict: false }) as { project: string; step: string };
  const { claim_id } = useSearch({ strict: false }) as { claim_id?: string };
  const { resolveProject } = useOrgContext();
  const projectId = resolveProject(project)?.id;
  const me = useMe();
  const status = useRunnerStatus(me.data?.org.id);
  const [cursor, setCursor] = useState('0');
  const allowed = projectAllowed(status.data, projectId);
  const query = new URLSearchParams({ after: cursor, ...(claim_id && { claim_id }) });
  const transcript = useRunnerQuery<Items<Entry> | string>(
    status.data,
    `/transcripts/${encodeURIComponent(step)}?${query}`,
    allowed,
  );
  const entries = transcriptEntries(transcript.data);
  const archiveDate = typeof transcript.data === 'object' ? transcript.data.archived_at : undefined;
  return (
    <div className="grid gap-3">
      <PageHeader title="Step transcript" description={step} />
      <Link to="/p/$project/agents" params={{ project }} className="w-fit text-accent underline">
        All agents
      </Link>
      {transcript.isLoading && <p role="status">Loading transcript…</p>}
      {status.data?.status === 'offline' && <p role="status">Runner is offline.</p>}
      {(status.error || transcript.error) && (
        <p role="alert" className="text-danger">
          {runnerError(status.error ?? transcript.error)}
        </p>
      )}
      {!allowed && status.data?.status === 'online' && (
        <p>This project is not allowed on the runner.</p>
      )}
      {transcript.isSuccess && !entries.length && (
        <p className="text-fg-muted">No transcript entries.</p>
      )}
      {archiveDate && <p className="text-sm text-fg-muted">Archived on {archiveDate}; read-only.</p>}
      {entries.length > 0 && (
        <ol className="grid gap-2" aria-label="Transcript entries">
          {entries.map(({ seq, claim_id: lease, entry }) => (
            <li key={seq} className={panel}>
              {!lease && <p className="text-fg-muted text-xs">Unattributed to a lease</p>}
              <pre className="whitespace-pre-wrap break-all text-sm">
                {typeof entry === 'string' ? entry : JSON.stringify(entry, null, 2)}
              </pre>
            </li>
          ))}
        </ol>
      )}
      <div className="flex gap-2">
        {cursor !== '0' && (
          <Button variant="secondary" onClick={() => setCursor('0')}>
            First page
          </Button>
        )}
        {entries.length === 1000 && (
          <Button variant="secondary" onClick={() => setCursor(String(entries.at(-1)!.seq))}>
            Next page
          </Button>
        )}
      </div>
    </div>
  );
}
