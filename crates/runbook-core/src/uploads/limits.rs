//! UP3: upload limits catalog and enforcement (PRD §14 "Uploads and images", §17).

use crate::uploads::sniff::{SniffedType, mime};
pub use runbook_model::UploadLimits;
use serde::{Deserialize, Serialize};

/// Maximum image byte size for the Free edition (1 MB).
pub const FREE_IMAGE_BYTES: u64 = 1024 * 1024;
/// Maximum other evidence file byte size for the Free edition (10 MB).
pub const FREE_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Maximum image byte size for the Paid edition (10 MB).
pub const PAID_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
/// Maximum other evidence file byte size for the Paid edition (25 MB).
pub const PAID_FILE_BYTES: u64 = 25 * 1024 * 1024;

/// Maximum width or height for uploaded images (8,000 px).
pub const IMAGE_MAX_DIMENSION: u32 = 8_000;
/// Images larger than this in either dimension are stored without a thumbnail (8,000 px).
pub const THUMBNAIL_MAX_DIMENSION: u32 = 8_000;

/// Maximum images per step run (10).
pub const IMAGES_PER_STEP_RUN: u32 = 10;
/// Maximum evidence files per execution, including images (100).
pub const FILES_PER_EXECUTION: u32 = 100;

/// Free edition storage quota per organization (5 GB).
pub const FREE_STORAGE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// Long edge clients downscale an over-limit image to (2,048 px).
pub const CLIENT_RESIZE_LONG_EDGE: u32 = 2_048;

/// Accepted image MIME types. SVG is never accepted; HEIC is refused for now (D51).
pub const ACCEPTED_IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Deployment / plan edition for upload limit defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edition {
    Free,
    Paid,
    SelfHosted,
}

impl Edition {
    pub fn limits(&self) -> UploadLimits {
        for_edition(*self)
    }
}

/// Construct [`UploadLimits`] for the given edition.
pub fn for_edition(edition: Edition) -> UploadLimits {
    match edition {
        Edition::Free => free(),
        Edition::Paid => paid(),
        Edition::SelfHosted => self_hosted(),
    }
}

/// Free edition upload limits (PRD §14, §17).
pub fn free() -> UploadLimits {
    UploadLimits {
        image_types: ACCEPTED_IMAGE_TYPES.iter().map(|&s| s.into()).collect(),
        image_bytes: Some(FREE_IMAGE_BYTES),
        file_bytes: Some(FREE_FILE_BYTES),
        image_max_dimension: Some(IMAGE_MAX_DIMENSION),
        image_max_pixels: None,
        thumbnail_max_dimension: Some(THUMBNAIL_MAX_DIMENSION),
        images_per_step_run: Some(IMAGES_PER_STEP_RUN),
        files_per_execution: Some(FILES_PER_EXECUTION),
        storage_bytes: Some(FREE_STORAGE_BYTES),
        client_resize_long_edge: CLIENT_RESIZE_LONG_EDGE,
    }
}

/// Paid edition upload limits (PRD §14, §17).
pub fn paid() -> UploadLimits {
    UploadLimits {
        image_types: ACCEPTED_IMAGE_TYPES.iter().map(|&s| s.into()).collect(),
        image_bytes: Some(PAID_IMAGE_BYTES),
        file_bytes: Some(PAID_FILE_BYTES),
        image_max_dimension: Some(IMAGE_MAX_DIMENSION),
        image_max_pixels: None,
        thumbnail_max_dimension: Some(THUMBNAIL_MAX_DIMENSION),
        images_per_step_run: Some(IMAGES_PER_STEP_RUN),
        files_per_execution: Some(FILES_PER_EXECUTION),
        storage_bytes: None,
        client_resize_long_edge: CLIENT_RESIZE_LONG_EDGE,
    }
}

/// CF12-5: a self-hosted Cloudflare deployment (the operator's own
/// account) drops the Free edition's per-run counts and 5 GB quota by
/// default, like any self-hosted install. File sizes stay: the signed
/// PUT is read whole into the isolate, whose 128 MB is the real bound.
pub fn cloudflare() -> UploadLimits {
    UploadLimits {
        images_per_step_run: None,
        files_per_execution: None,
        storage_bytes: None,
        ..free()
    }
}

/// Self-hosted default upload limits (PRD §14: no limits by default).
pub fn self_hosted() -> UploadLimits {
    UploadLimits {
        image_types: ACCEPTED_IMAGE_TYPES.iter().map(|&s| s.into()).collect(),
        image_bytes: None,
        file_bytes: None,
        image_max_dimension: None,
        image_max_pixels: None,
        thumbnail_max_dimension: Some(THUMBNAIL_MAX_DIMENSION),
        images_per_step_run: None,
        files_per_execution: None,
        storage_bytes: None,
        client_resize_long_edge: CLIENT_RESIZE_LONG_EDGE,
    }
}

/// Reason an upload was refused by limit or type policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadRefusal {
    TooLarge,
    TypeUnsupported,
    TooMany,
}

impl UploadRefusal {
    pub const TOO_LARGE: &'static str = "upload.too_large";
    pub const TYPE_UNSUPPORTED: &'static str = "upload.type_unsupported";
    pub const TOO_MANY: &'static str = "upload.too_many";

    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => Self::TOO_LARGE,
            Self::TypeUnsupported => Self::TYPE_UNSUPPORTED,
            Self::TooMany => Self::TOO_MANY,
        }
    }
}

impl std::fmt::Display for UploadRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code())
    }
}

impl std::error::Error for UploadRefusal {}

/// Check whether an upload of `file_type` and `size` is accepted under `limits`.
pub fn check(
    limits: &UploadLimits,
    file_type: SniffedType,
    size: u64,
) -> Result<(), UploadRefusal> {
    if file_type.is_image() {
        let m = mime(file_type);
        if !limits.image_types.iter().any(|t| t == m) {
            return Err(UploadRefusal::TypeUnsupported);
        }
        if limits.image_bytes.is_some_and(|max| size > max) {
            return Err(UploadRefusal::TooLarge);
        }
    } else if limits.file_bytes.is_some_and(|max| size > max) {
        return Err(UploadRefusal::TooLarge);
    }
    Ok(())
}

/// Check count limits for images and files.
pub fn check_counts(
    limits: &UploadLimits,
    is_image: bool,
    current_step_images: u32,
    current_execution_files: u32,
) -> Result<(), UploadRefusal> {
    if is_image
        && limits
            .images_per_step_run
            .is_some_and(|max| current_step_images >= max)
    {
        return Err(UploadRefusal::TooMany);
    }
    if limits
        .files_per_execution
        .is_some_and(|max| current_execution_files >= max)
    {
        return Err(UploadRefusal::TooMany);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloudflare_drops_counts_and_quota_but_keeps_file_sizes() {
        let limits = cloudflare();
        assert_eq!(limits.images_per_step_run, None);
        assert_eq!(limits.files_per_execution, None);
        assert_eq!(limits.storage_bytes, None);
        assert_eq!(limits.file_bytes, Some(FREE_FILE_BYTES));
        assert_eq!(limits.image_bytes, Some(FREE_IMAGE_BYTES));
        // Counts no longer refuse; a file over the memory-bound size does.
        assert!(check_counts(&limits, true, 10_000, 10_000).is_ok());
    }
}
