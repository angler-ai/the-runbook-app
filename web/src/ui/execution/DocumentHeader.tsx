import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { AmendedTag } from '../status/AmendedTag';
import { MonoRef } from '../status/MonoRef';
import { StatusPill } from '../status/StatusPill';

export type ExecutionStatus = components['schemas']['ExecutionStatus'];
export type ExecutionView = components['schemas']['ExecutionView'];

export interface DocumentHeaderProps {
  title?: string;
  status?: ExecutionStatus;
  result?: unknown;
  runbookSlug?: string | null;
  runbookVersion?: number | string | null;
  inputs?: Record<string, unknown> | null;
  fields?: Record<string, unknown> | null;
  amended?: boolean;
  id?: string | null;
  /** Pass the full ExecutionView from execution-document / header */
  header?: ExecutionView;
  className?: string;
}

export function DocumentHeader({
  title: propTitle,
  status: propStatus,
  result: propResult,
  runbookSlug: propRunbookSlug,
  runbookVersion: propRunbookVersion,
  inputs: propInputs,
  fields: _propFields,
  amended: propAmended,
  id: propId,
  header,
  className,
}: DocumentHeaderProps) {
  const title = propTitle ?? header?.title ?? 'Execution';
  const status = propStatus ?? header?.status ?? 'queued';
  const result = propResult !== undefined ? propResult : header?.result;
  const runbookSlug = propRunbookSlug ?? header?.runbook_slug ?? 'runbook';
  const runbookVersion = propRunbookVersion ?? header?.runbook_version;
  const inputs = propInputs ?? header?.inputs;
  const amended = propAmended ?? header?.amended;
  const id = propId ?? header?.id;

  const versionRef =
    runbookSlug && runbookVersion !== undefined && runbookVersion !== null
      ? `${runbookSlug}@${runbookVersion}`
      : runbookVersion !== undefined && runbookVersion !== null
        ? `@${runbookVersion}`
        : null;

  const inputEntries = inputs ? Object.entries(inputs) : [];

  return (
    <header
      data-slot="document-header"
      className={cn('flex flex-col gap-4 border-b border-border pb-6', className)}
    >
      {/* Eyebrow: runbook@version / execution id */}
      <div className="flex flex-wrap items-center gap-2 text-xs text-fg-muted">
        {versionRef && (
          <div className="flex items-center gap-1.5">
            <span className="font-sans">Runbook:</span>
            <MonoRef>{versionRef}</MonoRef>
          </div>
        )}
        {id && (
          <>
            <span className="text-border" aria-hidden="true">
              /
            </span>
            <MonoRef>{id}</MonoRef>
          </>
        )}
      </div>

      {/* Document title */}
      <h1 className="text-xl sm:text-2xl font-semibold text-fg tracking-tight truncate">
        {title}
      </h1>

      {/* Status, result and amended indicator */}
      <div className="flex flex-wrap items-center gap-3 text-xs">
        <StatusPill status={status} />

        {result !== null && result !== undefined && (
          <div className="flex items-center gap-1.5 text-fg-muted font-sans">
            <span>Result:</span>
            <MonoRef>{String(result)}</MonoRef>
          </div>
        )}

        {amended && <AmendedTag />}
      </div>

      {/* Inputs block */}
      {inputEntries.length > 0 && (
        <div
          data-slot="document-header-inputs"
          className="p-3 bg-surface-raised border border-border rounded-md text-xs font-mono text-fg-muted leading-relaxed overflow-x-auto"
        >
          <span className="font-sans font-medium text-fg mr-1.5">Inputs:</span>
          {inputEntries.map(([key, val], idx) => {
            const formatted = typeof val === 'object' && val !== null ? JSON.stringify(val) : String(val);
            return (
              <span key={key}>
                <span className="text-fg-muted">{key}: </span>
                <span className="text-fg">{formatted}</span>
                {idx < inputEntries.length - 1 && <span className="text-border mx-1.5">, </span>}
              </span>
            );
          })}
        </div>
      )}
    </header>
  );
}
