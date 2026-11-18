# runbook-web-wasm

The browser compiler (plan B3, A7/A8). It runs the server's compiler in a Web
Worker so the editor gets diagnostics, the outline and the start-form schemas
on every edit. Server diagnostics remain authoritative (F99).

It is a `cdylib` for `wasm32-unknown-unknown` with **no host imports** and no
wasm-bindgen: the module exports only `memory`, `alloc`, `free` and `call`.
Build it with `npm run wasm:build` in `web/` (release build, `wasm-opt -Oz`
when installed, copied to `web/src/wasm/runbook.wasm`). The loader is
`web/src/wasm/runbook.ts`.

## ABI

All strings are UTF-8. Pointers and lengths are wasm32 `i32` (unsigned).

| Export | Signature | Meaning |
|---|---|---|
| `alloc` | `(len) -> ptr` | Reserve `len` bytes for the host to write into. |
| `free` | `(ptr, len)` | Release a buffer from `alloc` or a result from `call`, with its exact length. |
| `call` | `(op_ptr, op_len, in_ptr, in_len) -> i64` | Run operation `op` on JSON `in`. Returns `(ptr << 32) \| len` of a new JSON result. |

A call:

1. `alloc` a buffer for the operation name and one for the input JSON, and
   copy the bytes in. Re-read `memory.buffer` after every `alloc` and `call`:
   memory growth detaches the old `ArrayBuffer`.
2. `call`. The inputs stay owned by the host; `free` them afterwards.
3. Split the returned `i64` (a `BigInt` in JS): `ptr = Number(r >> 32n)`,
   `len = Number(r & 0xffffffffn)`. Decode the result, then `free(ptr, len)`.

The module never calls the host. A Rust panic is a Wasm trap: discard the
instance and instantiate a fresh one.

## Operations

Every operation takes the same input:

```json
{"source": "<runbook YAML>", "context": {"users": [], "teams": [], "skills": [], "variables": {}}}
```

`context` is optional and is the directory the server compiles against
(`runbook_spec::authoring::CompileDirectory`): `users` are
`{id, name, active?}` (active defaults to true), `teams` are `{id, slug}`,
`skills` are the project's latest skill versions `{name, hash, reference?}`
(64 hex digits), and `variables` are the project's variable declarations.
Only `validate` reads it.

Every result is `{"ok": …}` or, for a bad call, `{"error": {"code", "message"}}`
with code `abi.unknown_operation`, `abi.invalid_input`, `abi.input_limit`
(the JSON request is over `MAX_INPUT_BYTES`, 8 MiB; checked before parsing),
`spec.source_limit` (source over 1 MiB, the server's 413) or
`vars.invalid_stored_declarations`. Hosts should refuse both limits before
`alloc`, as `web/src/wasm/runbook.ts` does: linear memory never shrinks, so a
copied 20 MB request would stay reserved for the life of the instance.
Compiler diagnostics are never errors; they are part of `ok`.

| Operation | `ok` value |
|---|---|
| `validate` | The `/v1/validate` body: `{"diagnostics": [{path, code, message, line?, column?}], "step_budget": n \| null}`. Same functions as the server (`runbook_spec::authoring::{compile_context, validate_source}`), except that the browser has no runbook catalog: `assume_calls_resolve` treats every `call` target as an unknown runbook that accepts what the call passes, so `validate` reports no call diagnostic but a call to the runbook itself (`spec.call_cycle`); the server checks calls (UI-2). |
| `outline` | `{"outline": Outline \| null, "diagnostics": [...]}`. `Outline` is the fixed contract in `web/src/ui/types.ts`. |
| `describe` | `{"runbook": {id, title, inputs, fields} \| null, "diagnostics": [...]}`. `inputs`/`fields` are JSON Schema from `runbook_spec::schema_json`, as `GET …/versions/{v}/form` returns. |

`outline` and `describe` need only the source (parse and normalize); they
return `null` with the diagnostics when the source does not normalize. In the
outline, `path` is the step's RFC 6901 pointer into the source (the same
pointers diagnostics use), switch cases are labelled `case 1`, `case 2`, …
plus an always-present `default`, and a group's non-empty `catch`/`finally`
bodies are branches.

## Tests

- `tests/abi.rs`: operation results and error envelopes, natively.
- `tests/outline_fixture.rs`: the flagship outline equals
  `web/src/ui/outline/fixtures/post-deploy.outline.json`.
- `tests/parity.rs`: a fresh Wasm build runs in Node (`tests/wasm-runner.mjs`)
  over `conformance/spec/**`, the flagship and every flagship prefix, with and
  without a directory; results must be byte-identical to native, and `validate`
  must equal the server's `runbook_api::authoring::validate` under the same call assumption. Without `node` it
  prints a notice and passes.
