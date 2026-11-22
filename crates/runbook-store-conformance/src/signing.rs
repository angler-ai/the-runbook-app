//! Signing behavior shared by the hosts that store and rotate runner keys.
use runbook_core::signing::{GRANT_TYP, Jwks, JwtSigner, KeyPurpose, KeyRing, verify};
use serde_json::{Value, json};

pub async fn rotation_keeps_the_previous_key_until_grace_expires(
    ring: impl AsyncFn() -> KeyRing,
    jwks: impl AsyncFn() -> Jwks,
    rotate_grant: impl AsyncFnOnce(),
    elapse_grace: impl AsyncFnOnce(&str),
) {
    let before = ring().await;
    let old = &before.current(KeyPurpose::Grant).unwrap().kid;
    let user = &before.current(KeyPurpose::UserAssertion).unwrap().kid;
    let initial = jwks().await;
    assert_eq!(initial.keys.len(), 2);
    assert!(initial.keys.iter().any(|key| &key.kid == old));
    assert!(initial.keys.iter().any(|key| &key.kid == user));

    let claims = json!({"sub": "sr_1", "iat": 1, "exp": i64::MAX / 2});
    let old_token = before.sign(KeyPurpose::Grant, GRANT_TYP, &claims).unwrap();
    rotate_grant().await;
    let after = ring().await;
    let current = &after.current(KeyPurpose::Grant).unwrap().kid;
    assert_ne!(current, old);
    let new_token = after.sign(KeyPurpose::Grant, GRANT_TYP, &claims).unwrap();
    let grace = jwks().await;
    assert_eq!(grace.keys.len(), 3);
    for token in [&old_token, &new_token] {
        assert_eq!(
            verify::<Value>(&grace, token, GRANT_TYP, 3).unwrap(),
            claims
        );
    }

    elapse_grace(old).await;
    let expired = jwks().await;
    assert_eq!(expired.keys.len(), 2);
    assert!(expired.keys.iter().all(|key| &key.kid != old));
    assert!(expired.keys.iter().any(|key| &key.kid == user));
    assert!(verify::<Value>(&expired, &old_token, GRANT_TYP, 3).is_err());
    assert_eq!(
        verify::<Value>(&expired, &new_token, GRANT_TYP, 3).unwrap(),
        claims
    );
}
