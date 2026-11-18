//! Evidence upload wire types (PRD §14 "Uploads and images"; D49–D51).
//! Frozen at T0 as planned operations; UP1 implements them.
use crate::*;

/// `/v1/meta` `limits.uploads`: the server's configured upload limits.
/// Null means no limit (the self-hosted default). Clients shrink images over
/// a limit before uploading; the server enforces whatever is configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UploadLimits {
    /// Accepted evidence image types. Never SVG; HEIC is refused for now (D51).
    pub image_types: Vec<String>,
    /// Bytes per image.
    pub image_bytes: Option<u64>,
    /// Bytes per other evidence file (logs, documents).
    pub file_bytes: Option<u64>,
    /// Largest accepted width or height, read from the header before decoding.
    pub image_max_dimension: Option<u32>,
    /// Largest accepted width × height, read from the header before
    /// decoding (UP2: bounds the image pipeline's memory). Null means no
    /// limit beyond `image_max_dimension`.
    #[serde(default)]
    pub image_max_pixels: Option<u64>,
    /// Images larger than this in either dimension are stored without a thumbnail.
    pub thumbnail_max_dimension: Option<u32>,
    pub images_per_step_run: Option<u32>,
    /// Evidence files per execution, images included.
    pub files_per_execution: Option<u32>,
    /// Bytes of uploads, logs and archives per organization.
    pub storage_bytes: Option<u64>,
    /// Long edge clients downscale an over-limit image to (2,048 px).
    pub client_resize_long_edge: u32,
}

/// `POST /v1/step-runs/{sr}/evidence`: ask for an upload slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateEvidenceRequest {
    /// File name to display; sanitized by the server.
    pub name: String,
    /// Declared MIME type; the signed URL is bound to it.
    #[serde(rename = "type")]
    pub content_type: String,
    /// Exact byte size; the signed URL is bound to it.
    pub size: u64,
}

/// The slot: PUT the exact bytes to `upload_url` on the user-content origin
/// before `expires_at_ms`, then finalize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvidenceSlotView {
    pub evidence_id: EvidenceId,
    pub upload_url: String,
    /// Headers the PUT must carry (Content-Type, Content-Length).
    pub headers: BTreeMap<String, String>,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Pending,
    Finalized,
    Rejected,
}

/// One evidence file; reports reference it by `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvidenceView {
    pub id: EvidenceId,
    pub step_run_id: StepRunId,
    pub name: String,
    /// Verified type once finalized, declared type before.
    #[serde(rename = "type")]
    pub content_type: String,
    pub size: u64,
    pub state: EvidenceState,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Signed read URL on the user-content origin (D50), valid one to two
    /// hours; null until finalized.
    pub url: Option<String>,
    /// Signed read URL of the image's 320 px thumbnail (D51); null for
    /// files that are not images.
    pub thumbnail_url: Option<String>,
    pub created_at_ms: i64,
    pub finalized_at_ms: Option<i64>,
}

/// `GET /v1/executions/{exe}/evidence`: a run's finalized evidence files in
/// upload order, with signed read URLs (UP2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvidencePage {
    pub items: Vec<EvidenceView>,
}

/// A finalized evidence file a report attached (D49): its `evd_` ID and the
/// type finalize verified from its bytes. Recorded in the report event so
/// replay re-checks `evidence.required`/`accepted` without the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvidenceFileRef {
    pub id: EvidenceId,
    #[serde(rename = "type")]
    pub content_type: String,
}
impl EvidenceFileRef {
    /// Images satisfy `screenshot` evidence; every file satisfies `file`.
    pub fn is_image(&self) -> bool {
        self.content_type.starts_with("image/")
    }
}

/// UP1/UP2: the implemented evidence operations (D49, D50).
pub const EVIDENCE_ROUTES: &[PublicRoute] = &[
    PublicRoute {
        method: "POST",
        path: "/v1/step-runs/{sr}/evidence",
        request: Some("CreateEvidenceRequest"),
        response: Some("EvidenceSlotView"),
        status: 201,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/step-runs/{sr}/evidence/{evd}/finalize",
        request: None,
        response: Some("EvidenceView"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/executions/{exe}/evidence",
        request: None,
        response: Some("EvidencePage"),
        status: 200,
    },
];
