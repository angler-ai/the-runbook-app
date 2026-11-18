//! The finalize decision (D49) and its seam to the magic-byte table.
//!
//! UP3 owns the table (`uploads/sniff.rs`): a function from a file's leading
//! bytes to what it is. Finalize only needs that function's signature, so the
//! seam is the [`Sniff`] type below. The host passes the function in; at the
//! UP3 merge the integrator passes `uploads::sniff::sniff`, whose type
//! [`SniffedType`] is re-exported here. Nothing here guesses a type
//! from a name or the client's declaration.
use super::VerifiedUpload;

/// What the magic-byte table recognizes (UP3's [`super::sniff::SniffedType`]).
/// SVG and anything else is unrecognized (`None`), so it never verifies;
/// HEIC is recognized only to refuse it (`upload.type_unsupported`, D51).
pub use super::sniff::SniffedType;

/// The seam: a file's leading bytes (at most [`SNIFF_PREFIX_BYTES`]) to its
/// type, or `None` when the table does not recognize it.
pub type Sniff = fn(&[u8]) -> Option<SniffedType>;

/// How many leading bytes finalize reads (a range read) for sniffing.
pub const SNIFF_PREFIX_BYTES: u64 = 64 * 1024;

/// Declared types finalize can ever verify; anything else is refused when the
/// slot is requested (`upload.type_unsupported`). Images also need to be in
/// the server's advertised `image_types`.
pub const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];
pub const FILE_TYPES: &[&str] = &[
    "application/pdf",
    "application/json",
    "text/plain",
    "text/markdown",
    "text/csv",
];
/// Refused with guidance for now (D51): convert to JPEG on the client.
pub const HEIC_TYPES: &[&str] = &["image/heic", "image/heif"];

/// Whether bytes of type `sniffed` may stand behind a slot declared as
/// `declared`. Images must match exactly; plain text covers the text
/// subtypes; JSON is also plain text.
pub fn satisfies(sniffed: SniffedType, declared: &str) -> bool {
    match sniffed {
        SniffedType::Heic => false,
        SniffedType::Text => matches!(declared, "text/plain" | "text/markdown" | "text/csv"),
        SniffedType::Json => matches!(
            declared,
            "application/json" | "text/plain" | "text/markdown" | "text/csv"
        ),
        other => other.mime() == declared,
    }
}

/// Judge stored bytes against their slot: size first (from `head`), then the
/// type from the sniffed prefix. `Err` is the stable rejection code; the
/// caller deletes the object.
pub fn judge(
    declared_type: &str,
    declared_size: u64,
    stored_size: u64,
    sniffed: Option<SniffedType>,
) -> Result<VerifiedUpload, &'static str> {
    if stored_size != declared_size {
        return Err("upload.size_mismatch");
    }
    match sniffed {
        Some(SniffedType::Heic) => Err("upload.type_unsupported"),
        Some(kind) if satisfies(kind, declared_type) => Ok(VerifiedUpload {
            size: stored_size,
            content_type: declared_type.to_owned(),
            // UP2 reads image dimensions from the header and makes the
            // thumbnail; UP1 verifies size and type only.
            width: None,
            height: None,
            thumbnail: false,
            thumbnail_bytes: 0,
        }),
        _ => Err("upload.type_mismatch"),
    }
}
