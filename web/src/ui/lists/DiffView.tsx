import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { computeUnifiedDiff } from './diff';
import { Plus, Minus, RefreshCw, FileDiff } from 'lucide-react';

export type VersionDiffView = components['schemas']['VersionDiffView'];

export interface DiffViewProps {
  /** Precomputed VersionDiffView from the API */
  diff?: VersionDiffView | null;
  /** Alternatively, raw before & after text to diff via internal LCS */
  before?: string;
  after?: string;
  className?: string;
}

export function DiffView({ diff, before, after, className }: DiffViewProps) {
  // If unified_diff is provided in diff, use it. Otherwise, compute via LCS if before/after are given.
  const unifiedDiff =
    diff?.unified_diff !== undefined
      ? diff.unified_diff
      : before !== undefined && after !== undefined
        ? computeUnifiedDiff(before, after)
        : '';

  const addedSteps = diff?.added_steps ?? [];
  const removedSteps = diff?.removed_steps ?? [];
  const changedSteps = diff?.changed_steps ?? [];

  const hasStepChanges =
    addedSteps.length > 0 || removedSteps.length > 0 || changedSteps.length > 0;
  const hasDiffText = unifiedDiff.trim().length > 0;

  if (!hasStepChanges && !hasDiffText) {
    return (
      <div
        data-slot="diff-view-empty"
        className={cn(
          'p-8 text-center border border-dashed border-border rounded-lg bg-surface/50 text-fg-muted text-sm w-full max-w-full min-w-0',
          className,
        )}
      >
        <FileDiff className="size-6 mx-auto mb-2 text-fg-muted" aria-hidden="true" />
        <p className="font-medium text-fg">No changes detected</p>
        <p className="text-xs text-fg-muted mt-0.5">The selected versions are identical.</p>
      </div>
    );
  }

  // Split unified diff into lines
  const diffLines = unifiedDiff.split('\n');

  return (
    <div
      data-slot="diff-view"
      className={cn('flex flex-col gap-4 w-full max-w-full min-w-0', className)}
    >
      {/* Step Changes Summary */}
      {hasStepChanges && (
        <div
          data-slot="step-changes"
          className="p-3 bg-surface border border-border rounded-lg flex flex-col gap-2.5 text-xs w-full max-w-full min-w-0"
        >
          <span className="font-semibold text-2xs uppercase tracking-wider text-fg-muted">
            Step Changes
          </span>
          <div className="flex flex-wrap gap-2">
            {addedSteps.map((stepId) => (
              <span
                key={`add-${stepId}`}
                data-slot="step-added"
                className="inline-flex items-center gap-1 px-2 py-0.5 rounded border border-verdict-pass-border bg-verdict-pass-bg text-verdict-pass-fg font-mono text-2xs"
              >
                <Plus className="size-3 shrink-0" aria-hidden="true" />
                {stepId}
              </span>
            ))}
            {removedSteps.map((stepId) => (
              <span
                key={`rem-${stepId}`}
                data-slot="step-removed"
                className="inline-flex items-center gap-1 px-2 py-0.5 rounded border border-verdict-fail-border bg-verdict-fail-bg text-verdict-fail-fg font-mono text-2xs"
              >
                <Minus className="size-3 shrink-0" aria-hidden="true" />
                {stepId}
              </span>
            ))}
            {changedSteps.map((stepId) => (
              <span
                key={`chg-${stepId}`}
                data-slot="step-changed"
                className="inline-flex items-center gap-1 px-2 py-0.5 rounded border border-verdict-warn-border bg-verdict-warn-bg text-verdict-warn-fg font-mono text-2xs"
              >
                <RefreshCw className="size-3 shrink-0" aria-hidden="true" />
                {stepId}
              </span>
            ))}
          </div>
        </div>
      )}

      {/* Unified Text Diff Rendering */}
      {hasDiffText && (
        <div
          data-slot="unified-diff-container"
          className="border border-border rounded-lg overflow-hidden bg-surface font-mono text-xs w-full max-w-full min-w-0"
        >
          <div className="px-3 py-2 border-b border-border bg-surface-raised flex items-center justify-between text-2xs text-fg-muted font-sans font-medium">
            <span>Unified Diff</span>
            <span>{diffLines.length} lines</span>
          </div>

          <pre
            data-slot="diff-pre"
            className="p-3 overflow-x-auto max-w-full text-xs leading-relaxed font-mono select-text"
          >
            {diffLines.map((line, idx) => {
              const isHunkHeader = line.startsWith('@@');
              const isAdd = line.startsWith('+') && !line.startsWith('+++');
              const isDel = line.startsWith('-') && !line.startsWith('---');
              const isFileHeader = line.startsWith('---') || line.startsWith('+++');

              return (
                <div
                  key={idx}
                  data-slot="diff-line"
                  data-diff-type={isAdd ? 'add' : isDel ? 'del' : isHunkHeader ? 'hunk' : 'context'}
                  className={cn(
                    'px-2 py-0.5 rounded-sm whitespace-pre',
                    isAdd && 'bg-verdict-pass-bg text-verdict-pass-fg font-medium',
                    isDel && 'bg-verdict-fail-bg text-verdict-fail-fg font-medium',
                    isHunkHeader && 'text-accent font-semibold bg-accent/5 my-1',
                    isFileHeader && 'text-fg-muted font-bold',
                    !isAdd && !isDel && !isHunkHeader && !isFileHeader && 'text-fg',
                  )}
                >
                  {/* Strict plain text rendering per A10 rule */}
                  {line || ' '}
                </div>
              );
            })}
          </pre>
        </div>
      )}
    </div>
  );
}
