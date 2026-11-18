// UI-4: the builder's menus. The palette (every kind with its icon and one
// line), the "+" on edges and at the end of each list, and each step's
// actions (insert above or below, move, duplicate, delete). All are Radix
// menus: keyboard reachable, arrow keys inside, Esc back to the trigger.
import { GripVertical, MoreHorizontal, Plus } from 'lucide-react';
import { useRef, type DragEvent, type ReactNode } from 'react';
import type { OutlineNode } from '../../ui/types';
import { cn } from '../../ui/primitives/cn';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from '../../ui/primitives/dropdown-menu';
import { useBuilder } from './context';
import { PALETTE, type PaletteKind } from './kinds';
import { moveProblem, moveTargets, type InsertTarget } from './ops';
import { within } from './model';

export const STEP_DRAG_TYPE = 'application/x-runbook-step';

/** The palette's items: one per kind, icon, name and what it does. */
export function PaletteItems({ onPick }: { onPick(kind: PaletteKind): void }) {
  return (
    <>
      {PALETTE.map((entry) => {
        const Icon = entry.icon;
        return (
          <DropdownMenuItem
            key={entry.kind}
            data-kind={entry.kind}
            className="items-start py-1.5"
            onSelect={() => onPick(entry.kind)}
          >
            <Icon className="mt-0.5 text-accent" aria-hidden="true" />
            <span className="grid min-w-0 gap-0.5">
              <span className="font-medium">{entry.label}</span>
              <span className="text-xs text-fg-muted">{entry.description}</span>
            </span>
          </DropdownMenuItem>
        );
      })}
    </>
  );
}

/** As tall as the room Radix finds on the side it opens (never cut off by the window). */
const PALETTE_WIDTH =
  'w-[340px] max-h-[min(560px,var(--radix-dropdown-menu-content-available-height))] overflow-y-auto';

/**
 * A menu whose action moves focus itself (to the new step's panel, the next
 * card): Radix must not put focus back on the menu's button as it closes.
 */
function useFocusHandoff() {
  const moved = useRef(false);
  return {
    take:
      <T extends unknown[]>(action: (...args: T) => void) =>
      (...args: T) => {
        moved.current = true;
        action(...args);
      },
    onCloseAutoFocus: (event: Event) => {
      if (moved.current) event.preventDefault();
      moved.current = false;
    },
  };
}

/** Drop a dragged step here (on an edge's "+" or at the end of a list). */
function useDropTarget(target: InsertTarget) {
  const api = useBuilder();
  const allowed = api.dragging !== null && moveProblem(api.model, api.dragging, target) === null;
  return {
    active: allowed,
    handlers: {
      onDragOver: (event: DragEvent) => {
        if (!allowed) return;
        event.preventDefault();
        event.dataTransfer.dropEffect = 'move';
      },
      onDrop: (event: DragEvent) => {
        if (!allowed || !api.dragging) return;
        event.preventDefault();
        api.move(api.dragging, target);
        api.setDragging(null);
      },
    },
  };
}

/** The "+" on an edge into a step. */
export function EdgeInsert({ target, label }: { target: InsertTarget; label: string }) {
  const api = useBuilder();
  const drop = useDropTarget(target);
  const focus = useFocusHandoff();
  return (
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label={label}
          title={label}
          data-slot="insert-button"
          {...drop.handlers}
          className={cn(
            'flex size-6 items-center justify-center rounded-full border border-border-strong bg-surface text-fg-muted shadow-card hover:border-accent hover:text-accent focus-visible:outline-2 focus-visible:outline-focus data-[state=open]:border-accent data-[state=open]:text-accent',
            drop.active && 'size-8 border-dashed border-accent bg-accent-subtle text-accent',
          )}
        >
          <Plus className="size-3.5" aria-hidden="true" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        className={PALETTE_WIDTH}
        collisionPadding={8}
        onCloseAutoFocus={focus.onCloseAutoFocus}
      >
        <DropdownMenuLabel>{label}</DropdownMenuLabel>
        <PaletteItems onPick={focus.take((kind: PaletteKind) => api.insert(target, kind))} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** The "Add step" pill at the end of a list. */
export function SlotInsert({
  target,
  label,
  description,
}: {
  target: InsertTarget;
  label: string;
  description: string;
}) {
  const api = useBuilder();
  const drop = useDropTarget(target);
  const focus = useFocusHandoff();
  return (
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label={description}
          data-slot="slot-button"
          {...drop.handlers}
          className={cn(
            'flex h-8 w-[168px] items-center justify-center gap-1.5 rounded-full border border-dashed border-border-strong bg-surface text-xs font-medium text-fg-muted hover:border-accent hover:text-accent focus-visible:outline-2 focus-visible:outline-focus data-[state=open]:border-accent data-[state=open]:text-accent',
            drop.active && 'border-accent bg-accent-subtle text-accent',
          )}
        >
          <Plus className="size-3.5" aria-hidden="true" />
          {label}
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        className={PALETTE_WIDTH}
        collisionPadding={8}
        onCloseAutoFocus={focus.onCloseAutoFocus}
      >
        <DropdownMenuLabel>{description}</DropdownMenuLabel>
        <PaletteItems onPick={focus.take((kind: PaletteKind) => api.insert(target, kind))} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** A toolbar button that opens the palette for `target`. */
export function PaletteButton({
  target,
  children,
  className,
}: {
  target: InsertTarget;
  children: ReactNode;
  className?: string;
}) {
  const api = useBuilder();
  const focus = useFocusHandoff();
  return (
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className={cn(
            'inline-flex h-8 items-center gap-1.5 rounded-md border border-border bg-surface px-2.5 text-sm font-medium text-fg hover:bg-hover focus-visible:outline-2 focus-visible:outline-focus',
            className,
          )}
        >
          {children}
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        className={PALETTE_WIDTH}
        align="start"
        collisionPadding={8}
        onCloseAutoFocus={focus.onCloseAutoFocus}
      >
        <PaletteItems onPick={focus.take((kind: PaletteKind) => api.insert(target, kind))} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** A card's actions: a drag handle and the step menu. */
export function StepActions({ node }: { node: OutlineNode }) {
  const api = useBuilder();
  const focus = useFocusHandoff();
  const step = api.model.byGraphId.get(node.id);
  if (!step) return null;
  const moves = moveTargets(api.model, step);
  const here = { body: step.body, index: step.index };
  const below = { body: step.body, index: step.index + 1 };
  const elsewhere = [...api.model.bodies.values()].filter(
    (b) =>
      !within(b.pointer, step.pointer) &&
      !(b.pointer === step.body) &&
      moveProblem(api.model, step.pointer, { body: b.pointer, index: b.size }) === null,
  );
  return (
    <>
      <span
        draggable
        aria-hidden="true"
        title="Drag onto a + to move"
        data-slot="drag-handle"
        className="flex h-6 w-4 cursor-grab items-center justify-center text-fg-muted opacity-60 hover:opacity-100"
        onDragStart={(event) => {
          event.dataTransfer.setData(STEP_DRAG_TYPE, step.pointer);
          event.dataTransfer.effectAllowed = 'move';
          api.setDragging(step.pointer);
        }}
        onDragEnd={() => api.setDragging(null)}
      >
        <GripVertical className="size-3.5" />
      </span>
      <DropdownMenu modal={false}>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            aria-label={`Actions for ${step.title}`}
            data-slot="step-menu"
            className="flex size-6 items-center justify-center rounded-sm text-fg-muted hover:bg-hover hover:text-fg focus-visible:outline-2 focus-visible:outline-focus"
          >
            <MoreHorizontal className="size-4" aria-hidden="true" />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-60" onCloseAutoFocus={focus.onCloseAutoFocus}>
          <DropdownMenuSub>
            <DropdownMenuSubTrigger>Insert a step above</DropdownMenuSubTrigger>
            <DropdownMenuSubContent className={PALETTE_WIDTH} collisionPadding={8}>
              <PaletteItems onPick={focus.take((kind: PaletteKind) => api.insert(here, kind))} />
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuSub>
            <DropdownMenuSubTrigger>Insert a step below</DropdownMenuSubTrigger>
            <DropdownMenuSubContent className={PALETTE_WIDTH} collisionPadding={8}>
              <PaletteItems onPick={focus.take((kind: PaletteKind) => api.insert(below, kind))} />
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            disabled={!moves.up}
            onSelect={() => moves.up && api.move(step.pointer, moves.up)}
          >
            Move up
            <Shortcut>Alt ↑</Shortcut>
          </DropdownMenuItem>
          <DropdownMenuItem
            disabled={!moves.down}
            onSelect={() => moves.down && api.move(step.pointer, moves.down)}
          >
            Move down
            <Shortcut>Alt ↓</Shortcut>
          </DropdownMenuItem>
          {moves.out ? (
            <DropdownMenuItem
              onSelect={() => moves.out && api.move(step.pointer, moves.out.target)}
            >
              Move out of {moves.out.owner.title}
            </DropdownMenuItem>
          ) : null}
          {moves.into ? (
            <DropdownMenuItem
              onSelect={() => moves.into && api.move(step.pointer, moves.into.target)}
            >
              Move into {moves.into.owner.title}
            </DropdownMenuItem>
          ) : null}
          {elsewhere.length > 0 ? (
            <DropdownMenuSub>
              <DropdownMenuSubTrigger>Move to the end of…</DropdownMenuSubTrigger>
              <DropdownMenuSubContent className="max-h-[60vh] w-72 overflow-y-auto">
                {elsewhere.map((b) => (
                  <DropdownMenuItem
                    key={b.pointer}
                    onSelect={() => api.move(step.pointer, { body: b.pointer, index: b.size })}
                  >
                    <span className="truncate" style={{ paddingLeft: `${b.depth * 10}px` }}>
                      {b.label}
                    </span>
                  </DropdownMenuItem>
                ))}
              </DropdownMenuSubContent>
            </DropdownMenuSub>
          ) : null}
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={focus.take(() => api.duplicate(step.pointer))}>
            Duplicate
            <Shortcut>Ctrl D</Shortcut>
          </DropdownMenuItem>
          <DropdownMenuItem destructive onSelect={focus.take(() => api.remove(step.pointer))}>
            Delete
            <Shortcut>Del</Shortcut>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </>
  );
}

function Shortcut({ children }: { children: ReactNode }) {
  return (
    <span aria-hidden="true" className="ml-auto font-mono text-2xs text-fg-muted">
      {children}
    </span>
  );
}
