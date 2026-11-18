// The browser compiler (B3): typed validate/outline/describe over the
// runbook-web-wasm module's JSON ABI (crates/runbook-web-wasm/README.md).
// Environment-agnostic: the Web Worker (worker.ts) and Node tests both use it.
// Diagnostics are provisional; the server's are authoritative (F99).
import type { components } from '../api/schema';
import type { Outline } from '../ui/types';

export type Diagnostic = components['schemas']['Diagnostic'];
/** The `/v1/validate` body, computed in the browser. */
export type ValidateResult = components['schemas']['ValidateResponse'];

/** The directory a source compiles against (runbook_spec::authoring::CompileDirectory). */
export type CompileDirectory = {
  /** Directory users; only active users are pinnable (active defaults to true). */
  users?: { id: string; name: string; active?: boolean }[];
  teams?: { id: string; slug: string }[];
  /** The project's latest skill versions: hash is 64 hex digits. */
  skills?: { name: string; hash: string; reference?: string | null }[];
  /** The project's variable declarations, as `GET …/variables` returns them. */
  variables?: Record<string, unknown>;
};

export type CompileRequest = {
  source: string;
  /** Read by `validate` only; `outline` and `describe` need just the source. */
  context?: CompileDirectory;
};

export type OutlineResult = { outline: Outline | null; diagnostics: Diagnostic[] };

export type RunbookDescription = {
  id: string;
  title: string;
  /** JSON Schema for the start inputs. */
  inputs: unknown;
  /** JSON Schema for the execution fields. */
  fields: unknown;
};
export type DescribeResult = { runbook: RunbookDescription | null; diagnostics: Diagnostic[] };

export type CompilerOperation = 'validate' | 'outline' | 'describe';
export type CompilerResults = {
  validate: ValidateResult;
  outline: OutlineResult;
  describe: DescribeResult;
};

/** A rejected call (bad input, source over 1 MiB, invalid variables), never a diagnostic. */
export class CompilerError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = 'CompilerError';
    this.code = code;
  }
}

export interface RunbookCompiler {
  call<Op extends CompilerOperation>(op: Op, request: CompileRequest): CompilerResults[Op];
  validate(request: CompileRequest): ValidateResult;
  outline(request: CompileRequest): OutlineResult;
  describe(request: CompileRequest): DescribeResult;
}

/** The module's exports (crates/runbook-web-wasm/README.md). */
export type CompilerExports = {
  memory: WebAssembly.Memory;
  alloc(len: number): number;
  free(ptr: number, len: number): void;
  call(opPtr: number, opLen: number, inPtr: number, inLen: number): bigint;
};
type Exports = CompilerExports;

/** The server's source limit (413 spec.source_limit), in UTF-8 bytes. */
export const MAX_SOURCE_BYTES = 1024 * 1024;
/** The module's request limit (runbook_web_wasm::MAX_INPUT_BYTES). */
export const MAX_INPUT_BYTES = 8 * 1024 * 1024;

const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });

/**
 * Split `call`'s `(ptr << 32) | len`. The export returns an i64, which JS
 * receives signed: read it unsigned, or a pointer at or above 2 GiB would
 * decode negative (U30).
 */
export function unpack(packed: bigint): { ptr: number; len: number } {
  const unsigned = BigInt.asUintN(64, packed);
  return { ptr: Number(unsigned >> 32n), len: Number(unsigned & 0xffffffffn) };
}

/**
 * The request's bytes, or a CompilerError when the source or the whole
 * request is over its limit (U29). Checked before anything is copied into
 * linear memory, which never shrinks: a 20 MB request would otherwise keep
 * the worker's memory grown for good.
 */
function encodeRequest(request: CompileRequest): Uint8Array {
  const source: unknown = request.source;
  // UTF-8 is never shorter than UTF-16: a long string is over without encoding.
  if (
    typeof source === 'string' &&
    (source.length > MAX_SOURCE_BYTES || encoder.encode(source).length > MAX_SOURCE_BYTES)
  ) {
    throw new CompilerError('spec.source_limit', 'the source is larger than 1 MiB');
  }
  const input = JSON.stringify(request);
  const bytes = input.length > MAX_INPUT_BYTES ? null : encoder.encode(input);
  if (!bytes || bytes.length > MAX_INPUT_BYTES) {
    throw new CompilerError('abi.input_limit', 'the request is larger than 8 MiB');
  }
  return bytes;
}

function instantiate(module: WebAssembly.Module): Exports {
  // The module has no imports: an empty import object is the whole host.
  const exports = new WebAssembly.Instance(module, {}).exports as unknown as Exports;
  for (const name of ['alloc', 'free', 'call'] as const) {
    if (typeof exports[name] !== 'function') throw new Error(`runbook.wasm lacks ${name}`);
  }
  if (!(exports.memory instanceof WebAssembly.Memory)) throw new Error('runbook.wasm lacks memory');
  return exports;
}

function invoke(exports: Exports, op: string, inBytes: Uint8Array): string {
  const put = (bytes: Uint8Array): number => {
    const ptr = exports.alloc(bytes.length) >>> 0;
    // Read memory.buffer after every allocation: growth detaches the old buffer.
    new Uint8Array(exports.memory.buffer, ptr, bytes.length).set(bytes);
    return ptr;
  };
  const opBytes = encoder.encode(op);
  const opPtr = put(opBytes);
  const inPtr = put(inBytes);
  const packed = exports.call(opPtr, opBytes.length, inPtr, inBytes.length);
  exports.free(opPtr, opBytes.length);
  exports.free(inPtr, inBytes.length);
  const { ptr, len } = unpack(packed);
  const text = decoder.decode(new Uint8Array(exports.memory.buffer, ptr, len));
  exports.free(ptr, len);
  return text;
}

/**
 * A compiler over a compiled module. A trap (a Rust panic) poisons the
 * instance, so the next call instantiates a fresh one and the trap rethrows.
 * Requests over the size limits are refused before the module sees them.
 * `create` is a test seam: it makes the instance from the module.
 */
export function createCompiler(
  module: WebAssembly.Module,
  create: (module: WebAssembly.Module) => CompilerExports = instantiate,
): RunbookCompiler {
  let instance: Exports | null = null;
  const call = <Op extends CompilerOperation>(
    op: Op,
    request: CompileRequest,
  ): CompilerResults[Op] => {
    const inBytes = encodeRequest(request);
    instance ??= create(module);
    let text: string;
    try {
      text = invoke(instance, op, inBytes);
    } catch (error) {
      instance = null;
      throw error;
    }
    const result = JSON.parse(text) as
      { ok: CompilerResults[Op] } | { error: { code: string; message: string } };
    if ('error' in result) throw new CompilerError(result.error.code, result.error.message);
    return result.ok;
  };
  return {
    call,
    validate: (request) => call('validate', request),
    outline: (request) => call('outline', request),
    describe: (request) => call('describe', request),
  };
}

/** Compile and wrap module bytes (for example from `fetch(url)` or a file). */
export async function loadCompiler(bytes: BufferSource): Promise<RunbookCompiler> {
  return createCompiler(await WebAssembly.compile(bytes));
}
