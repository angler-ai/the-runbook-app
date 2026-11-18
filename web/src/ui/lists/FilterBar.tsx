import { useState, type KeyboardEvent } from 'react';
import type { components } from '../../api/schema';
import { cn } from '../primitives/cn';
import { Button } from '../primitives/button';
import { Input } from '../primitives/input';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '../primitives/select';
import { Search, X, Tag as TagIcon, RotateCcw } from 'lucide-react';

export type ExecutionStatus = components['schemas']['ExecutionStatus'];

export const ALL_STATUSES: ExecutionStatus[] = [
  'queued',
  'running',
  'waiting',
  'paused',
  'succeeded',
  'failed',
  'cancelled',
];

export interface FilterBarProps {
  /** Selected status filter items */
  statuses?: ExecutionStatus[];
  onStatusChange?: (statuses: ExecutionStatus[]) => void;
  /** Runbook slug or search query */
  runbook?: string;
  onRunbookChange?: (runbook: string) => void;
  /**
   * The project's runbooks. Given, the runbook filter is a select over
   * them (the server matches an exact slug or ID only, U20); without it, a
   * free-text field.
   */
  runbookOptions?: { value: string; label: string }[];
  /** Active tag filters */
  tags?: string[];
  onTagsChange?: (tags: string[]) => void;
  /** Reset all filters */
  onReset?: () => void;
  className?: string;
}

export function FilterBar({
  statuses = [],
  onStatusChange,
  runbook = '',
  onRunbookChange,
  runbookOptions,
  tags = [],
  onTagsChange,
  onReset,
  className,
}: FilterBarProps) {
  const [tagInput, setTagInput] = useState('');

  const toggleStatus = (status: ExecutionStatus) => {
    if (!onStatusChange) return;
    if (statuses.includes(status)) {
      onStatusChange(statuses.filter((s) => s !== status));
    } else {
      onStatusChange([...statuses, status]);
    }
  };

  const handleAddTag = () => {
    const trimmed = tagInput.trim().toLowerCase();
    if (trimmed && !tags.includes(trimmed)) {
      onTagsChange?.([...tags, trimmed]);
      setTagInput('');
    }
  };

  const handleTagInputKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      handleAddTag();
    }
  };

  const removeTag = (tagToRemove: string) => {
    onTagsChange?.(tags.filter((t) => t !== tagToRemove));
  };

  const hasActiveFilters = statuses.length > 0 || runbook.trim().length > 0 || tags.length > 0;

  return (
    <div
      data-slot="filter-bar"
      className={cn(
        'p-3 bg-surface border border-border rounded-lg flex flex-col gap-3 text-xs',
        className,
      )}
    >
      {/* Top row: Runbook input and Tag input */}
      <div className="flex flex-wrap items-center gap-2.5">
        {/* Runbook filter: the shared Select over the catalog when given */}
        {runbookOptions ? (
          <div className="relative flex-1 min-w-[200px]">
            <Select
              value={runbook === '' ? 'all' : runbook}
              onValueChange={(value) => onRunbookChange?.(value === 'all' ? '' : value)}
            >
              <SelectTrigger aria-label="Filter by runbook">
                <SelectValue placeholder="All runbooks" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All runbooks</SelectItem>
                {runbook && !runbookOptions.some((option) => option.value === runbook) ? (
                  <SelectItem value={runbook}>{runbook}</SelectItem>
                ) : null}
                {runbookOptions.map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        ) : onRunbookChange ? (
          <div className="relative flex-1 min-w-[200px]">
            <Search
              className="absolute left-2.5 top-1/2 -translate-y-1/2 size-3.5 text-fg-muted pointer-events-none"
              aria-hidden="true"
            />
            <Input
              type="search"
              aria-label="Filter by runbook"
              placeholder="Filter by runbook slug or name..."
              value={runbook}
              onChange={(e) => onRunbookChange?.(e.target.value)}
              className="pl-8 text-xs h-8"
            />
            {runbook && (
              <button
                type="button"
                aria-label="Clear runbook filter"
                onClick={() => onRunbookChange?.('')}
                className="absolute right-1 top-1/2 -translate-y-1/2 size-6 min-w-6 min-h-6 flex items-center justify-center text-fg-muted hover:text-fg rounded focus-visible:outline-2 focus-visible:outline-accent"
              >
                <X className="size-3" aria-hidden="true" />
              </button>
            )}
          </div>
        ) : null}

        {/* Tag filter input */}
        <div className="relative flex-1 min-w-[180px]">
          <TagIcon
            className="absolute left-2.5 top-1/2 -translate-y-1/2 size-3.5 text-fg-muted pointer-events-none"
            aria-hidden="true"
          />
          <Input
            type="text"
            aria-label="Filter by tag"
            placeholder="Add tag filter (press Enter)..."
            value={tagInput}
            onChange={(e) => setTagInput(e.target.value)}
            onKeyDown={handleTagInputKeyDown}
            className="pl-8 text-xs h-8"
          />
        </div>

        {/* Reset button */}
        {hasActiveFilters && onReset && (
          <Button
            variant="ghost"
            size="sm"
            onClick={onReset}
            className="h-8 px-2 text-fg-muted hover:text-fg text-2xs gap-1"
          >
            <RotateCcw className="size-3" aria-hidden="true" />
            Reset
          </Button>
        )}
      </div>

      {/* Middle row: Status multi-select chips */}
      <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Status filters">
        <span className="text-2xs font-semibold text-fg-muted uppercase tracking-wider mr-1 select-none">
          Status:
        </span>
        {ALL_STATUSES.map((status) => {
          const isSelected = statuses.includes(status);
          return (
            <button
              key={status}
              type="button"
              role="checkbox"
              aria-checked={isSelected}
              onClick={() => toggleStatus(status)}
              className={cn(
                'inline-flex min-h-8 items-center px-2.5 py-1.5 rounded-md text-xs font-medium transition-colors cursor-pointer border select-none',
                isSelected
                  ? 'bg-accent text-accent-fg border-accent font-semibold shadow-xs'
                  : 'bg-surface-raised text-fg-muted border-border hover:text-fg hover:border-border-focus',
              )}
            >
              {status}
            </button>
          );
        })}
      </div>

      {/* Bottom row: Active tag pills */}
      {tags.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5 pt-1 border-t border-border">
          <span className="text-2xs font-semibold text-fg-muted uppercase tracking-wider mr-1">
            Active Tags:
          </span>
          {tags.map((tag) => (
            <span
              key={tag}
              data-slot="filter-tag-pill"
              className="inline-flex items-center gap-1 px-2 py-0.5 rounded bg-surface-raised border border-border text-fg text-2xs"
            >
              <span>#{tag}</span>
              <button
                type="button"
                aria-label={`Remove tag ${tag}`}
                onClick={() => removeTag(tag)}
                className="text-fg-muted hover:text-fg p-0.5 rounded focus-visible:outline-none"
              >
                <X className="size-3" aria-hidden="true" />
              </button>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}
