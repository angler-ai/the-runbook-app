//! Workers runner signing uses the portable Ed25519 implementation and
//! the same AES-GCM sealer as webhook secrets.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_core::{
    signing::{Jwks, JwtSigner, KeyPurpose, KeyRing, RETIRED_KEY_TTL_S, SigningError, SigningKey},
    webhooks::{SealedSecret, SecretSealer},
};
use runbook_store_d1::{
    D1,
    signing::{D1SigningKeys, StoredSigningKey},
};

fn context(kid: &str) -> String {
    format!("signing_key:{kid}")
}

fn unavailable<E>(_: E) -> SigningError {
    SigningError::Unavailable
}

fn new_key(
    sealer: &impl SecretSealer,
    purpose: KeyPurpose,
    now_ms: i64,
    random: impl Fn(&mut [u8]) -> Result<(), runbook_api::backends::NoEntropy>,
) -> Result<StoredSigningKey, SigningError> {
    let mut seed = [0; 32];
    random(&mut seed).map_err(unavailable)?;
    let key = SigningKey::from_seed(purpose, seed, now_ms / 1000);
    let sealed = sealer
        .seal(&context(&key.kid), &URL_SAFE_NO_PAD.encode(key.seed()))
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

pub(crate) async fn rotate(
    keys: &D1SigningKeys<impl D1>,
    sealer: &impl SecretSealer,
    purpose: KeyPurpose,
    now_ms: i64,
    random: impl Fn(&mut [u8]) -> Result<(), runbook_api::backends::NoEntropy>,
) -> Result<(), SigningError> {
    let key = new_key(sealer, purpose, now_ms, random)?;
    keys.rotate(&key, now_ms).await.map_err(unavailable)
}

pub async fn ring(
    keys: &D1SigningKeys<impl D1>,
    sealer: &impl SecretSealer,
    now_ms: i64,
    random: impl Fn(&mut [u8]) -> Result<(), runbook_api::backends::NoEntropy>,
) -> Result<KeyRing, SigningError> {
    let signing_keys = ensure_keys(keys, sealer, now_ms, random)
        .await?
        .into_iter()
        .map(|row| open_key(sealer, row))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(KeyRing::new(signing_keys))
}

async fn ensure_keys(
    keys: &D1SigningKeys<impl D1>,
    sealer: &impl SecretSealer,
    now_ms: i64,
    random: impl Fn(&mut [u8]) -> Result<(), runbook_api::backends::NoEntropy>,
) -> Result<Vec<StoredSigningKey>, SigningError> {
    let cutoff = now_ms - RETIRED_KEY_TTL_S * 1000;
    let mut rows = keys.load(cutoff).await.map_err(unavailable)?;
    let mut missing = false;
    for purpose in KeyPurpose::ALL {
        if rows
            .iter()
            .any(|key| key.purpose == purpose.as_str() && key.retired_at_ms.is_none())
        {
            continue;
        }
        let key = new_key(sealer, purpose, now_ms, &random)?;
        keys.insert_if_absent(&key).await.map_err(unavailable)?;
        missing = true;
    }
    if missing {
        rows = keys.load(cutoff).await.map_err(unavailable)?;
    }
    Ok(rows)
}

fn open_key(sealer: &impl SecretSealer, row: StoredSigningKey) -> Result<SigningKey, SigningError> {
    let purpose = KeyPurpose::parse(&row.purpose).ok_or(SigningError::Unavailable)?;
    let secret = sealer
        .open(&context(&row.kid), &SealedSecret(row.sealed_private))
        .map_err(unavailable)?;
    let seed = URL_SAFE_NO_PAD
        .decode(secret)
        .map_err(unavailable)?
        .try_into()
        .map_err(unavailable)?;
    let key = SigningKey::from_seed(purpose, seed, row.created_at_ms / 1000)
        .retired(row.retired_at_ms.map(|ms| ms / 1000));
    if key.kid != row.kid || key.public_key().as_slice() != row.public {
        return Err(SigningError::Unavailable);
    }
    Ok(key)
}

pub async fn jwks(
    keys: &D1SigningKeys<impl D1>,
    sealer: &impl SecretSealer,
    now_ms: i64,
    random: impl Fn(&mut [u8]) -> Result<(), runbook_api::backends::NoEntropy>,
) -> Result<Jwks, SigningError> {
    let keys = ensure_keys(keys, sealer, now_ms, random)
        .await?
        .into_iter()
        .filter_map(|row| open_key(sealer, row).ok())
        .collect();
    Ok(KeyRing::new(keys).jwks())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::sealer::AesSealer;
    use runbook_store_d1::native::NativeD1;
    use std::cell::Cell;

    #[test]
    fn rotation_keeps_the_previous_public_key_for_one_day() {
        runbook_worker_conformance::native::run(async {
            let db = NativeD1::new(0);
            runbook_store_d1::migrate::migrate(&db).await.unwrap();
            let keys = D1SigningKeys::new(db);
            let sealer = AesSealer::with_previous(&[6; 32], &[], |bytes| {
                getrandom::fill(bytes).map_err(|_| ())
            });
            let entropy = Cell::new(0);
            let random = |seed: &mut [u8]| {
                entropy.set(entropy.get() + 1);
                seed.fill(entropy.get());
                Ok(())
            };
            let now = Cell::new(1_000);
            runbook_store_conformance::signing::rotation_keeps_the_previous_key_until_grace_expires(
                async || ring(&keys, &sealer, now.get(), random).await.unwrap(),
                async || jwks(&keys, &sealer, now.get(), random).await.unwrap(),
                async || {
                    now.set(2_000);
                    rotate(&keys, &sealer, KeyPurpose::Grant, now.get(), random)
                        .await
                        .unwrap();
                    now.set(now.get() + RETIRED_KEY_TTL_S * 1000);
                },
                async |_| now.set(now.get() + 1),
            )
            .await;
            assert!(
                keys.load(0)
                    .await
                    .unwrap()
                    .iter()
                    .all(|key| key.sealed_private.starts_with("v2."))
            );
        });
    }
    #[test]
    fn jwks_omits_a_d1_key_that_does_not_match_its_sealed_seed() {
        runbook_worker_conformance::native::run(async {
            let db = NativeD1::new(0);
            runbook_store_d1::migrate::migrate(&db).await.unwrap();
            let keys = D1SigningKeys::new(db.clone());
            let sealer = AesSealer::with_previous(&[6; 32], &[], |bytes| {
                getrandom::fill(bytes).map_err(|_| ())
            });
            let random = |seed: &mut [u8]| {
                seed.fill(3);
                Ok(())
            };
            assert_eq!(
                jwks(&keys, &sealer, 1_000, random)
                    .await
                    .unwrap()
                    .keys
                    .len(),
                2
            );
            db.batch(vec![
                runbook_store_d1::sql::prepare(
                    "UPDATE signing_keys SET public = ? WHERE purpose = 'grant'",
                )
                .bind([7u8; 32].as_slice()),
            ])
            .await
            .unwrap();
            let published = jwks(&keys, &sealer, 1_000, random).await.unwrap();
            assert_eq!(published.keys.len(), 1);
            assert!(published.keys[0].kid.starts_with("user-"));
        });
    }
}
