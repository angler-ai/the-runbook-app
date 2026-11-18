//! Tests for UP3: upload sniffing and limits catalog (PRD §14, §17).

use runbook_core::uploads::{
    SniffedType, UploadLimits, UploadRefusal,
    limits::{
        ACCEPTED_IMAGE_TYPES, CLIENT_RESIZE_LONG_EDGE, Edition, FILES_PER_EXECUTION,
        FREE_FILE_BYTES, FREE_IMAGE_BYTES, FREE_STORAGE_BYTES, IMAGE_MAX_DIMENSION,
        IMAGES_PER_STEP_RUN, PAID_FILE_BYTES, PAID_IMAGE_BYTES, THUMBNAIL_MAX_DIMENSION, check,
        check_counts, for_edition, free, paid, self_hosted,
    },
    sniff::{mime, sniff},
};

#[test]
fn table_driven_sniff_minimal_headers() {
    let cases: &[(&[u8], SniffedType, &str)] = &[
        // PNG: 8-byte signature
        (b"\x89PNG\r\n\x1a\n", SniffedType::Png, "image/png"),
        // JPEG: FF D8 FF
        (b"\xFF\xD8\xFF", SniffedType::Jpeg, "image/jpeg"),
        // WebP: RIFF....WEBP (12 bytes minimal)
        (b"RIFF\x00\x00\x00\x00WEBP", SniffedType::Webp, "image/webp"),
        // GIF: GIF87a and GIF89a
        (b"GIF87a", SniffedType::Gif, "image/gif"),
        (b"GIF89a", SniffedType::Gif, "image/gif"),
        // HEIC: major brand
        (
            b"\x00\x00\x00\x10ftypheic\x00\x00\x00\x00",
            SniffedType::Heic,
            "image/heic",
        ),
        (
            b"\x00\x00\x00\x10ftypmif1\x00\x00\x00\x00",
            SniffedType::Heic,
            "image/heic",
        ),
        // HEIC: compatible brand
        (
            b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00heic\x00\x00\x00\x00",
            SniffedType::Heic,
            "image/heic",
        ),
        // PDF: %PDF-
        (b"%PDF-", SniffedType::Pdf, "application/pdf"),
        (b"%PDF-1.7\n", SniffedType::Pdf, "application/pdf"),
        // JSON: object
        (b"{\"ok\":true}", SniffedType::Json, "application/json"),
        (
            b"  \n\t{\"status\": 200}",
            SniffedType::Json,
            "application/json",
        ),
        // JSON: array
        (b"[1, 2, 3]", SniffedType::Json, "application/json"),
        (b"\r\n[\"a\", \"b\"]", SniffedType::Json, "application/json"),
        // Plain text: UTF-8 with allowed whitespace/CRLF
        (b"hello world", SniffedType::Text, "text/plain"),
        (
            b"Line 1\r\nLine 2\tindented\n",
            SniffedType::Text,
            "text/plain",
        ),
        (b"unicode: \xE2\x9C\x93", SniffedType::Text, "text/plain"), // ✓
    ];

    for (bytes, expected_type, expected_mime) in cases {
        let actual = sniff(bytes);
        assert_eq!(
            actual,
            Some(*expected_type),
            "expected {expected_type:?} for bytes {bytes:?}"
        );
        assert_eq!(
            mime(*expected_type),
            *expected_mime,
            "expected mime {expected_mime} for {expected_type:?}"
        );
    }
}

#[test]
fn svg_is_never_recognised() {
    let svgs: &[&[u8]] = &[
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><circle/></svg>",
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><svg><rect/></svg>",
        b"  \n<svg width=\"100\" height=\"100\"></svg>",
        b"\r\n\t<?xml version=\"1.0\"?><svg viewBox=\"0 0 10 10\"></svg>",
        b"\xEF\xBB\xBF<svg></svg>", // UTF-8 BOM + svg
    ];

    for bytes in svgs {
        assert_eq!(
            sniff(bytes),
            None,
            "SVG must never be recognised: {:?}",
            std::str::from_utf8(bytes)
        );
    }
}

#[test]
fn polyglots_classified_by_magic_never_as_text() {
    // PDF header followed by HTML
    let pdf_html = b"%PDF-1.4\n<html><body>malicious</body></html>";
    assert_eq!(sniff(pdf_html), Some(SniffedType::Pdf));

    // JPEG signature followed by script tag
    let jpeg_script = b"\xFF\xD8\xFF<script>alert(1)</script>";
    assert_eq!(sniff(jpeg_script), Some(SniffedType::Jpeg));

    // PNG signature followed by HTML
    let png_html = b"\x89PNG\r\n\x1a\n<!DOCTYPE html><html></html>";
    assert_eq!(sniff(png_html), Some(SniffedType::Png));

    // WebP signature followed by XML
    let webp_xml = b"RIFF\x00\x00\x00\x00WEBP<?xml version=\"1.0\"?>";
    assert_eq!(sniff(webp_xml), Some(SniffedType::Webp));
}

#[test]
fn html_files_give_none() {
    let htmls: &[&[u8]] = &[
        b"<!DOCTYPE html><html><head><title>Test</title></head><body>Hello</body></html>",
        b"<html><body><h1>Title</h1></body></html>",
        b"  \n<html lang=\"en\"><body>hi</body></html>",
        b"<!-- comment --><html></html>",
        b"<p>Just a paragraph</p>",
    ];

    for bytes in htmls {
        assert_eq!(
            sniff(bytes),
            None,
            "HTML must return None: {:?}",
            std::str::from_utf8(bytes)
        );
    }
}

#[test]
fn empty_input_gives_none() {
    assert_eq!(sniff(b""), None);
}

#[test]
fn json_array_and_object_recognised() {
    assert_eq!(
        sniff(b"{\"service\":\"web\",\"status\":\"ready\"}"),
        Some(SniffedType::Json)
    );
    assert_eq!(
        sniff(b"[\"us-east\", \"us-west\", \"eu-central\"]"),
        Some(SniffedType::Json)
    );
    assert_eq!(sniff(b"   [  ]"), Some(SniffedType::Json));
    assert_eq!(sniff(b"   {  }"), Some(SniffedType::Json));
}

#[test]
fn invalid_utf8_is_not_text() {
    let invalid: &[&[u8]] = &[
        b"\xFF\xFE\x00\x00",
        b"valid prefix \xFF followed by invalid",
        b"\xC0\xAF",     // overlong encoding
        b"\xED\xA0\x80", // surrogate half
    ];

    for bytes in invalid {
        assert_eq!(sniff(bytes), None);
    }
}

#[test]
fn control_characters_disqualify_plain_text() {
    // NUL byte
    assert_eq!(sniff(b"hello\x00world"), None);
    // BEL control char (0x07)
    assert_eq!(sniff(b"alert\x07beep"), None);
    // ESC control char (0x1B)
    assert_eq!(sniff(b"\x1B[31mred\x1B[0m"), None);
    // Backspace (0x08)
    assert_eq!(sniff(b"back\x08space"), None);
    // Tab, LF and CR are allowed
    assert_eq!(sniff(b"ok\twith\nall\rthree"), Some(SniffedType::Text));
}

#[test]
fn limits_accept_exact_limit_and_refuse_plus_one_for_each_class() {
    let free_limits = free();
    let paid_limits = paid();

    // 1. Image class (PNG, JPEG, WebP, GIF)
    // Free: 1 MB
    assert_eq!(
        check(&free_limits, SniffedType::Png, FREE_IMAGE_BYTES),
        Ok(())
    );
    assert_eq!(
        check(&free_limits, SniffedType::Png, FREE_IMAGE_BYTES + 1),
        Err(UploadRefusal::TooLarge)
    );
    // Paid: 10 MB
    assert_eq!(
        check(&paid_limits, SniffedType::Jpeg, PAID_IMAGE_BYTES),
        Ok(())
    );
    assert_eq!(
        check(&paid_limits, SniffedType::Jpeg, PAID_IMAGE_BYTES + 1),
        Err(UploadRefusal::TooLarge)
    );

    // 2. PDF class
    // Free: 10 MB
    assert_eq!(
        check(&free_limits, SniffedType::Pdf, FREE_FILE_BYTES),
        Ok(())
    );
    assert_eq!(
        check(&free_limits, SniffedType::Pdf, FREE_FILE_BYTES + 1),
        Err(UploadRefusal::TooLarge)
    );
    // Paid: 25 MB
    assert_eq!(
        check(&paid_limits, SniffedType::Pdf, PAID_FILE_BYTES),
        Ok(())
    );
    assert_eq!(
        check(&paid_limits, SniffedType::Pdf, PAID_FILE_BYTES + 1),
        Err(UploadRefusal::TooLarge)
    );

    // 3. Text class
    // Free: 10 MB
    assert_eq!(
        check(&free_limits, SniffedType::Text, FREE_FILE_BYTES),
        Ok(())
    );
    assert_eq!(
        check(&free_limits, SniffedType::Text, FREE_FILE_BYTES + 1),
        Err(UploadRefusal::TooLarge)
    );
    // Paid: 25 MB
    assert_eq!(
        check(&paid_limits, SniffedType::Text, PAID_FILE_BYTES),
        Ok(())
    );
    assert_eq!(
        check(&paid_limits, SniffedType::Text, PAID_FILE_BYTES + 1),
        Err(UploadRefusal::TooLarge)
    );

    // JSON follows file_bytes
    assert_eq!(
        check(&free_limits, SniffedType::Json, FREE_FILE_BYTES),
        Ok(())
    );
    assert_eq!(
        check(&free_limits, SniffedType::Json, FREE_FILE_BYTES + 1),
        Err(UploadRefusal::TooLarge)
    );

    // Self-hosted has no size limits
    let sh = self_hosted();
    assert_eq!(check(&sh, SniffedType::Png, 100 * 1024 * 1024), Ok(()));
    assert_eq!(check(&sh, SniffedType::Pdf, 100 * 1024 * 1024), Ok(()));
    assert_eq!(check(&sh, SniffedType::Text, 100 * 1024 * 1024), Ok(()));
}

#[test]
fn heic_is_refused_as_unsupported_by_default() {
    let free_limits = free();
    let paid_limits = paid();
    let sh_limits = self_hosted();

    assert_eq!(
        check(&free_limits, SniffedType::Heic, 1024),
        Err(UploadRefusal::TypeUnsupported)
    );
    assert_eq!(
        check(&paid_limits, SniffedType::Heic, 1024),
        Err(UploadRefusal::TypeUnsupported)
    );
    assert_eq!(
        check(&sh_limits, SniffedType::Heic, 1024),
        Err(UploadRefusal::TypeUnsupported)
    );
}

#[test]
fn count_limits_and_refusal_codes() {
    let free_limits = free();

    assert_eq!(UploadRefusal::TooLarge.code(), "upload.too_large");
    assert_eq!(
        UploadRefusal::TypeUnsupported.code(),
        "upload.type_unsupported"
    );
    assert_eq!(UploadRefusal::TooMany.code(), "upload.too_many");
    assert_eq!(format!("{}", UploadRefusal::TooMany), "upload.too_many");

    // Images count per step run
    assert_eq!(
        check_counts(&free_limits, true, IMAGES_PER_STEP_RUN - 1, 0),
        Ok(())
    );
    assert_eq!(
        check_counts(&free_limits, true, IMAGES_PER_STEP_RUN, 0),
        Err(UploadRefusal::TooMany)
    );

    // Files count per execution
    assert_eq!(
        check_counts(&free_limits, false, 0, FILES_PER_EXECUTION - 1),
        Ok(())
    );
    assert_eq!(
        check_counts(&free_limits, false, 0, FILES_PER_EXECUTION),
        Err(UploadRefusal::TooMany)
    );
}

#[test]
fn runbook_model_upload_limits_values_equal_constants() {
    // Free limits equal constants
    let free_limits: UploadLimits = free();
    assert_eq!(free_limits.image_bytes, Some(FREE_IMAGE_BYTES));
    assert_eq!(free_limits.file_bytes, Some(FREE_FILE_BYTES));
    assert_eq!(free_limits.image_max_dimension, Some(IMAGE_MAX_DIMENSION));
    assert_eq!(
        free_limits.thumbnail_max_dimension,
        Some(THUMBNAIL_MAX_DIMENSION)
    );
    assert_eq!(free_limits.images_per_step_run, Some(IMAGES_PER_STEP_RUN));
    assert_eq!(free_limits.files_per_execution, Some(FILES_PER_EXECUTION));
    assert_eq!(free_limits.storage_bytes, Some(FREE_STORAGE_BYTES));
    assert_eq!(free_limits.client_resize_long_edge, CLIENT_RESIZE_LONG_EDGE);
    assert_eq!(
        free_limits.image_types,
        ACCEPTED_IMAGE_TYPES
            .iter()
            .map(|&s| s.to_string())
            .collect::<Vec<_>>()
    );

    // Paid limits equal constants
    let paid_limits: UploadLimits = paid();
    assert_eq!(paid_limits.image_bytes, Some(PAID_IMAGE_BYTES));
    assert_eq!(paid_limits.file_bytes, Some(PAID_FILE_BYTES));
    assert_eq!(paid_limits.image_max_dimension, Some(IMAGE_MAX_DIMENSION));
    assert_eq!(
        paid_limits.thumbnail_max_dimension,
        Some(THUMBNAIL_MAX_DIMENSION)
    );
    assert_eq!(paid_limits.images_per_step_run, Some(IMAGES_PER_STEP_RUN));
    assert_eq!(paid_limits.files_per_execution, Some(FILES_PER_EXECUTION));
    assert_eq!(paid_limits.storage_bytes, None);
    assert_eq!(paid_limits.client_resize_long_edge, CLIENT_RESIZE_LONG_EDGE);

    // Self-hosted limits equal constants / None
    let sh_limits: UploadLimits = self_hosted();
    assert_eq!(sh_limits.image_bytes, None);
    assert_eq!(sh_limits.file_bytes, None);
    assert_eq!(sh_limits.image_max_dimension, None);
    assert_eq!(
        sh_limits.thumbnail_max_dimension,
        Some(THUMBNAIL_MAX_DIMENSION)
    );
    assert_eq!(sh_limits.images_per_step_run, None);
    assert_eq!(sh_limits.files_per_execution, None);
    assert_eq!(sh_limits.storage_bytes, None);
    assert_eq!(sh_limits.client_resize_long_edge, CLIENT_RESIZE_LONG_EDGE);

    // Edition helper equality
    assert_eq!(for_edition(Edition::Free), free_limits);
    assert_eq!(for_edition(Edition::Paid), paid_limits);
    assert_eq!(for_edition(Edition::SelfHosted), sh_limits);
    assert_eq!(Edition::Free.limits(), free_limits);
    assert_eq!(Edition::Paid.limits(), paid_limits);
    assert_eq!(Edition::SelfHosted.limits(), sh_limits);
}

/// Integration fix: an `ftyp` box shorter than 16 bytes followed by more
/// data must not panic (the compatible-brand scan starts at byte 16).
#[test]
fn short_ftyp_box_does_not_panic() {
    let mut bytes = vec![0, 0, 0, 12];
    bytes.extend_from_slice(b"ftypisom");
    bytes.extend_from_slice(&[0u8; 20]);
    assert_eq!(sniff(&bytes), None);
    let mut heic = vec![0, 0, 0, 12];
    heic.extend_from_slice(b"ftypisom");
    heic.extend_from_slice(b"\0\0\0\x14ftypheic");
    // The second box is not the brand list of the first; still no panic.
    let _ = sniff(&heic);
}

/// Finalize sniffs at most a 64 KiB prefix, so a long UTF-8 file can be cut
/// inside a multi-byte character. That must still be text.
#[test]
fn text_cut_mid_character_is_still_text() {
    let text = "é".repeat(40_000);
    let cut = &text.as_bytes()[..64 * 1024 - 1];
    assert!(std::str::from_utf8(cut).is_err());
    assert_eq!(sniff(cut), Some(SniffedType::Text));
    // A truly invalid byte in the middle is still refused.
    let mut bad = text.as_bytes()[..100].to_vec();
    bad[50] = 0xFF;
    assert_eq!(sniff(&bad), None);
}

/// A JSON file longer than the sniffed prefix is a valid JSON prefix, not
/// a complete document; it must still be recognised as JSON.
#[test]
fn json_longer_than_prefix_is_json() {
    let mut json = String::from("[");
    while json.len() < 100_000 {
        json.push_str("{\"key\":\"value\",\"n\":12345},");
    }
    json.push_str("{}]");
    let prefix = &json.as_bytes()[..64 * 1024];
    assert_eq!(sniff(prefix), Some(SniffedType::Json));
    // Bracketed prose is text, not JSON.
    assert_eq!(sniff(b"[note] deploy went fine\n"), Some(SniffedType::Text));
}
