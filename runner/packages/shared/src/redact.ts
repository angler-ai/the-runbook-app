// Redaction for everything the runner writes (transcripts, tool-call
// records, outputs, evidence): the core's own streaming redactor, compiled
// to WebAssembly from crates/runbook-redact-wasm (`npm run wasm:build`), so
// token shapes, keys and headers are redacted exactly as the core redacts
// logs; plus exact-value redaction of the secrets released to the step, in
// the encodings they're likely to be echoed in (design §7.8).
//
// Streams hold back a bounded tail (at most the longest released form, and
// the core's 512 bytes) so a value split across pushes is still caught.
import { readFile } from 'node:fs/promises';

export const RELEASED_MARKER = '[REDACTED:released]';
/** Released values at least this long are also matched in encoded forms. */
const MIN_ENCODED_LENGTH = 8;
/** Encoded fragments shorter than this are not matched on their own. */
const MIN_FORM_LENGTH = 8;

const DEFAULT_WASM = new URL('../wasm/runbook_redact.wasm', import.meta.url);

// The WebAssembly global is in Node but not in the ES lib typings.
interface WasmGlobal {
  instantiate(bytes: Uint8Array, imports: object): Promise<{ instance: { exports: unknown } }>;
}
const wasmGlobal = (globalThis as unknown as { WebAssembly: WasmGlobal }).WebAssembly;

interface Exports {
  memory: { buffer: ArrayBuffer };
  alloc(len: number): number;
  free(ptr: number, len: number): void;
  redactor_new(): number;
  redactor_push(handle: number, ptr: number, len: number): bigint;
  redactor_finish(handle: number): bigint;
  redactor_drop(handle: number): void;
}

export interface StreamRedactor {
  /** Redact the next chunk; returns what can be written so far. */
  push(chunk: string): string;
  /** End the stream; returns the rest. */
  finish(): string;
  /** Release the stream's wasm handle (also done by `finish` callers' GC). */
  dispose(): void;
}

export interface Redaction {
  stream(released?: readonly string[]): StreamRedactor;
  redact(text: string, released?: readonly string[]): string;
}

/**
 * The forms a released value is matched in: as is, base64 and base64url
 * (including inside a longer base64 value at each byte alignment, as in
 * Basic auth), URL-encoded, JSON-escaped, and hex.
 */
export function releasedForms(value: string): string[] {
  if (value === '') return [];
  const forms = new Set<string>([value]);
  if (value.length >= MIN_ENCODED_LENGTH) {
    const raw = Buffer.from(value, 'utf8');
    for (let k = 0; k < 3; k++) {
      // Encode behind k filler bytes; keep only the characters that depend on
      // the value's bytes alone, whatever comes before or after it.
      const encoded = Buffer.concat([Buffer.alloc(k), raw]).toString('base64').replace(/=+$/, '');
      const start = Math.ceil((8 * k) / 6);
      const end = Math.floor((8 * (k + raw.length)) / 6);
      const core = encoded.slice(start, end);
      forms.add(core);
      forms.add(core.replaceAll('+', '-').replaceAll('/', '_'));
    }
    forms.add(raw.toString('base64'));
    forms.add(raw.toString('base64url'));
    forms.add(encodeURIComponent(value));
    forms.add(JSON.stringify(value).slice(1, -1));
    forms.add(raw.toString('hex'));
    forms.add(raw.toString('hex').toUpperCase());
  }
  return [...forms].filter((f) => f === value || f.length >= MIN_FORM_LENGTH);
}

/** Exact-value matcher with a bounded hold-back so split values are caught. */
class ExactMatcher {
  private buffer = '';
  private readonly patterns: string[];
  private readonly maxLength: number;

  constructor(values: readonly string[]) {
    const forms = new Set<string>();
    for (const v of values) for (const f of releasedForms(v)) forms.add(f);
    this.patterns = [...forms].sort((a, b) => b.length - a.length);
    this.maxLength = this.patterns[0]?.length ?? 0;
  }

  push(chunk: string, final = false): string {
    if (this.patterns.length === 0) {
      const out = this.buffer + chunk;
      this.buffer = '';
      return out;
    }
    const buf = this.buffer + chunk;
    // Not final: a match may only be decided where every pattern fits.
    const limit = final ? buf.length : buf.length - this.maxLength + 1;
    let out = '';
    let i = 0;
    for (;;) {
      let best = -1;
      let bestLength = 0;
      for (const p of this.patterns) {
        const at = buf.indexOf(p, i);
        if (at !== -1 && (best === -1 || at < best || (at === best && p.length > bestLength))) {
          best = at;
          bestLength = p.length;
        }
      }
      if (best === -1 || best >= limit) break;
      out += buf.slice(i, best) + RELEASED_MARKER;
      i = best + bestLength;
    }
    const emitTo = Math.max(i, limit);
    out += buf.slice(i, emitTo);
    this.buffer = buf.slice(emitTo);
    return out;
  }
}

class WasmStream implements StreamRedactor {
  private handle: number | undefined;
  private highSurrogate = '';
  private readonly exact: ExactMatcher;
  private readonly exports: Exports;

  constructor(exports: Exports, released: readonly string[]) {
    this.exports = exports;
    this.handle = exports.redactor_new();
    this.exact = new ExactMatcher(released);
  }

  private take(result: bigint): string {
    const ptr = Number(result >> 32n);
    const len = Number(result & 0xffffffffn);
    if (len === 0) return '';
    const text = new TextDecoder().decode(new Uint8Array(this.exports.memory.buffer, ptr, len));
    this.exports.free(ptr, len);
    return text;
  }

  private core(text: string): string {
    if (this.handle === undefined) throw new Error('redaction stream is disposed');
    if (text === '') return '';
    const bytes = new TextEncoder().encode(text);
    const ptr = this.exports.alloc(bytes.length);
    // alloc may grow memory: take the buffer after it.
    new Uint8Array(this.exports.memory.buffer, ptr, bytes.length).set(bytes);
    try {
      return this.take(this.exports.redactor_push(this.handle, ptr, bytes.length));
    } finally {
      this.exports.free(ptr, bytes.length);
    }
  }

  push(chunk: string): string {
    // Hold a trailing high surrogate until its pair arrives.
    let text = this.highSurrogate + chunk;
    this.highSurrogate = '';
    const last = text.charCodeAt(text.length - 1);
    if (last >= 0xd800 && last <= 0xdbff) {
      this.highSurrogate = text.slice(-1);
      text = text.slice(0, -1);
    }
    return this.core(this.exact.push(text));
  }

  finish(): string {
    if (this.handle === undefined) throw new Error('redaction stream is disposed');
    const rest = this.exact.push(this.highSurrogate, true);
    this.highSurrogate = '';
    return this.core(rest) + this.take(this.exports.redactor_finish(this.handle));
  }

  dispose(): void {
    if (this.handle !== undefined) {
      this.exports.redactor_drop(this.handle);
      this.handle = undefined;
    }
  }
}

const loaded = new Map<string, Promise<Redaction>>();

/** Load the redactor (once per module file). */
export function loadRedaction(wasm: URL = DEFAULT_WASM): Promise<Redaction> {
  const key = wasm.href;
  let promise = loaded.get(key);
  if (!promise) {
    promise = instantiate(wasm);
    loaded.set(key, promise);
    promise.catch(() => loaded.delete(key));
  }
  return promise;
}

async function instantiate(wasm: URL): Promise<Redaction> {
  let bytes: Buffer;
  try {
    bytes = await readFile(wasm);
  } catch {
    throw new Error(`the redaction module is missing (${wasm.pathname}); run \`npm run wasm:build\` in runner/`);
  }
  const { instance } = await wasmGlobal.instantiate(bytes, {});
  const exports = instance.exports as unknown as Exports;
  const redaction: Redaction = {
    stream: (released = []) => new WasmStream(exports, released),
    redact(text, released = []) {
      const s = new WasmStream(exports, released);
      try {
        return s.push(text) + s.finish();
      } finally {
        s.dispose();
      }
    },
  };
  return redaction;
}

/** Redact keys and decoded strings, including values in nested tool results. */
export function redactValue(value: unknown, redact: (text: string) => string, depth = 0): unknown {
  if (depth > 64) return '[TRUNCATED]';
  if (typeof value === 'string') return redact(value);
  if (typeof value === 'number' || typeof value === 'boolean') {
    const text = String(value);
    const masked = redact(text);
    return masked === text ? value : masked;
  }
  if (Array.isArray(value)) return value.map((entry) => redactValue(entry, redact, depth + 1));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [redact(key), redactValue(entry, redact, depth + 1)]));
  }
  return value;
}
