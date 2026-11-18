//! UP4: Evidence upload tests against fake server (PRD §14, §22; plan §2.3).
mod fake_api;
use fake_api::*;
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb};
use serde_json::{Value, json};
use std::fs;

const SR: &str = "sr_01JABCDEFGHJKMNPQRSTVWXYZ1";
const CLAIM_REF: &str = "clm_01JABCDEFGHJKMNPQRSTVWXYZ3:1";

fn test_png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
        Rgb([(x % 255) as u8, (y % 255) as u8, 100])
    }))
    .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
    .unwrap();
    out
}

fn meta_limits(max_dim: u32, resize_edge: u32) -> Value {
    json!({
        "limits": {
            "uploads": {
                "image_types": ["image/png", "image/jpeg", "image/webp", "image/gif"],
                "image_bytes": 10 * 1024 * 1024,
                "file_bytes": 25 * 1024 * 1024,
                "image_max_dimension": max_dim,
                "client_resize_long_edge": resize_edge
            }
        }
    })
}

fn finalize_reply(evd_id: &str, sr: &str, size: usize) -> Reply {
    Reply::json(
        200,
        json!({
            "id": evd_id,
            "step_run_id": sr,
            "name": "photo.png",
            "type": "image/png",
            "size": size,
            "state": "finalized",
            "created_at_ms": 1700000000000i64,
            "finalized_at_ms": 1700000001000i64
        }),
    )
}

#[test]
fn step_evidence_single_upload_flow() {
    let home = tempfile::tempdir().unwrap();
    let img_bytes = test_png(20, 20);
    let img_file = home.path().join("photo.png");
    fs::write(&img_file, &img_bytes).unwrap();

    let (host, server) = serve(vec![
        Reply::json(200, meta_limits(8000, 2048)),
        // Slot response: we need host in upload_url, but host isn't known until listener binds.
        // fake_api Reply::json with relative path or host works since evidence.rs prepends host if needed.
        Reply::json(
            201,
            json!({
                "evidence_id": "evd_1",
                "upload_url": "/upload/evd_1",
                "headers": {
                    "content-type": "image/png"
                },
                "expires_at_ms": 1700000000000i64
            }),
        ),
        Reply::empty(204),
        finalize_reply("evd_1", SR, img_bytes.len()),
    ]);

    let o = cli(
        &host,
        home.path(),
        &["step", "evidence", SR, img_file.to_str().unwrap()],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("evidence evd_1 uploaded for sr_01JABCDEFGHJKMNPQRSTVWXYZ1"));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(line(&requests[0]), "GET /v1/meta");
    assert_eq!(
        line(&requests[1]),
        format!("POST /v1/step-runs/{SR}/evidence")
    );
    assert_eq!(line(&requests[2]), "PUT /upload/evd_1");
    assert_eq!(header(&requests[2], "content-type"), Some("image/png"));
    assert_eq!(
        line(&requests[3]),
        format!("POST /v1/step-runs/{SR}/evidence/evd_1/finalize")
    );
}

#[test]
fn step_evidence_json_output() {
    let home = tempfile::tempdir().unwrap();
    let img_bytes = test_png(10, 10);
    let img_file = home.path().join("photo.png");
    fs::write(&img_file, &img_bytes).unwrap();

    let (host, server) = serve(vec![
        Reply::json(200, meta_limits(8000, 2048)),
        Reply::json(
            201,
            json!({
                "evidence_id": "evd_2",
                "upload_url": "/upload/evd_2",
                "headers": {"content-type": "image/png"}
            }),
        ),
        Reply::empty(204),
        finalize_reply("evd_2", SR, img_bytes.len()),
    ]);

    let o = cli(
        &host,
        home.path(),
        &["step", "evidence", SR, img_file.to_str().unwrap(), "--json"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let parsed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(parsed["id"], "evd_2");
    assert_eq!(parsed["state"], "finalized");

    let _ = server.join().unwrap();
}

#[test]
fn step_report_with_local_evidence_file_replaces_with_id() {
    let home = tempfile::tempdir().unwrap();
    let img_bytes = test_png(15, 15);
    let img_file = home.path().join("screenshot.png");
    fs::write(&img_file, &img_bytes).unwrap();

    let (host, server) = serve(vec![
        Reply::json(200, meta_limits(8000, 2048)),
        Reply::json(
            201,
            json!({
                "evidence_id": "evd_reported",
                "upload_url": "/upload/evd_reported",
                "headers": {"content-type": "image/png"}
            }),
        ),
        Reply::empty(204),
        finalize_reply("evd_reported", SR, img_bytes.len()),
        Reply::json(200, json!({"step_run_id": SR, "status": "succeeded"})),
    ]);

    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "report",
            SR,
            "--status",
            "succeeded",
            "--claim",
            CLAIM_REF,
            "--evidence",
            img_file.to_str().unwrap(),
            "--json",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(line(&requests[0]), "GET /v1/meta");
    assert_eq!(
        line(&requests[1]),
        format!("POST /v1/step-runs/{SR}/evidence")
    );
    assert_eq!(line(&requests[2]), "PUT /upload/evd_reported");
    assert_eq!(
        line(&requests[3]),
        format!("POST /v1/step-runs/{SR}/evidence/evd_reported/finalize")
    );
    assert_eq!(
        line(&requests[4]),
        format!("POST /v1/step-runs/{SR}/report")
    );

    let report_body = body_of(&requests[4]);
    assert_eq!(
        report_body["evidence"],
        json!(["evd_reported"]),
        "Report body should replace local file path with returned evidence id"
    );
}

#[test]
fn step_report_repeated_evidence_mixes_url_existing_id_and_file() {
    let home = tempfile::tempdir().unwrap();
    let img_bytes = test_png(10, 10);
    let img_file = home.path().join("shot.png");
    fs::write(&img_file, &img_bytes).unwrap();

    let (host, server) = serve(vec![
        Reply::json(200, meta_limits(8000, 2048)),
        Reply::json(
            201,
            json!({
                "evidence_id": "evd_uploaded",
                "upload_url": "/upload/evd_uploaded"
            }),
        ),
        Reply::empty(204),
        finalize_reply("evd_uploaded", SR, img_bytes.len()),
        Reply::json(200, json!({"step_run_id": SR, "status": "succeeded"})),
    ]);

    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "report",
            SR,
            "--status",
            "succeeded",
            "--claim",
            CLAIM_REF,
            "--evidence",
            "https://example.com/log.txt",
            "--evidence",
            "evd_01JEXISTING1234567890",
            "--evidence",
            img_file.to_str().unwrap(),
            "--json",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));

    let requests = server.join().unwrap();
    let report_req = requests.iter().find(|r| r.contains("/report")).unwrap();
    let report_body = body_of(report_req);
    assert_eq!(
        report_body["evidence"],
        json!([
            "https://example.com/log.txt",
            "evd_01JEXISTING1234567890",
            "evd_uploaded"
        ])
    );
}

#[test]
fn step_evidence_unsupported_type_refused_early() {
    let home = tempfile::tempdir().unwrap();
    let svg_file = home.path().join("drawing.svg");
    fs::write(
        &svg_file,
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><circle cx=\"5\" cy=\"5\" r=\"5\"/></svg>",
    )
    .unwrap();

    let (host, server) = serve(vec![]);

    let o = cli(
        &host,
        home.path(),
        &["step", "evidence", SR, svg_file.to_str().unwrap()],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2), "{}", out(&o));
    assert!(err(&o).contains("unsupported or unrecognized file type"));

    let requests = server.join().unwrap();
    assert_eq!(
        requests.len(),
        0,
        "No network requests made on refused type"
    );
}

#[test]
fn step_evidence_oversize_refused_without_resize() {
    let home = tempfile::tempdir().unwrap();
    // 100x100 image, max dimension configured as 50
    let big_img = test_png(100, 100);
    let img_file = home.path().join("big.png");
    fs::write(&img_file, &big_img).unwrap();

    let (host, server) = serve(vec![Reply::json(200, meta_limits(50, 40))]);

    let o = cli(
        &host,
        home.path(),
        &["step", "evidence", SR, img_file.to_str().unwrap()],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2), "{}", out(&o));
    assert!(err(&o).contains("exceeds server limits"));
    assert!(err(&o).contains("pass --resize to downscale"));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1, "Only /v1/meta requested before refusal");
    assert_eq!(line(&requests[0]), "GET /v1/meta");
}

#[test]
fn step_evidence_oversize_downscaled_with_resize() {
    let home = tempfile::tempdir().unwrap();
    // 100x50 image, max dimension 50, resize to long edge 40
    let big_img = test_png(100, 50);
    let img_file = home.path().join("big.png");
    fs::write(&img_file, &big_img).unwrap();

    let (host, server) = serve(vec![
        Reply::json(200, meta_limits(50, 40)),
        Reply::json(
            201,
            json!({
                "evidence_id": "evd_resized",
                "upload_url": "/upload/evd_resized"
            }),
        ),
        Reply::empty(204),
        finalize_reply("evd_resized", SR, 500),
    ]);

    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "evidence",
            SR,
            img_file.to_str().unwrap(),
            "--resize",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("evidence evd_resized uploaded"));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    let slot_body = body_of(&requests[1]);
    let uploaded_size = slot_body["size"].as_u64().unwrap();
    assert!(
        uploaded_size < big_img.len() as u64,
        "Downscaled size ({uploaded_size}) should be smaller than original ({})",
        big_img.len()
    );
}

#[test]
fn step_report_nonexistent_file_returns_usage_error() {
    let home = tempfile::tempdir().unwrap();

    let (host, server) = serve(vec![]);

    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "report",
            SR,
            "--status",
            "succeeded",
            "--claim",
            CLAIM_REF,
            "--evidence",
            "nonexistent_image_file.png",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("evidence file 'nonexistent_image_file.png' not found"));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 0);
}
