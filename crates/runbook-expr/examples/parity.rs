#[path = "../tests/support/parity_corpus.rs"]
mod parity_corpus;

use std::sync::OnceLock;

// The allocation is initialized once and never modified or freed. Node receives
// only its address and length; no host callback, mutable static or unsafe read.
static CORPUS: OnceLock<Vec<u8>> = OnceLock::new();
fn bytes() -> &'static [u8] {
    CORPUS.get_or_init(|| serde_json::to_vec(&parity_corpus::corpus()).unwrap())
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
