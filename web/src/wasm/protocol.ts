// Messages between the page (client.ts) and the compiler worker (worker.ts).
import type { CompileRequest, CompilerOperation, CompilerResults } from './runbook';

export type WorkerRequest = {
  id: number;
  op: CompilerOperation;
  request: CompileRequest;
};

export type WorkerResponse =
  | { id: number; ok: true; result: CompilerResults[CompilerOperation] }
  | { id: number; ok: false; error: { code: string; message: string } };
