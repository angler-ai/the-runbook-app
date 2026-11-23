//! R0-1: Ed25519 signing keys per purpose, the JWKS and the portable
//! verifier (EdDSA only, 30 s of clock skew).
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_core::signing::{
    Delivery, GRANT_TYP, GrantClaims, GrantKind, JwtSigner, KeyPurpose, KeyRing, SigningError,
    SigningKey, USER_TYP, UserAssertionClaims, verify,
};
use std::collections::BTreeMap;

const NOW: i64 = 1_800_000_000;

fn ring() -> KeyRing {
    KeyRing::new(vec![
        SigningKey::from_seed(KeyPurpose::Grant, [1; 32], NOW - 10),
        SigningKey::from_seed(KeyPurpose::UserAssertion, [2; 32], NOW - 10),
    ])
}

fn grant(exp: i64) -> GrantClaims {
    GrantClaims {
        iss: "https://runbooks.example".into(),
        aud: "runner:rn_00000000000000000000000001".into(),
        sub: "sr_00000000000000000000000001".into(),
        kind: GrantKind::Credential,
        claim_id: "clm_1".into(),
        generation: 3,
        runner_id: "rn_00000000000000000000000001".into(),
        group_id: "rg_00000000000000000000000001".into(),
        agent_id: "agt_1".into(),
        agent_version: "agv_1".into(),
        project_id: "prj_00000000000000000000000001".into(),
        reach: None,
        destructive: false,
        network: None,
        connections: None,
        model_profile: Some("deep".into()),
        model_id: None,
        max_tokens: Some(750),
        timeout_ms: Some(15_000),
        step_deadline_ms: Some(NOW * 1000 + 15_000),
        credential_id: Some("cred_1".into()),
        credential_version: Some(2),
        delivery: Some(Delivery::Proxy),
        iat: NOW - 5,
        exp,
        jti: "jti-1".into(),
    }
}

#[test]
fn sign_then_verify_round_trips() {
    let ring = ring();
    let claims = grant(NOW + 600);
    let token = ring.sign(KeyPurpose::Grant, GRANT_TYP, &claims).unwrap();
    let back: GrantClaims = verify(&ring.jwks(), &token, GRANT_TYP, NOW).unwrap();
    assert_eq!(back, claims);

    let user = UserAssertionClaims {
        session_hash: None,
        fresh_mfa: None,
        iss: "https://runbooks.example".into(),
        aud: "runner:ri_1".into(),
        jti: "j".into(),
        iat: NOW,
        exp: NOW + 60,
        org_id: "org_1".into(),
        user_id: "usr_1".into(),
        principal: "human".into(),
        sponsor_id: None,
        org_role: "admin".into(),
        project_roles: BTreeMap::from([("prj_1".to_owned(), "editor".to_owned())]),
        scopes: vec!["runner:read".into()],
        htm: "GET".into(),
        htu: "/runner/v1/groups".into(),
        bh: "47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU".into(),
    };
    let token = ring
        .sign(KeyPurpose::UserAssertion, USER_TYP, &user)
        .unwrap();
    let back: UserAssertionClaims = verify(&ring.jwks(), &token, USER_TYP, NOW).unwrap();
    assert_eq!(back, user);
    // The capability kind carries no credential fields on the wire.
    let mut capability = grant(NOW + 600);
    capability.kind = GrantKind::Capability;
    capability.credential_id = None;
    capability.credential_version = None;
    capability.delivery = None;
    let token = ring
        .sign(KeyPurpose::Grant, GRANT_TYP, &capability)
        .unwrap();
    let payload = token.split('.').nth(1).unwrap();
    let payload: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
    assert_eq!(payload["kind"], "capability");
    assert!(payload.get("credential_id").is_none());
}

#[test]
fn verify_rejects_wrong_typ() {
    let ring = ring();
    let token = ring
        .sign(KeyPurpose::Grant, GRANT_TYP, &grant(NOW + 600))
        .unwrap();
    assert_eq!(
        verify::<GrantClaims>(&ring.jwks(), &token, USER_TYP, NOW),
        Err(SigningError::Type)
    );
    // A purpose signs only its own type.
    assert_eq!(
        ring.sign(KeyPurpose::UserAssertion, GRANT_TYP, &grant(NOW + 600)),
        Err(SigningError::Type)
    );
}

#[test]
fn verify_rejects_non_eddsa_alg() {
    let ring = ring();
    let token = ring
        .sign(KeyPurpose::Grant, GRANT_TYP, &grant(NOW + 600))
        .unwrap();
    let mut parts: Vec<String> = token.split('.').map(str::to_owned).collect();
    let mut header: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&parts[0]).unwrap()).unwrap();
    for alg in ["HS256", "none", "RS256", "ES256"] {
        header["alg"] = alg.into();
        parts[0] = URL_SAFE_NO_PAD.encode(header.to_string());
        assert_eq!(
            verify::<GrantClaims>(&ring.jwks(), &parts.join("."), GRANT_TYP, NOW),
            Err(SigningError::Algorithm),
            "{alg}"
        );
    }
}

#[test]
fn verify_allows_30s_skew_only() {
    let ring = ring();
    let jwks = ring.jwks();
    let ok = ring
        .sign(KeyPurpose::Grant, GRANT_TYP, &grant(NOW - 29))
        .unwrap();
    assert!(verify::<GrantClaims>(&jwks, &ok, GRANT_TYP, NOW).is_ok());
    let late = ring
        .sign(KeyPurpose::Grant, GRANT_TYP, &grant(NOW - 31))
        .unwrap();
    assert_eq!(
        verify::<GrantClaims>(&jwks, &late, GRANT_TYP, NOW),
        Err(SigningError::Expired)
    );
    // Issued in the future: 29 s is tolerated, 31 s is not.
    let mut early = grant(NOW + 600);
    early.iat = NOW + 29;
    let token = ring.sign(KeyPurpose::Grant, GRANT_TYP, &early).unwrap();
    assert!(verify::<GrantClaims>(&jwks, &token, GRANT_TYP, NOW).is_ok());
    early.iat = NOW + 31;
    let token = ring.sign(KeyPurpose::Grant, GRANT_TYP, &early).unwrap();
    assert_eq!(
        verify::<GrantClaims>(&jwks, &token, GRANT_TYP, NOW),
        Err(SigningError::NotYetValid)
    );
}

#[test]
fn purposes_use_distinct_kids() {
    let ring = ring();
    let jwks = ring.jwks();
    assert_eq!(jwks.keys.len(), 2);
    assert_ne!(jwks.keys[0].kid, jwks.keys[1].kid);
    for key in &jwks.keys {
        assert_eq!(
            (
                key.kty.as_str(),
                key.crv.as_str(),
                key.alg.as_str(),
                key.key_use.as_str()
            ),
            ("OKP", "Ed25519", "EdDSA", "sig")
        );
    }
    // Same seed, different purposes: still different kids.
    let same = KeyRing::new(vec![
        SigningKey::from_seed(KeyPurpose::Grant, [9; 32], NOW),
        SigningKey::from_seed(KeyPurpose::UserAssertion, [9; 32], NOW),
    ]);
    let keys = same.jwks().keys;
    assert_ne!(keys[0].kid, keys[1].kid);
    // A token signed for one purpose does not verify against the other
    // purpose's key even when its header names it.
    let token = ring
        .sign(KeyPurpose::Grant, GRANT_TYP, &grant(NOW + 60))
        .unwrap();
    let parts: Vec<&str> = token.split('.').collect();
    let mut header: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    let user_kid = ring
        .jwks()
        .keys
        .iter()
        .find(|k| k.kid.starts_with("user"))
        .unwrap()
        .kid
        .clone();
    header["kid"] = user_kid.into();
    let forged = format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        parts[1],
        parts[2]
    );
    assert!(verify::<GrantClaims>(&ring.jwks(), &forged, GRANT_TYP, NOW).is_err());
}

#[test]
fn tampered_payload_and_unknown_key_fail() {
    let ring = ring();
    let token = ring
        .sign(KeyPurpose::Grant, GRANT_TYP, &grant(NOW + 600))
        .unwrap();
    let parts: Vec<&str> = token.split('.').collect();
    let mut claims = grant(NOW + 600);
    claims.sub = "sr_other".into();
    let forged = format!(
        "{}.{}.{}",
        parts[0],
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
        parts[2]
    );
    assert_eq!(
        verify::<GrantClaims>(&ring.jwks(), &forged, GRANT_TYP, NOW),
        Err(SigningError::Signature)
    );
    let other = KeyRing::new(vec![SigningKey::from_seed(KeyPurpose::Grant, [3; 32], NOW)]);
    assert_eq!(
        verify::<GrantClaims>(&other.jwks(), &token, GRANT_TYP, NOW),
        Err(SigningError::UnknownKey)
    );
    assert_eq!(
        verify::<GrantClaims>(&ring.jwks(), "a.b", GRANT_TYP, NOW),
        Err(SigningError::Malformed)
    );
}
