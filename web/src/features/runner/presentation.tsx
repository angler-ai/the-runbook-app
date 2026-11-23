import { useQuery } from '@tanstack/react-query';
import { fromBase64Url } from './encoding';
import type { ReactNode } from 'react';
import type { RunnerStatus } from './api';

export const panel = 'rounded-lg border border-border bg-surface p-4';
export const docs = 'https://docs.therunbook.app/runner';

export function projectAllowed(status: RunnerStatus | undefined, projectId?: string) {
  return Boolean(
    projectId &&
    status?.install_id &&
    status.groups.some((group) => group.projects.includes(projectId)),
  );
}

export function VaultIdentityFingerprint({ publicKey }: { publicKey?: string | null }) {
  const fingerprint = useQuery({
    queryKey: ['vault-identity-fingerprint', publicKey],
    enabled: Boolean(publicKey),
    retry: false,
    queryFn: async () => {
      const bytes = fromBase64Url(publicKey!);
      if (bytes.length !== 32) throw new Error('Invalid vault identity');
      const digest = await crypto.subtle.digest('SHA-256', bytes);
      return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join(
        '',
      );
    },
  });
  return (
    <p className="mt-2 break-all text-sm text-fg-muted">
      Vault identity SHA-256{' '}
      <span className="font-mono">
        {publicKey
          ? (fingerprint.data ?? (fingerprint.isError ? 'Unavailable' : '…'))
          : 'Not enrolled'}
      </span>
    </p>
  );
}

export function SetupCard({ admin }: { admin: boolean }) {
  if (!admin) return null;
  return (
    <section className={panel}>
      <h2 className="text-lg font-semibold">Set up a runner</h2>
      <p className="mt-2 text-fg-muted">
        Enable the runner on your Runbook install. When it registers, return here to add a model and
        allow a project.
      </p>
      <a className="mt-2 inline-block text-accent underline" href={`${docs}/setup/`}>
        Install a runner
      </a>
    </section>
  );
}

export function OfflineBanner({ offline, children }: { offline: boolean; children: ReactNode }) {
  return (
    <>
      {offline && (
        <p role="status" className="mb-4 rounded-md border border-border bg-surface p-3 text-warn">
          Runner is offline. Edits are paused.
        </p>
      )}
      <fieldset className="min-w-0" disabled={offline}>
        {children}
      </fieldset>
    </>
  );
}

export function Rows({
  label,
  columns,
  rows,
  stackOnMobile = false,
}: {
  label: string;
  columns: string[];
  rows: { id: string; cells: ReactNode[] }[];
  stackOnMobile?: boolean;
}) {
  if (!rows.length) return <p className="text-fg-muted">Nothing to show yet.</p>;
  return (
    <div className="overflow-x-auto rounded-lg border border-border bg-surface">
      <table
        role="table"
        className={`w-full text-left text-sm ${stackOnMobile ? 'block sm:table' : ''}`}
        aria-label={label}
      >
        <thead className={`bg-surface-raised ${stackOnMobile ? 'sr-only sm:not-sr-only' : ''}`}>
          <tr role="row">
            {columns.map((column) => (
              <th role="columnheader" className="p-3 font-medium" scope="col" key={column}>
                {column}
              </th>
            ))}
          </tr>
        </thead>
        <tbody className={stackOnMobile ? 'block sm:table-row-group' : undefined}>
          {rows.map(({ id, cells }) => (
            <tr
              role="row"
              className={`border-t border-border ${stackOnMobile ? 'grid grid-cols-2 gap-x-3 gap-y-3 p-3 first:border-t-0 sm:table-row sm:first:border-t' : ''}`}
              key={id}
            >
              {cells.map((cell, index) => (
                <td
                  key={columns[index]}
                  className={
                    stackOnMobile
                      ? `min-w-0 align-top sm:p-3 ${index === 0 ? 'col-span-2 border-b border-border pb-3 sm:border-b-0' : ''}`
                      : 'p-3 align-top'
                  }
                >
                  {stackOnMobile && index > 0 && (
                    <span
                      aria-hidden
                      className="mb-1 block text-xs font-medium text-fg-muted sm:hidden"
                    >
                      {columns[index]}
                    </span>
                  )}
                  {cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="grid gap-1 text-sm font-medium">
      {label}
      {children}
    </label>
  );
}

export function list(value: string) {
  return value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
}

export function sharing(value: string) {
  return { projects: !value || value === 'none' ? 'none' : value === 'all' ? 'all' : list(value) };
}

export function sentenceCase(value: string) {
  const words = value.replaceAll('_', ' ');
  return words.charAt(0).toUpperCase() + words.slice(1);
}

export function resourceName(resource: string) {
  return resource === 'mcp-servers' ? 'MCP server' : sentenceCase(resource.replaceAll('-', ' '));
}
