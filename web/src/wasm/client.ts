// The page side of the compiler worker: typed, promise-based validate,
// outline and describe. One worker serves every request in order; callers
// debounce edits (A7: 150 ms). Results are provisional (F99).
import {
  CompilerError,
  type CompileRequest,
  type CompilerOperation,
  type CompilerResults,
  type DescribeResult,
  type OutlineResult,
  type ValidateResult,
} from './runbook';
import type { WorkerRequest, WorkerResponse } from './protocol';

export interface CompilerWorker {
  validate(request: CompileRequest): Promise<ValidateResult>;
  outline(request: CompileRequest): Promise<OutlineResult>;
  describe(request: CompileRequest): Promise<DescribeResult>;
  /** Stop the worker; pending and later calls reject with `wasm.terminated`. */
  terminate(): void;
}

export interface CompilerWorkerOptions {
  /**
   * How long one call may run once it reaches the head of the queue (the
   * worker answers in order). Past it the call rejects with `wasm.timeout`,
   * the worker is terminated, and queued calls rerun on a fresh one (U29).
   * Default 10 s; the first call includes fetching and compiling the module.
   */
  timeoutMs?: number;
}

export const DEFAULT_TIMEOUT_MS = 10_000;

type Pending = {
  message: WorkerRequest;
  resolve(value: unknown): void;
  reject(error: unknown): void;
};

const spawnWorker = () => new Worker(new URL('./worker.ts', import.meta.url), { type: 'module' });

export function createCompilerWorker(
  spawn: () => Worker = spawnWorker,
  options: CompilerWorkerOptions = {},
): CompilerWorker {
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  let next = 0;
  let terminated = false;
  let worker: Worker | null = null;
  let deadline: ReturnType<typeof setTimeout> | null = null;
  // In posting order, which is the worker's answering order.
  const pending = new Map<number, Pending>();

  const failAll = (error: CompilerError) => {
    for (const p of pending.values()) p.reject(error);
    pending.clear();
    disarm();
  };
  const disarm = () => {
    if (deadline !== null) clearTimeout(deadline);
    deadline = null;
  };
  /** Start the deadline of the call now at the head of the queue, if any. */
  const arm = () => {
    disarm();
    if (pending.size > 0) deadline = setTimeout(expire, timeoutMs);
  };
  const expire = () => {
    deadline = null;
    const head = pending.entries().next();
    if (head.done) return;
    const [id, stalled] = head.value;
    pending.delete(id);
    worker?.terminate();
    worker = null;
    stalled.reject(new CompilerError('wasm.timeout', `the compiler took over ${timeoutMs} ms`));
    // The rest never started: rerun them, in order, on a fresh worker.
    if (pending.size > 0) {
      const live = current();
      for (const p of pending.values()) live.postMessage(p.message);
      arm();
    }
  };
  const current = (): Worker => {
    if (worker) return worker;
    const w = spawn();
    w.onmessage = (event: MessageEvent<WorkerResponse>) => {
      if (w !== worker) return; // a late answer from a replaced worker
      const response = event.data;
      const p = pending.get(response.id);
      if (!p) return;
      pending.delete(response.id);
      arm();
      if (response.ok) p.resolve(response.result);
      else p.reject(new CompilerError(response.error.code, response.error.message));
    };
    w.onerror = (event: ErrorEvent) => {
      if (w !== worker) return;
      failAll(new CompilerError('wasm.worker_error', event.message || 'compiler worker failed'));
    };
    worker = w;
    return w;
  };
  const call = <Op extends CompilerOperation>(
    op: Op,
    request: CompileRequest,
  ): Promise<CompilerResults[Op]> =>
    new Promise((resolve, reject) => {
      if (terminated) {
        reject(new CompilerError('wasm.terminated', 'compiler worker terminated'));
        return;
      }
      const id = next++;
      const message: WorkerRequest = { id, op, request };
      pending.set(id, { message, resolve: resolve as (value: unknown) => void, reject });
      current().postMessage(message);
      if (pending.size === 1) arm();
    });
  return {
    validate: (request) => call('validate', request),
    outline: (request) => call('outline', request),
    describe: (request) => call('describe', request),
    terminate: () => {
      terminated = true;
      worker?.terminate();
      worker = null;
      failAll(new CompilerError('wasm.terminated', 'compiler worker terminated'));
    },
  };
}
