import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

assert.equal(process.argv.length, 3, 'usage: node wasm-runner.mjs fresh-example.wasm');
const module = await WebAssembly.compile(await readFile(process.argv[2]));
const imports = Object.create(null);
for (const entry of WebAssembly.Module.imports(module)) {
  assert.equal(entry.kind, 'function', `nonfunction host import: ${entry.module}.${entry.name}`);
  imports[entry.module] ??= Object.create(null);
  imports[entry.module][entry.name] = () => {
    throw new Error(`host import invoked: ${entry.module}.${entry.name}`);
  };
}
const { exports } = await WebAssembly.instantiate(module, imports);
assert.ok(exports.memory instanceof WebAssembly.Memory, 'exported memory');
assert.equal(typeof exports.parity_ptr, 'function');
assert.equal(typeof exports.parity_len, 'function');
const pointer = exports.parity_ptr() >>> 0;
const length = exports.parity_len() >>> 0;
assert.ok(length > 0, 'empty exported buffer');
assert.ok(pointer + length <= exports.memory.buffer.byteLength, 'buffer bounds');
assert.equal(exports.parity_ptr() >>> 0, pointer, 'stable immutable buffer pointer');
assert.equal(exports.parity_len() >>> 0, length, 'stable immutable buffer length');
const text = new TextDecoder('utf-8', { fatal: true }).decode(
  new Uint8Array(exports.memory.buffer, pointer, length),
);
const rows = JSON.parse(text);
assert.ok(Array.isArray(rows) && rows.length > 0, 'nonempty corpus');
const names = new Set();
for (const row of rows) {
  assert.ok(typeof row.name === 'string' && row.name.length > 0, 'named row');
  assert.ok(!names.has(row.name), `duplicate row: ${row.name}`);
  names.add(row.name);
}
process.stdout.write(text);
