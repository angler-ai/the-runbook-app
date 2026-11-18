// Runs the browser compiler's Wasm module over a list of requests through its
// public ABI (alloc/free/call), exactly as web/src/wasm/runbook.ts does.
//   node wasm-runner.mjs runbook_web_wasm.wasm requests.json > results.json
// requests.json: [{"op": string, "input": string}]; stdout: [result string].
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

assert.equal(process.argv.length, 4, 'usage: node wasm-runner.mjs module.wasm requests.json');
const module = await WebAssembly.compile(await readFile(process.argv[2]));
assert.deepEqual(WebAssembly.Module.imports(module), [], 'the module has no host imports');
const { exports } = await WebAssembly.instantiate(module, {});
for (const name of ['alloc', 'free', 'call']) assert.equal(typeof exports[name], 'function', name);
assert.ok(exports.memory instanceof WebAssembly.Memory, 'exported memory');

const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });
function put(bytes) {
  const ptr = exports.alloc(bytes.length) >>> 0;
  // Re-read the buffer after every allocation: growth detaches the old one.
  new Uint8Array(exports.memory.buffer, ptr, bytes.length).set(bytes);
  return ptr;
}
function call(op, input) {
  const opBytes = encoder.encode(op);
  const inBytes = encoder.encode(input);
  const opPtr = put(opBytes);
  const inPtr = put(inBytes);
  const packed = exports.call(opPtr, opBytes.length, inPtr, inBytes.length);
  exports.free(opPtr, opBytes.length);
  exports.free(inPtr, inBytes.length);
  // The export returns an i64: read it unsigned, or a pointer at or above
  // 2 GiB would decode negative.
  const unsigned = BigInt.asUintN(64, packed);
  const ptr = Number(unsigned >> 32n);
  const len = Number(unsigned & 0xffffffffn);
  assert.ok(ptr + len <= exports.memory.buffer.byteLength, 'result bounds');
  const text = decoder.decode(new Uint8Array(exports.memory.buffer, ptr, len));
  exports.free(ptr, len);
  return text;
}

const requests = JSON.parse(await readFile(process.argv[3], 'utf8'));
assert.ok(Array.isArray(requests) && requests.length > 0, 'nonempty request list');
process.stdout.write(JSON.stringify(requests.map(({ op, input }) => call(op, input))));
