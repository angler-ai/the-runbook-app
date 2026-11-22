import type { AdmissionResponse } from '@runbook/runner-shared/contracts/internal';
import type { Admission } from './claim.ts';

export function admissionLoop(capacity: number, admit: (free: number) => Promise<AdmissionResponse>,
  run: (item: Admission, shutdown: AbortSignal) => Promise<boolean>, failure: () => void) {
  const tasks = new Set<Promise<void>>();
  let pending: Promise<void> | undefined;
  let requested = false;
  const shutdown = new AbortController();

  function wake(): void {
    if (shutdown.signal.aborted) return;
    requested = true;
    if (pending) return;
    pending = drain().finally(() => { pending = undefined; if (requested) wake(); });
  }

  async function drain() {
    while (requested) {
      requested = false;
      const free = capacity - tasks.size;
      if (free <= 0 || shutdown.signal.aborted) return;
      try {
        const { items } = await admit(free);
        for (const item of items) {
          const task = run(item, shutdown.signal).then((finished) => {
            tasks.delete(task);
            if (finished) wake();
          }, () => { tasks.delete(task); failure(); });
          tasks.add(task);
        }
      } catch {
        failure();
      }
    }
  }

  const fallback = setInterval(wake, 30_000).unref();
  wake();
  return {
    wake,
    async close() {
      clearInterval(fallback);
      shutdown.abort();
      await pending;
      await Promise.allSettled(tasks);
    },
  };
}
