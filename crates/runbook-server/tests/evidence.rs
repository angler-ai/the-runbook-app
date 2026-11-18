//! UP1/UP2: evidence uploads end to end on PostgreSQL through the real
//! routers (PRD §14 "Uploads and images"; D49–D51, D32).
//!
//! - The agent holding the claim reserves a slot (limits first, HEIC
//!   refused), PUTs the bytes to the signed URL, finalizes, and reports the
//!   step with its `evd_` ID; the execution document lists it.
//! - A mislabeled file is deleted at finalize; a tampered URL is refused.
//! - Another run's token, and a person without the claim, are refused; the
//!   person holding a later step's claim uploads too.
//! - The drain sweeps slots still pending after 24 hours.
//!
//! - UP2: bytes go to the user-content listener (`usercontent_router`), which
//!   serves only `/u/*` with its safety headers and ignores cookies; the main
//!   listener no longer takes the PUT. Images come back re-encoded without
//!   EXIF, with a thumbnail, through signed read URLs.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use futures_util::FutureExt;
use image::{DynamicImage, ImageBuffer, ImageEncoder, ImageFormat, Rgb};
use runbook_core::{
    catalog::TenantScope,
    uploads::signing::{self, ReadGrant, ReadVariant, UploadGrant, UploadSigningKey},
};
use runbook_server::{
    config::{ServerConfig, UploadConfig},
    drain::{DrainSettings, Drainer},
    http::{Worker, router, usercontent_router},
};
use runbook_store_postgres::PostgresStore;
use serde_json::{Value, json};
use std::{panic::AssertUnwindSafe, time::Duration};
use tower::ServiceExt;

const USERCONTENT: &str = "https://usercontent.example";
const ORIGIN: &str = "https://runbooks.example";
const SHOT: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
metadata: {id: shot, title: Shot}\n\
spec:\n  steps:\n    - id: shot\n      title: Capture the dashboard\n      actor: agent\n      \
evidence: {required: [screenshot], accepted: [screenshot, link]}\n    \
- id: sign\n      title: Attach the change record\n      actor: human\n      \
evidence: {required: [file]}\n";

/// A real 20 × 15 PNG (finalize decodes images now).
fn png() -> Vec<u8> {
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(ImageBuffer::from_fn(20, 15, |x, y| {
        Rgb([x as u8 * 9, y as u8 * 9, 7])
    }))
    .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
    .unwrap();
    out
}
/// A little-endian EXIF (TIFF) block whose GPS IFD holds a latitude.
fn gps_exif() -> Vec<u8> {
    let mut t = b"II*\0".to_vec();
    t.extend(8u32.to_le_bytes());
    t.extend(1u16.to_le_bytes());
    t.extend(0x8825u16.to_le_bytes());
    t.extend(4u16.to_le_bytes());
    t.extend(1u32.to_le_bytes());
    t.extend(26u32.to_le_bytes());
    t.extend(0u32.to_le_bytes());
    // GPS IFD at 26: LatitudeRef "N" and Latitude (3 rationals at 44).
    t.extend(2u16.to_le_bytes());
    for (tag, kind, count, value) in [
        (1u16, 2u16, 2u32, u32::from_le_bytes(*b"N\0\0\0")),
        (2, 5, 3, 56),
    ] {
        t.extend(tag.to_le_bytes());
        t.extend(kind.to_le_bytes());
        t.extend(count.to_le_bytes());
        t.extend(value.to_le_bytes());
    }
    t.extend(0u32.to_le_bytes());
    for (num, den) in [(37u32, 1u32), (25, 1), (1932, 100)] {
        t.extend(num.to_le_bytes());
        t.extend(den.to_le_bytes());
    }
    t
}
/// A 1,000 × 400 JPEG carrying EXIF with GPS coordinates.
fn gps_jpeg() -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80);
    encoder.set_exif_metadata(gps_exif()).unwrap();
    ImageBuffer::from_fn(1000, 400, |x, y| {
        Rgb([(x % 251) as u8, (y % 241) as u8, 90])
    })
    .write_with_encoder(encoder)
    .unwrap();
    assert!(contains(&out, b"Exif\0\0") && contains(&out, b"N\0\0\0"));
    out
}
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
/// A PNG whose header claims 9,000 × 10 pixels; its data is junk.
fn oversized_png() -> Vec<u8> {
    let mut bytes = png();
    bytes[16..20].copy_from_slice(&9_000u32.to_be_bytes());
    bytes
}
fn pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.resize(300, b' ');
    bytes
}

#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
    text: String,
    etag: Option<String>,
    headers: axum::http::HeaderMap,
    bytes: Vec<u8>,
}
async fn call(
    app: &Router,
    token: Option<&str>,
    method: &str,
    path: &str,
    body: Value,
    key: Option<&str>,
    etag: Option<&str>,
) -> Reply {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    if let Some(etag) = etag {
        request = request.header("if-match", etag);
    }
    let body = if body.is_null() {
        Body::empty()
    } else {
        request = request.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    reply(app, request.body(body).unwrap()).await
}
async fn reply(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let etag = response
        .headers()
        .get("etag")
        .map(|v| v.to_str().unwrap().into());
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 4 * 1_048_576).await.unwrap();
    Reply {
        status,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text: String::from_utf8_lossy(&bytes).into_owned(),
        etag,
        headers,
        bytes: bytes.to_vec(),
    }
}

struct Fixture {
    _worker: Worker,
    app: Router,
    /// The user-content listener's router (D50).
    files: Router,
    org: String,
    database: runbook_store_postgres::DatabaseConfig,
    drainer: Drainer,
    admin: PostgresStore,
    owner: String,
    project: String,
}
impl Fixture {
    async fn new(
        db: &database::TestDatabase,
        uploads: UploadConfig,
        tweak: fn(&mut ServerConfig),
    ) -> Self {
        let mut config = ServerConfig::new(db.config().clone(), ORIGIN, "127.0.0.1:0").unwrap();
        config.uploads = uploads;
        tweak(&mut config);
        let issued = runbook_server::bootstrap::run(&config, &Default::default())
            .await
            .unwrap();
        let mut bytes = Vec::new();
        issued.write_once(&mut bytes).unwrap();
        let issued: Value = serde_json::from_slice(&bytes).unwrap();
        let worker = Worker::start(config.clone()).unwrap();
        let fixture = Self {
            app: router(worker.client()),
            files: usercontent_router(worker.client()),
            org: issued["org_id"].as_str().unwrap().into(),
            database: db.config().clone(),
            _worker: worker,
            drainer: Drainer::new(
                config,
                DrainSettings {
                    batch: 64,
                    lease_ms: 30_000,
                    idle: Duration::from_millis(10),
                },
            ),
            admin: PostgresStore::connect(db.config()).await.unwrap(),
            owner: issued["pat"].as_str().unwrap().into(),
            project: issued["project_id"].as_str().unwrap().into(),
        };
        let created = fixture
            .owner(
                "POST",
                &format!("/v1/projects/{}/runbooks", fixture.project),
                json!({"source": SHOT}),
                Some("create-shot"),
            )
            .await;
        assert_eq!(created.status, 201, "{}", created.text);
        let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
        let draft = fixture
            .owner(
                "GET",
                &format!("/v1/runbooks/{rb}/draft"),
                Value::Null,
                None,
            )
            .await;
        let etag = draft.etag;
        let published = call(
            &fixture.app,
            Some(&fixture.owner),
            "POST",
            &format!("/v1/runbooks/{rb}/versions"),
            json!({}),
            Some("publish-shot"),
            etag.as_deref(),
        )
        .await;
        assert_eq!(published.status, 201, "{}", published.text);
        fixture
    }
    async fn owner(&self, method: &str, path: &str, body: Value, key: Option<&str>) -> Reply {
        call(&self.app, Some(&self.owner), method, path, body, key, None).await
    }
    async fn drain(&mut self) -> usize {
        let mut swept = 0;
        for _ in 0..20 {
            let stats = self.drainer.drain_once().await.unwrap();
            swept += stats.uploads_swept;
            if !stats.busy() {
                break;
            }
        }
        swept
    }
    /// Start a run, activate it, mint an agent's execution token.
    async fn run(&mut self, n: u8) -> (String, String) {
        let started = self
            .owner(
                "POST",
                &format!("/v1/projects/{}/executions", self.project),
                json!({"runbook": "shot"}),
                Some(&format!("start-{n}")),
            )
            .await;
        assert_eq!(started.status, 201, "{}", started.text);
        let exe = started.body["id"].as_str().unwrap().to_owned();
        self.drain().await;
        let minted = self
            .owner(
                "POST",
                &format!("/v1/executions/{exe}/tokens"),
                json!({"session_id": format!("agent-{n}")}),
                Some(&format!("mint-{n}")),
            )
            .await;
        assert_eq!(minted.status, 201, "{}", minted.text);
        (
            exe,
            minted.body["access_token"].as_str().unwrap().to_owned(),
        )
    }
    async fn ready(&self, token: &str, exe: &str, path: &str) -> String {
        let brief = call(
            &self.app,
            Some(token),
            "GET",
            &format!("/v1/executions/{exe}/next"),
            Value::Null,
            None,
            None,
        )
        .await;
        brief.body["ready_work"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["path"] == path)
            .unwrap_or_else(|| panic!("{path} not ready: {}", brief.text))["step_run_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    async fn slot(&self, token: &str, sr: &str, ty: &str, size: usize, key: &str) -> Reply {
        call(
            &self.app,
            Some(token),
            "POST",
            &format!("/v1/step-runs/{sr}/evidence"),
            json!({"name": "dashboard.png", "type": ty, "size": size}),
            Some(key),
            None,
        )
        .await
    }
    async fn finalize(&self, token: &str, sr: &str, evd: &str) -> Reply {
        call(
            &self.app,
            Some(token),
            "POST",
            &format!("/v1/step-runs/{sr}/evidence/{evd}/finalize"),
            Value::Null,
            Some(&format!("finalize-{evd}")),
            None,
        )
        .await
    }
    /// PUT `bytes` to a signed URL on the user-content listener.
    async fn put(&self, url: &str, ty: &str, bytes: Vec<u8>) -> Reply {
        self.put_to(&self.files, url, ty, bytes).await
    }
    async fn put_to(&self, app: &Router, url: &str, ty: &str, bytes: Vec<u8>) -> Reply {
        let path = url.strip_prefix(USERCONTENT).expect("user-content URL");
        reply(
            app,
            Request::builder()
                .method("PUT")
                .uri(path)
                .header("content-type", ty)
                .header("cookie", "runbook.session_token=stolen")
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
    }
    /// GET a signed read URL on the user-content listener.
    async fn fetch(&self, url: &str) -> Reply {
        let path = url.strip_prefix(USERCONTENT).expect("user-content URL");
        reply(
            &self.files,
            Request::builder()
                .uri(path)
                .header("cookie", "runbook.session_token=stolen")
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }
    fn admin_config(&self) -> runbook_store_postgres::DatabaseConfig {
        self.database.clone()
    }
    fn scope(&self) -> TenantScope {
        TenantScope {
            org_id: self.org.parse().unwrap(),
            project_id: self.project.parse().unwrap(),
        }
    }
    /// Claim `sr` as the run's agent.
    async fn claim(&self, token: &str, sr: &str, n: u8) -> Value {
        let claimed = call(
            &self.app,
            Some(token),
            "POST",
            &format!("/v1/step-runs/{sr}/claim"),
            json!({"session_id": format!("agent-{n}"), "actor": {"kind": "agent", "agent_name": "claude"}}),
            Some(&format!("claim-{sr}")),
            None,
        )
        .await;
        assert_eq!(claimed.status, 200, "{}", claimed.text);
        claimed.body
    }
    /// Reserve, PUT and finalize one file; returns the finalize reply.
    async fn upload(&self, token: &str, sr: &str, ty: &str, bytes: Vec<u8>, key: &str) -> Reply {
        let slot = self.slot(token, sr, ty, bytes.len(), key).await;
        assert_eq!(slot.status, 201, "{}", slot.text);
        let put = self
            .put(slot.body["upload_url"].as_str().unwrap(), ty, bytes)
            .await;
        assert_eq!(put.status, 204, "{}", put.text);
        let evd = slot.body["evidence_id"].as_str().unwrap();
        self.finalize(token, sr, evd).await
    }
    async fn report(&self, token: &str, sr: &str, claim: &Value, evidence: Value) -> Reply {
        call(
            &self.app,
            Some(token),
            "POST",
            &format!("/v1/step-runs/{sr}/report"),
            json!({"step_run_id": sr, "claim": claim, "status": "succeeded", "evidence": evidence}),
            Some(&format!("report-{sr}-{evidence}")),
            None,
        )
        .await
    }
}

fn configured() -> UploadConfig {
    UploadConfig {
        signing_key: Some(UploadSigningKey::new([9; 32])),
        usercontent_origin: Some(USERCONTENT.into()),
        sniff: Some(runbook_core::uploads::sniff::sniff),
        ..Default::default()
    }
}

async fn with_fixture(uploads: UploadConfig, f: impl AsyncFnOnce(&mut Fixture)) {
    with_config(uploads, |_| {}, f).await;
}
async fn with_config(
    uploads: UploadConfig,
    tweak: fn(&mut ServerConfig),
    f: impl AsyncFnOnce(&mut Fixture),
) {
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut fixture = Fixture::new(&db, uploads, tweak).await;
        f(&mut fixture).await;
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn evidence_uploads_end_to_end() {
    with_fixture(configured(), async |f| {
        let meta = f.owner("GET", "/v1/meta", Value::Null, None).await;
        assert_eq!(meta.body["capabilities"]["uploads"], true, "{}", meta.text);
        assert_eq!(meta.body["limits"]["uploads"]["client_resize_long_edge"], 2048);
        // Every accepted image is decoded, so the pipeline's cap is advertised.
        assert_eq!(meta.body["limits"]["uploads"]["image_max_dimension"], 8000);
        assert!(!meta.text.contains("heic"), "{}", meta.text);

        let (exe, agent) = f.run(1).await;
        let sr = f.ready(&agent, &exe, "shot").await;
        // No claim yet: nothing to attach a slot to.
        let early = f.slot(&agent, &sr, "image/png", 300, "early").await;
        assert_eq!((early.status, early.body["code"].as_str()), (409, Some("step.not_claimed")));
        let claimed = call(
            &f.app,
            Some(&agent),
            "POST",
            &format!("/v1/step-runs/{sr}/claim"),
            json!({"session_id": "agent-1", "actor": {"kind": "agent", "agent_name": "claude", "model": "opus"}}),
            Some("claim-shot"),
            None,
        )
        .await;
        assert_eq!(claimed.status, 200, "{}", claimed.text);
        let claim = claimed.body.clone();

        // D51: HEIC is refused for now, before anything is issued.
        let heic = f.slot(&agent, &sr, "image/heic", 300, "heic").await;
        assert_eq!((heic.status, heic.body["code"].as_str()), (422, Some("upload.type_unsupported")));

        let shot = png();
        let len = shot.len();
        let slot = f.slot(&agent, &sr, "image/png", len, "shot-1").await;
        assert_eq!(slot.status, 201, "{}", slot.text);
        let evd = slot.body["evidence_id"].as_str().unwrap().to_owned();
        let url = slot.body["upload_url"].as_str().unwrap().to_owned();
        assert!(evd.starts_with("evd_"));
        assert!(url.starts_with(&format!("{USERCONTENT}/u/{exe}/{evd}?")), "{url}");
        assert_eq!(slot.body["headers"]["Content-Type"], "image/png");
        assert_eq!(slot.body["headers"]["Content-Length"], len.to_string());
        // A retry with the same key is the same slot.
        let again = f.slot(&agent, &sr, "image/png", len, "shot-1").await;
        assert_eq!((again.status, &again.body), (201, &slot.body));

        // Nothing uploaded yet; the report cannot use it either.
        let pending = f.finalize(&agent, &sr, &evd).await;
        assert_eq!((pending.status, pending.body["code"].as_str()), (409, Some("upload.not_uploaded")));
        let unready = f.report(&agent, &sr, &claim, json!([evd])).await;
        assert_eq!((unready.status, unready.body["code"].as_str()), (422, Some("evidence.not_finalized")));

        // UP2: the main listener no longer takes the signed PUT.
        let main = f.put_to(&f.app, &url, "image/png", shot.clone()).await;
        assert_ne!(main.status, 204, "{}", main.text);
        assert!(matches!(main.status, 401 | 404 | 415), "{} {}", main.status, main.text);
        let pending = f.finalize(&agent, &sr, &evd).await;
        assert_eq!(pending.body["code"], "upload.not_uploaded", "{}", pending.text);

        // The URL is bound to size and type; a tampered URL is refused.
        let tampered = url.replace(&format!("size={len}"), &format!("size={}", len + 1));
        let refused = f.put(&tampered, "image/png", vec![0; len + 1]).await;
        assert_eq!((refused.status, refused.body["code"].as_str()), (403, Some("upload.url_invalid")));
        let mut forged = url.clone();
        let last = forged.pop().unwrap();
        forged.push(if last == 'A' { 'B' } else { 'A' });
        let refused = f.put(&forged, "image/png", shot.clone()).await;
        assert_eq!((refused.status, refused.body["code"].as_str()), (403, Some("upload.url_invalid")));
        let short = f.put(&url, "image/png", shot[..len - 1].to_vec()).await;
        assert_eq!((short.status, short.body["code"].as_str()), (422, Some("upload.size_mismatch")));
        let relabeled = f.put(&url, "image/jpeg", shot.clone()).await;
        assert_eq!(
            (relabeled.status, relabeled.body["code"].as_str()),
            (422, Some("upload.type_mismatch"))
        );
        // An expired URL, signed with the server key, is refused.
        let key = UploadSigningKey::new([9; 32]);
        let expired = signing::upload_url(
            USERCONTENT,
            &key,
            &UploadGrant {
                scope: f.scope(),
                execution_id: exe.parse().unwrap(),
                evidence_id: evd.parse().unwrap(),
                size: len as u64,
                content_type: "image/png".into(),
                expires_at_ms: 1,
            },
        );
        let late = f.put(&expired, "image/png", shot.clone()).await;
        assert_eq!((late.status, late.body["code"].as_str()), (403, Some("upload.url_expired")));
        // Every user-content response carries the safety headers and no
        // cookie, whatever the request sent.
        for r in [&refused, &late] {
            assert_usercontent_headers(r);
        }
        let put = f.put(&url, "image/png", shot.clone()).await;
        assert_eq!(put.status, 204, "{}", put.text);
        assert_usercontent_headers(&put);

        let done = f.finalize(&agent, &sr, &evd).await;
        assert_eq!(done.status, 200, "{}", done.text);
        assert_eq!(done.body["state"], "finalized");
        assert_eq!(done.body["type"], "image/png");
        assert_eq!((done.body["width"].as_u64(), done.body["height"].as_u64()), (Some(20), Some(15)));
        let file = f.fetch(done.body["url"].as_str().unwrap()).await;
        assert_eq!(file.status, 200, "{}", file.text);
        assert_eq!(done.body["size"].as_u64(), Some(file.bytes.len() as u64));
        assert_eq!(file.headers["content-type"], "image/png");
        assert!(file.headers["content-disposition"].to_str().unwrap().starts_with("inline; filename=\"dashboard.png\""));
        assert_usercontent_headers(&file);
        // No CORS beyond the PUT: an image load needs none.
        assert!(file.headers.get("access-control-allow-origin").is_none());

        // A mislabeled file: declared PNG, bytes are a PDF. Deleted.
        let fake = f.slot(&agent, &sr, "image/png", 300, "shot-2").await;
        let fake_id = fake.body["evidence_id"].as_str().unwrap().to_owned();
        let put = f.put(fake.body["upload_url"].as_str().unwrap(), "image/png", pdf()).await;
        assert_eq!(put.status, 204, "{}", put.text);
        let mismatch = f.finalize(&agent, &sr, &fake_id).await;
        assert_eq!((mismatch.status, mismatch.body["code"].as_str()), (422, Some("upload.type_mismatch")));
        let objects: i64 = f
            .admin
            .client()
            .query_one(
                "SELECT count(*) FROM runbook.blob WHERE key LIKE '%' || $1 || '%'",
                &[&fake_id],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(objects, 0, "the mislabeled object is deleted");
        let twice = f.finalize(&agent, &sr, &fake_id).await;
        assert_eq!((twice.status, twice.body["code"].as_str()), (409, Some("upload.not_pending")));
        let reput = f.put(fake.body["upload_url"].as_str().unwrap(), "image/png", pdf()).await;
        assert_eq!((reput.status, reput.body["code"].as_str()), (409, Some("upload.not_pending")));

        // Another run's token, and a person without the claim: refused.
        let (_, other) = f.run(2).await;
        let foreign = f.slot(&other, &sr, "image/png", 300, "foreign").await;
        assert_eq!(foreign.status, 403, "{}", foreign.text);
        let person = f.slot(&f.owner, &sr, "image/png", 300, "person").await;
        assert_eq!((person.status, person.body["code"].as_str()), (403, Some("auth.forbidden")));

        // The report attaches the screenshot by evd_ ID.
        let reported = f
            .report(&agent, &sr, &claim, json!([evd, "https://example.test/dash"]))
            .await;
        assert_eq!(reported.status, 200, "{}", reported.text);

        // The person claims the next step and attaches a PDF as its file.
        let sign = f.ready(&f.owner, &exe, "sign").await;
        let claimed = f
            .owner(
                "POST",
                &format!("/v1/step-runs/{sign}/claim"),
                json!({"session_id": "person-1"}),
                Some("claim-sign"),
            )
            .await;
        assert_eq!(claimed.status, 200, "{}", claimed.text);
        let record = f.slot(&f.owner, &sign, "application/pdf", 300, "sign-1").await;
        assert_eq!(record.status, 201, "{}", record.text);
        let record_id = record.body["evidence_id"].as_str().unwrap().to_owned();
        let put = f
            .put(record.body["upload_url"].as_str().unwrap(), "application/pdf", pdf())
            .await;
        assert_eq!(put.status, 204, "{}", put.text);
        assert_eq!(f.finalize(&f.owner, &sign, &record_id).await.status, 200);
        let finalized = f.finalize(&f.owner, &sign, &record_id).await;
        assert_eq!(finalized.body["thumbnail_url"], Value::Null);
        let download = f.fetch(finalized.body["url"].as_str().unwrap()).await;
        assert_eq!(download.status, 200, "{}", download.text);
        assert_eq!(download.bytes, pdf(), "files that are not images are stored as uploaded");
        assert!(download.headers["content-disposition"].to_str().unwrap().starts_with("attachment;"));
        assert_usercontent_headers(&download);
        let reported = f
            .report(&f.owner, &sign, &claimed.body, json!([record_id]))
            .await;
        assert_eq!(reported.status, 200, "{}", reported.text);

        // The run's evidence list: finalized files with signed read URLs.
        let listed = f
            .owner("GET", &format!("/v1/executions/{exe}/evidence"), Value::Null, None)
            .await;
        assert_eq!(listed.status, 200, "{}", listed.text);
        let items = listed.body["items"].as_array().unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert_eq!(ids, [evd.as_str(), record_id.as_str()]);
        assert_eq!(items[0]["url"], done.body["url"], "stable within the hour");
        let fetched = f.fetch(items[0]["thumbnail_url"].as_str().unwrap()).await;
        assert_eq!(fetched.status, 200, "{}", fetched.text);
        let as_agent = call(
            &f.app,
            Some(&agent),
            "GET",
            &format!("/v1/executions/{exe}/evidence"),
            Value::Null,
            None,
            None,
        )
        .await;
        assert_eq!(as_agent.status, 403, "{}", as_agent.text);

        // The execution document references both files by evd_ ID.
        let document = f
            .owner("GET", &format!("/v1/executions/{exe}/report"), Value::Null, None)
            .await;
        assert_eq!(document.status, 200, "{}", document.text);
        assert_eq!(document.body["header"]["status"], "succeeded", "{}", document.text);
        let evidence: Vec<&Value> = document.body["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| &s["evidence"])
            .collect();
        assert_eq!(evidence[0], &json!([evd, "https://example.test/dash"]));
        assert_eq!(evidence[1], &json!([record_id]));

        // The sweeper removes slots still pending 24 hours after issue.
        let (exe3, agent3) = f.run(3).await;
        let sr3 = f.ready(&agent3, &exe3, "shot").await;
        let claimed = call(
            &f.app,
            Some(&agent3),
            "POST",
            &format!("/v1/step-runs/{sr3}/claim"),
            json!({"session_id": "agent-3", "actor": {"kind": "agent", "agent_name": "claude"}}),
            Some("claim-3"),
            None,
        )
        .await;
        assert_eq!(claimed.status, 200, "{}", claimed.text);
        let stale = f.slot(&agent3, &sr3, "image/png", png().len(), "stale").await;
        let stale_id = stale.body["evidence_id"].as_str().unwrap().to_owned();
        let put = f.put(stale.body["upload_url"].as_str().unwrap(), "image/png", png()).await;
        assert_eq!(put.status, 204, "{}", put.text);
        assert_eq!(f.drain().await, 0, "a fresh slot is kept");
        f.admin
            .client()
            .execute(
                "UPDATE runbook.evidence SET created_at_ms = created_at_ms - 86400001 \
                 WHERE evidence_id = $1",
                &[&stale_id],
            )
            .await
            .unwrap();
        assert_eq!(f.drain().await, 1);
        let left: i64 = f
            .admin
            .client()
            .query_one(
                "SELECT (SELECT count(*) FROM runbook.evidence WHERE evidence_id = $1) + \
                 (SELECT count(*) FROM runbook.blob WHERE key LIKE '%' || $1 || '%')",
                &[&stale_id],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(left, 0, "slot and object are gone");
        // Finalized slots are never swept.
        let kept: i64 = f
            .admin
            .client()
            .query_one(
                "SELECT count(*) FROM runbook.evidence WHERE evidence_id = $1",
                &[&evd],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(kept, 1);
    })
    .await;
}

/// D50: what every response on the user-content listener carries.
fn assert_usercontent_headers(r: &Reply) {
    assert_eq!(r.headers["x-content-type-options"], "nosniff", "{r:?}");
    assert_eq!(
        r.headers["content-security-policy"], "sandbox; default-src 'none'; frame-ancestors 'none'",
        "{r:?}"
    );
    assert_eq!(r.headers["x-frame-options"], "DENY", "{r:?}");
    assert!(r.headers.contains_key("content-disposition"), "{r:?}");
    assert!(r.headers.get("set-cookie").is_none(), "{r:?}");
}

/// D51 through the routes: a GPS-tagged photo comes back re-encoded without
/// EXIF, with a 320 px thumbnail; a header over the cap is refused before
/// decoding and deleted.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn images_are_stripped_and_thumbnailed_at_finalize() {
    with_fixture(configured(), async |f| {
        let (exe, agent) = f.run(1).await;
        let sr = f.ready(&agent, &exe, "shot").await;
        f.claim(&agent, &sr, 1).await;
        let photo = gps_jpeg();
        let done = f
            .upload(&agent, &sr, "image/jpeg", photo.clone(), "photo")
            .await;
        assert_eq!(done.status, 200, "{}", done.text);
        assert_eq!(done.body["type"], "image/jpeg");
        assert_eq!(
            (done.body["width"].as_u64(), done.body["height"].as_u64()),
            (Some(1000), Some(400))
        );
        let file = f.fetch(done.body["url"].as_str().unwrap()).await;
        assert_eq!(file.status, 200, "{}", file.text);
        assert_usercontent_headers(&file);
        assert_ne!(file.bytes, photo, "the original is replaced");
        assert!(!contains(&file.bytes, b"Exif"), "EXIF is gone");
        assert!(!contains(&file.bytes, b"N\0\0\0"), "GPS is gone");
        assert_eq!(done.body["size"].as_u64(), Some(file.bytes.len() as u64));
        let stored = image::load_from_memory(&file.bytes).unwrap();
        assert_eq!((stored.width(), stored.height()), (1000, 400));
        let thumb = f.fetch(done.body["thumbnail_url"].as_str().unwrap()).await;
        assert_eq!(thumb.status, 200, "{}", thumb.text);
        assert_usercontent_headers(&thumb);
        assert_eq!(thumb.headers["content-type"], "image/jpeg");
        assert!(!contains(&thumb.bytes, b"Exif"));
        let small = image::load_from_memory(&thumb.bytes).unwrap();
        assert_eq!((small.width(), small.height()), (320, 128));

        // Over the dimension cap by its header alone: refused, deleted.
        let big = f
            .upload(&agent, &sr, "image/png", oversized_png(), "big")
            .await;
        assert_eq!(
            (big.status, big.body["code"].as_str()),
            (422, Some("upload.image_too_large")),
            "{}",
            big.text
        );
        let left: i64 = f
            .admin
            .client()
            .query_one(
                "SELECT count(*) FROM runbook.blob b JOIN runbook.evidence e \
                 ON b.key LIKE '%' || e.evidence_id || '%' WHERE e.state = 'rejected'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(left, 0, "the refused image is deleted");
    })
    .await;
}

/// D50: the user-content listener serves only signed `/u/*` requests,
/// ignores cookies, and allows CORS only for the app origin's PUT.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn the_usercontent_listener_serves_only_signed_files() {
    with_fixture(configured(), async |f| {
        let (exe, agent) = f.run(1).await;
        let sr = f.ready(&agent, &exe, "shot").await;
        f.claim(&agent, &sr, 1).await;
        let done = f.upload(&agent, &sr, "image/png", png(), "shot").await;
        assert_eq!(done.status, 200, "{}", done.text);
        let url = done.body["url"].as_str().unwrap().to_owned();
        let evd = done.body["id"].as_str().unwrap().to_owned();

        // Anything but `/u/*` is 404 there, with the same headers.
        for (method, path) in [
            ("GET", "/"),
            ("GET", "/v1/meta"),
            ("GET", "/.well-known/runbook"),
            ("POST", "/v1/projects/x/executions"),
            ("GET", "/index.html"),
            ("GET", "/api/auth/ok"),
            ("DELETE", url.strip_prefix(USERCONTENT).unwrap()),
            ("POST", url.strip_prefix(USERCONTENT).unwrap()),
        ] {
            let r = reply(
                &f.files,
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("authorization", format!("Bearer {}", f.owner))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 404, "{method} {path}: {}", r.text);
            assert_usercontent_headers(&r);
        }

        // A cookie (even the app's session cookie) changes nothing.
        let plain = reply(
            &f.files,
            Request::builder()
                .uri(url.strip_prefix(USERCONTENT).unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let cookied = f.fetch(&url).await;
        assert_eq!(
            (plain.status, &plain.bytes),
            (cookied.status, &cookied.bytes)
        );
        assert!(cookied.headers.get("set-cookie").is_none());

        // Read URLs: tampered, expired, the wrong variant, an unknown file.
        let key = UploadSigningKey::new([9; 32]);
        let grant = |variant, expires_at_ms| ReadGrant {
            scope: f.scope(),
            execution_id: exe.parse().unwrap(),
            evidence_id: evd.parse().unwrap(),
            variant,
            expires_at_ms,
        };
        let mut tampered = url.clone();
        let last = tampered.pop().unwrap();
        tampered.push(if last == 'A' { 'B' } else { 'A' });
        let expired = signing::read_url(USERCONTENT, &key, &grant(ReadVariant::Original, 1));
        let swapped = url.replace(&format!("/{evd}?"), &format!("/{evd}/thumbnail?"));
        let other_key = signing::read_url(
            USERCONTENT,
            &UploadSigningKey::new([8; 32]),
            &grant(ReadVariant::Original, i64::MAX),
        );
        for (bad, code) in [
            (tampered, "upload.url_invalid"),
            (expired, "upload.url_expired"),
            (swapped, "upload.url_invalid"),
            (other_key, "upload.url_invalid"),
            (format!("{url}&size=1"), "upload.url_invalid"),
            (format!("{url}&sig=x"), "upload.url_invalid"),
        ] {
            let r = f.fetch(&bad).await;
            assert_eq!(
                (r.status, r.body["code"].as_str()),
                (403, Some(code)),
                "{bad}"
            );
            assert_usercontent_headers(&r);
        }
        let valid = signing::read_url(USERCONTENT, &key, &grant(ReadVariant::Original, i64::MAX));
        assert_eq!(f.fetch(&valid).await.status, 200);
        // A PUT signature does not read.
        let slot = f
            .slot(&agent, &sr, "image/png", png().len(), "second")
            .await;
        let put_url = slot.body["upload_url"].as_str().unwrap();
        assert_eq!(f.fetch(put_url).await.status, 403);

        // CORS: only the app origin, only for the PUT.
        let path = put_url.strip_prefix(USERCONTENT).unwrap();
        let preflight = |origin: &str| {
            Request::builder()
                .method("OPTIONS")
                .uri(path)
                .header("origin", origin)
                .header("access-control-request-method", "PUT")
                .header("access-control-request-headers", "content-type")
                .body(Body::empty())
                .unwrap()
        };
        let allowed = reply(&f.files, preflight(ORIGIN)).await;
        assert_eq!(allowed.status, 204, "{}", allowed.text);
        assert_eq!(allowed.headers["access-control-allow-origin"], ORIGIN);
        assert_eq!(allowed.headers["access-control-allow-methods"], "PUT");
        assert!(
            allowed
                .headers
                .get("access-control-allow-credentials")
                .is_none()
        );
        assert_usercontent_headers(&allowed);
        let foreign = reply(&f.files, preflight("https://evil.example")).await;
        assert_eq!(foreign.status, 404);
        assert!(foreign.headers.get("access-control-allow-origin").is_none());
        let put = reply(
            &f.files,
            Request::builder()
                .method("PUT")
                .uri(path)
                .header("origin", ORIGIN)
                .header("content-type", "image/png")
                .body(Body::from(png()))
                .unwrap(),
        )
        .await;
        assert_eq!(put.status, 204, "{}", put.text);
        assert_eq!(put.headers["access-control-allow-origin"], ORIGIN);
        let read = reply(
            &f.files,
            Request::builder()
                .uri(url.strip_prefix(USERCONTENT).unwrap())
                .header("origin", ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert!(read.headers.get("access-control-allow-origin").is_none());
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn uploads_are_refused_until_configured() {
    with_fixture(UploadConfig::default(), async |f| {
        let meta = f.owner("GET", "/v1/meta", Value::Null, None).await;
        assert_eq!(meta.body["capabilities"]["uploads"], false, "{}", meta.text);
        assert!(meta.body["limits"]["uploads"].is_object());
        let refused = f
            .owner(
                "POST",
                "/v1/step-runs/sr_00000000000000000000000001/evidence",
                json!({"name": "a.png", "type": "image/png", "size": 1}),
                Some("k"),
            )
            .await;
        assert_eq!(
            (refused.status, refused.body["code"].as_str()),
            (503, Some("uploads.not_configured"))
        );
        // The main listener has no upload PUT at all.
        let put = reply(
            &f.app,
            Request::builder()
                .method("PUT")
                .uri("/u/exe_00000000000000000000000001/evd_00000000000000000000000001?size=1&type=image/png&expires=1&sig=x")
                .header("content-type", "image/png")
                .body(Body::from(vec![0u8]))
                .unwrap(),
        )
        .await;
        assert!(matches!(put.status, 401 | 404 | 415), "{} {}", put.status, put.text);
        // The user-content listener refuses everything while uploads are off.
        let files = f
            .put("https://usercontent.example/u/exe_00000000000000000000000001/evd_00000000000000000000000001?size=1&type=image/png&expires=1&sig=x", "image/png", vec![0u8])
            .await;
        assert_eq!(files.status, 503, "{}", files.text);
        assert_usercontent_headers(&files);
    })
    .await;
}

// ---- UP2 fixes -----------------------------------------------------------

/// A body that records being polled, sends `first`, then never finishes
/// (a trickling client).
fn watched_body(read: std::sync::Arc<std::sync::atomic::AtomicBool>, first: Vec<u8>) -> Body {
    let stream = futures_util::stream::unfold((read, Some(first)), |(read, chunk)| async move {
        read.store(true, std::sync::atomic::Ordering::SeqCst);
        match chunk {
            Some(c) => Some((
                Ok::<_, std::io::Error>(axum::body::Bytes::from(c)),
                (read, None),
            )),
            None => {
                std::future::pending::<()>().await;
                None
            }
        }
    });
    Body::from_stream(stream)
}

fn path_of(url: &str) -> String {
    url.strip_prefix(USERCONTENT)
        .expect("user-content URL")
        .to_owned()
}

/// Fix 1: the URL, its signature and expiry, the request headers and the
/// slot's state are checked before any body byte is read or any body
/// budget is reserved.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn uploads_are_checked_before_any_body_byte_is_read() {
    with_fixture(configured(), async |f| {
        let (exe, agent) = f.run(1).await;
        let sr = f.ready(&agent, &exe, "shot").await;
        f.claim(&agent, &sr, 1).await;
        let shot = png();
        let len = shot.len();
        let slot = f.slot(&agent, &sr, "image/png", len, "pending").await;
        let url = slot.body["upload_url"].as_str().unwrap().to_owned();
        let evd = slot.body["evidence_id"].as_str().unwrap().to_owned();
        let done = f
            .upload(&agent, &sr, "image/png", shot.clone(), "done")
            .await;
        assert_eq!(done.status, 200, "{}", done.text);
        let done_slot = f.slot(&agent, &sr, "image/png", len, "done").await;
        let done_url = done_slot.body["upload_url"].as_str().unwrap().to_owned();

        let key = UploadSigningKey::new([9; 32]);
        let mut grant = UploadGrant {
            scope: f.scope(),
            execution_id: exe.parse().unwrap(),
            evidence_id: evd.parse().unwrap(),
            size: len as u64,
            content_type: "image/png".into(),
            expires_at_ms: 1,
        };
        let expired = signing::upload_url(USERCONTENT, &key, &grant);
        grant.expires_at_ms = i64::MAX;
        grant.scope.org_id = "org_00000000000000000000000009".parse().unwrap();
        let foreign = signing::upload_url(USERCONTENT, &key, &grant);
        let mut tampered = url.clone();
        let last = tampered.pop().unwrap();
        tampered.push(if last == 'A' { 'B' } else { 'A' });
        let moved = url.replace(
            &format!("org={}", f.org),
            "org=org_00000000000000000000000009",
        );
        for (target, content_type, length, status, code) in [
            (&tampered, "image/png", Some(len), 403, "upload.url_invalid"),
            (&moved, "image/png", Some(len), 403, "upload.url_invalid"),
            (&expired, "image/png", Some(len), 403, "upload.url_expired"),
            (&foreign, "image/png", Some(len), 403, "upload.url_invalid"),
            (&url, "image/jpeg", Some(len), 422, "upload.type_mismatch"),
            (
                &url,
                "image/png",
                Some(len + 1),
                422,
                "upload.size_mismatch",
            ),
            // Already finalized: refused before the body.
            (&done_url, "image/png", Some(len), 409, "upload.not_pending"),
        ] {
            let read = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut request = Request::builder()
                .method("PUT")
                .uri(path_of(target))
                .header("content-type", content_type);
            if let Some(length) = length {
                request = request.header("content-length", length);
            }
            let r = tokio::time::timeout(
                Duration::from_secs(5),
                reply(
                    &f.files,
                    request
                        .body(watched_body(read.clone(), vec![0; 16]))
                        .unwrap(),
                ),
            )
            .await
            .unwrap_or_else(|_| panic!("{code}: waited for a body"));
            assert_eq!(
                (r.status, r.body["code"].as_str()),
                (status, Some(code)),
                "{target}"
            );
            assert!(
                !read.load(std::sync::atomic::Ordering::SeqCst),
                "{code}: body was read"
            );
            assert_usercontent_headers(&r);
        }
        // The valid upload still works.
        assert_eq!(f.put(&url, "image/png", shot).await.status, 204);
        // A chunked body longer than the signed size is refused.
        let slot = f.slot(&agent, &sr, "image/png", len, "chunked").await;
        let chunked = futures_util::stream::iter(vec![
            Ok::<_, std::io::Error>(axum::body::Bytes::from(png())),
            Ok(axum::body::Bytes::from(vec![0u8; 10])),
        ]);
        let over = reply(
            &f.files,
            Request::builder()
                .method("PUT")
                .uri(path_of(slot.body["upload_url"].as_str().unwrap()))
                .header("content-type", "image/png")
                .header("transfer-encoding", "chunked")
                .body(Body::from_stream(chunked))
                .unwrap(),
        )
        .await;
        assert_eq!(
            (over.status, over.body["code"].as_str()),
            (422, Some("upload.size_mismatch")),
            "{}",
            over.text
        );
    })
    .await;
}

/// Fix 1: an upload that trickles its body holds only the user-content
/// listener's body budget, never the API's.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_trickling_upload_does_not_take_the_api_body_budget() {
    with_config(
        configured(),
        |config| config.transport.in_flight_body_bytes = 256 * 1024,
        async |f| {
            let (exe, agent) = f.run(1).await;
            let sr = f.ready(&agent, &exe, "shot").await;
            f.claim(&agent, &sr, 1).await;
            let size = 200 * 1024;
            let slot = f.slot(&agent, &sr, "image/png", size, "trickle").await;
            let read = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let request = Request::builder()
                .method("PUT")
                .uri(path_of(slot.body["upload_url"].as_str().unwrap()))
                .header("content-type", "image/png")
                .header("content-length", size)
                .body(watched_body(read.clone(), vec![0; 1024]))
                .unwrap();
            let trickle = tokio::spawn(f.files.clone().oneshot(request));
            for _ in 0..200 {
                if read.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(read.load(std::sync::atomic::Ordering::SeqCst), "the PUT is reading");
            // A 120 KiB API request still gets its body budget.
            let source = format!(
                "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: big, title: Big}}\n# {}\nspec:\n  steps:\n    - {{id: a, title: A}}\n",
                "x".repeat(120 * 1024)
            );
            let body = json!({"source": source}).to_string();
            let api = reply(
                &f.app,
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/projects/{}/runbooks", f.project))
                    .header("authorization", format!("Bearer {}", f.owner))
                    .header("idempotency-key", "big")
                    .header("content-type", "application/json")
                    .header("content-length", body.len())
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await;
            assert_ne!(api.body["code"], "server.busy", "{}", api.text);
            assert_eq!(api.status, 201, "{}", api.text);
            trickle.abort();
        },
    )
    .await;
}

/// Fix 3: one image is processed at a time; a finalize that would queue
/// behind it answers 503 `server.busy` with `Retry-After` and keeps its
/// slot. Files that are not images are not held up. Two finalizes of one
/// slot at once agree (fix 6 replaced the in-process guard).
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn finalize_answers_busy_instead_of_queueing_behind_an_image() {
    with_fixture(configured(), async |f| {
        let (exe, agent) = f.run(1).await;
        let sr = f.ready(&agent, &exe, "shot").await;
        f.claim(&agent, &sr, 1).await;
        let slot = f.slot(&agent, &sr, "image/png", png().len(), "held").await;
        let evd = slot.body["evidence_id"].as_str().unwrap().to_owned();
        assert_eq!(
            f.put(
                slot.body["upload_url"].as_str().unwrap(),
                "image/png",
                png()
            )
            .await
            .status,
            204
        );
        let held = f._worker.client().image_slot().try_acquire_owned().unwrap();
        let busy = f.finalize(&agent, &sr, &evd).await;
        assert_eq!(
            (busy.status, busy.body["code"].as_str()),
            (503, Some("server.busy")),
            "{}",
            busy.text
        );
        assert_eq!(busy.headers["retry-after"], "1");
        let pdf_done = f.upload(&agent, &sr, "application/pdf", pdf(), "pdf").await;
        assert_eq!(pdf_done.status, 200, "{}", pdf_done.text);
        drop(held);
        let done = f.finalize(&agent, &sr, &evd).await;
        assert_eq!(done.status, 200, "{}", done.text);

        // Two finalizes of one slot at once.
        let slot = f.slot(&agent, &sr, "image/png", png().len(), "twice").await;
        let evd = slot.body["evidence_id"].as_str().unwrap().to_owned();
        assert_eq!(
            f.put(
                slot.body["upload_url"].as_str().unwrap(),
                "image/png",
                png()
            )
            .await
            .status,
            204
        );
        let (a, b) = tokio::join!(f.finalize(&agent, &sr, &evd), f.finalize(&agent, &sr, &evd));
        for r in [&a, &b] {
            assert!(matches!(r.status, 200 | 503), "{}", r.text);
        }
        assert!(a.status == 200 || b.status == 200);
        let again = f.finalize(&agent, &sr, &evd).await;
        assert_eq!(again.status, 200, "{}", again.text);
        for r in [&a, &b] {
            if r.status == 200 {
                assert_eq!(r.body, again.body);
            }
        }
        let file = f.fetch(again.body["url"].as_str().unwrap()).await;
        assert_eq!(file.status, 200);
        assert_eq!(again.body["size"].as_u64(), Some(file.bytes.len() as u64));
    })
    .await;
}

/// Test gaps: reads across runs and tenants, unfinished slots, missing
/// thumbnails, HEAD, and CORS for a foreign origin.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn reads_stay_within_their_run_and_finished_files() {
    with_fixture(configured(), async |f| {
        let key = UploadSigningKey::new([9; 32]);
        let (exe, agent) = f.run(1).await;
        let sr = f.ready(&agent, &exe, "shot").await;
        f.claim(&agent, &sr, 1).await;
        let image = f.upload(&agent, &sr, "image/png", png(), "a").await;
        assert_eq!(image.status, 200, "{}", image.text);
        let (exe2, agent2) = f.run(2).await;
        let sr2 = f.ready(&agent2, &exe2, "shot").await;
        f.claim(&agent2, &sr2, 2).await;
        let other = f.upload(&agent2, &sr2, "image/png", png(), "b").await;
        let read = |scope: TenantScope, exe: &str, evd: &str, variant| {
            signing::read_url(
                USERCONTENT,
                &key,
                &ReadGrant {
                    scope,
                    execution_id: exe.parse().unwrap(),
                    evidence_id: evd.parse().unwrap(),
                    variant,
                    expires_at_ms: i64::MAX,
                },
            )
        };
        let other_id = other.body["id"].as_str().unwrap();
        let image_id = image.body["id"].as_str().unwrap();
        // Another run's file under this run: not found.
        let r = f
            .fetch(&read(f.scope(), &exe, other_id, ReadVariant::Original))
            .await;
        assert_eq!(r.status, 404, "{}", r.text);
        // This run's file under another tenant, or an unknown run.
        let mut foreign = f.scope();
        foreign.org_id = "org_00000000000000000000000009".parse().unwrap();
        let r = f
            .fetch(&read(foreign, &exe, image_id, ReadVariant::Original))
            .await;
        assert_eq!(
            (r.status, r.body["code"].as_str()),
            (403, Some("upload.url_invalid"))
        );
        let r = f
            .fetch(&read(
                f.scope(),
                "exe_00000000000000000000000099",
                image_id,
                ReadVariant::Original,
            ))
            .await;
        assert_eq!(
            (r.status, r.body["code"].as_str()),
            (403, Some("upload.url_invalid"))
        );

        // Pending and rejected slots, and a thumbnail of a non-image.
        let pending = f.slot(&agent, &sr, "image/png", 10, "pending").await;
        let pending_id = pending.body["evidence_id"].as_str().unwrap();
        assert_eq!(
            f.fetch(&read(f.scope(), &exe, pending_id, ReadVariant::Original))
                .await
                .status,
            404
        );
        let fake = f.slot(&agent, &sr, "image/png", 300, "fake").await;
        let fake_id = fake.body["evidence_id"].as_str().unwrap().to_owned();
        f.put(
            fake.body["upload_url"].as_str().unwrap(),
            "image/png",
            pdf(),
        )
        .await;
        assert_eq!(f.finalize(&agent, &sr, &fake_id).await.status, 422);
        assert_eq!(
            f.fetch(&read(f.scope(), &exe, &fake_id, ReadVariant::Original))
                .await
                .status,
            404
        );
        let doc = f.upload(&agent, &sr, "application/pdf", pdf(), "doc").await;
        let doc_id = doc.body["id"].as_str().unwrap();
        assert_eq!(
            f.fetch(&read(f.scope(), &exe, doc_id, ReadVariant::Thumbnail))
                .await
                .status,
            404
        );
        let streamed = f.fetch(doc.body["url"].as_str().unwrap()).await;
        assert_eq!(streamed.bytes, pdf());
        assert_eq!(streamed.headers["content-length"], pdf().len().to_string());

        // HEAD: the headers and length, no body.
        let head = reply(
            &f.files,
            Request::builder()
                .method("HEAD")
                .uri(path_of(image.body["url"].as_str().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(head.status, 200);
        assert_eq!(
            head.headers["content-length"],
            image.body["size"].to_string()
        );
        assert!(head.bytes.is_empty());
        assert_usercontent_headers(&head);

        // A PUT from a foreign origin gets no CORS grant.
        let slot = f
            .slot(&agent, &sr, "image/png", png().len(), "foreign-origin")
            .await;
        let put = reply(
            &f.files,
            Request::builder()
                .method("PUT")
                .uri(path_of(slot.body["upload_url"].as_str().unwrap()))
                .header("origin", "https://evil.example")
                .header("content-type", "image/png")
                .body(Body::from(png()))
                .unwrap(),
        )
        .await;
        assert_eq!(put.status, 204, "{}", put.text);
        assert!(put.headers.get("access-control-allow-origin").is_none());
    })
    .await;
}

/// A user with a PAT, as an operator would seed one.
async fn seed_user(
    f: &Fixture,
    org: &str,
    n: u8,
    project: Option<runbook_model::ProjectRole>,
    scopes: &[runbook_core::credentials::Scope],
) -> String {
    use runbook_core::{
        credentials::{
            ActorKind, CredentialPrincipal, CredentialRecord, CredentialStore, TokenKind,
            encode_secret, secret_hash,
        },
        directory::*,
    };
    use runbook_model::{OrganizationId, UserId};
    let db = f.admin_config();
    let org: OrganizationId = org.parse().unwrap();
    let directory = runbook_store_postgres::PostgresDirectory::new(db.clone());
    let user: UserId = format!("usr_{n:026}").parse().unwrap();
    let mut mutations = vec![];
    let revision = match directory.snapshot(&org).await {
        Ok(s) => s.revision,
        Err(_) => {
            mutations.push(DirectoryMutation::CreateOrganization {
                title: "Foreign".into(),
            });
            0
        }
    };
    mutations.push(DirectoryMutation::CreateUser(UserRecord {
        id: user.clone(),
        name: format!("user {n}"),
        active: true,
        role: OrgRole::Member,
    }));
    if let Some(role) = project {
        mutations.push(DirectoryMutation::GrantProjectRole {
            project_id: f.project.parse().unwrap(),
            user_id: user.clone(),
            role,
        });
    }
    directory
        .commit(DirectoryCommit {
            org_id: org.clone(),
            expected_revision: revision,
            mutations,
        })
        .await
        .unwrap();
    let secret = encode_secret(TokenKind::Personal, [n; 32]);
    runbook_store_postgres::PostgresCredentials::new(db)
        .insert(CredentialRecord {
            id: format!("tok_{n:026}").parse().unwrap(),
            kind: TokenKind::Personal,
            hash: secret_hash(&secret),
            principal: CredentialPrincipal::User {
                user_id: user,
                actor: ActorKind::Human,
            },
            org_id: org,
            scopes: scopes.iter().copied().collect(),
            projects: None,
            expires_at: Some(i64::MAX),
            revoked_at: None,
            last_used_at: None,
        })
        .await
        .unwrap();
    secret
}

/// Test gaps: the run's evidence list needs `executions:read` on the run's
/// project, in the run's organization.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn the_evidence_list_needs_read_access_to_the_run() {
    use runbook_core::credentials::Scope;
    use runbook_model::ProjectRole;
    with_fixture(configured(), async |f| {
        let (exe, _) = f.run(1).await;
        let path = format!("/v1/executions/{exe}/evidence");
        let org = f.org.clone();
        let outsider = seed_user(f, &org, 51, None, &[Scope::ExecutionsRead]).await;
        let narrow = seed_user(
            f,
            &org,
            52,
            Some(ProjectRole::Operator),
            &[Scope::RunbooksRead],
        )
        .await;
        let foreign = seed_user(
            f,
            "org_00000000000000000000000009",
            53,
            None,
            &[Scope::ExecutionsRead],
        )
        .await;
        for (token, status, code) in [
            (&outsider, 404, "resource.not_found"),
            (&narrow, 403, "auth.scope_missing"),
            (&foreign, 404, "resource.not_found"),
        ] {
            let r = call(&f.app, Some(token), "GET", &path, Value::Null, None, None).await;
            assert_eq!(
                (r.status, r.body["code"].as_str()),
                (status, Some(code)),
                "{}",
                r.text
            );
        }
        let reader = seed_user(
            f,
            &org,
            54,
            Some(ProjectRole::Viewer),
            &[Scope::ExecutionsRead],
        )
        .await;
        let r = call(&f.app, Some(&reader), "GET", &path, Value::Null, None, None).await;
        assert_eq!(r.status, 200, "{}", r.text);
    })
    .await;
}

/// A lossless WebP of `image` as an animation of two identical frames:
/// VP8X (animated) + ANIM + two ANMF chunks around its VP8L bitstream.
fn animated_webp(image: &DynamicImage) -> Vec<u8> {
    let mut still = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut still)
        .encode(
            image.as_bytes(),
            image.width(),
            image.height(),
            image.color().into(),
        )
        .unwrap();
    let at = still.windows(4).position(|w| w == b"VP8L").unwrap();
    let len = u32::from_le_bytes(still[at + 4..at + 8].try_into().unwrap()) as usize;
    let vp8l = &still[at..at + 8 + len + (len & 1)];
    let chunk = |tag: &[u8], body: Vec<u8>| {
        let mut c = tag.to_vec();
        c.extend((body.len() as u32).to_le_bytes());
        c.extend(&body);
        if body.len() % 2 == 1 {
            c.push(0);
        }
        c
    };
    let u24 = |v: u32| v.to_le_bytes()[..3].to_vec();
    let (w, h) = (image.width(), image.height());
    let mut vp8x = vec![0x02 | 0x10, 0, 0, 0];
    vp8x.extend(u24(w - 1));
    vp8x.extend(u24(h - 1));
    let mut anim = vec![0; 4];
    anim.extend(0u16.to_le_bytes());
    let mut frame = vec![];
    frame.extend(u24(0));
    frame.extend(u24(0));
    frame.extend(u24(w - 1));
    frame.extend(u24(h - 1));
    frame.extend(u24(100));
    frame.push(0);
    frame.extend(vp8l);
    let mut body = b"WEBP".to_vec();
    body.extend(chunk(b"VP8X", vp8x));
    body.extend(chunk(b"ANIM", anim));
    body.extend(chunk(b"ANMF", frame.clone()));
    body.extend(chunk(b"ANMF", frame));
    chunk(b"RIFF", body)
}

/// Test gaps: GIF to PNG (served as `.png`), animations and the pixel cap
/// through the routes.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn animations_gifs_and_the_pixel_cap_through_the_routes() {
    with_fixture(configured(), async |f| {
        let meta = f.owner("GET", "/v1/meta", Value::Null, None).await;
        assert_eq!(
            meta.body["limits"]["uploads"]["image_max_pixels"],
            4096 * 4096
        );
        let (exe, agent) = f.run(1).await;
        let sr = f.ready(&agent, &exe, "shot").await;
        f.claim(&agent, &sr, 1).await;
        let named = async |name: &str, ty: &str, bytes: Vec<u8>| {
            let slot = call(
                &f.app,
                Some(&agent),
                "POST",
                &format!("/v1/step-runs/{sr}/evidence"),
                json!({"name": name, "type": ty, "size": bytes.len()}),
                Some(name),
                None,
            )
            .await;
            assert_eq!(slot.status, 201, "{}", slot.text);
            let put = f
                .put(slot.body["upload_url"].as_str().unwrap(), ty, bytes)
                .await;
            assert_eq!(put.status, 204, "{}", put.text);
            f.finalize(&agent, &sr, slot.body["evidence_id"].as_str().unwrap())
                .await
        };
        // An animated GIF: its first frame, as a PNG named .png.
        let frame =
            |c: u8| image::Frame::new(ImageBuffer::from_pixel(24, 16, image::Rgba([c, 0, 0, 255])));
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            encoder.encode_frames([frame(200), frame(10)]).unwrap();
        }
        let done = named("anim.gif", "image/gif", gif).await;
        assert_eq!(done.status, 200, "{}", done.text);
        assert_eq!(done.body["type"], "image/png");
        assert_eq!(
            (done.body["width"].as_u64(), done.body["height"].as_u64()),
            (Some(24), Some(16))
        );
        let file = f.fetch(done.body["url"].as_str().unwrap()).await;
        assert_eq!(file.headers["content-type"], "image/png");
        assert!(
            file.headers["content-disposition"]
                .to_str()
                .unwrap()
                .contains("filename=\"anim.png\""),
            "{:?}",
            file.headers
        );
        let first = image::load_from_memory(&file.bytes).unwrap().to_rgba8();
        assert_eq!(first.get_pixel(3, 3).0, [200, 0, 0, 255]);
        // An animated WebP: its first frame, still WebP.
        let still = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(30, 20, Rgb([1, 2, 3])));
        let done = named("anim.webp", "image/webp", animated_webp(&still)).await;
        assert_eq!(done.status, 200, "{}", done.text);
        assert_eq!(done.body["type"], "image/webp");
        assert_eq!(
            (done.body["width"].as_u64(), done.body["height"].as_u64()),
            (Some(30), Some(20))
        );
        // The pixel cap: 4,096 × 4,096 passes, one more column does not.
        let solid = |w, h| {
            let mut out = Vec::new();
            DynamicImage::ImageLuma8(ImageBuffer::from_pixel(w, h, image::Luma([7])))
                .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
                .unwrap();
            out
        };
        let at = named("cap.png", "image/png", solid(4096, 4096)).await;
        assert_eq!(at.status, 200, "{}", at.text);
        assert_eq!(
            (at.body["width"].as_u64(), at.body["height"].as_u64()),
            (Some(4096), Some(4096))
        );
        let over = named("over.png", "image/png", solid(4097, 4096)).await;
        assert_eq!(
            (over.status, over.body["code"].as_str()),
            (422, Some("upload.image_too_large")),
            "{}",
            over.text
        );
    })
    .await;
}
