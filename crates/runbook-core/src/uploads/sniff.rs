//! UP3: upload magic bytes sniffing for evidence files (PRD §14).
//!
//! Recognises PNG, JPEG, WebP, GIF, HEIC, PDF, JSON, and plain text.
//! Rejects SVG, XML, HTML, and non-UTF-8 or control-character-laden text.

use serde::{Deserialize, Serialize};

/// Recognised evidence file format from content inspection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SniffedType {
    Png,
    Jpeg,
    Webp,
    Gif,
    Heic,
    Pdf,
    Text,
    Json,
}

impl SniffedType {
    /// True for image types: PNG, JPEG, WebP, GIF, HEIC.
    pub const fn is_image(self) -> bool {
        matches!(
            self,
            Self::Png | Self::Jpeg | Self::Webp | Self::Gif | Self::Heic
        )
    }
    /// MIME type string for this format.
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
            Self::Heic => "image/heic",
            Self::Pdf => "application/pdf",
            Self::Text => "text/plain",
            Self::Json => "application/json",
        }
    }
}

/// MIME type string for a sniffed format.
pub const fn mime(t: SniffedType) -> &'static str {
    t.mime()
}

const HEIC_BRANDS: &[&[u8; 4]] = &[
    b"heic", b"heix", b"hevc", b"heim", b"heis", b"mif1", b"msf1",
];

fn is_heic(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let major = &bytes[8..12];
    if HEIC_BRANDS.iter().any(|&b| b == major) {
        return true;
    }
    let box_size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let limit = if box_size >= 8 {
        box_size.min(bytes.len())
    } else {
        bytes.len()
    };
    // Compatible brands start at byte 16; a box shorter than that has none.
    if limit > 16 {
        let (chunks, _) = bytes[16..limit].as_chunks::<4>();
        for chunk in chunks {
            if HEIC_BRANDS.contains(&chunk) {
                return true;
            }
        }
    }
    false
}

/// Inspect the initial bytes of an evidence file and determine its type.
///
/// Returns `None` for unrecognized formats, empty input, invalid UTF-8,
/// XML/SVG/HTML content, or binary files with unrecognized headers.
pub fn sniff(bytes: &[u8]) -> Option<SniffedType> {
    if bytes.is_empty() {
        return None;
    }

    // 1. Binary signatures take precedence over text inspection.
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(SniffedType::Png);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(SniffedType::Jpeg);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(SniffedType::Webp);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(SniffedType::Gif);
    }
    if is_heic(bytes) {
        return Some(SniffedType::Heic);
    }
    if bytes.starts_with(b"%PDF-") {
        return Some(SniffedType::Pdf);
    }

    // 2. Text inspection: must be valid UTF-8 without NUL or disallowed control characters.
    // Finalize sniffs a prefix (`verify::SNIFF_PREFIX_BYTES`), which can end
    // inside a multi-byte character: an incomplete trailing sequence is
    // allowed (`error_len() == None`), an invalid byte anywhere is not.
    let s = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).ok()?
        }
        Err(_) => return None,
    };

    if s.chars()
        .any(|c| c == '\0' || (c.is_control() && c != '\t' && c != '\n' && c != '\r'))
    {
        return None;
    }

    let trimmed = s.trim_start_matches('\u{feff}').trim_start();

    // JSON check: first non-whitespace byte is '{' or '[' and the text is a
    // JSON document or a valid prefix of one (the sniffed prefix of a longer
    // file ends early: serde_json reports that as an EOF error).
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        match serde_json::from_str::<serde::de::IgnoredAny>(trimmed) {
            Ok(_) => return Some(SniffedType::Json),
            Err(error) if error.is_eof() => return Some(SniffedType::Json),
            Err(_) => {}
        }
    }

    // SVG / XML / HTML: any text starting with '<' must NEVER be recognised as text.
    if trimmed.starts_with('<') {
        return None;
    }

    Some(SniffedType::Text)
}
