// Compile the same private checkpoint into this executable; do not expose an
// executable compiler API from runbook-spec just to support a conformance probe.
pub use runbook_spec::{
    DataSchema, DataType, Diagnostic, SourceSpan, Spec, ir, normalize, parse, parse_declarations,
    schema, validate_data,
};
#[path = "../src/scopes.rs"]
mod scopes;
use std::sync::OnceLock;
static CORPUS: OnceLock<Vec<u8>> = OnceLock::new();
fn bytes() -> &'static [u8] {
    CORPUS.get_or_init(|| serde_json::to_vec(&scopes::corpus::corpus()).unwrap())
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
