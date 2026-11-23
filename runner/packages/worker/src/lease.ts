export const HEARTBEAT_MS = 10_000;

export async function sleep(ms: number, signal?: AbortSignal) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const wait = () => new Promise<void>((resolve) => { timer = setTimeout(resolve, ms); });
  try {
    await (signal ? whileActive(signal, wait) : wait());
  } finally {
    clearTimeout(timer);
  }
}

export function superviseLease(intervalMs: number, expiresAt: () => number, shutdown: AbortSignal,
  refresh: (signal: AbortSignal) => Promise<void>, stale: (error: unknown) => boolean, lost: () => void) {
  const stop = new AbortController();
  const signal = AbortSignal.any([stop.signal, shutdown]);
  const margin = Math.min(1000, intervalMs / 2);
  const remaining = () => expiresAt() - Date.now() - margin;
  const loop = (async () => {
    while (!signal.aborted) {
      const wait = remaining();
      if (wait <= 0) { lost(); return; }
      await sleep(Math.min(intervalMs, wait), signal);
      if (remaining() <= 0) { lost(); return; }
      const request = new AbortController();
      const tick = AbortSignal.any([signal, request.signal]);
      const timeout = setTimeout(() => request.abort(), Math.min(intervalMs, remaining()));
      try {
        await whileActive(tick, () => refresh(tick));
      } catch (error) {
        if (!signal.aborted && (stale(error) || remaining() <= 0)) { lost(); return; }
      } finally {
        clearTimeout(timeout);
      }
    }
  })().catch(() => undefined);
  return async () => {
    stop.abort();
    await loop;
  };
}

export async function whileActive<T>(signal: AbortSignal, work: () => Promise<T>): Promise<T> {
  signal.throwIfAborted();
  let cancel: (() => void) | undefined;
  try {
    return await Promise.race([new Promise<never>((_, reject) => {
      cancel = () => reject(signal.reason);
      signal.addEventListener('abort', cancel, { once: true });
    }), work()]);
  } finally {
    if (cancel) signal.removeEventListener('abort', cancel);
  }
}
