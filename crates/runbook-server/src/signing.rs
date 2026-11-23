//! R0-1 on the native server: one Ed25519 key per purpose, stored in
//! `runbook.signing_keys` with the private key sealed by the server's
//! secret key (the webhook sealer, `RUNBOOK_WEBHOOK_SECRET_KEY`). Without
//! that key nothing is signed (`signing.unavailable`) and the JWKS is
//! empty. Keys are created on first use; rotation retires the current key,
//! which stays published (and verifiable) for 24 hours.
use crate::webhooks::AeadSealer;
use runbook_core::{
    signing::{Jwks, JwtSigner, KeyPurpose, KeyRing, RETIRED_KEY_TTL_S, SigningError, SigningKey},
    webhooks::{SealedSecret, SecretSealer},
};
use runbook_store_postgres::{DatabaseConfig, PostgresSigningKeys, StoredSigningKey};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// How long a loaded key ring signs before it is read again (another
/// replica may have rotated; the retired key stays valid for 24 hours).
const RING_CACHE: Duration = Duration::from_secs(30);

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
fn seal_context(kid: &str) -> String {
    format!("signing_key:{kid}")
}
fn unavailable<E>(_: E) -> SigningError {
    SigningError::Unavailable
}

#[derive(Clone)]
pub struct ServerSigner {
    keys: PostgresSigningKeys,
    sealer: Option<Arc<AeadSealer>>,
    cache: Arc<Mutex<Option<(Instant, KeyRing)>>>,
}
impl ServerSigner {
    pub fn new(database: &DatabaseConfig, webhooks: &crate::config::WebhookConfig) -> Self {
        Self {
            keys: PostgresSigningKeys::new(database.clone()),
            sealer: webhooks.secret_key.as_ref().map(|key| {
                Arc::new(AeadSealer::with_previous(
                    key,
                    &webhooks.previous_secret_keys,
                ))
            }),
            cache: Arc::default(),
        }
    }

    fn sealer(&self) -> Result<&AeadSealer, SigningError> {
        self.sealer.as_deref().ok_or(SigningError::Unavailable)
    }

    fn new_key(&self, purpose: KeyPurpose, now_ms: i64) -> Result<StoredSigningKey, SigningError> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(unavailable)?;
        let key = SigningKey::from_seed(purpose, seed, now_ms / 1000);
        let sealed = self
            .sealer()?
            .seal(&seal_context(&key.kid), &base64_seed(&key.seed()))
            .map_err(unavailable)?;
        Ok(StoredSigningKey {
            kid: key.kid.clone(),
            purpose: purpose.as_str().into(),
            sealed_private: sealed.0,
            public: key.public_key().to_vec(),
            created_at_ms: now_ms,
            retired_at_ms: None,
        })
    }

    /// The keys that sign now (current per purpose, created when missing)
    /// and those retired in the last 24 hours. Cached for 30 seconds.
    pub async fn ring(&self) -> Result<KeyRing, SigningError> {
        if let Some((at, ring)) = &*self.cache.lock().unwrap_or_else(|e| e.into_inner())
            && at.elapsed() < RING_CACHE
        {
            return Ok(ring.clone());
        }
        let sealer = self.sealer()?;
        let keys = self
            .ensure_keys(now_ms())
            .await?
            .into_iter()
            .map(|row| open_key(sealer, row))
            .collect::<Result<Vec<_>, _>>()?;
        let ring = KeyRing::new(keys);
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), ring.clone()));
        Ok(ring)
    }

    async fn ensure_keys(&self, now: i64) -> Result<Vec<StoredSigningKey>, SigningError> {
        let mut stored = self.load(now).await?;
        let mut created = false;
        for purpose in KeyPurpose::ALL {
            let has_active = stored
                .iter()
                .any(|k| k.purpose == purpose.as_str() && k.retired_at_ms.is_none());
            if !has_active {
                self.keys
                    .insert_if_absent(&self.new_key(purpose, now)?)
                    .await
                    .map_err(unavailable)?;
                created = true;
            }
        }
        if created {
            stored = self.load(now).await?;
        }
        Ok(stored)
    }

    async fn load(&self, now_ms: i64) -> Result<Vec<StoredSigningKey>, SigningError> {
        self.keys
            .load(now_ms - RETIRED_KEY_TTL_S * 1000)
            .await
            .map_err(unavailable)
    }

    /// Only keys whose sealed seeds reproduce their ID and public key.
    pub async fn jwks(&self) -> Result<Jwks, SigningError> {
        let Some(sealer) = self.sealer.as_deref() else {
            return Ok(Jwks { keys: Vec::new() });
        };
        let keys = self
            .ensure_keys(now_ms())
            .await?
            .into_iter()
            .filter_map(|row| open_key(sealer, row).ok())
            .collect();
        Ok(KeyRing::new(keys).jwks())
    }

    /// Retire the purpose's current key and start signing with a new one.
    pub async fn rotate(&self, purpose: KeyPurpose) -> Result<(), SigningError> {
        let now = now_ms();
        let key = self.new_key(purpose, now)?;
        self.keys.rotate(&key, now).await.map_err(unavailable)?;
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
}

fn open_key(sealer: &AeadSealer, row: StoredSigningKey) -> Result<SigningKey, SigningError> {
    let purpose = KeyPurpose::parse(&row.purpose).ok_or(SigningError::Unavailable)?;
    let seed = sealer
        .open(&seal_context(&row.kid), &SealedSecret(row.sealed_private))
        .map_err(unavailable)?;
    let key = SigningKey::from_seed(purpose, decode_seed(&seed)?, row.created_at_ms / 1000)
        .retired(row.retired_at_ms.map(|t| t / 1000));
    if key.kid != row.kid || key.public_key().as_slice() != row.public {
        return Err(SigningError::Unavailable);
    }
    Ok(key)
}

fn base64_seed(seed: &[u8; 32]) -> String {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    URL_SAFE_NO_PAD.encode(seed)
}
fn decode_seed(text: &str) -> Result<[u8; 32], SigningError> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    URL_SAFE_NO_PAD
        .decode(text)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(SigningError::Unavailable)
}
