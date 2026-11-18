import { useId } from 'react';
import { Checkbox } from '../primitives/checkbox';
import { cn } from '../primitives/cn';

export interface ChecklistItem {
  title: string;
  completed: boolean;
  disabled?: boolean;
}

export interface ChecklistFormProps {
  items: ChecklistItem[];
  onToggle?: (index: number, completed: boolean) => void;
  disabled?: boolean;
  className?: string;
  ariaLabel?: string;
}

export function ChecklistForm({
  items,
  onToggle,
  disabled = false,
  className,
  ariaLabel = 'Checklist form',
}: ChecklistFormProps) {
  const baseId = useId();

  if (items.length === 0) {
    return (
      <div className="text-xs text-fg-muted italic p-3 text-center border border-dashed border-border rounded-md">
        No checklist items.
      </div>
    );
  }

  return (
    <div
      role="group"
      aria-label={ariaLabel}
      data-slot="checklist-form"
      className={cn('flex flex-col gap-1 w-full', className)}
    >
      {items.map((item, idx) => {
        const id = `${baseId}-check-${idx}`;
        const itemDisabled = disabled || item.disabled;

        return (
          <div
            key={idx}
            className={cn(
              'min-h-[44px] flex items-center gap-3 px-3 py-2 rounded-md transition-colors select-none',
              itemDisabled ? 'opacity-50 cursor-not-allowed' : 'hover:bg-surface-raised cursor-pointer',
            )}
          >
            <Checkbox
              id={id}
              checked={item.completed}
              disabled={itemDisabled}
              onCheckedChange={(checked) => {
                onToggle?.(idx, Boolean(checked));
              }}
            />
            <label
              htmlFor={id}
              className={cn(
                'text-xs text-fg flex-1 cursor-pointer select-none leading-tight py-2',
                item.completed && 'text-fg-muted line-through',
                itemDisabled && 'cursor-not-allowed',
              )}
            >
              {item.title}
            </label>
          </div>
        );
      })}
    </div>
  );
}
