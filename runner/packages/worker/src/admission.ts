import { retryDelay } from '@runbook/runner-shared/retry';
import type { AdmissionResponse } from '@runbook/runner-shared/contracts/internal';
import type { Admission } from './claim.ts';

export function admissionLoop(capacity: number, admit: (free: number) => Promise<AdmissionResponse>,
  run: (item: Admission, shutdown: AbortSignal) => Promise<boolean>, failure: () => void) {
  const tasks = new Set<Promise<void>>();
  let pending: Promise<void> | undefined;
  let requested = false;
  let failures = 0;
  let retry: NodeJS.Timeout | undefined;
  const shutdown = new AbortController();

  function wake(): void {
    if (shutdown.signal.aborted) return;
    requested = true;
    if (pending || retry) return;
    pending = drain().finally(() => { pending = undefined; if (requested) wake(); });
  }

  async function drain() {
    while (requested) {
      requested = false;
      const free = capacity - tasks.size;
      if (free <= 0 || shutdown.signal.aborted) return;
      try {
        const { items } = await admit(free);
        failures = 0;
        for (const item of items) {
          const task = run(item, shutdown.signal).then((finished) => {
            tasks.delete(task);
            if (finished) wake();
          }, () => { tasks.delete(task); failure(); });
          tasks.add(task);
        }
      } catch {
        failure();
        if (!shutdown.signal.aborted) {
          retry = setTimeout(() => {
            retry = undefined;
            wake();
          }, retryDelay(failures++, 2_000, 30_000)).unref();
        }
        return;
      }
    }
  }

  const fallback = setInterval(wake, 30_000).unref();
  wake();
  return {
    wake,
    async close() {
      clearInterval(fallback);
      clearTimeout(retry);
      shutdown.abort();
      await pending;
      await Promise.allSettled(tasks);
    },
  };
}
