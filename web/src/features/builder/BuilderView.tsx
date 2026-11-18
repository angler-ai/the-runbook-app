// UI-4: the graph builder. The editor's Graph tab: the runbook drawn from
// its YAML, with a "+" on every edge into a step, an "Add step" pill at the
// end of every list, a menu and a drag handle on every card, the selected
// step's form beside it and the runbook's settings when nothing is
// selected. Every change is a YAML edit (see yamlEdit.ts), so the YAML tab
// and this view always show the same draft. While the YAML does not parse,
// the last graph that did is shown read-only with the parse error.
//
// Review round 1: YAML the compiler refuses outright (anchors, aliases,
// tags, a byte order mark) is drawn read-only with the reason. Every edit is
// computed inside a guard: one that throws shows an error and changes
// nothing. An edit that had to rewrite the whole file says so. Step
// shortcuts (Delete, Alt+arrows, Ctrl+D) act only from a card or the
// canvas, and deleting a container with steps inside asks first.
import { Redo2, Undo2, Plus } from 'lucide-react';
import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from 'react';
import { RunbookGraph } from '../../ui/graph';
import { EmptyState } from '../../ui/layout';
import type { Outline, OutlineNode } from '../../ui/types';
import type { Diagnostic } from '../../wasm/runbook';
import { diagnosticMessage } from '../editor/diagnostics';
import {
  BuilderContext,
  type BuilderApi,
  type BuilderDirectory,
  type CallTarget,
  type EditOptions,
} from './context';
import { paletteEntry, type PaletteKind } from './kinds';
import { EdgeInsert, PaletteButton, SlotInsert, StepActions } from './menus';
import { readBuilder, stepForPath, type BuilderModel, type ParseProblem } from './model';
import { parentBody } from '../../ui/graph/bodies';
import {
  deleteStep,
  duplicateStep,
  insertStep,
  moveStep,
  moveTargets,
  type InsertTarget,
} from './ops';
import { SettingsPanel } from './SettingsPanel';
import { StepPanel } from './StepPanel';
import type { EditResult } from './yamlEdit';

export interface BuilderHistory {
  canUndo: boolean;
  canRedo: boolean;
  undo(): void;
  redo(): void;
}

export interface BuilderViewProps {
  source: string;
  /** Replace the draft's text with a builder edit. */
  onEdit(next: string, options: EditOptions): void;
  readOnly: boolean;
  diagnostics: Diagnostic[];
  stepBudget: number | null | undefined;
  directory: BuilderDirectory;
  callTargets: CallTarget[];
  /** The selected step's graph ID (shared with the Outline tab). */
  selected: string | undefined;
  onSelect(id: string | undefined): void;
  history: BuilderHistory;
  /** A runbook not saved yet: its ID may still change. */
  isNew: boolean;
  /** Open the YAML tab at a position (a parse error's link). */
  onShowYaml(position: { line: number; column: number }): void;
  callHref?: (slug: string) => string | undefined;
  /** A webhook trigger's URL, once the runbook is saved. */
  triggerUrl?: (id: string) => string;
  onNavigate?: (href: string) => void;
  viewKey?: string;
}

/** A card or the canvas itself has focus (not a field, a button in the panel, the toolbar). */
function onCanvas(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.closest('[data-node-id]')) return true;
  return target.matches(
    '.react-flow, .react-flow__pane, .react-flow__renderer, .react-flow__viewport',
  );
}

/** How many steps are inside a step (containers), all levels down. */
function stepsInside(model: BuilderModel, pointer: string): number {
  return model.steps.filter((s) => s.pointer.startsWith(`${pointer}/`)).length;
}

function isTyping(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  if (target.closest('input, textarea, select, [role="menu"], [role="menuitem"]')) return true;
  return false;
}

export function BuilderView(props: BuilderViewProps) {
  const {
    source,
    onEdit,
    readOnly,
    diagnostics,
    stepBudget,
    directory,
    callTargets,
    selected,
    onSelect,
    history,
    isNew,
    onShowYaml,
    callHref,
    triggerUrl,
    onNavigate,
    viewKey,
  } = props;
  const read = useMemo(() => readBuilder(source), [source]);
  const locked = read.ok ? read.locked : undefined;
  /** Whether the builder may change the text (the host allows it and the YAML is editable). */
  const editable = !readOnly && !locked;
  const [lastGood, setLastGood] = useState<{ outline: Outline; model: BuilderModel } | null>(null);
  if (read.ok && lastGood?.model !== read.model)
    setLastGood({ outline: read.model.outline, model: read.model });

  const [freshIds, setFreshIds] = useState<ReadonlySet<string>>(() => new Set());
  const [dragging, setDragging] = useState<string | null>(null);
  /** What the status region says; `seq` makes the same words twice a new announcement. */
  const [announcement, setAnnouncement] = useState<{ text: string; seq: number } | null>(null);
  /** An edit that threw: shown until the next edit that works. */
  const [editError, setEditError] = useState<string | null>(null);
  /** The last edit rewrote the whole YAML (`minimal: false`). */
  const [normalized, setNormalized] = useState(false);
  /** UI-4b: the last edit replaced a value that was not a list (`steps: 5`). */
  const [replaced, setReplaced] = useState<EditResult['replaced'] | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const statusRef = useRef<HTMLParagraphElement>(null);
  const announce = useCallback(
    (text: string) => setAnnouncement((a) => ({ text, seq: (a?.seq ?? 0) + 1 })),
    [],
  );
  // The same words twice in a row would leave the live region unchanged
  // and unread: clear it, then say them a frame later.
  useEffect(() => {
    const region = statusRef.current;
    if (!region || !announcement) return undefined;
    region.textContent = '';
    const frame = requestAnimationFrame(() => {
      region.textContent = announcement.text;
    });
    return () => cancelAnimationFrame(frame);
  }, [announcement]);

  const model = read.ok ? read.model : null;
  const yamlDoc = read.ok ? read.document : null;

  // Found by a data attribute, not a ref: edits (which focus) are handed to render.
  const scope = useId();
  const focusSoon = useCallback(
    (selector: string) => {
      requestAnimationFrame(() =>
        requestAnimationFrame(() => {
          // The person may already be somewhere else (typing in a field, in a
          // menu they just opened): never pull focus away from that.
          const active = globalThis.document.activeElement;
          if (active instanceof HTMLElement && active.closest('input, textarea, select')) return;
          if (globalThis.document.querySelector('[role="menu"][data-state="open"]')) return;
          globalThis.document
            .querySelector<HTMLElement>(`[data-builder-scope="${CSS.escape(scope)}"] ${selector}`)
            ?.focus();
        }),
      );
    },
    [scope],
  );

  const edit = useCallback(
    (make: () => EditResult & { pointer?: string }, options: EditOptions = {}) => {
      if (!editable) return;
      let result: EditResult & { pointer?: string };
      try {
        result = make();
      } catch (error) {
        setEditError(error instanceof Error ? error.message : String(error));
        return;
      }
      setEditError(null);
      if (result.text === source) return;
      setNormalized(!result.minimal);
      setReplaced(result.replaced ?? null);
      if (options.announce) announce(options.announce);
      onEdit(result.text, options);
      // Read the edit back now: select what it asked for, and keep the
      // selected step selected when its ID changed (a title typed on a new step).
      const after = readBuilder(result.text);
      if (!after.ok) return;
      const select = options.selectResult ? result.pointer : options.select;
      if (select) {
        const step = after.model.byPointer.get(select);
        onSelect(step?.graphId);
        if (step && options.focusPanel !== false) focusSoon('[data-slot="node-details-heading"]');
      } else if (selected && !after.model.byGraphId.has(selected)) {
        const was = model?.byGraphId.get(selected)?.pointer;
        onSelect(was ? after.model.byPointer.get(was)?.graphId : undefined);
      }
      if (options.focusCard) {
        const step = after.model.byPointer.get(options.focusCard);
        if (step)
          focusSoon(`[data-node-id="${CSS.escape(step.graphId)}"] [data-slot="node-select"]`);
      }
    },
    [editable, source, onEdit, onSelect, selected, model, focusSoon, announce],
  );

  const problemsAt = useCallback(
    (pointer: string, options: { exact?: boolean } = {}) =>
      diagnostics
        .filter((d) =>
          options.exact
            ? d.path === pointer
            : d.path === pointer || d.path.startsWith(`${pointer}/`),
        )
        .map(diagnosticMessage),
    [diagnostics],
  );

  const issues = useMemo(() => {
    const counts = new Map<string, number>();
    if (!model) return counts;
    for (const d of diagnostics) {
      const step = stepForPath(model, d.path);
      if (step) counts.set(step.graphId, (counts.get(step.graphId) ?? 0) + 1);
    }
    return counts;
  }, [diagnostics, model]);

  const insert = useCallback(
    (target: InsertTarget, kind: PaletteKind) => {
      if (!model) return;
      const callTarget = callTargets.find((t) => t.slug !== model.outline.id)?.slug;
      const pointer = `${target.body}/${target.index}`;
      edit(
        () => {
          const result = insertStep(source, model, target, kind, callTarget ? { callTarget } : {});
          const after = readBuilder(result.text);
          if (after.ok) {
            const made = [...after.model.ids].filter((id) => !model.ids.has(id));
            setFreshIds((prev) => new Set([...prev, ...made]));
          }
          return result;
        },
        {
          select: pointer,
          announce: `Added a ${paletteEntry(kind).label.toLowerCase()} step.`,
        },
      );
    },
    [model, source, callTargets, edit],
  );

  const remove = useCallback(
    (pointer: string) => {
      const step = model?.byPointer.get(pointer);
      if (!model || !step) return;
      const size = model.bodies.get(step.body)?.size ?? 0;
      const owner = model.bodies.get(step.body)?.owner;
      const neighbour =
        step.index < size - 1
          ? `${step.body}/${step.index}`
          : step.index > 0
            ? `${step.body}/${step.index - 1}`
            : owner;
      onSelect(undefined);
      edit(() => deleteStep(source, pointer), {
        ...(neighbour ? { focusCard: neighbour } : {}),
        announce: `Deleted ${step.title}.`,
      });
    },
    [model, source, edit, onSelect],
  );

  const move = useCallback(
    (pointer: string, target: InsertTarget) => {
      const step = model?.byPointer.get(pointer);
      if (!model || !step) return;
      edit(() => moveStep(source, model, pointer, target) ?? { text: source, minimal: true }, {
        selectResult: true,
        focusPanel: false,
        announce: `Moved ${step.title}.`,
      });
    },
    [model, source, edit],
  );

  const duplicate = useCallback(
    (pointer: string) => {
      if (!model) return;
      const step = model.byPointer.get(pointer);
      const at = parentBody(pointer);
      edit(() => duplicateStep(source, model, pointer), {
        ...(at ? { select: `${at.body}/${at.index + 1}` } : {}),
        announce: `Duplicated ${step?.title ?? 'the step'}.`,
      });
    },
    [model, source, edit],
  );

  const selectPointer = useCallback(
    (pointer: string | undefined) => {
      const step = pointer ? model?.byPointer.get(pointer) : undefined;
      onSelect(step?.graphId);
      if (step) focusSoon('[data-slot="node-details-heading"]');
    },
    [model, onSelect, focusSoon],
  );

  const forgetFresh = useCallback(
    (id: string) =>
      setFreshIds((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      }),
    [],
  );
  const renameFresh = useCallback(
    (from: string, to: string) =>
      setFreshIds((prev) => {
        const next = new Set(prev);
        next.delete(from);
        next.add(to);
        return next;
      }),
    [],
  );

  const api = useMemo<BuilderApi | null>(
    () =>
      model && yamlDoc
        ? {
            source,
            model,
            document: yamlDoc,
            edit,
            insert,
            remove,
            move,
            duplicate,
            select: selectPointer,
            problemsAt,
            diagnostics,
            directory,
            callTargets,
            freshIds,
            forgetFresh,
            renameFresh,
            isNew,
            dragging,
            setDragging,
            ...(callHref ? { callHref } : {}),
            ...(triggerUrl ? { triggerUrl } : {}),
          }
        : null,
    [
      model,
      yamlDoc,
      source,
      edit,
      insert,
      remove,
      move,
      duplicate,
      selectPointer,
      problemsAt,
      diagnostics,
      directory,
      callTargets,
      freshIds,
      forgetFresh,
      renameFresh,
      isNew,
      dragging,
      callHref,
      triggerUrl,
    ],
  );

  const builder = useMemo(
    () =>
      api && editable
        ? {
            renderInsert: (point: InsertTarget, label: string) => (
              <EdgeInsert target={point} label={label} />
            ),
            renderSlot: (slot: InsertTarget & { label: string; description: string }) => (
              <SlotInsert
                target={{ body: slot.body, index: slot.index }}
                label={slot.label}
                description={slot.description}
              />
            ),
            renderActions: (node: OutlineNode) => <StepActions node={node} />,
            issues,
          }
        : undefined,
    [api, editable, issues],
  );

  const userSelect = useCallback(
    (id: string | undefined) => {
      onSelect(id);
      if (id) focusSoon('[data-slot="node-details-heading"]');
    },
    [onSelect, focusSoon],
  );

  const onKeyDown = useCallback(
    (event: globalThis.KeyboardEvent) => {
      if (!api || !editable || event.defaultPrevented || isTyping(event.target)) return;
      const mod = event.metaKey || event.ctrlKey;
      const key = event.key.toLowerCase();
      if (mod && key === 'z') {
        event.preventDefault();
        if (event.shiftKey) history.redo();
        else history.undo();
        return;
      }
      if (mod && key === 'y') {
        event.preventDefault();
        history.redo();
        return;
      }
      // Step shortcuts act from a card or the canvas only: Backspace on a
      // panel button or heading must not delete the step.
      if (!onCanvas(event.target)) return;
      const step = selected ? api.model.byGraphId.get(selected) : undefined;
      if (!step) return;
      if (event.key === 'Delete' || (event.key === 'Backspace' && !mod)) {
        event.preventDefault();
        const inside = stepsInside(api.model, step.pointer);
        if (
          inside > 0 &&
          !window.confirm(
            `Delete ${step.title} and the ${inside} ${inside === 1 ? 'step' : 'steps'} inside it?`,
          )
        )
          return;
        api.remove(step.pointer);
      } else if (event.altKey && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
        const moves = moveTargets(api.model, step);
        const target = event.key === 'ArrowUp' ? moves.up : moves.down;
        event.preventDefault();
        if (target) api.move(step.pointer, target);
      } else if (mod && key === 'd') {
        event.preventDefault();
        api.duplicate(step.pointer);
      }
    },
    [api, editable, history, selected],
  );

  useEffect(() => {
    const root = rootRef.current;
    if (!root) return undefined;
    root.addEventListener('keydown', onKeyDown);
    return () => root.removeEventListener('keydown', onKeyDown);
  }, [onKeyDown]);

  // A new outline object per render would lay the graph out again on every
  // selection and drag (review round 1): one per model and budget.
  const graphOutline = useMemo(
    () => (model ? { ...model.outline, stepBudget: stepBudget ?? null } : null),
    [model, stepBudget],
  );

  const status = (
    <p
      ref={statusRef}
      role="status"
      aria-live="polite"
      className="sr-only"
      data-testid="builder-status"
    />
  );
  const notes = (
    <>
      {locked ? (
        <p
          role="note"
          data-testid="builder-locked"
          className="rounded-md border border-border p-3 text-sm"
        >
          {locked}
        </p>
      ) : null}
      {editError ? (
        <p
          role="alert"
          data-testid="builder-error"
          className="rounded-md border border-verdict-fail-border bg-verdict-fail-bg p-3 text-sm text-verdict-fail-fg"
        >
          That change could not be made, so the YAML is unchanged: {editError}
        </p>
      ) : null}
      {replaced && !editError ? (
        <p
          role="status"
          data-testid="builder-replaced"
          className="rounded-md border border-stale-border bg-stale-bg p-3 text-sm text-stale-fg"
        >
          {replaced.path.join('.')} was {replaced.was}, not a list, so it was replaced. Undo
          (Ctrl+Z) brings it back.
        </p>
      ) : null}
      {normalized && !editError ? (
        <p
          role="status"
          data-testid="builder-normalized"
          className="rounded-md border border-stale-border bg-stale-bg p-3 text-sm text-stale-fg"
        >
          That change could not be made in place, so the YAML was rewritten in the standard layout:
          comments and formatting may have moved. Undo (Ctrl+Z) brings the previous text back.
        </p>
      ) : null}
    </>
  );

  if (!read.ok) {
    return (
      <div className="grid gap-3" data-testid="builder">
        {status}
        {notes}
        <ParseErrorNote problems={read.problems} onShowYaml={onShowYaml} />
        {lastGood ? (
          <RunbookGraph
            outline={lastGood.outline}
            className="h-[min(72vh,720px)]"
            fillViewport
            {...(callHref ? { callHref } : {})}
            selectedId={selected}
            onSelect={onSelect}
            {...(onNavigate ? { onNavigate } : {})}
            {...(viewKey ? { viewKey } : {})}
            panToSelected
          />
        ) : (
          <EmptyState
            title="Nothing to draw yet"
            description="The YAML does not parse. Fix it in the YAML tab and the graph comes back."
          />
        )}
      </div>
    );
  }

  const top = api
    ? { body: '/spec/steps', index: api.model.bodies.get('/spec/steps')?.size ?? 0 }
    : null;
  const stepCount = model?.steps.length ?? 0;

  return (
    <BuilderContext.Provider value={api}>
      <div ref={rootRef} className="grid gap-2" data-testid="builder" data-builder-scope={scope}>
        {status}
        {notes}
        {!editable ? null : (
          <div role="toolbar" aria-label="Builder" className="flex flex-wrap items-center gap-2">
            <ToolButton
              label="Undo"
              shortcut="Ctrl+Z"
              disabled={!history.canUndo}
              onClick={history.undo}
            >
              <Undo2 className="size-4" aria-hidden="true" />
            </ToolButton>
            <ToolButton
              label="Redo"
              shortcut="Ctrl+Shift+Z"
              disabled={!history.canRedo}
              onClick={history.redo}
            >
              <Redo2 className="size-4" aria-hidden="true" />
            </ToolButton>
            {top ? (
              <PaletteButton target={top}>
                <Plus className="size-4" aria-hidden="true" />
                Add step
              </PaletteButton>
            ) : null}
            <p className="ml-auto text-xs text-fg-muted" data-testid="builder-summary">
              {stepCount} {stepCount === 1 ? 'step' : 'steps'}
              {stepBudget != null ? ` · at most ${stepBudget} step runs (provisional)` : ''}
            </p>
            <details className="relative text-xs">
              <summary className="cursor-pointer rounded-sm text-fg-muted hover:text-fg focus-visible:outline-2 focus-visible:outline-focus">
                Keyboard
              </summary>
              <div className="absolute right-0 z-30 mt-1 w-72 rounded-md border border-border bg-surface p-3 text-fg shadow-overlay">
                <ul className="grid gap-1">
                  <li>Tab to a step, Enter to edit it, Esc to close the panel.</li>
                  <li>Each step’s ⋯ menu inserts, moves, duplicates and deletes.</li>
                  <li>
                    On a step’s card: Delete removes it (a container asks first), Alt+↑/↓ moves it,
                    Ctrl+D duplicates it.
                  </li>
                  <li>Ctrl+Z undoes, Ctrl+Shift+Z redoes (⌘ on macOS).</li>
                </ul>
              </div>
            </details>
          </div>
        )}
        {graphOutline ? (
          <RunbookGraph
            outline={graphOutline}
            className="h-[min(72vh,720px)]"
            fillViewport
            {...(callHref ? { callHref } : {})}
            selectedId={selected}
            onSelect={userSelect}
            {...(onNavigate ? { onNavigate } : {})}
            {...(viewKey ? { viewKey } : {})}
            {...(builder ? { builder } : {})}
            focusPanelOnSelect={!editable}
            // UI-4b: "Show <step>" pans there in a read-only editor too.
            panToSelected
            {...(api && editable
              ? {
                  renderDetails: (node: OutlineNode, close: () => void) => (
                    <StepPanel node={node} onClose={close} />
                  ),
                  idlePanel: <SettingsPanel stepBudget={stepBudget} />,
                }
              : {})}
          />
        ) : null}
      </div>
    </BuilderContext.Provider>
  );
}

function ToolButton({
  label,
  shortcut,
  disabled,
  onClick,
  children,
}: {
  label: string;
  shortcut: string;
  disabled: boolean;
  onClick(): void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={`${label} (${shortcut})`}
      aria-keyshortcuts={shortcut.replace('Ctrl', 'Control')}
      disabled={disabled}
      onClick={onClick}
      className="inline-flex size-8 items-center justify-center rounded-md border border-border bg-surface text-fg hover:bg-hover focus-visible:outline-2 focus-visible:outline-focus disabled:opacity-40"
    >
      {children}
    </button>
  );
}

function ParseErrorNote({
  problems,
  onShowYaml,
}: {
  problems: ParseProblem[];
  onShowYaml(position: { line: number; column: number }): void;
}) {
  const first = problems[0];
  return (
    <div
      role="alert"
      data-testid="builder-parse-error"
      className="grid gap-1.5 rounded-md border border-verdict-fail-border bg-verdict-fail-bg p-3 text-sm text-verdict-fail-fg"
    >
      <p className="font-medium">
        The YAML does not parse, so the builder is read-only. This is the last graph that did.
      </p>
      {first ? (
        <p>
          Line {first.line}, column {first.column}: {first.message}{' '}
          <button
            type="button"
            className="font-medium underline underline-offset-2"
            onClick={() => onShowYaml({ line: first.line, column: first.column })}
          >
            Go to line {first.line} in the YAML
          </button>
        </p>
      ) : null}
    </div>
  );
}
