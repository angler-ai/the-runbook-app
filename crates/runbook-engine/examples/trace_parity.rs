#[path = "../tests/support/corpus.rs"]
mod corpus;
#[path = "../tests/support/mod.rs"]
mod support;
use std::sync::OnceLock;
static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
fn bytes() -> &'static [u8] {
    BYTES.get_or_init(|| serde_json::to_vec(&corpus::rows()).unwrap())
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
