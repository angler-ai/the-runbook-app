// The host side of the compiler ABI (runbook.ts) with a fake module, so these
// run without the built runbook.wasm: trap recovery and the pointer decode
// (U30), and the size limits checked before anything is copied in (U29).
import { describe, expect, it, vi } from 'vitest';
import {
  CompilerError,
  createCompiler,
  MAX_INPUT_BYTES,
  MAX_SOURCE_BYTES,
  unpack,
  type CompilerExports,
} from './runbook';

/** A JS stand-in for the module's exports: a bump allocator and an echo `call`. */
function fakeExports(behaviour: { trap?: boolean } = {}): CompilerExports & {
  alloc: ReturnType<typeof vi.fn>;
} {
  const memory = new WebAssembly.Memory({ initial: 1 });
  let top = 8;
  const alloc = vi.fn((len: number) => {
    const ptr = top;
    top += len;
    if (top > memory.buffer.byteLength) memory.grow(Math.ceil(len / 65536) + 1);
    return ptr;
  });
  const decoder = new TextDecoder();
  return {
    memory,
    alloc,
    free: () => {},
    call(opPtr, opLen) {
      if (behaviour.trap) throw new WebAssembly.RuntimeError('unreachable');
      const op = decoder.decode(new Uint8Array(memory.buffer, opPtr, opLen));
      const out = new TextEncoder().encode(JSON.stringify({ ok: { op } }));
      const ptr = alloc(out.length);
      new Uint8Array(memory.buffer, ptr, out.length).set(out);
      return (BigInt(ptr) << 32n) | BigInt(out.length);
    },
  };
}

const module = {} as WebAssembly.Module;

describe('pointer decode (U30)', () => {
  it('reads the packed i64 unsigned, so a pointer at or above 2 GiB stays positive', () => {
    // What the engine hands JS for ptr 0x8000_0010, len 5: a negative i64.
    const packed = BigInt.asIntN(64, (0x8000_0010n << 32n) | 5n);
    expect(packed < 0n).toBe(true);
    expect(unpack(packed)).toEqual({ ptr: 0x8000_0010, len: 5 });
    expect(unpack((0xffff_fff0n << 32n) | 0xffff_ffffn)).toEqual({
      ptr: 0xffff_fff0,
      len: 0xffff_ffff,
    });
    expect(unpack((16n << 32n) | 3n)).toEqual({ ptr: 16, len: 3 });
  });
});

describe('trap recovery (U30)', () => {
  it('rethrows a trap, then answers the next call on a fresh instance', () => {
    const made: ReturnType<typeof fakeExports>[] = [];
    const compiler = createCompiler(module, () => {
      const next = fakeExports({ trap: made.length === 0 });
      made.push(next);
      return next;
    });
    expect(() => compiler.outline({ source: 'x' })).toThrow(WebAssembly.RuntimeError);
    expect(made).toHaveLength(1);
    expect(compiler.call('outline', { source: 'x' })).toEqual({ op: 'outline' });
    expect(made).toHaveLength(2);
    // A healthy instance is kept.
    expect(compiler.call('describe', { source: 'x' })).toEqual({ op: 'describe' });
    expect(made).toHaveLength(2);
  });
});

describe('size limits before alloc (U29)', () => {
  function limited() {
    const exports = fakeExports();
    const instantiate = vi.fn(() => exports);
    return { compiler: createCompiler(module, instantiate), exports, instantiate };
  }
  const code = (f: () => unknown) => {
    try {
      f();
    } catch (error) {
      expect(error).toBeInstanceOf(CompilerError);
      return (error as CompilerError).code;
    }
    throw new Error('expected a CompilerError');
  };

  it('refuses a source over 1 MiB without touching the module', () => {
    const { compiler, exports, instantiate } = limited();
    expect(code(() => compiler.validate({ source: ' '.repeat(2 * MAX_SOURCE_BYTES) }))).toBe(
      'spec.source_limit',
    );
    // Over the limit in UTF-8 bytes, under it in UTF-16 code units.
    const wide = 'é'.repeat(MAX_SOURCE_BYTES / 2 + 1);
    expect(wide.length).toBeLessThan(MAX_SOURCE_BYTES);
    expect(code(() => compiler.outline({ source: wide }))).toBe('spec.source_limit');
    expect(exports.alloc).not.toHaveBeenCalled();
    expect(instantiate).not.toHaveBeenCalled();
    // At the limit, it runs.
    expect(compiler.call('outline', { source: ' '.repeat(MAX_SOURCE_BYTES) })).toEqual({
      op: 'outline',
    });
  });

  it('refuses a request over 8 MiB (a huge context) without touching the module', () => {
    const { compiler, exports } = limited();
    const users = Array.from({ length: 100_000 }, (_, i) => ({
      id: `usr_${String(i).padStart(26, '0')}`,
      name: 'x'.repeat(60),
    }));
    expect(JSON.stringify({ source: '', context: { users } }).length).toBeGreaterThan(
      MAX_INPUT_BYTES,
    );
    expect(code(() => compiler.validate({ source: '', context: { users } }))).toBe(
      'abi.input_limit',
    );
    expect(exports.alloc).not.toHaveBeenCalled();
  });
});
