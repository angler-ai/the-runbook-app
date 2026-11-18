//! UP1 upload use cases on the memory adapter: signed URLs, slot limits,
//! finalize (size and magic bytes, delete on mismatch), the report's file
//! references and the 24-hour sweeper. UP3 owns the real magic-byte table;
//! these tests use the minimal stand-in below.
use runbook_core::{
    ExecutionProjection, StoreError,
    blob::{BlobKey, BlobStore},
    catalog::TenantScope,
    store::memory::MemoryExecutionStore,
    uploads::{
        self, EvidenceState, NewSlot, UPLOAD_SLOT_SWEEP_MS, UPLOAD_URL_TTL_MS, UploadError,
        UploadStore,
        signing::{self, UploadGrant, UploadSigningKey, UploadUrlError},
        verify::SniffedType,
    },
};
use runbook_model::{
    CreateEvidenceRequest, EvidenceId, ExecutionId, ExecutionStatus, StepRunId, UploadLimits,
};

/// Test-only stand-in for UP3's table: PNG, JPEG, PDF and a HEIC brand.
fn stub_sniff(bytes: &[u8]) -> Option<SniffedType> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(SniffedType::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(SniffedType::Jpeg)
    } else if bytes.starts_with(b"%PDF-") {
        Some(SniffedType::Pdf)
    } else if bytes.get(4..12) == Some(b"ftypheic") {
        Some(SniffedType::Heic)
    } else {
        None
    }
}
fn png(len: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.resize(len, 0);
    bytes
}

fn scope(n: u8) -> TenantScope {
    TenantScope {
        org_id: format!("org_{n:026}").parse().unwrap(),
        project_id: format!("prj_{n:026}").parse().unwrap(),
    }
}
fn exe() -> ExecutionId {
    "exe_00000000000000000000000001".parse().unwrap()
}
fn sr(n: u8) -> StepRunId {
    format!("sr_{n:026}").parse().unwrap()
}
fn evd(n: u8) -> EvidenceId {
    format!("evd_{n:026}").parse().unwrap()
}
fn store() -> MemoryExecutionStore {
    let projection = ExecutionProjection {
        execution_id: exe(),
        runbook_id: "rb_00000000000000000000000001".parse().unwrap(),
        version_id: "rbv_00000000000000000000000001".parse().unwrap(),
        compiled_hash: [7; 32],
        engine_version: runbook_engine::ENGINE_VERSION,
        sequence: 0,
        status: ExecutionStatus::Running,
        phase: runbook_engine::ExecutionPhase::Ordinary,
        completed_at_ms: None,
        result: serde_json::json!(null),
        outputs: Default::default(),
        steps: vec![],
    };
    MemoryExecutionStore::new(scope(1), projection, 1_000_000)
}
/// The self-hosted column with a few limits set, as an operator would.
fn limits() -> UploadLimits {
    UploadLimits {
        image_types: ["image/png", "image/jpeg", "image/webp", "image/gif"]
            .map(String::from)
            .into(),
        image_bytes: Some(1_048_576),
        file_bytes: Some(10_485_760),
        image_max_dimension: Some(8_000),
        image_max_pixels: None,
        thumbnail_max_dimension: Some(8_000),
        images_per_step_run: Some(10),
        files_per_execution: Some(100),
        storage_bytes: None,
        client_resize_long_edge: 2_048,
    }
}
fn request(name: &str, content_type: &str, size: u64) -> CreateEvidenceRequest {
    CreateEvidenceRequest {
        name: name.into(),
        content_type: content_type.into(),
        size,
    }
}
fn ready<T>(f: impl std::future::Future<Output = T>) -> T {
    use std::{
        pin::pin,
        task::{Context, Poll, Waker},
    };
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory yielded"),
    }
}
fn reserve(
    s: &MemoryExecutionStore,
    n: u8,
    step: u8,
    req: &CreateEvidenceRequest,
) -> Result<uploads::EvidenceRecord, UploadError> {
    ready(uploads::reserve(
        s,
        NewSlot {
            scope: &scope(1),
            execution_id: &exe(),
            step_run_id: &sr(step),
            id: evd(n),
            request: req,
            limits: &limits(),
            created_by: "execution:fam_1",
        },
    ))
}
fn key(n: u8) -> BlobKey {
    BlobKey::Upload {
        execution_id: exe(),
        evd_id: evd(n),
    }
}
fn finalize(s: &MemoryExecutionStore, n: u8) -> Result<uploads::EvidenceRecord, UploadError> {
    ready(uploads::finalize(s, &scope(1), &sr(1), &evd(n), stub_sniff))
}

fn grant() -> UploadGrant {
    UploadGrant {
        scope: scope(1),
        execution_id: exe(),
        evidence_id: evd(1),
        size: 2048,
        content_type: "image/png".into(),
        expires_at_ms: 1_000_000 + UPLOAD_URL_TTL_MS,
    }
}

#[test]
fn signed_urls_bind_key_size_type_tenant_and_expiry() {
    let key = UploadSigningKey::new([7; 32]);
    let g = grant();
    let url = signing::upload_url("https://usercontent.example.test/", &key, &g);
    let (path, query) = url
        .strip_prefix("https://usercontent.example.test")
        .unwrap()
        .split_once('?')
        .unwrap();
    let query: std::collections::BTreeMap<String, String> = serde_urlencoded_like(query);
    // UP2 fixes: the URL carries the tenant, so the signature is checked
    // before any lookup; the host then requires the run to belong to it.
    assert_eq!(query["org"], scope(1).org_id.as_str());
    assert_eq!(query["prj"], scope(1).project_id.as_str());
    let parsed = signing::parse_upload(path, &query).unwrap();
    assert_eq!(parsed.grant(), g);
    let mut moved = query.clone();
    moved.insert("org".into(), scope(2).org_id.as_str().into());
    let moved = signing::parse_upload(path, &moved).unwrap();
    assert_eq!(
        signing::verify(&key, &moved.grant(), &moved.signature, 0),
        Err(UploadUrlError::Mismatch)
    );
    let sig = parsed.signature.clone();
    assert_eq!(signing::verify(&key, &g, &sig, g.expires_at_ms - 1), Ok(()));
    assert_eq!(
        signing::verify(&key, &g, &sig, g.expires_at_ms),
        Err(UploadUrlError::Expired)
    );
    // Every bound field, the tenant and the key change the signature.
    let mut tampered = vec![g.clone(); 6];
    tampered[0].size += 1;
    tampered[1].content_type = "image/jpeg".into();
    tampered[2].evidence_id = evd(2);
    tampered[3].expires_at_ms += 1;
    tampered[4].scope = scope(2);
    tampered[5].execution_id = "exe_00000000000000000000000002".parse().unwrap();
    for t in &tampered {
        assert_eq!(
            signing::verify(&key, t, &sig, 0),
            Err(UploadUrlError::Mismatch),
            "{t:?}"
        );
    }
    assert_eq!(
        signing::verify(&UploadSigningKey::new([8; 32]), &g, &sig, 0),
        Err(UploadUrlError::Mismatch)
    );
    // The PUT must carry exactly the bound length and type.
    assert_eq!(
        signing::check_put(&key, &g, &sig, 0, 2048, Some("image/png")),
        Ok(())
    );
    for (len, ty, error) in [
        (2047, Some("image/png"), UploadUrlError::SizeMismatch),
        (2048, Some("image/jpeg"), UploadUrlError::TypeMismatch),
        (2048, None, UploadUrlError::TypeMismatch),
    ] {
        assert_eq!(signing::check_put(&key, &g, &sig, 0, len, ty), Err(error));
    }
    assert!(!format!("{key:?}").contains('7'));
    // Not an upload path, or an extra parameter: malformed.
    assert_eq!(
        signing::parse_upload("/v1/u/x", &query).unwrap_err(),
        UploadUrlError::Malformed
    );
    let mut extra = query.clone();
    extra.insert("org".into(), "x".into());
    assert_eq!(
        signing::parse_upload(path, &extra).unwrap_err(),
        UploadUrlError::Malformed
    );
}
fn serde_urlencoded_like(query: &str) -> std::collections::BTreeMap<String, String> {
    query
        .split('&')
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap();
            (k.to_owned(), v.to_owned())
        })
        .collect()
}

#[test]
fn slot_requests_check_types_names_and_sizes_first() {
    let s = store();
    let code = |r: Result<_, UploadError>| r.unwrap_err().code();
    // D51: HEIC is refused for now; SVG and unknown types always.
    for ty in [
        "image/heic",
        "image/heif",
        "image/svg+xml",
        "application/zip",
        "Image/PNG",
    ] {
        assert_eq!(
            code(reserve(&s, 1, 1, &request("a", ty, 10))),
            "upload.type_unsupported",
            "{ty}"
        );
    }
    assert_eq!(
        code(reserve(&s, 1, 1, &request("a.png", "image/png", 1_048_577))),
        "upload.too_large"
    );
    assert_eq!(
        code(reserve(&s, 1, 1, &request("a.png", "image/png", 0))),
        "upload.empty"
    );
    assert_eq!(
        code(reserve(&s, 1, 1, &request(" /\\ ", "image/png", 10))),
        "upload.invalid_name"
    );
    // Documents have their own size limit.
    reserve(&s, 2, 1, &request("run.log", "text/plain", 1_048_577)).unwrap();
    let r = reserve(&s, 1, 1, &request("../x/shot\n.png", "image/png", 10)).unwrap();
    assert_eq!(r.name, "_x_shot_.png");
    assert_eq!(r.state, EvidenceState::Pending);
    assert_eq!(r.created_at_ms, 1_000_000);
    assert_eq!(r.url_expires_at_ms, 1_000_000 + UPLOAD_URL_TTL_MS);
    // A retry returns the same slot; another request under that ID is not.
    s.set_time(2_000_000);
    assert_eq!(
        reserve(&s, 1, 1, &request("../x/shot\n.png", "image/png", 10)).unwrap(),
        r
    );
    assert_eq!(
        code(reserve(&s, 1, 1, &request("other.png", "image/png", 10))),
        "upload.duplicate"
    );
    // A run outside the tenant reads as absent.
    let foreign = ready(uploads::reserve(
        &s,
        NewSlot {
            scope: &scope(2),
            execution_id: &exe(),
            step_run_id: &sr(1),
            id: evd(9),
            request: &request("a.png", "image/png", 10),
            limits: &limits(),
            created_by: "human:usr_1",
        },
    ));
    assert_eq!(foreign, Err(UploadError::Store(StoreError::NotFound)));
}

#[test]
fn finalize_verifies_size_then_magic_bytes_and_deletes_on_mismatch() {
    let s = store();
    for (n, ty) in [
        (1, "image/png"),
        (2, "image/png"),
        (3, "image/png"),
        (4, "application/pdf"),
    ] {
        reserve(&s, n, 1, &request("f", ty, 64)).unwrap();
    }
    reserve(&s, 5, 1, &request("f", "image/png", 64)).unwrap();
    // Nothing uploaded yet: the slot stays pending.
    assert_eq!(finalize(&s, 1).unwrap_err().code(), "upload.not_uploaded");
    // Wrong size.
    ready(s.put(key(1), &png(63))).unwrap();
    assert_eq!(finalize(&s, 1).unwrap_err().code(), "upload.size_mismatch");
    assert_eq!(ready(s.get(&key(1))), Err(StoreError::NotFound));
    // Declared PNG, bytes are JPEG.
    let mut jpeg = vec![0xff, 0xd8, 0xff];
    jpeg.resize(64, 0);
    ready(s.put(key(2), &jpeg)).unwrap();
    assert_eq!(finalize(&s, 2).unwrap_err().code(), "upload.type_mismatch");
    assert_eq!(ready(s.head(&key(2))), Err(StoreError::NotFound));
    // HEIC bytes are refused as unsupported (D51), whatever was declared.
    let mut heic = b"\0\0\0\x18ftypheic".to_vec();
    heic.resize(64, 0);
    ready(s.put(key(5), &heic)).unwrap();
    assert_eq!(
        finalize(&s, 5).unwrap_err().code(),
        "upload.type_unsupported"
    );
    assert_eq!(ready(s.head(&key(5))), Err(StoreError::NotFound));
    for n in [1, 2, 5] {
        let slot = ready(s.get_evidence(&scope(1), &evd(n))).unwrap().unwrap();
        assert_eq!(slot.state, EvidenceState::Rejected);
        assert_eq!(finalize(&s, n).unwrap_err().code(), "upload.not_pending");
    }
    // Good bytes verify, from the stored object; a repeat returns the slot.
    ready(s.put(key(3), &png(64))).unwrap();
    s.set_time(1_000_500);
    let done = finalize(&s, 3).unwrap();
    assert_eq!(done.state, EvidenceState::Finalized);
    assert_eq!(done.finalized_at_ms, Some(1_000_500));
    let verified = done.verified.clone().unwrap();
    assert_eq!(
        (verified.size, verified.content_type.as_str()),
        (64, "image/png")
    );
    assert_eq!(finalize(&s, 3).unwrap(), done);
    assert_eq!(ready(s.get(&key(3))).unwrap(), png(64));
    let mut pdf = b"%PDF-1.7".to_vec();
    pdf.resize(64, b' ');
    ready(s.put(key(4), &pdf)).unwrap();
    assert_eq!(
        finalize(&s, 4).unwrap().verified.unwrap().content_type,
        "application/pdf"
    );
    // Another step run's path cannot finalize the slot.
    assert_eq!(
        ready(uploads::finalize(
            &s,
            &scope(1),
            &sr(2),
            &evd(3),
            stub_sniff
        )),
        Err(UploadError::Store(StoreError::NotFound))
    );
}

#[test]
fn reports_reference_only_finalized_files_of_their_step_run() {
    let s = store();
    reserve(&s, 1, 1, &request("a.png", "image/png", 64)).unwrap();
    reserve(&s, 2, 1, &request("b.png", "image/png", 64)).unwrap();
    reserve(&s, 3, 2, &request("c.png", "image/png", 64)).unwrap();
    for n in [1, 3] {
        ready(s.put(key(n), &png(64))).unwrap();
        ready(uploads::finalize(
            &s,
            &scope(1),
            &sr(if n == 3 { 2 } else { 1 }),
            &evd(n),
            stub_sniff,
        ))
        .unwrap();
    }
    let files = |entries: &[&str]| {
        ready(uploads::report_files(
            &s,
            &scope(1),
            &exe(),
            &sr(1),
            &entries.iter().map(|e| e.to_string()).collect::<Vec<_>>(),
        ))
    };
    let one = evd(1).to_string();
    let found = files(&["https://example.test/a", &one, &one]).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, evd(1));
    assert_eq!(found[0].content_type, "image/png");
    assert!(found[0].is_image());
    assert_eq!(
        files(&[&evd(2).to_string()]).unwrap_err().code(),
        "evidence.not_finalized"
    );
    // Another step run's file, and an unknown ID, are not this report's.
    for other in [evd(3), evd(9)] {
        assert_eq!(
            files(&[&other.to_string()]).unwrap_err().code(),
            "evidence.not_found"
        );
    }
}

#[test]
fn the_sweeper_removes_slots_still_pending_after_24_hours() {
    let s = store();
    reserve(&s, 1, 1, &request("old.png", "image/png", 64)).unwrap();
    reserve(&s, 2, 1, &request("done.png", "image/png", 64)).unwrap();
    for n in [1, 2] {
        ready(s.put(key(n), &png(64))).unwrap();
    }
    finalize(&s, 2).unwrap();
    s.set_time(1_000_000 + UPLOAD_SLOT_SWEEP_MS);
    reserve(&s, 3, 1, &request("new.png", "image/png", 64)).unwrap();
    // Exactly 24 hours old is not yet stale.
    assert_eq!(ready(uploads::sweep_stale(&s, &scope(1), 10)), Ok(0));
    s.set_time(1_000_001 + UPLOAD_SLOT_SWEEP_MS);
    assert_eq!(ready(uploads::sweep_stale(&s, &scope(1), 10)), Ok(1));
    assert_eq!(ready(s.get_evidence(&scope(1), &evd(1))), Ok(None));
    assert_eq!(ready(s.get(&key(1))), Err(StoreError::NotFound));
    for n in [2, 3] {
        assert!(ready(s.get_evidence(&scope(1), &evd(n))).unwrap().is_some());
    }
    assert!(ready(s.get(&key(2))).is_ok());
    assert_eq!(ready(uploads::sweep_stale(&s, &scope(1), 10)), Ok(0));
}

/// UP2 (D50): signed read URLs for originals and thumbnails.
#[test]
fn read_urls_bind_object_variant_tenant_and_expiry() {
    use signing::{ReadGrant, ReadVariant};
    let key = UploadSigningKey::new([7; 32]);
    // One URL per hour, valid one to two hours: browsers can cache it.
    let hour = signing::UPLOAD_READ_URL_TTL_MS;
    assert_eq!(signing::read_expiry(5 * hour), 7 * hour);
    assert_eq!(signing::read_expiry(6 * hour - 1), 7 * hour);
    for variant in [ReadVariant::Original, ReadVariant::Thumbnail] {
        let g = ReadGrant {
            scope: scope(1),
            execution_id: exe(),
            evidence_id: evd(1),
            variant,
            expires_at_ms: 7 * hour,
        };
        let url = signing::read_url("https://usercontent.example.test/", &key, &g);
        let suffix = match variant {
            ReadVariant::Original => "",
            ReadVariant::Thumbnail => "/thumbnail",
        };
        let expected = format!(
            "https://usercontent.example.test/u/{}/{}{suffix}?",
            exe(),
            evd(1)
        );
        assert!(url.starts_with(&expected), "{url}");
        let (path, query) = url
            .strip_prefix("https://usercontent.example.test")
            .unwrap()
            .split_once('?')
            .unwrap();
        let query = serde_urlencoded_like(query);
        let parsed = signing::parse_read(path, &query).unwrap();
        assert_eq!(parsed.grant(), g);
        let mut moved = query.clone();
        moved.insert("prj".into(), scope(2).project_id.as_str().into());
        let moved = signing::parse_read(path, &moved).unwrap();
        assert_eq!(
            signing::verify_read(&key, &moved.grant(), &moved.signature, 0),
            Err(UploadUrlError::Mismatch)
        );
        let sig = parsed.signature.clone();
        assert_eq!(signing::verify_read(&key, &g, &sig, 7 * hour - 1), Ok(()));
        assert_eq!(
            signing::verify_read(&key, &g, &sig, 7 * hour),
            Err(UploadUrlError::Expired)
        );
        let mut tampered = vec![g.clone(); 5];
        tampered[0].evidence_id = evd(2);
        tampered[1].expires_at_ms += 1;
        tampered[2].scope = scope(2);
        tampered[3].variant = match variant {
            ReadVariant::Original => ReadVariant::Thumbnail,
            ReadVariant::Thumbnail => ReadVariant::Original,
        };
        tampered[4].execution_id = "exe_00000000000000000000000002".parse().unwrap();
        for t in &tampered {
            assert_eq!(
                signing::verify_read(&key, t, &sig, 0),
                Err(UploadUrlError::Mismatch)
            );
        }
        // Upload parameters, or any extra one, make a read URL malformed.
        let mut extra = query.clone();
        extra.insert("size".into(), "1".into());
        assert_eq!(
            signing::parse_read(path, &extra),
            Err(UploadUrlError::Malformed)
        );
    }
    // A PUT signature never reads, and a read signature never uploads.
    let put = grant();
    let put_sig = signing::sign(&key, &put);
    let read = ReadGrant {
        scope: scope(1),
        execution_id: exe(),
        evidence_id: evd(1),
        variant: ReadVariant::Original,
        expires_at_ms: put.expires_at_ms,
    };
    assert_eq!(
        signing::verify_read(&key, &read, &put_sig, 0),
        Err(UploadUrlError::Mismatch)
    );
    let read_sig = signing::sign_read(&key, &read);
    assert_eq!(
        signing::verify(&key, &put, &read_sig, 0),
        Err(UploadUrlError::Mismatch)
    );
    for bad in [
        format!("/u/{}/{}/original", exe(), evd(1)),
        format!("/u/{}/{}/thumbnail/x", exe(), evd(1)),
        format!("/u/{}", exe()),
        "/v1/x".to_owned(),
    ] {
        let q = serde_urlencoded_like(&format!(
            "org={}&prj={}&expires=1&sig=x",
            scope(1).org_id,
            scope(1).project_id
        ));
        assert_eq!(
            signing::parse_read(&bad, &q),
            Err(UploadUrlError::Malformed),
            "{bad}"
        );
    }
}

/// A stand-in for the host's D51 pipeline: it "strips" by dropping the
/// last 16 bytes and makes a 4-byte thumbnail; `bad` refuses.
struct StubImages {
    calls: std::cell::Cell<u32>,
}
impl uploads::ImagePipeline for StubImages {
    async fn process(
        &self,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<Option<uploads::ProcessedImage>, &'static str> {
        self.calls.set(self.calls.get() + 1);
        if bytes.ends_with(b"bad") {
            return Err("upload.image_too_large");
        }
        Ok(Some(uploads::ProcessedImage {
            bytes: bytes[..bytes.len() - 16].to_vec(),
            content_type: if content_type == "image/gif" {
                "image/png".into()
            } else {
                content_type.into()
            },
            width: 40,
            height: 20,
            thumbnail: b"thmb".to_vec(),
        }))
    }
}

fn finalize_images(
    s: &MemoryExecutionStore,
    n: u8,
    images: &StubImages,
    limits: &UploadLimits,
) -> Result<uploads::EvidenceRecord, UploadError> {
    ready(uploads::finalize_with(
        s,
        &scope(1),
        &sr(1),
        &evd(n),
        stub_sniff,
        images,
        limits,
    ))
}
fn stub() -> StubImages {
    StubImages {
        calls: Default::default(),
    }
}

#[test]
fn finalize_stores_processed_images_beside_the_original_then_deletes_it() {
    let s = store();
    reserve(&s, 1, 1, &request("a.png", "image/png", 64)).unwrap();
    reserve(&s, 2, 1, &request("b.png", "image/png", 64)).unwrap();
    reserve(&s, 3, 1, &request("c.pdf", "application/pdf", 64)).unwrap();
    let images = stub();
    ready(s.put(key(1), &png(64))).unwrap();
    let done = finalize_images(&s, 1, &images, &limits()).unwrap();
    let verified = done.verified.clone().unwrap();
    assert_eq!(
        verified,
        uploads::VerifiedUpload {
            size: 48,
            content_type: "image/png".into(),
            width: Some(40),
            height: Some(20),
            thumbnail: true,
            thumbnail_bytes: 4,
        }
    );
    // The processed bytes live under a derived key; the original (with its
    // metadata) is deleted only after the slot is marked.
    let stored = uploads::stored_key(&done);
    assert_eq!(
        stored,
        BlobKey::Upload {
            execution_id: exe(),
            evd_id: uploads::processed_id(&evd(1)),
        }
    );
    assert_eq!(ready(s.get(&stored)).unwrap(), png(48));
    assert_eq!(ready(s.head(&key(1))), Err(StoreError::NotFound));
    let thumb = uploads::thumbnail_key(&done);
    assert_eq!(
        thumb,
        BlobKey::Upload {
            execution_id: exe(),
            evd_id: uploads::thumbnail_id(&evd(1)),
        }
    );
    let ids = [
        evd(1),
        uploads::processed_id(&evd(1)),
        uploads::thumbnail_id(&evd(1)),
    ];
    assert!(ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2]);
    assert_eq!(ready(s.get(&thumb)).unwrap(), b"thmb");
    // Usage counts what is stored: the processed file and its thumbnail.
    assert_eq!(done.counted_bytes(), 52);
    // A repeat returns the slot without processing again.
    assert_eq!(finalize_images(&s, 1, &images, &limits()).unwrap(), done);
    assert_eq!(images.calls.get(), 1);
    let view = uploads::view(&done, Some("u".into()), Some("t".into()));
    assert_eq!((view.width, view.height), (Some(40), Some(20)));
    assert_eq!((view.size, view.content_type.as_str()), (48, "image/png"));
    assert_eq!(
        (view.url.as_deref(), view.thumbnail_url.as_deref()),
        (Some("u"), Some("t"))
    );

    // A refusal deletes the original and rejects the slot with its code.
    let mut bad = png(61);
    bad.extend(b"bad");
    ready(s.put(key(2), &bad)).unwrap();
    assert_eq!(
        finalize_images(&s, 2, &images, &limits())
            .unwrap_err()
            .code(),
        "upload.image_too_large"
    );
    assert_eq!(ready(s.head(&key(2))), Err(StoreError::NotFound));
    let slot = ready(s.get_evidence(&scope(1), &evd(2))).unwrap().unwrap();
    assert_eq!(slot.state, EvidenceState::Rejected);

    // Files that are not images never reach the pipeline and stay in place.
    let mut pdf = b"%PDF-1.7".to_vec();
    pdf.resize(64, b' ');
    ready(s.put(key(3), &pdf)).unwrap();
    let calls = images.calls.get();
    let done = finalize_images(&s, 3, &images, &limits()).unwrap();
    assert_eq!(images.calls.get(), calls);
    let verified = done.verified.clone().unwrap();
    assert_eq!(
        (verified.size, verified.thumbnail, verified.width),
        (64, false, None)
    );
    assert_eq!(uploads::stored_key(&done), key(3));
    assert_eq!(ready(s.get(&key(3))).unwrap(), pdf);
}

/// A store whose next `finalize_evidence` fails like a dropped connection.
struct Flaky {
    inner: MemoryExecutionStore,
    fail: std::cell::Cell<bool>,
}
impl UploadStore for Flaky {
    async fn upload_time_ms(&self) -> Result<i64, StoreError> {
        self.inner.upload_time_ms().await
    }
    async fn reserve_evidence(
        &self,
        scope: &TenantScope,
        record: uploads::EvidenceRecord,
        quota: &uploads::UploadQuota,
    ) -> Result<bool, StoreError> {
        self.inner.reserve_evidence(scope, record, quota).await
    }
    async fn get_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
    ) -> Result<Option<uploads::EvidenceRecord>, StoreError> {
        self.inner.get_evidence(scope, id).await
    }
    async fn finalize_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
        outcome: uploads::UploadOutcome,
        quota: &uploads::UploadQuota,
    ) -> Result<uploads::EvidenceRecord, StoreError> {
        if self.fail.replace(false) {
            return Err(StoreError::Backend("test.connection_reset"));
        }
        self.inner
            .finalize_evidence(scope, id, outcome, quota)
            .await
    }
    async fn list_evidence(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
    ) -> Result<Vec<uploads::EvidenceRecord>, StoreError> {
        self.inner.list_evidence(scope, execution_id).await
    }
    async fn stale_evidence(
        &self,
        scope: &TenantScope,
        before_ms: i64,
        limit: usize,
    ) -> Result<Vec<uploads::EvidenceRecord>, StoreError> {
        self.inner.stale_evidence(scope, before_ms, limit).await
    }
    async fn delete_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
    ) -> Result<bool, StoreError> {
        self.inner.delete_evidence(scope, id).await
    }
}
impl BlobStore for Flaky {
    async fn put(&self, key: BlobKey, bytes: &[u8]) -> Result<(), StoreError> {
        self.inner.put(key, bytes).await
    }
    async fn get(&self, key: &BlobKey) -> Result<Vec<u8>, StoreError> {
        self.inner.get(key).await
    }
    async fn get_range(
        &self,
        key: &BlobKey,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>, StoreError> {
        self.inner.get_range(key, range).await
    }
    async fn head(&self, key: &BlobKey) -> Result<runbook_core::blob::BlobHead, StoreError> {
        self.inner.head(key).await
    }
    async fn delete(&self, key: &BlobKey) -> Result<(), StoreError> {
        self.inner.delete(key).await
    }
    async fn list_prefix(
        &self,
        prefix: &runbook_core::blob::BlobPrefix,
    ) -> Result<Vec<BlobKey>, StoreError> {
        self.inner.list_prefix(prefix).await
    }
}

#[test]
fn a_transient_error_while_marking_keeps_the_upload() {
    let s = Flaky {
        inner: store(),
        fail: std::cell::Cell::new(true),
    };
    reserve(&s.inner, 1, 1, &request("a.png", "image/png", 64)).unwrap();
    ready(s.put(key(1), &png(64))).unwrap();
    let images = stub();
    let run = || {
        ready(uploads::finalize_with(
            &s,
            &scope(1),
            &sr(1),
            &evd(1),
            stub_sniff,
            &images,
            &limits(),
        ))
    };
    assert_eq!(
        run(),
        Err(UploadError::Store(StoreError::Backend(
            "test.connection_reset"
        )))
    );
    // Nothing was lost: the original is intact and the slot still pending.
    assert_eq!(ready(s.get(&key(1))).unwrap(), png(64));
    let slot = ready(s.get_evidence(&scope(1), &evd(1))).unwrap().unwrap();
    assert_eq!(slot.state, EvidenceState::Pending);
    // The retry finishes, writing the same processed bytes again.
    let done = run().unwrap();
    assert_eq!(ready(s.get(&uploads::stored_key(&done))).unwrap(), png(48));
    assert_eq!(ready(s.head(&key(1))), Err(StoreError::NotFound));
}

/// Two finalizes of one slot interleaved at the pipeline: both answer with
/// the same finalized slot, and its bytes are intact.
#[test]
fn concurrent_finalizes_of_one_slot_agree() {
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    let s = store();
    reserve(&s, 1, 1, &request("a.png", "image/png", 64)).unwrap();
    ready(s.put(key(1), &png(64))).unwrap();
    let images = YieldingImages(stub());
    let (sc, step, id, lim) = (scope(1), sr(1), evd(1), limits());
    let finalize = || uploads::finalize_with(&s, &sc, &step, &id, stub_sniff, &images, &lim);
    let (mut a, mut b) = (pin!(finalize()), pin!(finalize()));
    let mut cx = Context::from_waker(Waker::noop());
    let (mut ra, mut rb) = (None, None);
    while ra.is_none() || rb.is_none() {
        if ra.is_none()
            && let Poll::Ready(r) = a.as_mut().poll(&mut cx)
        {
            ra = Some(r);
        }
        if rb.is_none()
            && let Poll::Ready(r) = b.as_mut().poll(&mut cx)
        {
            rb = Some(r);
        }
    }
    let (ra, rb) = (ra.unwrap().unwrap(), rb.unwrap().unwrap());
    assert_eq!(ra, rb);
    assert_eq!(
        images.0.calls.get(),
        2,
        "both processed before either marked"
    );
    assert_eq!(ready(s.get(&uploads::stored_key(&ra))).unwrap(), png(48));
    assert_eq!(ready(s.get(&uploads::thumbnail_key(&ra))).unwrap(), b"thmb");
}
/// The stub pipeline, yielding once before it answers.
struct YieldingImages(StubImages);
impl uploads::ImagePipeline for YieldingImages {
    async fn process(
        &self,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<Option<uploads::ProcessedImage>, &'static str> {
        let mut yielded = false;
        std::future::poll_fn(|_| {
            if yielded {
                std::task::Poll::Ready(())
            } else {
                yielded = true;
                std::task::Poll::Pending
            }
        })
        .await;
        self.0.process(bytes, content_type).await
    }
}

#[test]
fn limits_apply_to_the_stored_bytes_after_processing() {
    // Per image: the processed file and its thumbnail must fit.
    let s = store();
    let mut tight = limits();
    tight.image_bytes = Some(64);
    reserve(&s, 1, 1, &request("a.png", "image/png", 64)).unwrap();
    ready(s.put(key(1), &png(64))).unwrap();
    let growing = GrowingImages;
    let refused = ready(uploads::finalize_with(
        &s,
        &scope(1),
        &sr(1),
        &evd(1),
        stub_sniff,
        &growing,
        &tight,
    ));
    assert_eq!(refused.unwrap_err().code(), "upload.processed_too_large");
    for key in [
        key(1),
        BlobKey::Upload {
            execution_id: exe(),
            evd_id: uploads::processed_id(&evd(1)),
        },
        BlobKey::Upload {
            execution_id: exe(),
            evd_id: uploads::thumbnail_id(&evd(1)),
        },
    ] {
        assert_eq!(ready(s.head(&key)), Err(StoreError::NotFound), "{key:?}");
    }
    // Per organization: storage counts stored bytes, checked again when the
    // slot is marked.
    let s = store();
    let mut org = limits();
    org.storage_bytes = Some(170);
    for n in [1, 2] {
        ready(uploads::reserve(
            &s,
            NewSlot {
                scope: &scope(1),
                execution_id: &exe(),
                step_run_id: &sr(1),
                id: evd(n),
                request: &request("a.png", "image/png", 64),
                limits: &org,
                created_by: "execution:fam_1",
            },
        ))
        .unwrap();
        ready(s.put(key(n), &png(64))).unwrap();
    }
    // 64 + 64 declared fits; slot 1 grows to 96 + 4: 64 + 100 still fits.
    let first = ready(uploads::finalize_with(
        &s,
        &scope(1),
        &sr(1),
        &evd(1),
        stub_sniff,
        &growing,
        &org,
    ))
    .unwrap();
    assert_eq!(first.counted_bytes(), 100);
    // 100 + 100 > 170: the second is refused and deleted.
    let second = ready(uploads::finalize_with(
        &s,
        &scope(1),
        &sr(1),
        &evd(2),
        stub_sniff,
        &growing,
        &org,
    ));
    assert_eq!(second.unwrap_err().code(), "upload.limit_reached");
    assert_eq!(ready(s.head(&key(2))), Err(StoreError::NotFound));
}
/// A pipeline whose output is larger than its input (lossless re-encode).
struct GrowingImages;
impl uploads::ImagePipeline for GrowingImages {
    async fn process(
        &self,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<Option<uploads::ProcessedImage>, &'static str> {
        let mut out = bytes;
        out.resize(96, 0);
        Ok(Some(uploads::ProcessedImage {
            bytes: out,
            content_type: content_type.into(),
            width: 4,
            height: 4,
            thumbnail: b"thmb".to_vec(),
        }))
    }
}
