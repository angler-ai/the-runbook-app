import { useState, useRef, type KeyboardEvent, type ComponentType } from 'react';
import type { Outline, OutlineNode, OutlineNodeKind } from '../types';
import { cn } from '../primitives/cn';
import {
  CheckSquare,
  Split,
  Repeat,
  RotateCw,
  GitBranch,
  Clock,
  ShieldCheck,
  Sliders,
  Folder,
  Square,
  Workflow,
  ChevronRight,
  ChevronDown,
  Info,
  Layers,
} from 'lucide-react';

export interface RunbookOutlineProps {
  outline: Outline;
  selectedId?: string;
  onSelect?: (nodeId: string) => void;
  className?: string;
}

const KIND_ICONS: Record<
  OutlineNodeKind,
  ComponentType<{ className?: string; 'aria-hidden'?: boolean | 'true' | 'false' }>
> = {
  task: CheckSquare,
  group: Folder,
  parallel: Split,
  foreach: Repeat,
  loop: RotateCw,
  switch: GitBranch,
  wait: Clock,
  approval: ShieldCheck,
  set: Sliders,
  end: Square,
  call: Workflow,
};

interface FlattenedTreeItem {
  node: OutlineNode;
  level: number;
  branchLabel?: string;
  isFinally?: boolean;
}

export function RunbookOutline({ outline, selectedId, onSelect, className }: RunbookOutlineProps) {
  // Collapsed state for nodes with children/branches
  const [collapsedNodes, setCollapsedNodes] = useState<Set<string>>(new Set());
  // Show technical details disclosure (ids, step budget)
  const [showTechnicalDetails, setShowTechnicalDetails] = useState(false);
  // Keyboard navigation focus
  const [focusedId, setFocusedId] = useState<string | null>(selectedId ?? null);
  const containerRef = useRef<HTMLDivElement>(null);

  const toggleCollapse = (nodeId: string) => {
    setCollapsedNodes((prev) => {
      const next = new Set(prev);
      if (next.has(nodeId)) {
        next.delete(nodeId);
      } else {
        next.add(nodeId);
      }
      return next;
    });
  };

  // Helper to flatten visible items for keyboard navigation (ArrowUp, ArrowDown)
  const getVisibleItems = (): FlattenedTreeItem[] => {
    const list: FlattenedTreeItem[] = [];

    const traverse = (
      node: OutlineNode,
      level: number,
      branchLabel?: string,
      isFinally = false,
    ) => {
      list.push({ node, level, branchLabel, isFinally });
      const isCollapsed = collapsedNodes.has(node.id);

      if (!isCollapsed) {
        if (node.children) {
          for (const child of node.children) {
            traverse(child, level + 1, undefined, isFinally);
          }
        }
        if (node.branches) {
          for (const branch of node.branches) {
            for (const child of branch.children) {
              traverse(child, level + 1, undefined, isFinally);
            }
          }
        }
      }
    };

    for (const step of outline.steps) {
      traverse(step, 1, undefined, false);
    }
    for (const f of outline.finally) {
      traverse(f, 1, undefined, true);
    }

    return list;
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const visible = getVisibleItems();
    if (visible.length === 0) return;

    const currentIdx = visible.findIndex((item) => item.node.id === focusedId);

    switch (e.key) {
      case 'ArrowDown': {
        e.preventDefault();
        const nextIdx = currentIdx < visible.length - 1 ? currentIdx + 1 : 0;
        const nextId = visible[nextIdx]!.node.id;
        setFocusedId(nextId);
        focusItem(nextId);
        break;
      }
      case 'ArrowUp': {
        e.preventDefault();
        const prevIdx = currentIdx > 0 ? currentIdx - 1 : visible.length - 1;
        const prevId = visible[prevIdx]!.node.id;
        setFocusedId(prevId);
        focusItem(prevId);
        break;
      }
      case 'ArrowRight': {
        e.preventDefault();
        if (focusedId) {
          const currentItem = visible.find((it) => it.node.id === focusedId);
          const hasExpandable =
            (currentItem?.node.children && currentItem.node.children.length > 0) ||
            (currentItem?.node.branches && currentItem.node.branches.length > 0);

          if (hasExpandable) {
            if (collapsedNodes.has(focusedId)) {
              toggleCollapse(focusedId);
            } else if (currentIdx < visible.length - 1) {
              const nextId = visible[currentIdx + 1]!.node.id;
              setFocusedId(nextId);
              focusItem(nextId);
            }
          }
        }
        break;
      }
      case 'ArrowLeft': {
        e.preventDefault();
        if (focusedId) {
          const isExpanded = !collapsedNodes.has(focusedId);
          const currentItem = visible.find((it) => it.node.id === focusedId);
          const hasExpandable =
            (currentItem?.node.children && currentItem.node.children.length > 0) ||
            (currentItem?.node.branches && currentItem.node.branches.length > 0);

          if (hasExpandable && isExpanded) {
            toggleCollapse(focusedId);
          } else if (currentItem && currentItem.level > 1) {
            // Find parent item (item with level = currentItem.level - 1 preceding this one)
            for (let i = currentIdx - 1; i >= 0; i--) {
              if (visible[i]!.level === currentItem.level - 1) {
                const parentId = visible[i]!.node.id;
                setFocusedId(parentId);
                focusItem(parentId);
                break;
              }
            }
          }
        }
        break;
      }
      case 'Enter':
      case ' ': {
        e.preventDefault();
        if (focusedId) {
          onSelect?.(focusedId);
        }
        break;
      }
    }
  };

  const focusItem = (id: string) => {
    const el = containerRef.current?.querySelector<HTMLElement>(`[data-node-id="${id}"]`);
    el?.focus();
  };

  const renderNode = (node: OutlineNode, level: number, isFinally = false) => {
    const Icon = KIND_ICONS[node.kind] || CheckSquare;
    const hasChildren = (node.children && node.children.length > 0) || false;
    const hasBranches = (node.branches && node.branches.length > 0) || false;
    const isExpandable = hasChildren || hasBranches;
    const isCollapsed = collapsedNodes.has(node.id);
    const isSelected = selectedId === node.id;
    const isFocused = (focusedId ?? selectedId ?? outline.steps[0]?.id ?? '') === node.id;

    // Hint: at most ONE short hint per PRD requirements
    const hint =
      node.hint ||
      (node.check
        ? node.actor
          ? `${node.actor === 'agent' ? 'Agent' : node.actor === 'human' ? 'Human' : 'Any'} · check`
          : 'check'
        : node.destructive
          ? 'destructive'
          : undefined);

    return (
      <div key={node.id} role="none" className="flex flex-col">
        {/* Node row */}
        <div
          data-slot="outline-row"
          data-node-id={node.id}
          role="treeitem"
          tabIndex={isFocused ? 0 : -1}
          aria-level={level}
          aria-expanded={isExpandable ? !isCollapsed : undefined}
          aria-selected={isSelected}
          onClick={() => {
            setFocusedId(node.id);
            onSelect?.(node.id);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              onSelect?.(node.id);
            }
          }}
          onFocus={() => setFocusedId(node.id)}
          className={cn(
            'group flex items-center justify-between gap-2.5 px-3 py-2 rounded-lg cursor-pointer transition-colors select-none text-xs border border-transparent',
            isSelected
              ? 'bg-accent/10 border-accent/30 text-fg'
              : 'hover:bg-surface-raised focus-visible:bg-surface-raised text-fg',
            'focus-visible:outline-2 focus-visible:outline-accent',
          )}
        >
          {/* Left: expander + icon + title */}
          <div className="flex items-center gap-2 min-w-0">
            {isExpandable ? (
              <button
                type="button"
                aria-label={
                  isCollapsed
                    ? `Expand ${node.title || node.id}`
                    : `Collapse ${node.title || node.id}`
                }
                onClick={(e) => {
                  e.stopPropagation();
                  toggleCollapse(node.id);
                }}
                // 24×24 px (WCAG 2.5.8); -my-1 keeps the row height. The ring
                // is the row's, so keyboard users see which control has focus.
                className="size-6 -my-1 shrink-0 flex items-center justify-center text-fg-muted hover:text-fg rounded focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent"
              >
                {isCollapsed ? (
                  <ChevronRight className="size-3.5" aria-hidden="true" />
                ) : (
                  <ChevronDown className="size-3.5" aria-hidden="true" />
                )}
              </button>
            ) : (
              <span className="size-6 -my-1 shrink-0" aria-hidden="true" />
            )}

            <Icon
              className={cn(
                'size-4 shrink-0',
                node.destructive
                  ? 'text-verdict-fail-fg'
                  : node.check
                    ? 'text-verdict-pass-fg'
                    : isSelected
                      ? 'text-accent'
                      : 'text-fg-muted group-hover:text-fg',
              )}
              aria-hidden="true"
            />

            <span className="font-medium text-fg truncate">{node.title || node.id}</span>
          </div>

          {/* Right: At most ONE short hint */}
          <div className="flex items-center gap-2 shrink-0">
            {hint && (
              <span
                data-slot="outline-hint"
                className="text-2xs text-fg-muted font-sans font-medium px-1.5 py-0.5 rounded bg-surface-raised border border-border"
              >
                {hint}
              </span>
            )}
          </div>
        </div>

        {/* Children indented under guide line */}
        {isExpandable && !isCollapsed && (
          <div
            role="group"
            className="relative ml-5 pl-3 border-l border-border flex flex-col gap-1 my-1"
          >
            {node.children?.map((child) => renderNode(child, level + 1, isFinally))}
            {node.branches?.map((branch) => (
              <div
                key={branch.label}
                role="group"
                aria-label={`Branch: ${branch.label}`}
                className="flex flex-col gap-1"
              >
                <div
                  aria-hidden="true"
                  className="flex items-center gap-1.5 py-1 text-2xs font-semibold text-fg-muted uppercase tracking-wider"
                >
                  <span className="h-px w-3 bg-border" />
                  <span>Branch: {branch.label}</span>
                </div>
                {branch.children.map((child) => renderNode(child, level + 1, isFinally))}
              </div>
            ))}
          </div>
        )}
      </div>
    );
  };

  return (
    <div
      data-slot="runbook-outline"
      className={cn('flex flex-col gap-4 text-xs select-none', className)}
    >
      {/* Outline Header & Technical Details Disclosure */}
      <div className="flex items-center justify-between pb-2 border-b border-border">
        <div className="flex items-center gap-2">
          <Layers className="size-4 text-accent" aria-hidden="true" />
          <h3 className="font-semibold text-xs text-fg">{outline.title}</h3>
        </div>

        {/* Disclosure for technical detail (ids, step budget) */}
        <button
          type="button"
          onClick={() => setShowTechnicalDetails(!showTechnicalDetails)}
          aria-expanded={showTechnicalDetails}
          className="inline-flex items-center gap-1 text-2xs text-fg-muted hover:text-fg rounded px-1.5 py-0.5 border border-border bg-surface-raised"
        >
          <Info className="size-3 text-fg-muted" aria-hidden="true" />
          <span>{showTechnicalDetails ? 'Hide details' : 'Technical details'}</span>
        </button>
      </div>

      {/* Expanded Technical Details */}
      {showTechnicalDetails && (
        <div
          data-slot="technical-details"
          className="p-3 bg-surface-raised border border-border rounded-lg text-2xs flex flex-col gap-2 font-mono text-fg-muted"
        >
          <div className="flex items-center justify-between">
            <span className="font-sans font-semibold text-fg">Runbook ID:</span>
            <span className="text-fg">{outline.id}</span>
          </div>
          <div className="flex items-center justify-between">
            <span className="font-sans font-semibold text-fg">Step Budget:</span>
            <span className="text-fg font-semibold">
              {outline.stepBudget != null ? `${outline.stepBudget} steps max` : 'Unlimited'}
            </span>
          </div>
        </div>
      )}

      {/* Tree container */}
      <div
        ref={containerRef}
        role="tree"
        tabIndex={0}
        aria-label={`${outline.title} Outline`}
        onKeyDown={handleKeyDown}
        className="flex flex-col gap-4 focus:outline-none"
      >
        {/* Main Steps */}
        <div role="group" aria-label="Steps" className="flex flex-col gap-1">
          {outline.steps.map((step) => renderNode(step, 1, false))}
        </div>

        {/* Always at the end (finally) section */}
        {outline.finally.length > 0 && (
          <div
            role="group"
            aria-label="Always at the end"
            className="mt-3 pt-3 border-t border-dashed border-border flex flex-col gap-1"
          >
            <div
              aria-hidden="true"
              className="text-2xs font-semibold uppercase tracking-wider text-fg-muted mb-1 px-1"
            >
              Always at the end
            </div>
            <div role="none" className="flex flex-col gap-1">
              {outline.finally.map((step) => renderNode(step, 1, true))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
