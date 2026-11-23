import { Copy, Download } from 'lucide-react';
import { useState } from 'react';
import { Button } from '../../ui/primitives/button';

/** Backup codes, shown once: copy or download, then they are gone from the page. */
export function BackupCodes({ codes }: { codes: string[] }) {
  const [status, setStatus] = useState<string | null>(null);
  const text = `The Runbook backup codes\nEach code works once. Keep them somewhere safe.\n\n${codes.join('\n')}\n`;

  async function copy() {
    try {
      await navigator.clipboard.writeText(codes.join('\n'));
      setStatus('Backup codes copied.');
    } catch {
      setStatus('Copy failed. Select the codes and copy them yourself.');
    }
  }

  function download() {
    const url = URL.createObjectURL(new Blob([text], { type: 'text/plain' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = 'runbook-backup-codes.txt';
    document.body.append(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 0);
    setStatus('Backup codes downloaded.');
  }

  return (
    <section aria-labelledby="backup-codes-title" className="grid gap-3">
      <div className="grid gap-1">
        <h3 id="backup-codes-title" className="text-sm font-semibold text-fg">
          Save your backup codes
        </h3>
        <p className="text-sm text-fg-muted">
          If you lose your phone, each of these codes signs you in once. This is the only time they
          are shown.
        </p>
      </div>
      <ol
        aria-label="Backup codes"
        className="grid grid-cols-2 gap-x-6 gap-y-1 rounded-md border border-border bg-bg p-3 font-mono text-sm text-fg"
      >
        {codes.map((code) => (
          <li key={code}>{code}</li>
        ))}
      </ol>
      <div className="flex flex-wrap gap-2">
        <Button type="button" variant="secondary" size="sm" onClick={() => void copy()}>
          <Copy aria-hidden="true" />
          Copy codes
        </Button>
        <Button type="button" variant="secondary" size="sm" onClick={download}>
          <Download aria-hidden="true" />
          Download
        </Button>
      </div>
      <p role="status" className="text-xs text-fg-muted">
        {status ?? ''}
      </p>
    </section>
  );
}
