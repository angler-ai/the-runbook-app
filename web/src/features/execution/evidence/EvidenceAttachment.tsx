import { useEffect, useRef, useState, type DragEvent, type ChangeEvent } from 'react';
import { Download, ExternalLink, FileText, RefreshCw, Trash2, UploadCloud } from 'lucide-react';
import { ApiError, type Schemas } from '../../../api/client';
import { Button } from '../../../ui/primitives/button';
import { cn } from '../../../ui/primitives/cn';
import { friendlyError } from '../gating';
import {
  formatBytes,
  uploadEvidenceFile,
  validateEvidenceFile,
} from './upload';

export type UploadStatus =
  | 'validating'
  | 'requesting_slot'
  | 'uploading'
  | 'finalizing'
  | 'ready'
  | 'error';

export interface EvidenceUploadItem {
  id: string;
  file: File;
  mime: string;
  status: UploadStatus;
  progress: number;
  loaded?: number;
  total?: number;
  error?: string;
  evidence?: Schemas['EvidenceView'];
}

export interface EvidenceAttachmentProps {
  stepRunId: string;
  uploadsEnabled: boolean;
  limits?: Schemas['UploadLimits'] | null;
  maxRequestBytes?: number | null;
  items: EvidenceUploadItem[];
  onItemsChange: (items: EvidenceUploadItem[]) => void;
  disabled?: boolean;
}

export function EvidenceAttachment({
  stepRunId,
  uploadsEnabled,
  limits,
  maxRequestBytes,
  items,
  onItemsChange,
  disabled = false,
}: EvidenceAttachmentProps) {
  const [isDragOver, setIsDragOver] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const itemsRef = useRef(items);
  const controllersRef = useRef<Map<string, AbortController>>(new Map());

  useEffect(() => {
    itemsRef.current = items;
  }, [items]);

  useEffect(() => {
    const controllers = controllersRef.current;
    return () => {
      for (const controller of controllers.values()) {
        controller.abort();
      }
      controllers.clear();
    };
  }, []);

  if (!uploadsEnabled) {
    return (
      <div className="grid gap-1 pt-1" data-testid="uploads-disabled">
        <span className="text-xs font-semibold text-fg-muted uppercase tracking-wider">
          Evidence
        </span>
        <p className="text-xs text-fg-muted" data-testid="uploads-disabled-note">
          Evidence uploads are disabled because no user-content origin is configured.
        </p>
      </div>
    );
  }

  const applyUpdate = (updater: (current: EvidenceUploadItem[]) => EvidenceUploadItem[]) => {
    const next = updater(itemsRef.current);
    itemsRef.current = next;
    onItemsChange(next);
  };

  const removeItem = (id: string) => {
    const controller = controllersRef.current.get(id);
    if (controller) {
      controller.abort();
      controllersRef.current.delete(id);
    }
    applyUpdate((current) => current.filter((it) => it.id !== id));
  };

  const startUpload = async (item: EvidenceUploadItem) => {
    const controller = new AbortController();
    controllersRef.current.set(item.id, controller);

    try {
      applyUpdate((current) =>
        current.map((it) =>
          it.id === item.id
            ? {
                ...it,
                status: 'requesting_slot',
                progress: 0,
                loaded: undefined,
                total: undefined,
                error: undefined,
              }
            : it,
        ),
      );

      const evidence = await uploadEvidenceFile({
        stepRunId,
        file: item.file,
        mime: item.mime,
        signal: controller.signal,
        onProgress: (loaded, total, pct) => {
          applyUpdate((current) =>
            current.map((it) =>
              it.id === item.id
                ? {
                    ...it,
                    status: pct < 100 ? 'uploading' : 'finalizing',
                    progress: pct,
                    loaded,
                    total,
                  }
                : it,
            ),
          );
        },
      });

      applyUpdate((current) =>
        current.map((it) =>
          it.id === item.id
            ? {
                ...it,
                status: 'ready',
                progress: 100,
                evidence,
              }
            : it,
        ),
      );
    } catch (err) {
      if (controller.signal.aborted || (err instanceof DOMException && err.name === 'AbortError')) {
        return;
      }
      let message = 'Evidence upload failed';
      if (err instanceof ApiError) {
        message = friendlyError(err.code);
      } else if (err instanceof Error) {
        message = err.message;
      }
      applyUpdate((current) =>
        current.map((it) =>
          it.id === item.id
            ? {
                ...it,
                status: 'error',
                error: message,
              }
            : it,
        ),
      );
    } finally {
      controllersRef.current.delete(item.id);
    }
  };

  const handleFiles = (incomingFiles: FileList | File[]) => {
    if (disabled) return;
    const newItems: EvidenceUploadItem[] = [];

    for (let i = 0; i < incomingFiles.length; i++) {
      const file = incomingFiles[i];
      if (!file) continue;
      const localId = `upload-${file.name}-${file.size}-${file.lastModified}-${Math.random().toString(36).slice(2, 7)}`;
      const validation = validateEvidenceFile(file, limits, maxRequestBytes);

      if (!validation.ok) {
        newItems.push({
          id: localId,
          file,
          mime: file.type || 'application/octet-stream',
          status: 'error',
          progress: 0,
          error: validation.error,
        });
      } else {
        const item: EvidenceUploadItem = {
          id: localId,
          file,
          mime: validation.mime,
          status: 'requesting_slot',
          progress: 0,
        };
        newItems.push(item);
      }
    }

    if (newItems.length > 0) {
      applyUpdate((current) => [...current, ...newItems]);
      for (const item of newItems) {
        if (item.status === 'requesting_slot') {
          void startUpload(item);
        }
      }
    }
  };

  const onDragOverHandler = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    if (!disabled) setIsDragOver(true);
  };

  const onDragLeaveHandler = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setIsDragOver(false);
  };

  const onDropHandler = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setIsDragOver(false);
    if (!disabled && e.dataTransfer.files) {
      handleFiles(e.dataTransfer.files);
    }
  };

  const onFileInputChange = (e: ChangeEvent<HTMLInputElement>) => {
    if (e.target.files) {
      handleFiles(e.target.files);
      // Reset input value so re-selecting same file triggers change
      e.target.value = '';
    }
  };

  return (
    <div className="grid gap-2 pt-1" data-testid="evidence-attachment">
      <div className="flex items-center justify-between">
        <label
          htmlFor={`evidence-file-input-${stepRunId}`}
          className="text-xs font-semibold text-fg-muted uppercase tracking-wider"
        >
          Attach Evidence
        </label>
        <span className="text-2xs text-fg-muted">
          PNG, JPEG, WebP, GIF, and documents
        </span>
      </div>

      <input
        ref={fileInputRef}
        id={`evidence-file-input-${stepRunId}`}
        type="file"
        multiple
        disabled={disabled}
        onChange={onFileInputChange}
        className="sr-only"
        data-testid="evidence-file-input"
      />

      <div
        onDragOver={onDragOverHandler}
        onDragLeave={onDragLeaveHandler}
        onDrop={onDropHandler}
        onClick={() => !disabled && fileInputRef.current?.click()}
        className={cn(
          'flex flex-col items-center justify-center gap-1.5 rounded-md border-2 border-dashed p-4 text-center cursor-pointer transition-colors',
          isDragOver
            ? 'border-accent bg-accent/5 text-fg'
            : 'border-border hover:border-fg-muted/50 bg-surface-raised/40 text-fg-muted',
          disabled && 'opacity-50 pointer-events-none',
        )}
        data-testid="evidence-drop-zone"
        role="button"
        tabIndex={disabled ? -1 : 0}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            fileInputRef.current?.click();
          }
        }}
        aria-label="Upload evidence files by clicking or dragging files here"
      >
        <UploadCloud className="size-6 text-fg-muted" aria-hidden="true" />
        <p className="text-xs font-medium text-fg">
          <span className="text-accent underline underline-offset-2">Choose files</span> or drag
          them here
        </p>
        <p className="text-2xs text-fg-muted">
          {limits?.image_bytes
            ? `Images up to ${formatBytes(limits.image_bytes)} · Files up to ${formatBytes(limits.file_bytes ?? 52428800)}`
            : 'Evidence files attached to this step report'}
        </p>
      </div>

      {items.length > 0 && (
        <ul className="grid gap-2 pt-1" data-testid="evidence-items-list">
          {items.map((item) => {
            const isImage =
              item.mime.startsWith('image/') ||
              Boolean(item.evidence?.thumbnail_url);

            return (
              <li
                key={item.id}
                className="flex flex-col sm:flex-row sm:items-center justify-between gap-2 rounded border border-border bg-surface p-2 text-xs"
                data-testid={`evidence-item-${item.id}`}
              >
                <div className="flex items-center gap-2 min-w-0 flex-1">
                  {item.status === 'ready' && isImage && item.evidence ? (
                    <a
                      href={item.evidence.url ?? '#'}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="group relative block size-12 shrink-0 overflow-hidden rounded border border-border bg-surface-raised focus-visible:outline-2 focus-visible:outline-accent"
                      aria-label={`View full image: ${item.file.name}`}
                    >
                      <img
                        src={item.evidence.thumbnail_url || item.evidence.url || ''}
                        alt={item.file.name}
                        className="size-full object-cover transition-transform duration-150 group-hover:scale-105"
                      />
                    </a>
                  ) : (
                    <FileText className="size-6 shrink-0 text-fg-muted" aria-hidden="true" />
                  )}

                  <div className="flex min-w-0 flex-1 flex-col">
                    <div className="flex items-center gap-1.5 min-w-0">
                      <span className="truncate font-medium text-fg" title={item.file.name}>
                        {item.file.name}
                      </span>
                      <span className="shrink-0 text-2xs text-fg-muted">
                        ({formatBytes(item.file.size)})
                      </span>
                    </div>

                    {item.status === 'ready' && item.evidence && (
                      <div className="flex items-center gap-2 text-2xs text-fg-muted">
                        <span className="text-status-succeeded-fg font-medium">Ready</span>
                        <span>·</span>
                        <a
                          href={item.evidence.url ?? '#'}
                          target="_blank"
                          rel="noopener noreferrer"
                          className="inline-flex items-center gap-0.5 text-accent hover:underline"
                        >
                          <span>{isImage ? 'View image' : 'Download file'}</span>
                          {isImage ? (
                            <ExternalLink className="size-2.5" aria-hidden="true" />
                          ) : (
                            <Download className="size-2.5" aria-hidden="true" />
                          )}
                        </a>
                      </div>
                    )}

                    {(item.status === 'uploading' ||
                      item.status === 'requesting_slot' ||
                      item.status === 'finalizing') && (
                      <div className="flex flex-col gap-1 pt-1">
                        <div className="flex items-center justify-between text-2xs text-fg-muted">
                          <span>
                            {item.status === 'requesting_slot'
                              ? 'Requesting upload slot…'
                              : item.status === 'finalizing'
                                ? 'Finalizing upload…'
                                : item.loaded != null && item.total != null && item.total > 0
                                  ? `Uploading… ${formatBytes(item.loaded)} / ${formatBytes(item.total)} (${item.progress}%)`
                                  : `Uploading… ${item.progress}%`}
                          </span>
                          <span>{item.progress}%</span>
                        </div>
                        <div
                          className="h-1.5 w-full overflow-hidden rounded-full bg-border"
                          role="progressbar"
                          aria-valuenow={item.progress}
                          aria-valuemin={0}
                          aria-valuemax={100}
                          aria-label={`Upload progress for ${item.file.name}`}
                        >
                          <div
                            className="h-full bg-accent transition-all duration-150"
                            style={{ width: `${item.progress}%` }}
                          />
                        </div>
                      </div>
                    )}

                    {item.status === 'error' && (
                      <p
                        className="text-2xs text-status-failed-fg font-medium pt-0.5"
                        role="alert"
                      >
                        {item.error || 'Upload failed'}
                      </p>
                    )}
                  </div>
                </div>

                <div className="flex items-center gap-1.5 self-end sm:self-center shrink-0">
                  {item.status === 'error' && (
                    <Button
                      type="button"
                      variant="secondary"
                      size="sm"
                      onClick={() => void startUpload(item)}
                      disabled={disabled}
                      aria-label={`Retry upload for ${item.file.name}`}
                    >
                      <RefreshCw className="size-3" aria-hidden="true" />
                      <span>Retry</span>
                    </Button>
                  )}

                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => removeItem(item.id)}
                    disabled={disabled}
                    aria-label={`Remove ${item.file.name}`}
                  >
                    <Trash2 className="size-3.5 text-fg-muted hover:text-fg-danger" aria-hidden="true" />
                    <span className="sr-only">Remove</span>
                  </Button>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
