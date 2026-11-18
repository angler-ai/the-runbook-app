// The compiler Web Worker (B3, A7): compiles runbook.wasm once and answers
// validate/outline/describe requests off the main thread. Start it with
// `createCompilerWorker()` from client.ts. The module URL comes from Vite's
// `?url` import: build it with `npm run wasm:build`.
import wasmUrl from './runbook.wasm?url';
import { CompilerError, createCompiler, type RunbookCompiler } from './runbook';
import type { WorkerRequest, WorkerResponse } from './protocol';

type WorkerScope = {
  onmessage: ((event: MessageEvent<WorkerRequest>) => void) | null;
  postMessage(message: WorkerResponse): void;
};
const scope = self as unknown as WorkerScope;

let compiler: Promise<RunbookCompiler> | null = null;
function load(): Promise<RunbookCompiler> {
  compiler ??= (async () => {
    const response = await fetch(wasmUrl);
    if (!response.ok)
      throw new CompilerError('wasm.unavailable', `runbook.wasm: HTTP ${response.status}`);
    // compileStreaming needs application/wasm; fall back to bytes otherwise.
    const module =
      response.headers.get('content-type') === 'application/wasm'
        ? await WebAssembly.compileStreaming(response)
        : await WebAssembly.compile(await response.arrayBuffer());
    return createCompiler(module);
  })().catch((error: unknown) => {
    compiler = null; // retry on the next request
    throw error;
  });
  return compiler;
}

scope.onmessage = (event) => {
  const { id, op, request } = event.data;
  load()
    .then((c) => scope.postMessage({ id, ok: true, result: c.call(op, request) }))
    .catch((error: unknown) => {
      const code = error instanceof CompilerError ? error.code : 'wasm.failed';
      const message = error instanceof Error ? error.message : String(error);
      scope.postMessage({ id, ok: false, error: { code, message } });
    });
};
