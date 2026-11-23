//! The browser compiler (B3): `validate`, `outline` and `describe` over a
//! JSON-in/JSON-out ABI with no host imports and no wasm-bindgen. See README.md.
//!
//! [`call`] is the whole interface; the Wasm exports in [`abi`] only move
//! bytes in and out of linear memory. Native tests call [`call`] directly, so
//! native and Wasm run the same code and must return the same bytes.
use runbook_spec::authoring::{
    CompileDirectory, MAX_SOURCE_BYTES, compile_context, validate_source, wire_diagnostics,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod outline;
pub use outline::{
    Outline, OutlineApproval, OutlineBranch, OutlineCall, OutlineEnd, OutlineForeach, OutlineJoin,
    OutlineLoop, OutlineNode, OutlineOption, OutlineOutput, OutlineSet, OutlineWait, outline,
};

/// The largest input (the JSON request) any operation accepts: a 1 MiB
/// source escapes to at most 6 MiB of JSON, which leaves 2 MiB for the
/// directory context. Checked before parsing (U29); hosts check it before
/// copying the request into linear memory, which never shrinks.
pub const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// The input of every operation. `context` is the directory the server would
/// compile against; only `validate` reads it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    source: String,
    #[serde(default)]
    context: CompileDirectory,
}

/// `describe`: the runbook's identity and its start-form schemas.
#[derive(Serialize)]
struct Description {
    id: String,
    title: String,
    inputs: Value,
    fields: Value,
}

/// Run one operation. Always returns UTF-8 JSON: `{"ok": …}` or
/// `{"error": {"code", "message"}}`. Compiler diagnostics are `ok` values.
pub fn call(op: &[u8], input: &[u8]) -> Vec<u8> {
    let result = match op {
        b"validate" => run(input, |i| {
            let mut context = compile_context(&i.context, &i.source).map_err(|_| {
                (
                    "vars.invalid_stored_declarations",
                    "the project's variable declarations are invalid",
                )
            })?;
            assume_calls_resolve(&i.source, &mut context);
            Ok(json!(validate_source(&i.source, &context)))
        }),
        b"outline" => run(input, |i| {
            Ok(
                match runbook_spec::parse(&i.source).and_then(|s| runbook_spec::normalize(&s)) {
                    Ok(ir) => json!({"outline": outline(&ir), "diagnostics": []}),
                    Err(e) => json!({"outline": null, "diagnostics": wire_diagnostics(e)}),
                },
            )
        }),
        b"describe" => run(input, |i| {
            Ok(
                match runbook_spec::parse(&i.source).and_then(|s| runbook_spec::normalize(&s)) {
                    Ok(ir) => json!({
                        "runbook": Description {
                            id: ir.metadata.id,
                            title: ir.metadata.title,
                            inputs: runbook_spec::schema_json(&ir.inputs),
                            fields: runbook_spec::schema_json(&ir.fields),
                        },
                        "diagnostics": [],
                    }),
                    Err(e) => json!({"runbook": null, "diagnostics": wire_diagnostics(e)}),
                },
            )
        }),
        _ => Err((
            "abi.unknown_operation",
            "expected validate, outline or describe",
        )),
    };
    let envelope = match result {
        Ok(value) => json!({ "ok": value }),
        Err((code, message)) => json!({"error": {"code": code, "message": message}}),
    };
    serde_json::to_vec(&envelope).expect("JSON values always serialize")
}

/// The browser has no catalog of published runbooks, so it cannot know
/// whether a `call` target exists or what it declares. Rather than report
/// `spec.call_unresolved` for a runbook that is published (UI-2), resolve
/// every call reference in `source` to a permissive placeholder: no calls of
/// its own, not destructive, every input the calls supply optional and every
/// output they read of any shape. No call diagnostic can then come from the
/// browser except a call to the runbook's own slug (`spec.call_cycle`), which
/// the server reports too; the server checks calls when it validates. Other
/// checks run as before (call errors used to stop compilation early).
///
/// A source that does not normalize is left alone: compiling it reports the
/// same diagnostics before calls are resolved.
pub fn assume_calls_resolve(source: &str, context: &mut runbook_spec::CompileContext) {
    use runbook_spec::calls::call_key;
    use runbook_spec::ir::{CallResolution, Node, NodeKind, OutputKind};
    fn visit(nodes: &[Node], out: &mut std::collections::BTreeMap<String, CallResolution>) {
        for node in nodes {
            match &node.kind {
                NodeKind::Call(call) => {
                    let entry = out
                        .entry(call_key(&call.runbook, call.version))
                        .or_insert_with(|| CallResolution {
                            runbook_id: String::new(),
                            version_id: String::new(),
                            version: call.version.unwrap_or(1),
                            depth: 0,
                            reaches: vec![],
                            destructive: false,
                            agent_decidable: Some(false),
                            inputs: Default::default(),
                            input_schemas: Default::default(),
                            outputs: Default::default(),
                        });
                    for name in call.with.keys() {
                        entry.inputs.insert(name.clone(), false);
                    }
                    if let runbook_spec::DataType::Object { properties } = &call.outputs.kind {
                        for name in properties.keys() {
                            entry.outputs.insert(name.clone(), OutputKind::Any);
                        }
                    }
                }
                NodeKind::Group(g) => {
                    visit(&g.children, out);
                    visit(&g.catch, out);
                    visit(&g.finally, out);
                }
                NodeKind::Parallel { children, .. } => visit(children, out),
                NodeKind::Foreach(f) => visit(&f.children, out),
                NodeKind::Loop(l) => visit(&l.children, out),
                NodeKind::Switch(s) => {
                    for case in &s.cases {
                        visit(&case.children, out);
                    }
                    visit(&s.default, out);
                }
                NodeKind::Task(_)
                | NodeKind::Wait(_)
                | NodeKind::Approval(_)
                | NodeKind::Set(_)
                | NodeKind::End(_) => {}
            }
        }
    }
    let Ok(ir) = runbook_spec::parse(source).and_then(|s| runbook_spec::normalize(&s)) else {
        return;
    };
    let mut placeholders = std::collections::BTreeMap::new();
    visit(&ir.steps, &mut placeholders);
    visit(&ir.finally, &mut placeholders);
    for (key, placeholder) in placeholders {
        context.runbooks.entry(key).or_insert(placeholder);
    }
}

type Failure = (&'static str, &'static str);
fn run(input: &[u8], op: impl FnOnce(Input) -> Result<Value, Failure>) -> Result<Value, Failure> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(("abi.input_limit", "the request is larger than 8 MiB"));
    }
    let input: Input = serde_json::from_slice(input).map_err(|_| {
        (
            "abi.invalid_input",
            "expected {\"source\": string, \"context\"?: directory}",
        )
    })?;
    if input.source.len() > MAX_SOURCE_BYTES {
        return Err(("spec.source_limit", "the source is larger than 1 MiB"));
    }
    op(input)
}

/// The Wasm exports. Native builds omit them: `free` would shadow libc.
#[cfg(target_arch = "wasm32")]
pub mod abi {
    use std::ptr::{NonNull, slice_from_raw_parts_mut};

    /// Reserve `len` zeroed bytes for the host to write into; release with `free`.
    #[unsafe(no_mangle)]
    pub extern "C" fn alloc(len: usize) -> *mut u8 {
        if len == 0 {
            return NonNull::dangling().as_ptr();
        }
        Box::into_raw(vec![0u8; len].into_boxed_slice()).cast()
    }

    /// Release a buffer from `alloc` or a result from `call`, with its exact length.
    ///
    /// # Safety
    /// `ptr`/`len` must be one live buffer this module returned, freed once.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn free(ptr: *mut u8, len: usize) {
        if len != 0 {
            drop(unsafe { Box::from_raw(slice_from_raw_parts_mut(ptr, len)) });
        }
    }

    /// Run operation `op` on JSON `input`. Both stay owned by the caller.
    /// Returns `(ptr << 32) | len` of a new JSON result the caller must `free`.
    ///
    /// # Safety
    /// Both ranges must be readable memory of this module (for example from `alloc`).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn call(
        op_ptr: *const u8,
        op_len: usize,
        in_ptr: *const u8,
        in_len: usize,
    ) -> u64 {
        let read = |ptr: *const u8, len: usize| -> &[u8] {
            if len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(ptr, len) }
            }
        };
        let out = super::call(read(op_ptr, op_len), read(in_ptr, in_len)).into_boxed_slice();
        let len = out.len();
        let ptr = Box::into_raw(out).cast::<u8>();
        ((ptr as usize as u64) << 32) | len as u64
    }
}
