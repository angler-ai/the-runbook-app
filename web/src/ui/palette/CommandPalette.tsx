import type { ComponentType } from 'react';
import * as DialogPrimitive from '@radix-ui/react-dialog';
import { Command } from 'cmdk';
import { cn } from '../primitives/cn';
import { CornerDownLeft, Search } from 'lucide-react';

export interface CommandPaletteItem {
  id: string;
  label: string;
  group: string;
  hint?: string;
  keywords?: string[];
  icon?: ComponentType<{ className?: string; 'aria-hidden'?: boolean | 'true' | 'false' }>;
  onSelect: () => void;
}

export interface CommandPaletteProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  items: CommandPaletteItem[];
  recentItems?: CommandPaletteItem[];
  placeholder?: string;
  className?: string;
}

// Shelf's group labels: small mono capitals, letter-spaced.
const groupHeadingClasses =
  '[&_[cmdk-group-heading]]:px-2.5 [&_[cmdk-group-heading]]:pt-2.5 [&_[cmdk-group-heading]]:pb-1 [&_[cmdk-group-heading]]:font-mono [&_[cmdk-group-heading]]:text-[10px] [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:tracking-[0.12em] [&_[cmdk-group-heading]]:text-fg-muted [&_[cmdk-group-heading]]:uppercase';

export function CommandPalette({
  open,
  onOpenChange,
  items,
  recentItems,
  placeholder = 'Type a command or search...',
  className,
}: CommandPaletteProps) {
  // If recentItems is not explicitly provided, extract any items whose group is "Recent"
  const recents = recentItems ?? items.filter((item) => item.group.toLowerCase() === 'recent');

  // Collect other groups in order of appearance (excluding 'Recent' if already shown in recents)
  const otherGroupsMap = new Map<string, CommandPaletteItem[]>();
  for (const item of items) {
    if (recentItems ? true : item.group.toLowerCase() !== 'recent') {
      const existing = otherGroupsMap.get(item.group) ?? [];
      existing.push(item);
      otherGroupsMap.set(item.group, existing);
    }
  }

  const handleSelectItem = (item: CommandPaletteItem) => {
    item.onSelect();
    onOpenChange(false);
  };

  return (
    <DialogPrimitive.Root open={open} onOpenChange={onOpenChange}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay
          data-slot="command-palette-overlay"
          className="fixed inset-0 z-50 bg-overlay animate-in fade-in-0 duration-150"
        />
        <DialogPrimitive.Content
          data-slot="command-palette-content"
          aria-describedby={undefined}
          className={cn(
            // Shelf: anchored high (12vh), 540px, a 12px radius and a soft shadow.
            'fixed left-1/2 top-[12vh] z-50 w-[calc(100%-2rem)] max-w-[540px] -translate-x-1/2 overflow-hidden rounded-xl border border-border bg-surface text-fg shadow-overlay focus-visible:outline-none',
            className,
          )}
        >
          <DialogPrimitive.Title className="sr-only">
            Command Palette
          </DialogPrimitive.Title>

          <Command
            label="Command Palette"
            className="flex flex-col w-full h-full text-xs"
          >
            {/* Search Header */}
            <div className="flex items-center gap-2.5 border-b border-border px-4">
              <Search className="size-4 shrink-0 text-fg-muted" aria-hidden="true" />
              <Command.Input
                placeholder={placeholder}
                className="h-12 w-full bg-transparent text-base text-fg outline-none placeholder:text-fg-muted disabled:cursor-not-allowed disabled:opacity-50"
              />
              <kbd className="shrink-0 rounded border border-border px-1.5 py-0.5 font-mono text-2xs leading-none text-fg-muted">
                esc
              </kbd>
            </div>

            {/* Suggestions List */}
            <Command.List className="flex max-h-[min(420px,52vh)] flex-col overflow-x-hidden overflow-y-auto p-1.5">
              <Command.Empty className="py-8 text-center text-xs text-fg-muted">
                No matching commands found.
              </Command.Empty>

              {/* Recent Group */}
              {recents.length > 0 && (
                <Command.Group
                  heading="Recent"
                  className={groupHeadingClasses}
                >
                  {recents.map((item) => (
                    <CommandItemRow
                      key={`recent-${item.id}`}
                      item={item}
                      onSelect={() => handleSelectItem(item)}
                    />
                  ))}
                </Command.Group>
              )}

              {/* Other Groups */}
              {Array.from(otherGroupsMap.entries()).map(([groupName, groupItems]) => (
                <Command.Group
                  key={groupName}
                  heading={groupName}
                  className={groupHeadingClasses}
                >
                  {groupItems.map((item) => (
                    <CommandItemRow
                      key={item.id}
                      item={item}
                      onSelect={() => handleSelectItem(item)}
                    />
                  ))}
                </Command.Group>
              ))}
            </Command.List>

            {/* Keyboard Hint Footer */}
            <div
              data-slot="command-palette-footer"
              className="flex items-center justify-between border-t border-border bg-surface-raised px-4 py-2 text-2xs text-fg-muted select-none"
            >
              <div className="flex items-center gap-3">
                <span className="flex items-center gap-1">
                  <kbd className="inline-flex items-center justify-center rounded border border-border bg-surface px-1.5 py-0.5 font-mono text-2xs text-fg-muted">
                    ↵
                  </kbd>
                  <span>to select</span>
                </span>
                <span className="flex items-center gap-1">
                  <kbd className="inline-flex items-center justify-center rounded border border-border bg-surface px-1.5 py-0.5 font-mono text-2xs text-fg-muted">
                    ↑↓
                  </kbd>
                  <span>to navigate</span>
                </span>
              </div>

              <div className="flex items-center gap-1">
                <kbd className="inline-flex items-center justify-center rounded border border-border bg-surface px-1.5 py-0.5 font-mono text-2xs text-fg-muted">
                  esc
                </kbd>
                <span>to close</span>
              </div>
            </div>
          </Command>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

function CommandItemRow({
  item,
  onSelect,
}: {
  item: CommandPaletteItem;
  onSelect: () => void;
}) {
  const Icon = item.icon;
  const searchableValue = `${item.label} ${item.hint ?? ''} ${item.group} ${item.keywords?.join(' ') ?? ''}`;

  return (
    <Command.Item
      value={searchableValue}
      onSelect={onSelect}
      className="group flex cursor-pointer items-center gap-2.5 rounded-md px-2.5 py-2 text-sm text-fg select-none data-[selected=true]:bg-accent-subtle"
    >
      {Icon && <Icon className="size-4 shrink-0 text-fg-muted group-data-[selected=true]:text-accent" aria-hidden="true" />}
      <span className="min-w-0 flex-1 truncate">{item.label}</span>

      {item.hint && (
        <span
          data-slot="command-palette-item-hint"
          className="ml-2 max-w-[40%] shrink-0 truncate font-mono text-2xs text-fg-muted"
        >
          {item.hint}
        </span>
      )}
      <CornerDownLeft
        className="invisible size-3.5 shrink-0 text-accent group-data-[selected=true]:visible"
        aria-hidden="true"
      />
    </Command.Item>
  );
}
