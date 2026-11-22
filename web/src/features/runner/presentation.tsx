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
}: {
  label: string;
  columns: string[];
  rows: { id: string; cells: ReactNode[] }[];
}) {
  if (!rows.length) return <p className="text-fg-muted">Nothing to show yet.</p>;
  return (
    <div className="overflow-x-auto rounded-lg border border-border bg-surface">
      <table className="w-full text-left text-sm" aria-label={label}>
        <thead className="bg-surface-raised">
          <tr>
            {columns.map((column) => (
              <th className="p-3 font-medium" scope="col" key={column}>
                {column}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map(({ id, cells }) => (
            <tr className="border-t border-border" key={id}>
              {cells.map((cell, index) => (
                <td key={columns[index]} className="p-3 align-top">
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
