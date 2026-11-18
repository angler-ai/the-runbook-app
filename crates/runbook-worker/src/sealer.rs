//! CF3: the webhook secret sealer on Workers, byte-compatible with the
//! native `ring` sealer (`runbook-server/src/webhooks.rs`), so a sealed
//! secret moves between editions (CF8 import/export) and opens on both.
//!
//! - `v2.<base64url(kid || nonce || ciphertext || tag)>`: AES-256-GCM with
//!   a 96-bit nonce; the associated data is `kid || context` (the
//!   subscription ID), so a value opens only for its own row; `kid` is the
//!   first byte of `SHA-256("runbook.webhook-key.v1\0" || key)`.
//! - `v1.<base64url(nonce || ciphertext || tag)>`, associated data the
//!   context alone, written before key ids: still opened with any key.
//!
//! The current key seals; previous keys still open what they sealed. The
//! host supplies each nonce (`crypto.getRandomValues` on Workers).
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_core::{
    StoreError,
    webhooks::{SealedSecret, SecretSealer},
};
use sha2::{Digest, Sha256};

const NONCE_LEN: usize = 12;

pub struct AesSealer {
    /// The current key first; it alone seals.
    keys: Vec<(u8, Aes256Gcm)>,
    nonce: fn(&mut [u8]) -> Result<(), ()>,
}

impl AesSealer {
    /// `nonce` fills a fresh nonce from the platform's CSPRNG.
    pub fn with_previous(
        current: &[u8; 32],
        previous: &[[u8; 32]],
        nonce: fn(&mut [u8]) -> Result<(), ()>,
    ) -> Self {
        Self {
            keys: std::iter::once(current)
                .chain(previous)
                .map(|key| (Self::key_id(key), Aes256Gcm::new(key.into())))
                .collect(),
            nonce,
        }
    }
    /// A one-byte key fingerprint; it selects keys, never authenticates.
    pub fn key_id(key: &[u8; 32]) -> u8 {
        let mut hasher = Sha256::new();
        hasher.update(b"runbook.webhook-key.v1\0");
        hasher.update(key);
        hasher.finalize()[0]
    }
}

fn v2_aad(kid: u8, context: &str) -> Vec<u8> {
    let mut aad = vec![kid];
    aad.extend_from_slice(context.as_bytes());
    aad
}
fn open_with(key: &Aes256Gcm, aad: &[u8], sealed: &[u8]) -> Option<String> {
    let (nonce, data) = sealed.split_at_checked(NONCE_LEN)?;
    let plain = key
        .decrypt(Nonce::from_slice(nonce), Payload { msg: data, aad })
        .ok()?;
    String::from_utf8(plain).ok()
}

impl SecretSealer for AesSealer {
    fn seal(&self, context: &str, secret: &str) -> Result<SealedSecret, StoreError> {
        let (kid, key) = &self.keys[0];
        let mut nonce = [0u8; NONCE_LEN];
        (self.nonce)(&mut nonce).map_err(|()| StoreError::Backend("webhooks.entropy"))?;
        let aad = v2_aad(*kid, context);
        let data = key
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: secret.as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| StoreError::Backend("webhooks.seal_failed"))?;
        let mut out = vec![*kid];
        out.extend(nonce);
        out.extend(data);
        Ok(SealedSecret(format!("v2.{}", URL_SAFE_NO_PAD.encode(out))))
    }
    fn open(&self, context: &str, sealed: &SealedSecret) -> Result<String, StoreError> {
        let failed = || StoreError::Backend("webhooks.open_failed");
        let (version, body) = sealed.0.split_once('.').ok_or_else(failed)?;
        let bytes = URL_SAFE_NO_PAD.decode(body).map_err(|_| failed())?;
        match version {
            "v2" => {
                let (&kid, rest) = bytes.split_first().ok_or_else(failed)?;
                let aad = v2_aad(kid, context);
                self.keys
                    .iter()
                    .filter(|(id, _)| *id == kid)
                    .find_map(|(_, key)| open_with(key, &aad, rest))
                    .ok_or_else(failed)
            }
            "v1" => self
                .keys
                .iter()
                .find_map(|(_, key)| open_with(key, context.as_bytes(), &bytes))
                .ok_or_else(failed),
            _ => Err(failed()),
        }
    }
}
