// Compile the private module into a conformance executable without adding a
// public compiler/hash API solely for testing.
pub use runbook_spec::{
    DataSchema, DataType, DefaultValue, Diagnostic, estimate_step_runs, ir, normalize, parse,
    parse_declarations,
};
#[path = "../src/bindings.rs"]
mod bindings;
#[path = "../src/decimal.rs"]
mod decimal;
#[path = "../src/semantic.rs"]
mod semantic;
#[path = "../src/snapshot.rs"]
mod snapshot;
#[path = "../src/wire.rs"]
mod wire;
use std::sync::OnceLock;
static CORPUS: OnceLock<Vec<u8>> = OnceLock::new();
fn bytes() -> &'static [u8] {
    CORPUS.get_or_init(|| serde_json::to_vec(&semantic::vectors::corpus()).unwrap())
}
#[unsafe(no_mangle)]
pub extern "C" fn parity_ptr() -> *const u8 {
    bytes().as_ptr()
}
#[unsafe(no_mangle)]
pub extern "C" fn parity_len() -> usize {
    bytes().len()
}
fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    println!("{}", std::str::from_utf8(bytes()).unwrap());
}

pub use runbook_spec::{CompileContext, CompiledRunbook, compile, restore, validate_policy};
#[path = "../src/compile/corpus.rs"]
mod compiler_corpus;
