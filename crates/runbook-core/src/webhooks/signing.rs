//! `Runbook-Signature: v1=<hex HMAC-SHA256 of "{timestamp}.{body}">`.
//! During a rotation overlap the header lists one `v1=` per secret,
//! comma-separated, newest first. The HMAC key is the secret string's bytes.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

/// Receivers should reject timestamps further than this from their clock.
pub const SIGNATURE_TOLERANCE_S: i64 = 300;
pub const SECRET_PREFIX: &str = "whsec_";

/// A new signing secret from 32 bytes of host entropy.
pub fn encode_webhook_secret(random: [u8; 32]) -> String {
    format!("{SECRET_PREFIX}{}", URL_SAFE_NO_PAD.encode(random))
}

/// RFC 2104 HMAC over SHA-256.
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    for part in parts {
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// Hex signature of `"{timestamp}.{body}"` under one secret.
pub fn sign(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let prefix = format!("{timestamp}.");
    crate::authoring::hex(&hmac_sha256(secret.as_bytes(), &[prefix.as_bytes(), body]))
}

/// Header value carrying one `v1=` signature per secret, in order.
pub fn signature_header(secrets: &[&str], timestamp: i64, body: &[u8]) -> String {
    secrets
        .iter()
        .map(|secret| format!("v1={}", sign(secret, timestamp, body)))
        .collect::<Vec<_>>()
        .join(",")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureError {
    Malformed,
    Stale,
    Mismatch,
}

/// Receiver-side check, shared by tests and the example relays: the
/// timestamp must be within `tolerance_s` of `now_s`, and at least one `v1=`
/// entry must match this secret (constant-time comparison).
pub fn verify_signature(
    secret: &str,
    timestamp: &str,
    body: &[u8],
    header: &str,
    now_s: i64,
    tolerance_s: i64,
) -> Result<(), SignatureError> {
    if timestamp.is_empty() || !timestamp.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SignatureError::Malformed);
    }
    let at: i64 = timestamp.parse().map_err(|_| SignatureError::Malformed)?;
    if (now_s - at).abs() > tolerance_s {
        return Err(SignatureError::Stale);
    }
    let expected = sign(secret, at, body);
    let mut seen = false;
    for entry in header.split(',') {
        let Some(candidate) = entry.trim().strip_prefix("v1=") else {
            continue;
        };
        seen = true;
        if constant_time_eq(candidate.as_bytes(), expected.as_bytes()) {
            return Ok(());
        }
    }
    Err(if seen {
        SignatureError::Mismatch
    } else {
        SignatureError::Malformed
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hmac_matches_rfc_4231_vectors() {
        // Test case 2.
        assert_eq!(
            crate::authoring::hex(&hmac_sha256(
                b"Jefe",
                &[b"what do ya want ", b"for nothing?"]
            )),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: a key longer than the block size is hashed first.
        assert_eq!(
            crate::authoring::hex(&hmac_sha256(
                &[0xaa; 131],
                &[b"Test Using Larger Than Block-Size Key - Hash Key First"]
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
