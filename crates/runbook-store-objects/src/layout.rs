//! STO1: BlobKey ↔ object store path layout.
//!
//! Object key: `{prefix}{org}/{project}/{encoded blob key}/{sha256 hex}`
//!
//! `{encoded blob key}` is the JSON string of `BlobKey` percent-encoded
//! using a safe ASCII set so it never contains `/` or characters unsafe
//! in a single path segment.
//!
//! `{sha256 hex}` is the 64-character lowercase hexadecimal representation
//! of the content hash.
use object_store::path::Path;
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use runbook_core::{blob::BlobKey, catalog::TenantScope};

const ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'?')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b':')
    .add(b'=')
    .add(b'&')
    .add(b'%')
    .add(b'+');

pub fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

pub fn encode_blob_key(key: &BlobKey) -> Result<String, serde_json::Error> {
    let json = serde_json::to_string(key)?;
    Ok(utf8_percent_encode(&json, ENCODE_SET).to_string())
}

pub fn decode_blob_key(encoded: &str) -> Option<BlobKey> {
    let decoded = percent_decode_str(encoded).decode_utf8().ok()?;
    serde_json::from_str(&decoded).ok()
}

pub fn normalize_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim();
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.ends_with('/') {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/")
    }
}

pub fn object_path(prefix: &str, scope: &TenantScope, key: &BlobKey, sha256: &[u8; 32]) -> Path {
    let prefix = normalize_prefix(prefix);
    let encoded_key = encode_blob_key(key).expect("BlobKey serialization cannot fail");
    let sha256_hex = hex(sha256);
    let full = format!(
        "{prefix}{}/{}/{encoded_key}/{sha256_hex}",
        scope.org_id, scope.project_id
    );
    Path::parse(&full).expect("valid object path")
}

pub fn parse_object_path(prefix: &str, path: &Path) -> Option<(TenantScope, BlobKey, [u8; 32])> {
    let (scope, raw_key, sha256) = parse_object_path_raw(prefix, path)?;
    let key = serde_json::from_str(&raw_key).ok()?;
    Some((scope, key, sha256))
}

pub fn parse_object_path_raw(prefix: &str, path: &Path) -> Option<(TenantScope, String, [u8; 32])> {
    let path_str = path.as_ref();
    let prefix = normalize_prefix(prefix);
    let rest = if prefix.is_empty() {
        path_str
    } else {
        path_str.strip_prefix(&prefix)?
    };
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() != 4 {
        return None;
    }
    let org_id = parts[0].parse().ok()?;
    let project_id = parts[1].parse().ok()?;
    let raw_key = percent_decode_str(parts[2])
        .decode_utf8()
        .ok()?
        .into_owned();
    let sha256 = unhex(parts[3])?;
    Some((TenantScope { org_id, project_id }, raw_key, sha256))
}

#[cfg(test)]
mod tests {
    use super::*;
    use runbook_model::{EvidenceId, ExecutionId};

    #[test]
    fn roundtrip_layout() {
        let scope = TenantScope {
            org_id: "org_01234567890123456789012345".parse().unwrap(),
            project_id: "prj_01234567890123456789012345".parse().unwrap(),
        };
        let exe: ExecutionId = "exe_01234567890123456789012345".parse().unwrap();
        let evd: EvidenceId = "evd_01234567890123456789012345".parse().unwrap();
        let key = BlobKey::Upload {
            execution_id: exe,
            evd_id: evd,
        };
        let hash = [42u8; 32];
        let path = object_path("runbook/", &scope, &key, &hash);
        let parsed = parse_object_path("runbook/", &path).unwrap();
        assert_eq!(parsed.0, scope);
        assert_eq!(parsed.1, key);
        assert_eq!(parsed.2, hash);
    }
}
