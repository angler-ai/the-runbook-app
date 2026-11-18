// The page side of the compiler worker (client.ts): the per-call deadline
// (U29). A compile that stalls the worker is abandoned, the worker is
// terminated and replaced, and queued calls run on the new one.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createCompilerWorker } from './client';
import type { WorkerRequest, WorkerResponse } from './protocol';
import { CompilerError } from './runbook';

class FakeWorker {
  onmessage: ((event: MessageEvent<WorkerResponse>) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  posted: WorkerRequest[] = [];
  terminated = false;
  postMessage(message: WorkerRequest) {
    this.posted.push(message);
  }
  terminate() {
    this.terminated = true;
  }
  answer(id: number) {
    const request = this.posted.find((m) => m.id === id);
    if (!request) throw new Error(`no request ${id}`);
    this.onmessage?.({
      data: { id, ok: true, result: { op: request.op } },
    } as unknown as MessageEvent<WorkerResponse>);
  }
}

describe('compiler worker deadline (U29)', () => {
  let workers: FakeWorker[];
  const spawn = () => {
    const worker = new FakeWorker();
    workers.push(worker);
    return worker as unknown as Worker;
  };
  beforeEach(() => {
    workers = [];
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('replaces a stalled worker and reruns the queued calls on the new one', async () => {
    const client = createCompilerWorker(spawn, { timeoutMs: 1000 });
    const stalled = client.validate({ source: 'slow' });
    const queued = client.outline({ source: 'next' });
    expect(workers).toHaveLength(1);
    const first = workers[0]!;
    expect(first.posted.map((m) => m.op)).toEqual(['validate', 'outline']);

    const refused = expect(stalled).rejects.toMatchObject({ code: 'wasm.timeout' });
    await vi.advanceTimersByTimeAsync(1000);
    await refused;
    expect(first.terminated).toBe(true);
    expect(workers).toHaveLength(2);
    const second = workers[1]!;
    // Only the call still waiting moves; the stalled one is not retried.
    expect(second.posted.map((m) => m.op)).toEqual(['outline']);
    second.answer(second.posted[0]!.id);
    await expect(queued).resolves.toEqual({ op: 'outline' });
    // A late answer from the terminated worker is ignored.
    first.answer(first.posted[1]!.id);
  });

  it('times each call from when it reaches the head of the queue', async () => {
    const client = createCompilerWorker(spawn, { timeoutMs: 1000 });
    const a = client.validate({ source: 'a' });
    const b = client.outline({ source: 'b' });
    const worker = workers[0]!;
    await vi.advanceTimersByTimeAsync(900);
    worker.answer(worker.posted[0]!.id);
    await expect(a).resolves.toEqual({ op: 'validate' });
    // b has waited 900 ms in the queue but only now started its own deadline.
    await vi.advanceTimersByTimeAsync(900);
    worker.answer(worker.posted[1]!.id);
    await expect(b).resolves.toEqual({ op: 'outline' });
    expect(worker.terminated).toBe(false);
    expect(workers).toHaveLength(1);
  });

  it('new calls arriving behind a stalled one do not extend its deadline', async () => {
    const client = createCompilerWorker(spawn, { timeoutMs: 1000 });
    const stalled = client.validate({ source: 'slow' });
    const refused = expect(stalled).rejects.toMatchObject({ code: 'wasm.timeout' });
    const later: Promise<unknown>[] = [];
    for (let i = 0; i < 3; i++) {
      await vi.advanceTimersByTimeAsync(400);
      later.push(client.outline({ source: `edit ${i}` }));
    }
    // 1200 ms after the stalled call started, edits every 400 ms.
    await refused;
    expect(workers).toHaveLength(2);
    expect(workers[1]!.posted).toHaveLength(3);
    for (const m of workers[1]!.posted) workers[1]!.answer(m.id);
    await expect(Promise.all(later)).resolves.toHaveLength(3);
  });

  it('rejects calls after terminate instead of leaving them pending', async () => {
    const client = createCompilerWorker(spawn, { timeoutMs: 1000 });
    const pending = client.describe({ source: 'x' });
    client.terminate();
    await expect(pending).rejects.toBeInstanceOf(CompilerError);
    await expect(client.describe({ source: 'y' })).rejects.toMatchObject({
      code: 'wasm.terminated',
    });
  });
});
