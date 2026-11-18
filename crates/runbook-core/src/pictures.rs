//! Profile photos and project images (2026-11-17): a PNG or JPEG of at
//! most 512×512 pixels and 1 MiB, stored in the directory database. A
//! person's photo may instead point at their Google photo (`source_url`),
//! taken from their Google sign-in until they upload their own.
use crate::directory::DirectoryError;
use runbook_model::{OrganizationId, ProjectId, UserId};
use std::future::Future;

/// The largest side, in pixels.
pub const MAX_SIDE: u32 = 512;
/// The largest file, in bytes.
pub const MAX_BYTES: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PictureOwner {
    User(UserId),
    Project(OrganizationId, ProjectId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// `image/png` or `image/jpeg`; empty with `source_url`.
    pub content_type: String,
    /// The image itself; empty with `source_url`.
    pub bytes: Vec<u8>,
    /// An `https://` image elsewhere (a Google photo) shown instead.
    pub source_url: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureError {
    /// Neither a PNG nor a JPEG.
    Format,
    /// Over [`MAX_SIDE`] pixels on a side, or over [`MAX_BYTES`].
    TooLarge,
}

impl PictureError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Format => "picture.unsupported_format",
            Self::TooLarge => "picture.too_large",
        }
    }
}

/// The content type and size of an uploaded image, read from its header
/// (PNG `IHDR`, JPEG start-of-frame); nothing else is decoded.
pub fn inspect(bytes: &[u8]) -> Result<(&'static str, u32, u32), PictureError> {
    if bytes.len() > MAX_BYTES {
        return Err(PictureError::TooLarge);
    }
    let (kind, width, height) = if let Some(size) = png_size(bytes) {
        ("image/png", size.0, size.1)
    } else if let Some(size) = jpeg_size(bytes) {
        ("image/jpeg", size.0, size.1)
    } else {
        return Err(PictureError::Format);
    };
    if width == 0 || height == 0 {
        return Err(PictureError::Format);
    }
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(PictureError::TooLarge);
    }
    Ok((kind, width, height))
}

fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if b.len() < 24 || b[..8] != SIGNATURE || &b[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(b[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(b[20..24].try_into().ok()?);
    Some((width, height))
}

fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 4 || b[0] != 0xff || b[1] != 0xd8 {
        return None;
    }
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xff {
            return None;
        }
        let marker = b[i + 1];
        // Fill bytes and markers without a length.
        if marker == 0xff {
            i += 1;
            continue;
        }
        if marker == 0xd8 || (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
        if len < 2 {
            return None;
        }
        // Start of frame (baseline, progressive, ...), not DHT/JPG/DAC.
        if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
            if i + 9 > b.len() {
                return None;
            }
            let height = u32::from(u16::from_be_bytes([b[i + 5], b[i + 6]]));
            let width = u32::from(u16::from_be_bytes([b[i + 7], b[i + 8]]));
            return Some((width, height));
        }
        i += 2 + len;
    }
    None
}

/// Where pictures are kept: beside the directory, in its database.
pub trait PictureStore {
    fn picture(
        &self,
        owner: &PictureOwner,
    ) -> impl Future<Output = Result<Option<Picture>, DirectoryError>>;
    /// Store `picture`, or remove the owner's picture with `None`.
    fn set_picture(
        &self,
        owner: &PictureOwner,
        picture: Option<Picture>,
    ) -> impl Future<Output = Result<(), DirectoryError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&width.to_be_bytes());
        b.extend_from_slice(&height.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        b
    }

    fn jpeg(width: u16, height: u16, progressive: bool) -> Vec<u8> {
        let mut b = vec![0xff, 0xd8];
        // An APP0 segment first, as most files have.
        b.extend_from_slice(&[0xff, 0xe0, 0, 16]);
        b.extend_from_slice(&[0; 14]);
        b.extend_from_slice(&[0xff, if progressive { 0xc2 } else { 0xc0 }, 0, 17, 8]);
        b.extend_from_slice(&height.to_be_bytes());
        b.extend_from_slice(&width.to_be_bytes());
        b.extend_from_slice(&[3; 12]);
        b
    }

    #[test]
    fn reads_png_and_jpeg_sizes() {
        assert_eq!(inspect(&png(512, 300)), Ok(("image/png", 512, 300)));
        assert_eq!(inspect(&jpeg(64, 48, false)), Ok(("image/jpeg", 64, 48)));
        assert_eq!(inspect(&jpeg(500, 512, true)), Ok(("image/jpeg", 500, 512)));
    }

    #[test]
    fn refuses_other_formats_and_large_images() {
        assert_eq!(inspect(b"GIF89a....."), Err(PictureError::Format));
        assert_eq!(inspect(b"<svg/>"), Err(PictureError::Format));
        assert_eq!(inspect(&png(513, 10)), Err(PictureError::TooLarge));
        assert_eq!(inspect(&jpeg(10, 600, false)), Err(PictureError::TooLarge));
        assert_eq!(inspect(&png(0, 10)), Err(PictureError::Format));
        let mut big = png(10, 10);
        big.resize(MAX_BYTES + 1, 0);
        assert_eq!(inspect(&big), Err(PictureError::TooLarge));
    }
}
