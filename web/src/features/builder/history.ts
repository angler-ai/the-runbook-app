// UI-4: undo and redo over the draft's text. Every change is recorded, from
// the builder or typed in the YAML tab, so undo always steps back through
// what happened. Changes with the same key less than 1.5 s apart (typing in
// one field, typing in the YAML) are one step. A reload starts over.
import { useCallback, useState } from 'react';

const COALESCE_MS = 1500;
const LIMIT = 200;

interface History {
  past: string[];
  future: string[];
  last: { key: string; at: number } | null;
}

const EMPTY: History = { past: [], future: [], last: null };

export interface DraftHistory {
  canUndo: boolean;
  canRedo: boolean;
  /** Record `next` as the new text (from `current`). */
  record(current: string, next: string, key?: string): void;
  /** The text to go back to, or null. */
  undo(current: string): string | null;
  redo(current: string): string | null;
  reset(): void;
}

export function useDraftHistory(): DraftHistory {
  const [history, setHistory] = useState<History>(EMPTY);
  const record = useCallback((current: string, next: string, key?: string) => {
    if (current === next) return;
    const now = Date.now();
    setHistory((h) => {
      const merge = key !== undefined && h.last?.key === key && now - h.last.at < COALESCE_MS;
      return {
        past: merge ? h.past : [...h.past, current].slice(-LIMIT),
        future: [],
        last: key ? { key, at: now } : null,
      };
    });
  }, []);
  const undo = useCallback(
    (current: string) => {
      const previous = history.past[history.past.length - 1];
      if (previous === undefined) return null;
      setHistory((h) => ({
        past: h.past.slice(0, -1),
        future: [current, ...h.future],
        last: null,
      }));
      return previous;
    },
    [history.past],
  );
  const redo = useCallback(
    (current: string) => {
      const next = history.future[0];
      if (next === undefined) return null;
      setHistory((h) => ({ past: [...h.past, current], future: h.future.slice(1), last: null }));
      return next;
    },
    [history.future],
  );
  const reset = useCallback(() => setHistory(EMPTY), []);
  return {
    canUndo: history.past.length > 0,
    canRedo: history.future.length > 0,
    record,
    undo,
    redo,
    reset,
  };
}
