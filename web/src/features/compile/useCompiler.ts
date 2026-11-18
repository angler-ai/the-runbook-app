// useCompiler (M4, A7): validate, outline and describe for a source, run in
// the shared compiler worker 150 ms after the last edit. Every result is
// provisional (F99): the diagnostics a draft save or publish returns are the
// server's and replace these.
import { useEffect, useRef, useState } from 'react';
import { createCompilerWorker, type CompilerWorker } from '../../wasm/client';
import type {
  CompileDirectory,
  DescribeResult,
  OutlineResult,
  ValidateResult,
} from '../../wasm/runbook';

export type CompilerBackend = Pick<CompilerWorker, 'validate' | 'outline' | 'describe'>;

export interface CompilerState {
  /** The source these results were computed from (null before the first run). */
  source: string | null;
  validate: ValidateResult | null;
  outline: OutlineResult | null;
  describe: DescribeResult | null;
  /** The compiler could not run (module missing, worker failed, source too large). */
  error: string | null;
  /** An edit is waiting for (or in) its compile. */
  pending: boolean;
}

const INITIAL: CompilerState = {
  source: null,
  validate: null,
  outline: null,
  describe: null,
  error: null,
  pending: false,
};

let sharedWorker: CompilerWorker | null = null;
function defaultBackend(): CompilerBackend {
  if (typeof Worker === 'undefined') throw new Error('The browser has no Web Workers.');
  sharedWorker ??= createCompilerWorker();
  return sharedWorker;
}

export interface UseCompilerOptions {
  /** The directory to validate against (buildCompileContext); without it, validate is skipped. */
  context?: CompileDirectory;
  /** Default 150 ms (A7). */
  debounceMs?: number;
  enabled?: boolean;
  /** Test seam: anything with the worker's promise API (for example a wrapped RunbookCompiler). */
  backend?: CompilerBackend;
}

export function useCompiler(source: string, options: UseCompilerOptions = {}): CompilerState {
  const { context, debounceMs = 150, enabled = true, backend } = options;
  const [state, setState] = useState<CompilerState>(INITIAL);
  const latest = useRef(0);

  useEffect(() => {
    if (!enabled) return undefined;
    let compiler: CompilerBackend;
    try {
      compiler = backend ?? defaultBackend();
    } catch (error) {
      setState((prev) => ({ ...prev, error: messageOf(error), pending: false }));
      return undefined;
    }
    const run = ++latest.current;
    setState((prev) => (prev.pending ? prev : { ...prev, pending: true }));
    const timer = setTimeout(() => {
      // outline and describe read only the source; only validate needs the directory.
      const bare = { source };
      Promise.all([
        context ? compiler.validate({ source, context }) : Promise.resolve(null),
        compiler.outline(bare),
        compiler.describe(bare),
      ])
        .then(([validate, outline, describe]) => {
          if (latest.current !== run) return;
          setState({ source, validate, outline, describe, error: null, pending: false });
        })
        .catch((error: unknown) => {
          if (latest.current !== run) return;
          setState((prev) => ({ ...prev, error: messageOf(error), pending: false }));
        });
    }, debounceMs);
    return () => clearTimeout(timer);
  }, [source, context, debounceMs, enabled, backend]);

  return state;
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : 'The browser compiler failed.';
}
