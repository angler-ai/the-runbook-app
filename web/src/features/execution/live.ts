// Live execution updates (A6): one SSE stream per tab over fetch (the
// bearer header rules out EventSource), debounced document refetch,
// reconnect with backoff and Last-Event-ID, terminal stop, refetch on
// 410. The server closes every stream after its lifetime (an hour) on
// purpose: that is a reconnect, not a failure. Polling covers 429/503
// (the server caps live streams per credential) and three failures in a
// row, and the stream is retried from there after a backoff. A stall
// watchdog reconnects when no byte (event or keep-alive, sent about every
// 30 s while idle) arrives within the window.
import { useEffect, useRef, useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { ApiError, api, type ApiClient } from '../../api/client';
import { queryKeys } from '../../api/query-keys';
import { readEventStream } from '../../lib/sse';
import { invalidateInbox } from '../inbox/hooks';

/**
 * `reconnecting`: the stream failed or stalled and the loop is about to try
 * again (before it falls back to polling): updates are not arriving.
 */
export type LiveStatus = 'connecting' | 'live' | 'reconnecting' | 'polling' | 'closed' | 'expired';

/**
 * The stream's `event:` field is the engine's variant name (`Event::name`
 * in crates/runbook-engine/src/event.rs, framed by event_stream.rs), so
 * completion arrives as `ExecutionCompleted`; the server closes the stream
 * right after it.
 */
export const TERMINAL_EVENT = 'ExecutionCompleted';
/** event_stream.rs reports a failed reload as `event: error` with `{"code": …}`, then closes. */
const ERROR_EVENT = 'error';

function errorCode(data: string): string | null {
  try {
    const parsed: unknown = JSON.parse(data);
    if (parsed && typeof parsed === 'object' && 'code' in parsed) {
      const code = (parsed as { code: unknown }).code;
      return typeof code === 'string' ? code : null;
    }
  } catch {
    // Not JSON: treat as an unnamed stream failure.
  }
  return null;
}

export interface LiveLoopDeps {
  client: ApiClient;
  executionId: string;
  onRefetch(): void;
  onStatus?(status: LiveStatus): void;
  signal: AbortSignal;
  debounceMs?: number;
  pollMs?: number;
  backoffBaseMs?: number;
  /** How long to poll before trying the stream again (default 60 s). */
  streamRetryMs?: number;
  /** Reconnect when no byte arrives for this long (default 75 s: keep-alives come every ~30 s). */
  stallMs?: number;
  /** Resume after this event id (a remount of the same execution). */
  lastEventId?: string;
  onEventId?(id: string): void;
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(new Error('aborted'));
      return;
    }
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(new Error('aborted'));
    };
    signal.addEventListener('abort', onAbort, { once: true });
  });
}

type Outcome =
  /** The server ended the stream after sending something: reconnect at once. */
  | 'planned-close'
  | 'terminal'
  | 'expired'
  /** 429/503: the server is full; poll for a while. */
  | 'busy'
  /** Refused, dropped, closed with nothing sent, or stalled. */
  | 'failed'
  | 'aborted';

const MAX_FAILURES = 3;

export async function runLiveLoop(deps: LiveLoopDeps): Promise<void> {
  const debounceMs = deps.debounceMs ?? 250;
  const pollMs = deps.pollMs ?? 10_000;
  const backoffBaseMs = deps.backoffBaseMs ?? 500;
  const streamRetryMs = deps.streamRetryMs ?? 60_000;
  const stallMs = deps.stallMs ?? 75_000;
  const { signal } = deps;
  let lastEventId = deps.lastEventId;
  let failures = 0;
  let debounce: ReturnType<typeof setTimeout> | null = null;

  const scheduleRefetch = () => {
    if (debounce) return;
    debounce = setTimeout(() => {
      debounce = null;
      deps.onRefetch();
    }, debounceMs);
  };
  const flushRefetch = () => {
    if (debounce) {
      clearTimeout(debounce);
      debounce = null;
    }
    deps.onRefetch();
  };

  /** Poll for `streamRetryMs`; false when aborted. */
  const pollForAWhile = async (): Promise<boolean> => {
    deps.onStatus?.('polling');
    const until = Date.now() + streamRetryMs;
    while (!signal.aborted) {
      deps.onRefetch();
      const left = until - Date.now();
      if (left <= 0) return true;
      try {
        await sleep(Math.min(pollMs, left), signal);
      } catch {
        return false;
      }
      if (Date.now() >= until) return true;
    }
    return false;
  };

  /** One stream: open, read until it ends, and say how it ended. */
  const streamOnce = async (): Promise<Outcome> => {
    // Its own controller, so the watchdog can drop this connection only.
    const connection = new AbortController();
    const forwardAbort = () => connection.abort();
    signal.addEventListener('abort', forwardAbort, { once: true });
    let stalled = false;
    let watchdog: ReturnType<typeof setTimeout> | undefined;
    let stream: TransformStreamDefaultController<Uint8Array> | null = null;
    const touch = () => {
      clearTimeout(watchdog);
      watchdog = setTimeout(() => {
        stalled = true;
        // With a body, erroring the pipe cancels the response body (and its
        // connection); before headers, abort the request itself.
        if (stream) stream.error(new Error('stream.stalled'));
        else connection.abort();
      }, stallMs);
    };
    let receivedBytes = false;
    try {
      touch(); // a connect that never answers is a stall too
      const { data } = await deps.client.GET('/v1/executions/{exe}/events', {
        params: {
          path: { exe: deps.executionId },
          header: lastEventId ? { 'Last-Event-ID': lastEventId } : {},
        },
        parseAs: 'stream',
        signal: connection.signal,
      });
      const body = data as unknown as ReadableStream<Uint8Array> | null;
      if (!body) return 'failed';
      failures = 0;
      deps.onStatus?.('live');
      touch();
      // Every chunk, keep-alive comments included, feeds the watchdog.
      const watched = body.pipeThrough(
        new TransformStream<Uint8Array, Uint8Array>({
          start(controller) {
            stream = controller;
          },
          transform(chunk, controller) {
            if (chunk.byteLength > 0) {
              receivedBytes = true;
              touch();
            }
            controller.enqueue(chunk);
          },
        }),
      );
      for await (const event of readEventStream(watched, connection.signal)) {
        if (event.id) {
          lastEventId = event.id;
          deps.onEventId?.(event.id);
        }
        if (event.event === TERMINAL_EVENT) return 'terminal';
        if (event.event === ERROR_EVENT) {
          if (errorCode(event.data) === 'execution.history_expired') return 'expired';
          continue; // the server closes right after; that close is a failure below
        }
        // Events matter with the tab out of focus too; never gate on visibility.
        scheduleRefetch();
      }
      // A clean end. After real traffic it is the planned lifetime close
      // (or the run just ended: the refetch shows it). A stream closed
      // with nothing at all is treated as a failure, so it cannot spin.
      return receivedBytes ? 'planned-close' : 'failed';
    } catch (error) {
      if (signal.aborted) return 'aborted';
      if (stalled) return 'failed';
      if (error instanceof ApiError) {
        if (error.status === 410) return 'expired';
        if (error.status === 429 || error.status === 503 || error.code === 'stream.too_many') {
          return 'busy';
        }
      }
      return 'failed';
    } finally {
      clearTimeout(watchdog);
      signal.removeEventListener('abort', forwardAbort);
    }
  };

  deps.onStatus?.('connecting');
  while (!signal.aborted) {
    const outcome = await streamOnce();
    switch (outcome) {
      case 'aborted':
        return;
      case 'terminal':
        flushRefetch();
        deps.onStatus?.('closed');
        return;
      case 'expired':
        flushRefetch();
        deps.onStatus?.('expired');
        return;
      case 'planned-close':
        // Anything missed between streams is replayed after Last-Event-ID;
        // one refetch also catches a run that ended as the stream closed.
        scheduleRefetch();
        break;
      case 'busy':
        if (!(await pollForAWhile())) return;
        continue;
      case 'failed':
        failures += 1;
        if (failures < MAX_FAILURES) deps.onStatus?.('reconnecting');
        if (failures >= MAX_FAILURES) {
          if (!(await pollForAWhile())) return;
          // One more refusal goes straight back to polling.
          failures = MAX_FAILURES - 1;
          continue;
        }
        break;
    }
    if (signal.aborted) return;
    try {
      await sleep(Math.min(backoffBaseMs * 2 ** failures, 5_000), signal);
    } catch {
      return;
    }
  }
}

// Exactly one stream per tab: starting a new one aborts the previous.
let activeStream: AbortController | null = null;

/**
 * The last event id this tab saw per execution, so opening the page again
 * resumes after it instead of replaying the whole history (U18). The very
 * first open still replays: the document does not carry its revision.
 */
const lastSeen = new Map<string, string>();

/** Forget every resume point (tests; a sign-out may call it too). */
export function forgetLiveResumePoints() {
  lastSeen.clear();
}

export function useLiveExecution(executionId: string, enabled: boolean): LiveStatus {
  const queryClient = useQueryClient();
  const [status, setStatus] = useState<LiveStatus>('connecting');
  const statusRef = useRef<LiveStatus>('connecting');

  useEffect(() => {
    if (!enabled) return undefined;
    activeStream?.abort();
    const controller = new AbortController();
    activeStream = controller;
    void runLiveLoop({
      client: api,
      executionId,
      signal: controller.signal,
      lastEventId: lastSeen.get(executionId),
      onEventId: (id) => lastSeen.set(executionId, id),
      onRefetch: () => {
        void queryClient.invalidateQueries({ queryKey: queryKeys.executionDocument(executionId) });
        void queryClient.invalidateQueries({ queryKey: queryKeys.execution(executionId) });
        // A change in the run on screen may open or close one of this
        // person's tasks (an approval for them, say): refresh the count (#10).
        invalidateInbox(queryClient);
      },
      onStatus: (next) => {
        statusRef.current = next;
        setStatus(next);
      },
    });
    return () => {
      controller.abort();
      if (activeStream === controller) activeStream = null;
    };
  }, [enabled, executionId, queryClient]);

  return status;
}
