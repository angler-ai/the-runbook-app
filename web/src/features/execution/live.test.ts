import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createApiRuntime } from '../../api/client';
import { server } from '../../test/msw/server';
import { CANCELLED_RUN_STREAM } from './fixtures/server-events';
import { runLiveLoop, TERMINAL_EVENT, type LiveLoopDeps, type LiveStatus } from './live';

/** The captured stream, one SSE block (frame or keep-alive comment) per entry. */
const BLOCKS = CANCELLED_RUN_STREAM.split(/(?<=\n\n)/);
const frameWithId = (id: number) => {
  const block = BLOCKS.find((b) => b.startsWith(`id: ${id}\n`));
  if (!block) throw new Error(`no captured frame ${id}`);
  return block;
};
const TERMINAL_FRAME = BLOCKS.find((b) => b.includes(`\nevent: ${TERMINAL_EVENT}\n`)) as string;

const KEEP_ALIVE = BLOCKS.find((b) => b.startsWith(':')) as string;

const SSE = { headers: { 'Content-Type': 'text/event-stream' } };
const sse = (blocks: string[]) => HttpResponse.text(blocks.join(''), SSE);
const encoder = new TextEncoder();
/**
 * A stream that sends `blocks` and then stays open: silent, or with a
 * keep-alive every `keepAliveMs`, until the client aborts the request.
 */
function openStream(blocks: string[] = [], keepAliveMs?: number, request?: Request) {
  let timer: ReturnType<typeof setInterval> | undefined;
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      // Like a socket the client dropped: error the body when the request aborts.
      request?.signal.addEventListener('abort', () => {
        clearInterval(timer);
        controller.error(new Error('client aborted'));
      });
      for (const block of blocks) controller.enqueue(encoder.encode(block));
      if (keepAliveMs) {
        timer = setInterval(() => {
          try {
            controller.enqueue(encoder.encode(KEEP_ALIVE));
          } catch {
            clearInterval(timer);
          }
        }, keepAliveMs);
      }
    },
    cancel() {
      clearInterval(timer);
    },
  });
  return new HttpResponse(body, SSE);
}
const problem = (code: string, status: number) =>
  HttpResponse.json(
    { code, status, detail: 'x', type: 't', title: 't', instance: 'urn:request:r', errors: [] },
    { status, headers: { 'Content-Type': 'application/problem+json' } },
  );

const running: AbortController[] = [];
afterEach(() => {
  for (const controller of running.splice(0)) controller.abort();
});

function setup(handler: Parameters<typeof http.get>[1], overrides: Partial<LiveLoopDeps> = {}) {
  server.use(http.get('*/v1/executions/:exe/events', handler));
  const runtime = createApiRuntime({ getToken: () => 'tok', onUnauthorized: null });
  const onRefetch = vi.fn();
  const statuses: LiveStatus[] = [];
  const controller = new AbortController();
  running.push(controller);
  const done = runLiveLoop({
    client: runtime.client,
    executionId: 'exe_1',
    onRefetch,
    onStatus: (s) => statuses.push(s),
    signal: controller.signal,
    debounceMs: 5,
    pollMs: 15,
    backoffBaseMs: 1,
    ...overrides,
  });
  return { onRefetch, statuses, controller, done };
}

/** Generous: these run beside the page tests and MSW's abort path is slow under load. */
const WAIT = { timeout: 3000 };
const tick = (ms = 30) => new Promise((r) => setTimeout(r, ms));
/** Resolves with the loop's end, or 'running' if it is still going after `ms`. */
const settled = (done: Promise<void>, ms = 300) =>
  Promise.race([done.then(() => 'ended' as const), tick(ms).then(() => 'running' as const)]);

describe('runLiveLoop against frames captured from runbook-server (U7)', () => {
  it('the captured stream ends with the engine name ExecutionCompleted', () => {
    expect(TERMINAL_EVENT).toBe('ExecutionCompleted');
    expect(TERMINAL_FRAME).toMatch(/^id: 15\nevent: ExecutionCompleted\ndata: \{/);
    // Every frame's event: field is an engine variant name, never a dotted name.
    const names = BLOCKS.flatMap((b) => /\nevent: (.+)\n/.exec(b)?.[1] ?? []);
    expect(names.length).toBe(15);
    for (const name of names) expect(name).toMatch(/^[A-Z][A-Za-z]+$/);
  });

  it('stops at ExecutionCompleted with a final refetch', async () => {
    let opens = 0;
    const { onRefetch, statuses, done } = setup(() => {
      opens += 1;
      return sse(BLOCKS);
    });
    expect(await settled(done)).toBe('ended');
    expect(onRefetch).toHaveBeenCalled();
    expect(statuses.at(-1)).toBe('closed');
    expect(opens).toBe(1);
  });

  it('collapses a burst of events into one refetch', async () => {
    // Eight events on a stream that stays open: no terminal flush, no reconnect.
    const burst = [1, 2, 3, 4, 5, 6, 7, 8].map(frameWithId);
    const { onRefetch, statuses } = setup(({ request }) => openStream(burst, undefined, request), {
      debounceMs: 50,
    });
    await vi.waitFor(() => expect(onRefetch).toHaveBeenCalledTimes(1), WAIT);
    await tick(150);
    expect(onRefetch).toHaveBeenCalledTimes(1);
    expect(statuses).toEqual(['connecting', 'live']);
  });

  it('refetches on events while the tab is hidden', async () => {
    // A person waiting on an approval in another tab still gets the update.
    Object.defineProperty(document, 'hidden', { value: true, configurable: true });
    Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true });
    try {
      const { onRefetch, statuses } = setup(({ request }) =>
        openStream([frameWithId(1), frameWithId(2)], undefined, request),
      );
      await vi.waitFor(() => expect(onRefetch).toHaveBeenCalledTimes(1), WAIT);
      // The stream is still open: the refetch came from the events, not a
      // terminal flush, a reconnect or polling.
      expect(statuses).toEqual(['connecting', 'live']);
    } finally {
      Reflect.deleteProperty(document, 'hidden');
      Reflect.deleteProperty(document, 'visibilityState');
    }
  });

  it('reconnects with Last-Event-ID after the stream drops', async () => {
    const seen: (string | null)[] = [];
    const { controller, done } = setup(({ request }) => {
      seen.push(request.headers.get('last-event-id'));
      return seen.length === 1
        ? sse([frameWithId(1), frameWithId(2), frameWithId(7)])
        : sse([TERMINAL_FRAME]);
    });
    expect(await settled(done)).toBe('ended');
    controller.abort();
    expect(seen).toEqual([null, '7']);
  });

  it('refetches once and stops on 410', async () => {
    const { onRefetch, statuses, done } = setup(() => problem('execution.history_expired', 410));
    await done;
    expect(onRefetch).toHaveBeenCalledTimes(1);
    expect(statuses.at(-1)).toBe('expired');
  });

  it('stops as expired on the server’s in-stream error frame for purged history', async () => {
    // event_stream.rs writes `event: error` with the load failure's code, then closes.
    const { statuses, done } = setup(() =>
      sse([frameWithId(1), 'event: error\ndata: {"code":"execution.history_expired"}\n\n']),
    );
    expect(await settled(done)).toBe('ended');
    expect(statuses.at(-1)).toBe('expired');
  });
});

describe('runLiveLoop reconnects instead of degrading (U6)', () => {
  it('treats the server’s planned close (one-hour lifetime) as a reconnect, not a failure', async () => {
    // Three lifetimes end with only keep-alives (a run waiting hours on an
    // approval); each reopen carries Last-Event-ID; the fourth stays open.
    const seen: (string | null)[] = [];
    const { statuses } = setup(({ request }) => {
      seen.push(request.headers.get('last-event-id'));
      if (seen.length === 1) return sse([frameWithId(8), KEEP_ALIVE]);
      if (seen.length <= 3) return sse([KEEP_ALIVE]);
      return openStream();
    });
    await vi.waitFor(() => expect(seen.length).toBe(4), WAIT);
    await tick(60);
    expect(seen).toEqual([null, '8', '8', '8']);
    expect(statuses).not.toContain('polling');
    expect(statuses.at(-1)).toBe('live');
  });

  it('resets the failure count on every successful open', async () => {
    // Two failures, a planned close, two failures, then a stream that stays:
    // never three failures in a row, so never polling.
    let opens = 0;
    const { statuses } = setup(() => {
      opens += 1;
      if (opens === 3) return sse([KEEP_ALIVE]);
      if (opens === 6) return openStream([], 10);
      return problem('internal.error', 500);
    });
    await vi.waitFor(() => expect(opens).toBe(6), WAIT);
    // Each failure says Reconnecting… until the sixth stream opens.
    await vi.waitFor(() => expect(statuses.at(-1)).toBe('live'), WAIT);
    expect(statuses).not.toContain('polling');
    expect(statuses).toContain('reconnecting');
  });

  it('counts a stream the server closes with no bytes at all as a failure (no hot loop)', async () => {
    let opens = 0;
    const { statuses, controller } = setup(
      () => {
        opens += 1;
        return sse([]);
      },
      { streamRetryMs: 10_000 },
    );
    await vi.waitFor(() => expect(statuses).toContain('polling'), WAIT);
    const atPolling = opens;
    await tick(80);
    expect(atPolling).toBe(3);
    expect(opens).toBe(3);
    controller.abort();
  });

  it('reconnects with Last-Event-ID when nothing (not even a keep-alive) arrives within the stall window', async () => {
    const seen: (string | null)[] = [];
    const { statuses } = setup(
      ({ request }) => {
        seen.push(request.headers.get('last-event-id'));
        // The socket died after frame 2 (laptop sleep, NAT drop): no close, no bytes.
        return seen.length === 1
          ? openStream([frameWithId(2)], undefined, request)
          : openStream([], 10, request);
      },
      { stallMs: 50 },
    );
    await vi.waitFor(() => expect(seen.length).toBe(2), WAIT);
    expect(seen).toEqual([null, '2']);
    // Keep-alives every 10 ms keep the second stream alive past the window.
    await tick(200);
    expect(seen.length).toBe(2);
    // Between the stall and the new stream the indicator says so (not Live).
    expect(statuses).toEqual(['connecting', 'live', 'reconnecting', 'live']);
  });

  it('reconnects when the connect itself never answers', async () => {
    let opens = 0;
    const { statuses } = setup(
      () => {
        opens += 1;
        return opens === 1 ? new Promise<never>(() => {}) : openStream([], 10);
      },
      { stallMs: 50 },
    );
    await vi.waitFor(() => expect(opens).toBe(2), WAIT);
    await vi.waitFor(() => expect(statuses.at(-1)).toBe('live'), WAIT);
  });

  it('leaves polling on 429 stream.too_many and retries the stream after a backoff', async () => {
    let opens = 0;
    const { onRefetch, statuses } = setup(
      () => {
        opens += 1;
        return opens === 1 ? problem('stream.too_many', 429) : openStream([], 10);
      },
      { streamRetryMs: 60 },
    );
    await vi.waitFor(() => expect(statuses).toContain('polling'), WAIT);
    await vi.waitFor(() => expect(onRefetch.mock.calls.length).toBeGreaterThanOrEqual(2), WAIT);
    await vi.waitFor(() => expect(statuses.at(-1)).toBe('live'), WAIT);
    expect(opens).toBe(2);
  });

  it('polls on 503 and keeps retrying the stream until it opens', async () => {
    let opens = 0;
    const { statuses } = setup(
      () => {
        opens += 1;
        return opens <= 2 ? problem('stream.capacity', 503) : openStream([], 10);
      },
      { streamRetryMs: 40 },
    );
    await vi.waitFor(() => expect(statuses.at(-1)).toBe('live'), WAIT);
    expect(statuses.filter((s) => s === 'polling').length).toBe(2);
    expect(opens).toBe(3);
  });

  it('falls back to polling only after three failures in a row, then tries the stream again', async () => {
    let opens = 0;
    let opensAtPolling: number | null = null;
    const seen: LiveStatus[] = [];
    setup(
      () => {
        opens += 1;
        return opens <= 3 ? problem('internal.error', 500) : openStream([], 10);
      },
      {
        streamRetryMs: 40,
        onStatus: (status) => {
          seen.push(status);
          if (status === 'polling') opensAtPolling ??= opens;
        },
      },
    );
    await vi.waitFor(() => expect(seen.at(-1)).toBe('live'), WAIT);
    // Two failures back off and retry; the third switches to polling.
    expect(opensAtPolling).toBe(3);
    expect(seen).toEqual(['connecting', 'reconnecting', 'reconnecting', 'polling', 'live']);
    expect(opens).toBe(4);
  });
});
