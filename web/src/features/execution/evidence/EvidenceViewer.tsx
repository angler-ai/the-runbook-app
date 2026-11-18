import { useMemo, useState } from 'react';
import { Download, ExternalLink, FileText } from 'lucide-react';
import type { Schemas } from '../../../api/client';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../../../ui/primitives/dialog';
import { useExecutionEvidence } from '../hooks';
import { formatBytes } from './upload';

export interface EvidenceListProps {
  evidence: string[];
  executionId?: string;
  items?: Schemas['EvidenceView'][];
}

function isHttps(value: string): boolean {
  try {
    return new URL(value).protocol === 'https:';
  } catch {
    return false;
  }
}

export function EvidenceList({ evidence, executionId, items }: EvidenceListProps) {
  const [selectedImage, setSelectedImage] = useState<Schemas['EvidenceView'] | null>(null);

  // If items are not directly passed, query the execution evidence
  const evidenceQuery = useExecutionEvidence(
    executionId,
    Boolean(executionId && (!items || items.length === 0)),
  );

  const evidenceMap = useMemo(() => {
    const list = items ?? evidenceQuery.data ?? [];
    const map = new Map<string, Schemas['EvidenceView']>();
    for (const item of list) {
      map.set(item.id, item);
    }
    return map;
  }, [items, evidenceQuery.data]);

  if (!evidence || evidence.length === 0) return null;

  return (
    <>
      <ul data-slot="evidence-list" className="grid gap-2 text-xs">
        {evidence.map((item) => {
          if (isHttps(item)) {
            return (
              <li key={item} className="min-w-0 truncate">
                <a
                  href={item}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex max-w-full items-center gap-1 text-accent underline-offset-2 hover:underline"
                >
                  <span className="truncate">{item}</span>
                  <ExternalLink className="size-3 shrink-0" aria-hidden="true" />
                </a>
              </li>
            );
          }

          const file = evidenceMap.get(item);
          if (!file) {
            return (
              <li key={item} className="min-w-0 truncate">
                <span className="font-mono text-fg-muted">
                  {item.startsWith('evd_') ? `Upload ${item}` : item}
                </span>
              </li>
            );
          }

          const isImage = Boolean(
            file.thumbnail_url || (file.type && file.type.startsWith('image/')),
          );

          if (isImage) {
            return (
              <li key={file.id} className="min-w-0" data-testid={`evidence-file-${file.id}`}>
                <div className="flex flex-col gap-1.5 sm:flex-row sm:items-center sm:gap-3 py-1">
                  <button
                    type="button"
                    onClick={() => setSelectedImage(file)}
                    className="group relative block size-16 shrink-0 overflow-hidden rounded border border-border bg-surface-raised hover:border-accent focus-visible:outline-2 focus-visible:outline-accent cursor-pointer"
                    aria-label={`View full image: ${file.name}`}
                  >
                    <img
                      src={file.thumbnail_url || file.url || ''}
                      alt={file.name}
                      className="size-full object-cover transition-transform duration-150 group-hover:scale-105"
                      loading="lazy"
                    />
                  </button>
                  <div className="flex min-w-0 flex-col gap-0.5 text-xs">
                    <button
                      type="button"
                      onClick={() => setSelectedImage(file)}
                      className="font-medium text-accent hover:underline text-left truncate cursor-pointer"
                    >
                      {file.name}
                    </button>
                    <span className="text-2xs text-fg-muted font-mono">
                      {formatBytes(file.size)}
                      {file.width && file.height ? ` · ${file.width}×${file.height}` : ''}
                    </span>
                    <a
                      href={file.url ?? '#'}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="inline-flex items-center gap-1 text-2xs text-fg-muted hover:text-fg underline-offset-2 hover:underline"
                    >
                      <span>Open full image</span>
                      <ExternalLink className="size-2.5" aria-hidden="true" />
                    </a>
                  </div>
                </div>
              </li>
            );
          }

          return (
            <li key={file.id} className="min-w-0" data-testid={`evidence-file-${file.id}`}>
              <div className="flex flex-wrap items-center gap-2 py-1">
                <FileText className="size-4 shrink-0 text-fg-muted" aria-hidden="true" />
                <a
                  href={file.url ?? '#'}
                  download={file.name}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="font-medium text-accent hover:underline truncate"
                >
                  {file.name}
                </a>
                <span className="text-2xs text-fg-muted font-mono">
                  ({formatBytes(file.size)} · {file.type})
                </span>
                <a
                  href={file.url ?? '#'}
                  download={file.name}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-1 text-2xs text-fg-muted hover:text-fg underline-offset-2 hover:underline ml-1"
                  aria-label={`Download ${file.name}`}
                >
                  <Download className="size-3" aria-hidden="true" />
                  <span>Download</span>
                </a>
              </div>
            </li>
          );
        })}
      </ul>

      {selectedImage && (
        <Dialog open={Boolean(selectedImage)} onOpenChange={(open) => !open && setSelectedImage(null)}>
          <DialogContent className="max-w-3xl">
            <DialogHeader>
              <DialogTitle className="truncate">{selectedImage.name}</DialogTitle>
              <DialogDescription>
                {formatBytes(selectedImage.size)} · {selectedImage.type}
                {selectedImage.width && selectedImage.height
                  ? ` · ${selectedImage.width}×${selectedImage.height}`
                  : ''}
              </DialogDescription>
            </DialogHeader>
            <div className="flex items-center justify-center overflow-hidden rounded-md bg-surface-raised p-2 max-h-[70vh]">
              <img
                src={selectedImage.url || selectedImage.thumbnail_url || ''}
                alt={selectedImage.name}
                className="max-h-[65vh] w-auto max-w-full object-contain"
              />
            </div>
            <DialogFooter>
              <a
                href={selectedImage.url ?? '#'}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-1 text-xs text-accent hover:underline"
              >
                <span>Open original in new tab</span>
                <ExternalLink className="size-3" aria-hidden="true" />
              </a>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}
    </>
  );
}
