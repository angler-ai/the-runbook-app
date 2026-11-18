// UI-4: what the builder's panels and menus share: the draft's text and
// model, the edit entry point, the compiler's problems, and the directory.
import { createContext, useContext } from 'react';
import type { Document } from 'yaml';
import type { Diagnostic } from '../../wasm/runbook';
import type { PaletteKind } from './kinds';
import type { BuilderModel } from './model';
import type { InsertTarget } from './ops';
import type { EditResult } from './yamlEdit';

export interface EditOptions {
  /** Edits with the same key in a row are one undo step (typing in a field). */
  coalesce?: string;
  /** Select the step at this pointer once the edit is read back. */
  select?: string;
  /** Select the step at the pointer the edit itself reports (a move). */
  selectResult?: boolean;
  /** With `select`: move focus to the step's panel (default) or leave it. */
  focusPanel?: boolean;
  /** Move focus to this step's card once the edit is read back. */
  focusCard?: string;
  /** Said by the screen reader's status region. */
  announce?: string;
}

export interface CallTarget {
  runbookId: string;
  slug: string;
  title: string;
  latestVersion: number;
}

export interface BuilderDirectory {
  users: string[];
  teams: string[];
  skills: string[];
}

export interface BuilderApi {
  source: string;
  model: BuilderModel;
  document: Document;
  /**
   * Make an edit. `make` computes it from the current text; if it throws,
   * the builder shows the error and the text stays as it was (review
   * round 1: no edit fails silently).
   */
  edit(make: () => EditResult & { pointer?: string }, options?: EditOptions): void;
  insert(target: InsertTarget, kind: PaletteKind): void;
  remove(pointer: string): void;
  move(pointer: string, target: InsertTarget): void;
  duplicate(pointer: string): void;
  select(pointer: string | undefined): void;
  /** The compiler's messages for this part of the YAML and below it. */
  problemsAt(pointer: string, options?: { exact?: boolean }): string[];
  diagnostics: Diagnostic[];
  directory: BuilderDirectory;
  callTargets: CallTarget[];
  /** Step IDs this session made up; a typed title renames them. */
  freshIds: ReadonlySet<string>;
  forgetFresh(id: string): void;
  renameFresh(from: string, to: string): void;
  /** A new runbook (not saved yet): its ID can still change. */
  isNew: boolean;
  /** The step being dragged (pointer), for drop targets. */
  dragging: string | null;
  setDragging(pointer: string | null): void;
  callHref?: (slug: string) => string | undefined;
  /** A saved runbook's URL for a webhook trigger (absent for a new runbook). */
  triggerUrl?: (id: string) => string;
}

export const BuilderContext = createContext<BuilderApi | null>(null);

export function useBuilder(): BuilderApi {
  const api = useContext(BuilderContext);
  if (!api) throw new Error('useBuilder outside the builder');
  return api;
}
