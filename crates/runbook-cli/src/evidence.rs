//! Evidence upload helper for step reports and standalone uploads (PRD §14, §22; plan §2.3 UP4).

use crate::agent::{Env, failed};
use crate::cliutil::usage;
use crate::session::Session;
use serde_json::{Value, json};
use std::path::Path;

/// Uploads a single evidence file for a step run:
/// 1. Reads the file and verifies credential store boundaries.
/// 2. Sniffs file type (refuses unrecognized or disallowed formats).
/// 3. Reads limits from `/v1/meta`, checking image dimensions and downscaling if `--resize`.
/// 4. Requests an upload slot `POST /v1/step-runs/{sr}/evidence`.
/// 5. Sends `PUT` with exact signed content type and length to `upload_url`.
/// 6. Finalizes via `POST /v1/step-runs/{sr}/evidence/{evd}/finalize`.
///
/// Returns the finalized `EvidenceView` JSON.
pub fn upload_evidence(
    env: &Env,
    session: &mut Session,
    sr: &str,
    file_path: &Path,
    resize: bool,
    json: bool,
) -> Result<Value, i32> {
    if let Some(store) = &env.store {
        store.refuse_inside(file_path).map_err(usage)?;
    }

    let mut bytes = std::fs::read(file_path)
        .map_err(|e| usage(format!("cannot read '{}': {e}", file_path.display())))?;

    let name = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("evidence")
        .to_string();

    let sniffed = runbook_core::uploads::sniff(&bytes).ok_or_else(|| {
        usage(format!(
            "unsupported or unrecognized file type for '{}'",
            file_path.display()
        ))
    })?;

    let mut content_type = sniffed.mime().to_string();

    // Check /v1/meta limits
    let meta = session.get("/v1/meta", &[]).ok();
    let upload_limits = meta
        .as_ref()
        .and_then(|m| m.get("limits"))
        .and_then(|l| l.get("uploads"));

    if sniffed.is_image() {
        if content_type == "image/heic" {
            return Err(usage(
                "image type 'image/heic' is not supported (HEIC uploads are refused)",
            ));
        }

        if let Some(allowed_types) = upload_limits
            .and_then(|u| u.get("image_types"))
            .and_then(|t| t.as_array())
        {
            let allowed: Vec<&str> = allowed_types.iter().filter_map(|v| v.as_str()).collect();
            if !allowed.is_empty() && !allowed.contains(&content_type.as_str()) {
                return Err(usage(format!(
                    "image type '{content_type}' is not accepted by the server"
                )));
            }
        }

        let max_dim = upload_limits
            .and_then(|u| u.get("image_max_dimension"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let max_pixels = upload_limits
            .and_then(|u| u.get("image_max_pixels"))
            .and_then(|v| v.as_u64());
        let client_resize_long_edge = upload_limits
            .and_then(|u| u.get("client_resize_long_edge"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(2048);

        if let Ok(size) = imagesize::blob_size(&bytes) {
            let w = size.width as u32;
            let h = size.height as u32;
            let pixels = w as u64 * h as u64;
            let exceeds_dim = max_dim.is_some_and(|d| w > d || h > d);
            let exceeds_pixels = max_pixels.is_some_and(|p| pixels > p);

            if exceeds_dim || exceeds_pixels {
                if !resize {
                    let reason = if exceeds_dim && exceeds_pixels {
                        format!(
                            "dimensions {w}x{h} exceed max dimension {} and total pixels {pixels} exceed max pixels {}",
                            max_dim.unwrap_or(0),
                            max_pixels.unwrap_or(0)
                        )
                    } else if exceeds_dim {
                        format!(
                            "dimensions {w}x{h} exceed max dimension {}",
                            max_dim.unwrap_or(0)
                        )
                    } else {
                        format!(
                            "total pixels {pixels} ({w}x{h}) exceed max pixels {}",
                            max_pixels.unwrap_or(0)
                        )
                    };
                    return Err(usage(format!(
                        "image '{}' exceeds server limits ({reason}); pass --resize to downscale",
                        file_path.display()
                    )));
                }

                // Downscale image
                let img = image::load_from_memory(&bytes).map_err(|e| {
                    usage(format!(
                        "cannot decode image '{}': {e}",
                        file_path.display()
                    ))
                })?;
                let resized = img.resize(
                    client_resize_long_edge,
                    client_resize_long_edge,
                    image::imageops::FilterType::Lanczos3,
                );
                let mut out_bytes = Vec::new();
                let format = match sniffed {
                    runbook_core::uploads::sniff::SniffedType::Png => image::ImageFormat::Png,
                    runbook_core::uploads::sniff::SniffedType::Jpeg => image::ImageFormat::Jpeg,
                    runbook_core::uploads::sniff::SniffedType::Webp => image::ImageFormat::WebP,
                    runbook_core::uploads::sniff::SniffedType::Gif => image::ImageFormat::Gif,
                    _ => image::ImageFormat::Png,
                };
                resized
                    .write_to(&mut std::io::Cursor::new(&mut out_bytes), format)
                    .map_err(|e| usage(format!("cannot encode resized image: {e}")))?;
                bytes = out_bytes;
                if let Some(new_sniff) = runbook_core::uploads::sniff(&bytes) {
                    content_type = new_sniff.mime().to_string();
                }
            }
        }

        if let Some(image_bytes) = upload_limits
            .and_then(|u| u.get("image_bytes"))
            .and_then(|v| v.as_u64())
            && bytes.len() as u64 > image_bytes
        {
            return Err(usage(format!(
                "image '{}' ({} bytes) exceeds maximum image size ({} bytes)",
                file_path.display(),
                bytes.len(),
                image_bytes
            )));
        }
    } else if let Some(file_bytes) = upload_limits
        .and_then(|u| u.get("file_bytes"))
        .and_then(|v| v.as_u64())
        && bytes.len() as u64 > file_bytes
    {
        return Err(usage(format!(
            "file '{}' ({} bytes) exceeds maximum file size ({} bytes)",
            file_path.display(),
            bytes.len(),
            file_bytes
        )));
    }

    // Step 4: Request upload slot
    let slot_key = crate::session::random_hex().map_err(usage)?;
    let slot_body = json!({
        "name": name,
        "type": content_type,
        "size": bytes.len() as u64,
    });
    let slot = session
        .post(
            &format!("/v1/step-runs/{sr}/evidence"),
            &slot_key,
            &slot_body,
        )
        .map_err(|e| failed(e, json))?;

    let evidence_id = slot["evidence_id"]
        .as_str()
        .or_else(|| slot["id"].as_str())
        .ok_or_else(|| usage("slot response missing evidence_id"))?;
    let upload_url = slot["upload_url"]
        .as_str()
        .ok_or_else(|| usage("slot response missing upload_url"))?;

    // Step 5: PUT to signed upload URL
    let put_url = if upload_url.starts_with("http://") || upload_url.starts_with("https://") {
        upload_url.to_string()
    } else {
        format!("{}{}", env.host.trim_end_matches('/'), upload_url)
    };
    // Never follow redirects with evidence bytes (the rest of the CLI refuses
    // them too): a redirect could send the file somewhere else.
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(std::time::Duration::from_secs(300))
        .build();
    let mut req = agent.put(&put_url);
    for (name, value) in put_headers(&content_type, &slot) {
        req = req.set(&name, &value);
    }
    req.send_bytes(&bytes).map_err(|e| {
        eprintln!("error: evidence upload PUT failed: {e}");
        1
    })?;

    // Step 6: Finalize upload
    let finalize_key = crate::session::random_hex().map_err(usage)?;
    let finalized = session
        .post(
            &format!("/v1/step-runs/{sr}/evidence/{evidence_id}/finalize"),
            &finalize_key,
            &json!({}),
        )
        .map_err(|e| failed(e, json))?;

    Ok(finalized)
}

/// The headers for the signed PUT: the sniffed type, then the slot's
/// headers, each name once whatever its case (a slot header replaces ours).
/// ureq replaces only exact-case names, and a repeated `Content-Type` reaches
/// a Workers server joined as "a, a", which the signed type check refuses.
fn put_headers(content_type: &str, slot: &Value) -> Vec<(String, String)> {
    let mut headers = vec![("content-type".to_owned(), content_type.to_owned())];
    if let Some(slot_headers) = slot["headers"].as_object() {
        for (name, value) in slot_headers {
            if let Some(value) = value.as_str() {
                headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
                headers.push((name.clone(), value.to_owned()));
            }
        }
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_headers_name_each_header_once_whatever_its_case() {
        // ureq replaces a header only on an exact-case name match, so the
        // sniffed `content-type` plus the slot's `Content-Type` went out
        // twice; the Workers runtime joins repeats ("a, a") and the
        // user-content host refused the PUT with 422 upload.type_mismatch.
        let slot = json!({"headers": {"Content-Type": "application/json", "Content-Length": "61"}});
        let headers = put_headers("application/json", &slot);
        let mut names: Vec<String> = headers
            .iter()
            .map(|(k, _)| k.to_ascii_lowercase())
            .collect();
        names.sort();
        assert_eq!(names, ["content-length", "content-type"]);
        let types: Vec<&str> = headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(types, ["application/json"]);
        // Without slot headers the sniffed type is sent.
        assert_eq!(
            put_headers("image/png", &json!({})),
            [("content-type".to_owned(), "image/png".to_owned())]
        );
        // The slot's type is the one the URL is signed for, so it wins.
        let other = json!({"headers": {"CONTENT-TYPE": "text/plain"}});
        assert_eq!(
            put_headers("application/json", &other),
            [("CONTENT-TYPE".to_owned(), "text/plain".to_owned())]
        );
    }
}
