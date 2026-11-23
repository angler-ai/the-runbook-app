//! The core's streaming redactor ([`runbook_core::redact::Redactor`]) for the
//! runner (R1-F3), so transcripts, tool-call records and outputs written by
//! the runner are redacted exactly as the core redacts logs. The runner loads
//! the Wasm build (`runner/packages/shared/src/redact.ts`) and adds its own
//! exact-value redaction of released secrets on top.
//!
//! Same shape as `runbook-web-wasm`: no wasm-bindgen and no host imports; the
//! exports in [`abi`] only move bytes in and out of linear memory. Each
//! redactor is a handle into a table, so one module serves many streams.
use runbook_core::redact::Redactor;
use std::cell::RefCell;

thread_local! {
    static REDACTORS: RefCell<Vec<Option<Redactor>>> = const { RefCell::new(Vec::new()) };
}

/// Start a stream. Handles are reused after [`drop_redactor`].
pub fn new_redactor() -> u32 {
    REDACTORS.with_borrow_mut(|table| {
        if let Some(i) = table.iter().position(Option::is_none) {
            table[i] = Some(Redactor::default());
            i as u32
        } else {
            table.push(Some(Redactor::default()));
            (table.len() - 1) as u32
        }
    })
}

fn with<R>(handle: u32, f: impl FnOnce(&mut Redactor) -> R) -> Option<R> {
    REDACTORS.with_borrow_mut(|table| table.get_mut(handle as usize)?.as_mut().map(f))
}

/// Push the next chunk of a stream (UTF-8; invalid sequences become U+FFFD).
/// Returns what can be written so far; `None` for an unknown handle.
pub fn push(handle: u32, chunk: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(chunk);
    with(handle, |r| r.push(&text))
}

/// End a stream: an incomplete recognized credential is redacted.
pub fn finish(handle: u32) -> Option<String> {
    with(handle, Redactor::finish)
}

/// Release a stream's handle.
pub fn drop_redactor(handle: u32) {
    REDACTORS.with_borrow_mut(|table| {
        if let Some(slot) = table.get_mut(handle as usize) {
            *slot = None;
        }
    });
}

/// The Wasm exports. Native builds omit them: `free` would shadow libc.
#[cfg(target_arch = "wasm32")]
pub mod abi {
    use std::ptr::{NonNull, slice_from_raw_parts_mut};

    /// Returned for an unknown handle: no bytes at address 0.
    const NONE: u64 = 0;

    fn out(text: Option<String>) -> u64 {
        let Some(text) = text else { return NONE };
        let bytes = text.into_bytes().into_boxed_slice();
        let len = bytes.len();
        if len == 0 {
            return NONE;
        }
        let ptr = Box::into_raw(bytes).cast::<u8>();
        ((ptr as usize as u64) << 32) | len as u64
    }

    /// Reserve `len` zeroed bytes for the host to write into; release with `free`.
    #[unsafe(no_mangle)]
    pub extern "C" fn alloc(len: usize) -> *mut u8 {
        if len == 0 {
            return NonNull::dangling().as_ptr();
        }
        Box::into_raw(vec![0u8; len].into_boxed_slice()).cast()
    }

    /// Release a buffer from `alloc` or a result, with its exact length.
    ///
    /// # Safety
    /// `ptr`/`len` must be one live buffer this module returned, freed once.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn free(ptr: *mut u8, len: usize) {
        if len != 0 {
            drop(unsafe { Box::from_raw(slice_from_raw_parts_mut(ptr, len)) });
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn redactor_new() -> u32 {
        super::new_redactor()
    }

    /// Push `len` bytes at `ptr` (owned by the caller). Returns
    /// `(ptr << 32) | len` of the output the caller must `free`; 0 when empty.
    ///
    /// # Safety
    /// The range must be readable memory of this module (for example from `alloc`).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn redactor_push(handle: u32, ptr: *const u8, len: usize) -> u64 {
        let chunk: &[u8] = if len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(ptr, len) }
        };
        out(super::push(handle, chunk))
    }

    /// Finish the stream; same result convention as `redactor_push`.
    #[unsafe(no_mangle)]
    pub extern "C" fn redactor_finish(handle: u32) -> u64 {
        out(super::finish(handle))
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn redactor_drop(handle: u32) {
        super::drop_redactor(handle)
    }
}
