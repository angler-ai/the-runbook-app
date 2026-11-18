// Polite announcements of live changes in the execution document (U11).
// Successive documents are diffed into short sentences ("Step 4, Sign-off:
// succeeded"), batched so a burst of events is one announcement, and
// rendered as text in a role="log" region by the page.
import { useEffect, useRef, useState } from 'react';
import type { Schemas } from '../../api/client';

type Doc = Schemas['ExecutionDocumentView'];

export const statusWords = (status: string) => status.replace(/_/g, ' ');

/** Sentences for what changed from `prev` to `next`; none for the first document. */
export function describeChanges(prev: Doc | undefined, next: Doc): string[] {
  if (!prev) return [];
  const messages: string[] = [];
  const before = new Map(prev.sections.map((section) => [section.step_run_id, section.status]));
  next.sections.forEach((section, index) => {
    const was = before.get(section.step_run_id);
    if (was === section.status || (was === undefined && section.status === 'pending')) return;
    messages.push(`Step ${index + 1}, ${section.title}: ${statusWords(section.status)}`);
  });
  if (prev.header.status !== next.header.status) {
    messages.push(`Run ${statusWords(next.header.status)}`);
  }
  return messages;
}

const MAX_PER_ANNOUNCEMENT = 3;
const KEEP = 5;

function summarize(batch: string[]): string {
  const shown = batch.slice(0, MAX_PER_ANNOUNCEMENT).join('. ');
  const more = batch.length - MAX_PER_ANNOUNCEMENT;
  return more > 0 ? `${shown}. And ${more} more change${more === 1 ? '' : 's'}.` : `${shown}.`;
}

export interface Announcement {
  id: number;
  text: string;
}

/**
 * The announcements for a document as it refetches, at most one every
 * `throttleMs` (a burst of live events is one announcement).
 */
export function useStatusAnnouncements(doc: Doc | undefined, throttleMs = 1500): Announcement[] {
  const previous = useRef<Doc | undefined>(undefined);
  const pending = useRef<string[]>([]);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const lastFlush = useRef(0);
  const nextId = useRef(0);
  const [log, setLog] = useState<Announcement[]>([]);

  useEffect(() => {
    if (!doc) return;
    const changes = describeChanges(previous.current, doc);
    previous.current = doc;
    if (changes.length === 0) return;
    pending.current.push(...changes);
    if (timer.current) return;
    const wait = Math.max(0, lastFlush.current + throttleMs - Date.now());
    timer.current = setTimeout(() => {
      timer.current = undefined;
      lastFlush.current = Date.now();
      const batch = pending.current.splice(0);
      if (batch.length === 0) return;
      nextId.current += 1;
      const entry = { id: nextId.current, text: summarize(batch) };
      setLog((current) => [...current, entry].slice(-KEEP));
    }, wait);
  }, [doc, throttleMs]);

  useEffect(() => () => clearTimeout(timer.current), []);
  return log;
}
