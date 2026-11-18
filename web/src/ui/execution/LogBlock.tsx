// UI-3: a step's log in a quiet mono block, collapsed by default, with
// copy. Long logs are virtualized: lines do not wrap (the block scrolls
// sideways), so every row is one fixed height and only the rows in view
// (plus a margin) are in the DOM. Copy takes the whole log (loading the
// pages not read yet when it can, and saying so when it cannot). An open
// log prints every loaded line; a closed one prints where to find it.
import { useEffect, useRef, useState, type UIEvent } from 'react';
import { flushSync } from 'react-dom';
import { Check, ChevronDown, ChevronRight, Copy } from 'lucide-react';
import { cn } from '../primitives/cn';

/** Rows are `leading-5`: 20px each. */
export const LOG_ROW_HEIGHT = 20;
/** Up to this many lines render plainly; more are virtualized. */
export const LOG_VIRTUALIZE_ABOVE = 200;
const VIEW_HEIGHT = 320;
const OVERSCAN = 20;

export interface LogBlockProps {
  lines: string[];
  loading?: boolean;
  error?: string | null;
  hasMore?: boolean;
  onLoadMore?: () => void;
  /** Called the first time the block opens (fetch the log then). */
  onOpen?: () => void;
  defaultOpen?: boolean;
  /** "Log", or "Agent log". */
  label?: string;
  className?: string;
  /** Every line, the pages not loaded yet included (for Copy); `complete` false when capped. */
  onCopyAll?: () => Promise<{ lines: string[]; complete: boolean }>;
  /** What a printout says in place of a closed log (where to read it). */
  printNote?: string;
}

export function LogBlock({
  lines,
  loading = false,
  error = null,
  hasMore = false,
  onLoadMore,
  onOpen,
  defaultOpen = false,
  label = 'Log',
  className,
  onCopyAll,
  printNote = 'Log not printed: open it on the run page to print it.',
}: LogBlockProps) {
  const [open, setOpen] = useState(defaultOpen);
  /** After a copy: what was copied ("Copied", "Copied the 200 loaded lines"). */
  const [copied, setCopied] = useState<string | null>(null);
  const opened = useRef(false);
  useEffect(() => {
    if (open && !opened.current) {
      opened.current = true;
      onOpen?.();
    }
  }, [open, onOpen]);
  useEffect(() => {
    if (!copied) return undefined;
    const timer = setTimeout(() => setCopied(null), 2500);
    return () => clearTimeout(timer);
  }, [copied]);

  const copy = async () => {
    const report = (complete: boolean, count: number) =>
      setCopied(complete ? 'Copied' : `Copied the ${count} loaded lines`);
    const clipboard = navigator.clipboard;
    // Loading the unread pages takes network reads, and Safari (Firefox
    // too, after a few seconds) refuses a clipboard write that is not part
    // of the click. A ClipboardItem with a pending Blob starts the write
    // inside the click and fills it when the pages arrive (UI-3 re-check).
    if (hasMore && onCopyAll && typeof ClipboardItem !== 'undefined' && clipboard?.write) {
      let result = { lines, complete: false };
      const blob = onCopyAll().then((all) => {
        result = all;
        return new Blob([all.lines.join('\n')], { type: 'text/plain' });
      });
      try {
        await clipboard.write([new ClipboardItem({ 'text/plain': blob })]);
        report(result.complete, result.lines.length);
        return;
      } catch {
        // Fall through: copy what is loaded instead.
      }
    }
    try {
      await clipboard?.writeText(lines.join('\n'));
      report(!hasMore, lines.length);
    } catch {
      setCopied('Copy failed');
    }
  };

  return (
    <div data-slot="log-block" className={cn('text-xs', className)}>
      {open ? (
        <p className="hidden font-medium text-fg-muted print:block">
          {label} ({lines.length} {lines.length === 1 ? 'line' : 'lines'})
        </p>
      ) : (
        <p data-slot="log-print-note" className="hidden text-fg-muted print:block">
          {printNote}
        </p>
      )}
      <div className="flex items-center gap-2 print:hidden">
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setOpen((value) => !value)}
          className="inline-flex items-center gap-1 rounded-sm font-medium text-fg-muted hover:text-fg focus-visible:outline-2 focus-visible:outline-focus"
        >
          {open ? (
            <ChevronDown className="size-3.5" aria-hidden="true" />
          ) : (
            <ChevronRight className="size-3.5" aria-hidden="true" />
          )}
          {open ? `Hide ${label.toLowerCase()}` : `Show ${label.toLowerCase()}`}
          {lines.length > 0 ? (
            <span className="font-mono text-2xs">
              ({lines.length}
              {hasMore ? '+' : ''} lines)
            </span>
          ) : null}
        </button>
        {open && lines.length > 0 ? (
          <button
            type="button"
            onClick={() => void copy()}
            className="ml-auto inline-flex items-center gap-1 rounded-sm px-1.5 py-0.5 text-2xs text-fg-muted hover:bg-hover hover:text-fg print:hidden"
          >
            {copied && copied !== 'Copy failed' ? (
              <Check className="size-3" aria-hidden="true" />
            ) : (
              <Copy className="size-3" aria-hidden="true" />
            )}
            {copied ?? 'Copy'}
          </button>
        ) : null}
      </div>
      {open ? (
        <div className="mt-1.5 overflow-hidden rounded-md border border-border bg-canvas">
          {loading && lines.length === 0 ? (
            <p className="px-3 py-2 font-mono text-fg-muted">Loading the log…</p>
          ) : null}
          {error ? (
            <p role="alert" className="px-3 py-2 text-fg">
              {error}
            </p>
          ) : null}
          {!loading && !error && lines.length === 0 ? (
            <p className="px-3 py-2 text-fg-muted">No log lines for this step.</p>
          ) : null}
          {lines.length > 0 ? <LogLines lines={lines} /> : null}
          {hasMore && onLoadMore ? (
            <div className="border-t border-border px-3 py-1.5">
              <button
                type="button"
                onClick={onLoadMore}
                className="text-2xs font-medium text-accent hover:underline print:hidden"
              >
                Load more
              </button>
              <p className="hidden text-2xs text-fg-muted print:block">
                Only the first {lines.length} lines were loaded; the rest are on the run page.
              </p>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** True while the browser prints (every row renders then, not only those in view). */
function usePrinting(): boolean {
  const [printing, setPrinting] = useState(false);
  useEffect(() => {
    // `beforeprint` is the last moment before the browser snapshots the
    // page: render every line synchronously, or the print has only the
    // rows that were in view (UI-3 re-check).
    const on = () => flushSync(() => setPrinting(true));
    const off = () => setPrinting(false);
    window.addEventListener('beforeprint', on);
    window.addEventListener('afterprint', off);
    return () => {
      window.removeEventListener('beforeprint', on);
      window.removeEventListener('afterprint', off);
    };
  }, []);
  return printing;
}

function LogLines({ lines }: { lines: string[] }) {
  const [scrollTop, setScrollTop] = useState(0);
  const printing = usePrinting();
  const virtual = lines.length > LOG_VIRTUALIZE_ABOVE && !printing;
  const onScroll = (event: UIEvent<HTMLDivElement>) => {
    if (virtual) setScrollTop(event.currentTarget.scrollTop);
  };
  const gutter = String(lines.length).length;
  const first = virtual ? Math.max(0, Math.floor(scrollTop / LOG_ROW_HEIGHT) - OVERSCAN) : 0;
  const last = virtual
    ? Math.min(lines.length, Math.ceil((scrollTop + VIEW_HEIGHT) / LOG_ROW_HEIGHT) + OVERSCAN)
    : lines.length;
  return (
    <div
      data-slot="log-lines"
      data-virtual={virtual || undefined}
      // A scrollable region must be reachable by keyboard (axe), as the
      // runbook page's source is.
      // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
      tabIndex={0}
      role="region"
      aria-label="Log lines"
      onScroll={onScroll}
      className="max-h-80 overflow-auto font-mono text-2xs leading-5 text-fg focus-visible:outline-2 focus-visible:outline-focus print:max-h-none"
    >
      <div
        style={
          virtual ? { height: lines.length * LOG_ROW_HEIGHT, position: 'relative' } : undefined
        }
        className="min-w-max py-1 print:min-w-0"
      >
        <div style={virtual ? { transform: `translateY(${first * LOG_ROW_HEIGHT}px)` } : undefined}>
          {lines.slice(first, last).map((line, i) => (
            <div key={first + i} data-slot="log-line" className="flex h-5 px-3 hover:bg-hover print:h-auto">
              <span
                aria-hidden="true"
                className="mr-3 shrink-0 select-none text-right text-fg-muted"
                style={{ width: `${gutter}ch` }}
              >
                {first + i + 1}
              </span>
              <span className="whitespace-pre print:whitespace-pre-wrap print:break-all">{line}</span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
