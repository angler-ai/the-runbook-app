import { useEffect, useRef } from 'react';
import { Button } from '../../ui/primitives/button';
import { ALL_EVENT_KINDS, EVENT_GROUPS } from './events';

export interface EventPickerProps {
  selected: ReadonlySet<string>;
  onChange(selected: Set<string>): void;
  disabled?: boolean;
}

/**
 * Every event in its hierarchy, pickable per event or per group, with
 * Select all / Deselect all. Nothing starts selected: the caller
 * initialises the selection (a new webhook passes an empty set).
 */
export function EventPicker({ selected, onChange, disabled }: EventPickerProps) {
  const groupRefs = useRef<(HTMLInputElement | null)[]>([]);
  useEffect(() => {
    EVENT_GROUPS.forEach((group, index) => {
      const input = groupRefs.current[index];
      if (!input) return;
      const chosen = group.events.filter((event) => selected.has(event.kind)).length;
      input.indeterminate = chosen > 0 && chosen < group.events.length;
    });
  }, [selected]);

  const toggle = (kinds: string[], on: boolean) => {
    const next = new Set(selected);
    for (const kind of kinds) {
      if (on) next.add(kind);
      else next.delete(kind);
    }
    onChange(next);
  };

  return (
    <div className="grid gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-xs text-fg-muted" role="status">
          {selected.size} of {ALL_EVENT_KINDS.length} events selected
        </span>
        <span className="ml-auto flex gap-1">
          <Button
            type="button"
            size="sm"
            variant="ghost"
            disabled={disabled}
            onClick={() => onChange(new Set(ALL_EVENT_KINDS))}
          >
            Select all
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            disabled={disabled}
            onClick={() => onChange(new Set())}
          >
            Deselect all
          </Button>
        </span>
      </div>
      <div className="grid gap-3 sm:grid-cols-2">
        {EVENT_GROUPS.map((group, groupIndex) => {
          const groupKinds = group.events.map((event) => event.kind);
          const allOn = groupKinds.every((kind) => selected.has(kind));
          return (
            <fieldset
              key={group.label}
              className="rounded-md border border-border p-3"
              data-group={group.label}
            >
              <legend className="sr-only">{group.label}</legend>
              <label className="flex items-center gap-2 text-xs font-semibold text-fg">
                <input
                  ref={(el) => {
                    groupRefs.current[groupIndex] = el;
                  }}
                  type="checkbox"
                  checked={allOn}
                  disabled={disabled}
                  onChange={(event) => toggle(groupKinds, event.target.checked)}
                  className="size-4 rounded border-border text-accent focus:ring-accent"
                />
                {group.label}
              </label>
              <div className="mt-2 grid gap-1.5 pl-6">
                {group.events.map((event) => (
                  <label key={event.kind} className="flex items-center gap-2 text-xs">
                    <input
                      type="checkbox"
                      checked={selected.has(event.kind)}
                      disabled={disabled}
                      onChange={(e) => toggle([event.kind], e.target.checked)}
                      className="size-4 rounded border-border text-accent focus:ring-accent"
                    />
                    <span>
                      {event.label}{' '}
                      <span className="font-mono text-2xs text-fg-muted">{event.kind}</span>
                    </span>
                  </label>
                ))}
              </div>
            </fieldset>
          );
        })}
      </div>
    </div>
  );
}
