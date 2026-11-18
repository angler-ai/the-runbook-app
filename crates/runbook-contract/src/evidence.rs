//! UP1: evidence upload slots and finalize; UP2: the image pipeline, signed
//! read URLs and the run's evidence list (PRD §14 "Uploads and images", §9;
//! D49–D51).
use serde_json::{Value, json};

/// Describe one evidence operation in place.
pub(crate) fn describe(path: &str, operation: &mut Value) {
    let problem = super::problem_response;
    if path == "/v1/executions/{exe}/evidence" {
        return describe_list(operation);
    }
    let finalize = path.ends_with("/finalize");
    operation["x-implementation"] = json!("implemented");
    operation["x-package"] = json!("UP1");
    operation["x-decision"] = json!("D49");
    operation["x-authorization-alternatives"] = json!([
        {"credential":"execution_access","resource":"own_current_step_run_claim"},
        {"credential":"human","scopes":["executions:act"],"resource":"own_current_step_run_claim"}
    ]);
    operation["summary"] = json!(if finalize {
        "Finalize an uploaded evidence file"
    } else {
        "Request an evidence upload slot"
    });
    operation["description"] = json!(if finalize {
        "Implemented on Postgres (UP1, UP2). The caller must hold the step run's current claim (an execution access credential of the claiming family, or the claiming person). Checks the stored object's size, then its type from its first bytes (magic bytes, never the name or the declared type); on any mismatch the object is deleted, the slot is rejected and the evidence is not attached (422 upload.size_mismatch, upload.type_mismatch, or upload.type_unsupported for HEIC). Images then go through the image pipeline (D51): width and height (and a GIF's first frame) are read from the file header before any decoding and refused over limits.uploads.image_max_dimension or image_max_pixels, or when the pipeline's estimated memory would exceed its budget (16-bit RGB/RGBA PNGs above about 9 MP) (422 upload.image_too_large); the image is decoded under memory limits (422 upload.image_invalid when it cannot be) and re-encoded without any metadata (EXIF and GPS location, XMP, color profiles, text), after applying its EXIF orientation. The re-encoded file replaces the upload: PNG stays PNG, JPEG stays JPEG, WebP becomes lossless WebP and GIF becomes a PNG of its first frame, so `type`, `size`, `width` and `height` describe the stored file. Each image gets a thumbnail with a 320 px long edge. The stored file plus its thumbnail must fit limits.uploads.image_bytes (422 upload.processed_too_large) and the organization's storage, which counts stored bytes (422 upload.limit_reached); either refusal deletes the upload. One image is processed at a time per server: a finalize that would wait answers 503 server.busy with Retry-After and keeps its slot for a retry. `url` and `thumbnail_url` are signed read URLs on the user-content origin, valid one to two hours. A finalized slot is returned again on retry, and concurrent finalizes of one slot answer with the same file. Reports reference finalized evidence by evd_ ID in `evidence`; `screenshot` evidence needs an image, `file` any upload."
    } else {
        "Implemented on Postgres (UP1). The caller must hold the step run's current claim (an execution access credential of the claiming family, or the claiming person). Checks the /v1/meta upload limits before issuing anything: image types (PNG, JPEG, WebP, GIF; HEIC is refused with upload.type_unsupported for now, D51; never SVG), PDF, plain text, Markdown, CSV and JSON files, bytes per image or file, images per step run, files per execution and storage per organization. Returns an evd_ ID and a PUT URL on the user-content origin, signed with the server key and bound to the object, the exact size and the type, valid 15 minutes. A retry with the same Idempotency-Key returns the same slot. Slots never finalized are deleted after 24 hours. Answers 503 uploads.not_configured until the server has a signing key, a user-content origin and the magic-byte table (capabilities.uploads)."
    });
    operation["parameters"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .filter(|p| p["name"] == "Idempotency-Key")
        .for_each(|p| p["schema"]["maxLength"] = json!(256));
    if finalize {
        operation["responses"]["200"]["description"] = json!("The finalized evidence file.");
        operation["responses"]["422"] = problem(
            "The stored object does not match its slot, or the image is too large or unreadable; it was deleted.",
            &[
                "upload.size_mismatch",
                "upload.type_mismatch",
                "upload.type_unsupported",
                "upload.image_too_large",
                "upload.image_invalid",
                "upload.processed_too_large",
                "upload.limit_reached",
            ],
        );
        operation["responses"]["409"] = problem(
            "Nothing was uploaded yet, the slot was already rejected, or the caller no longer holds the claim.",
            &[
                "upload.not_uploaded",
                "upload.not_pending",
                "step.not_claimed",
            ],
        );
    } else {
        operation["responses"]["201"] = json!({"description":"The upload slot and its signed URL.","content":{"application/json":{"schema":{"$ref":"#/components/schemas/EvidenceSlotView"}}}});
        operation["responses"]["422"] = problem(
            "The request is over a limit or names a refused type; no slot is issued.",
            &[
                "request.invalid",
                "upload.type_unsupported",
                "upload.too_large",
                "upload.empty",
                "upload.invalid_name",
                "upload.limit_reached",
            ],
        );
        operation["responses"]["409"] = problem(
            "The caller no longer holds the claim, or the Idempotency-Key names another slot.",
            &["step.not_claimed", "upload.duplicate"],
        );
        operation["responses"]["428"] = problem(
            "Idempotency-Key is missing.",
            &["request.idempotency_key_required"],
        );
    }
    operation["responses"]["401"] = problem(
        "Missing or invalid credential.",
        &["auth.required", "auth.invalid_credential"],
    );
    operation["responses"]["403"] = problem(
        "The caller does not hold this step run's claim, or lacks executions:act.",
        &["auth.forbidden", "auth.scope_missing", "auth.csrf"],
    );
    operation["responses"]["404"] = problem("Unknown step run or slot.", &["resource.not_found"]);
    operation["responses"]["503"] = problem(
        "Uploads are not configured on this server, storage is unavailable, or (finalize) another image is being processed: retry after Retry-After.",
        &[
            "uploads.not_configured",
            "storage.unavailable",
            "server.busy",
        ],
    );
}

fn describe_list(operation: &mut Value) {
    let problem = super::problem_response;
    operation["x-implementation"] = json!("implemented");
    operation["x-package"] = json!("UP2");
    operation["x-decision"] = json!("D50");
    operation["x-required-scopes"] = json!(["executions:read"]);
    operation["summary"] = json!("List a run's evidence files");
    operation["description"] = json!(
        "Implemented on Postgres (UP2). executions:read on the run's project (PAT, service token or session); execution tokens are refused and get their files' URLs from finalize. Returns the run's finalized evidence files in upload order. `url` and `thumbnail_url` are signed read URLs on the user-content origin (D50), valid one to two hours and stable within the hour, so pages can cache images; the execution document references the same files by evd_ ID. Files are served with Content-Disposition (inline for images, attachment otherwise), X-Content-Type-Options: nosniff and Content-Security-Policy: sandbox; default-src 'none'. Answers 503 uploads.not_configured while uploads are off (capabilities.uploads)."
    );
    operation["responses"]["200"] = json!({"description":"The run's finalized evidence files.","content":{"application/json":{"schema":{"$ref":"#/components/schemas/EvidencePage"}}}});
    operation["responses"]["401"] = problem(
        "Missing or invalid credential.",
        &["auth.required", "auth.invalid_credential"],
    );
    operation["responses"]["403"] = problem(
        "An execution token, or a credential without executions:read.",
        &["auth.forbidden", "auth.scope_missing"],
    );
    operation["responses"]["404"] = problem(
        "Unknown run, or one the caller may not read.",
        &["resource.not_found"],
    );
    operation["responses"]["503"] = problem(
        "Uploads are not configured on this server, or storage is unavailable.",
        &["uploads.not_configured", "storage.unavailable"],
    );
}
