//! R0-1: asymmetric signing for the runner (design §7.5, §11.4).
//!
//! One Ed25519 key pair per purpose: release grants (`rb-grant+jwt`) and
//! user assertions (`rb-user+jwt`). Public keys are published at
//! `/.well-known/jwks.json`; verifiers accept only `alg: EdDSA` and allow
//! [`CLOCK_SKEW_S`] of clock skew. Hosts keep the private keys (sealed at
//! rest natively, `runbook-server/src/signing.rs`) and hand this module a
//! [`KeyRing`] of the current and recently retired keys.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt};

/// Verify a vault statement whose UTF-8 fields are individually length-prefixed.
pub fn verify_vault_statement(public: &str, signature: &str, fields: &[&str]) -> bool {
    let Ok(public) = URL_SAFE_NO_PAD.decode(public) else {
        return false;
    };
    let Ok(public) = <[u8; 32]>::try_from(public) else {
        return false;
    };
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(&public) else {
        return false;
    };
    let Ok(signature) = URL_SAFE_NO_PAD.decode(signature) else {
        return false;
    };
    let Ok(signature) = ed25519_dalek::Signature::from_slice(&signature) else {
        return false;
    };
    let mut statement = Vec::new();
    for field in fields {
        let Ok(length) = u32::try_from(field.len()) else {
            return false;
        };
        statement.extend(length.to_be_bytes());
        statement.extend(field.as_bytes());
    }
    key.verify_strict(&statement, &signature).is_ok()
}

/// JWT `typ` of a release or capability grant.
pub const GRANT_TYP: &str = "rb-grant+jwt";
/// JWT `typ` of a user assertion forwarded through the runner proxy.
pub const USER_TYP: &str = "rb-user+jwt";
/// The only accepted algorithm.
pub const ALG: &str = "EdDSA";
/// Seconds of clock skew a verifier tolerates on `exp`, `iat` and `nbf`.
pub const CLOCK_SKEW_S: i64 = 30;
/// How long a retired key stays in the JWKS (and verifies) after rotation.
pub const RETIRED_KEY_TTL_S: i64 = 24 * 3600;
/// User assertions live this long.
pub const USER_ASSERTION_TTL_S: i64 = 60;

/// What a key signs. Each purpose has its own key and accepted types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyPurpose {
    Grant,
    UserAssertion,
}
impl KeyPurpose {
    pub const ALL: [KeyPurpose; 2] = [Self::Grant, Self::UserAssertion];
    /// The stored and `kid` prefix name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grant => "grant",
            Self::UserAssertion => "user",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == value)
    }
    /// The primary `typ` this purpose's key signs.
    pub fn typ(self) -> &'static str {
        match self {
            Self::Grant => GRANT_TYP,
            Self::UserAssertion => USER_TYP,
        }
    }
    fn accepts(self, typ: &str) -> bool {
        self.typ() == typ
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SigningError {
    /// Not three base64url parts of JSON.
    Malformed,
    /// A header `alg` other than `EdDSA`.
    Algorithm,
    /// A header `typ` other than the expected one, or a purpose asked to
    /// sign another purpose's type.
    Type,
    /// No published key with the header's `kid` for this type.
    UnknownKey,
    Signature,
    /// `exp` more than [`CLOCK_SKEW_S`] in the past.
    Expired,
    /// `iat` or `nbf` more than [`CLOCK_SKEW_S`] in the future.
    NotYetValid,
    /// The payload does not have the expected claims.
    Claims,
    /// The host has no key for this purpose (no sealing key configured).
    Unavailable,
}
impl SigningError {
    /// A stable problem code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Malformed => "jwt.malformed",
            Self::Algorithm => "jwt.algorithm",
            Self::Type => "jwt.type",
            Self::UnknownKey => "jwt.unknown_key",
            Self::Signature => "jwt.signature",
            Self::Expired => "jwt.expired",
            Self::NotYetValid => "jwt.not_yet_valid",
            Self::Claims => "jwt.claims",
            Self::Unavailable => "signing.unavailable",
        }
    }
}
impl fmt::Display for SigningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

pub use runbook_model::runner_api::{Jwk, Jwks};

/// A private key for one purpose. Never serialized; Debug is redacted.
#[derive(Clone)]
pub struct SigningKey {
    pub purpose: KeyPurpose,
    pub kid: String,
    key: ed25519_dalek::SigningKey,
    /// Epoch seconds.
    pub created_at: i64,
    /// Epoch seconds; set once rotated out. A retired key only verifies.
    pub retired_at: Option<i64>,
}
impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigningKey")
            .field("purpose", &self.purpose)
            .field("kid", &self.kid)
            .field("key", &"[REDACTED]")
            .finish()
    }
}
impl SigningKey {
    /// A key from 32 random bytes (the host's RNG).
    pub fn from_seed(purpose: KeyPurpose, seed: [u8; 32], created_at: i64) -> Self {
        let key = ed25519_dalek::SigningKey::from_bytes(&seed);
        Self {
            purpose,
            kid: kid(purpose, key.verifying_key().as_bytes()),
            key,
            created_at,
            retired_at: None,
        }
    }
    pub fn retired(mut self, at: Option<i64>) -> Self {
        self.retired_at = at;
        self
    }
    /// The private seed, for the host to seal at rest.
    pub fn seed(&self) -> [u8; 32] {
        self.key.to_bytes()
    }
    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }
    pub fn jwk(&self) -> Jwk {
        jwk(&self.kid, &self.public_key())
    }
}

/// `<purpose>-<first 16 base64url chars of SHA-256(purpose || public key)>`.
pub fn kid(purpose: KeyPurpose, public: &[u8; 32]) -> String {
    let mut hash = Sha256::new();
    hash.update(purpose.as_str().as_bytes());
    hash.update([0]);
    hash.update(public);
    let digest = URL_SAFE_NO_PAD.encode(hash.finalize());
    format!("{}-{}", purpose.as_str(), &digest[..16])
}
pub fn jwk(kid: &str, public: &[u8; 32]) -> Jwk {
    Jwk {
        kty: "OKP".into(),
        crv: "Ed25519".into(),
        x: URL_SAFE_NO_PAD.encode(public),
        kid: kid.into(),
        key_use: "sig".into(),
        alg: ALG.into(),
    }
}

/// Signs JWTs for one purpose with its current key and publishes the JWKS.
pub trait JwtSigner {
    fn sign<C: Serialize>(
        &self,
        purpose: KeyPurpose,
        typ: &str,
        claims: &C,
    ) -> Result<String, SigningError>;
    fn jwks(&self) -> Jwks;
}

/// The keys a host holds: per purpose, the current key (newest unretired)
/// and retired ones still published for [`RETIRED_KEY_TTL_S`].
#[derive(Clone, Debug, Default)]
pub struct KeyRing {
    keys: Vec<SigningKey>,
}
impl KeyRing {
    pub fn new(keys: Vec<SigningKey>) -> Self {
        Self { keys }
    }
    pub fn keys(&self) -> &[SigningKey] {
        &self.keys
    }
    /// The current key of a purpose.
    pub fn current(&self, purpose: KeyPurpose) -> Option<&SigningKey> {
        self.keys
            .iter()
            .filter(|k| k.purpose == purpose && k.retired_at.is_none())
            .max_by_key(|k| k.created_at)
    }
}
impl JwtSigner for KeyRing {
    fn sign<C: Serialize>(
        &self,
        purpose: KeyPurpose,
        typ: &str,
        claims: &C,
    ) -> Result<String, SigningError> {
        if !purpose.accepts(typ) {
            return Err(SigningError::Type);
        }
        let key = self.current(purpose).ok_or(SigningError::Unavailable)?;
        let header = serde_json::json!({"alg": ALG, "typ": typ, "kid": key.kid});
        let payload = serde_json::to_vec(claims).map_err(|_| SigningError::Claims)?;
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(payload)
        );
        let signature = key.key.sign(input.as_bytes());
        Ok(format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }
    fn jwks(&self) -> Jwks {
        let mut keys: Vec<&SigningKey> = self.keys.iter().collect();
        keys.sort_by_key(|k| (k.purpose, k.created_at));
        Jwks {
            keys: keys.into_iter().map(SigningKey::jwk).collect(),
        }
    }
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    typ: Option<String>,
    kid: Option<String>,
}

/// Verify a compact JWT against a JWKS: the header's `alg` must be `EdDSA`
/// (checked first), its `typ` the expected one, its `kid` a published key
/// of that type's purpose; then the signature; then `exp` (required),
/// `iat` and `nbf` with [`CLOCK_SKEW_S`] of skew at `now` (epoch seconds).
pub fn verify<C: DeserializeOwned>(
    jwks: &Jwks,
    token: &str,
    typ: &str,
    now: i64,
) -> Result<C, SigningError> {
    let mut parts = token.split('.');
    let (Some(header_b64), Some(payload_b64), Some(signature_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(SigningError::Malformed);
    };
    let decode = |part: &str| {
        URL_SAFE_NO_PAD
            .decode(part)
            .map_err(|_| SigningError::Malformed)
    };
    let header: Header =
        serde_json::from_slice(&decode(header_b64)?).map_err(|_| SigningError::Malformed)?;
    if header.alg != ALG {
        return Err(SigningError::Algorithm);
    }
    if header.typ.as_deref() != Some(typ) {
        return Err(SigningError::Type);
    }
    let purpose = KeyPurpose::ALL
        .into_iter()
        .find(|p| p.accepts(typ))
        .ok_or(SigningError::Type)?;
    let kid = header.kid.ok_or(SigningError::UnknownKey)?;
    // A kid names its purpose: a token of one type never verifies with the
    // other purpose's key.
    if !kid.starts_with(&format!("{}-", purpose.as_str())) {
        return Err(SigningError::UnknownKey);
    }
    let jwk = jwks
        .keys
        .iter()
        .find(|k| k.kid == kid && k.kty == "OKP" && k.crv == "Ed25519" && k.alg == ALG)
        .ok_or(SigningError::UnknownKey)?;
    let public: [u8; 32] = decode(&jwk.x)?
        .try_into()
        .map_err(|_| SigningError::UnknownKey)?;
    let key =
        ed25519_dalek::VerifyingKey::from_bytes(&public).map_err(|_| SigningError::UnknownKey)?;
    let signature: [u8; 64] = decode(signature_b64)?
        .try_into()
        .map_err(|_| SigningError::Signature)?;
    key.verify_strict(
        format!("{header_b64}.{payload_b64}").as_bytes(),
        &ed25519_dalek::Signature::from_bytes(&signature),
    )
    .map_err(|_| SigningError::Signature)?;
    let payload: serde_json::Value =
        serde_json::from_slice(&decode(payload_b64)?).map_err(|_| SigningError::Malformed)?;
    let exp = payload
        .get("exp")
        .and_then(serde_json::Value::as_i64)
        .ok_or(SigningError::Claims)?;
    if now > exp.saturating_add(CLOCK_SKEW_S) {
        return Err(SigningError::Expired);
    }
    for field in ["iat", "nbf"] {
        if let Some(at) = payload.get(field) {
            let at = at.as_i64().ok_or(SigningError::Claims)?;
            if at > now.saturating_add(CLOCK_SKEW_S) {
                return Err(SigningError::NotYetValid);
            }
        }
    }
    serde_json::from_value(payload).map_err(|_| SigningError::Claims)
}

pub use runbook_model::runner_api::{Delivery, GrantKind};

/// A release or capability grant (`typ: rb-grant+jwt`). `exp` is the
/// lease deadline; `sub` the step run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantClaims {
    pub iss: String,
    /// `runner:<runner id>`.
    pub aud: String,
    pub sub: String,
    pub kind: GrantKind,
    pub claim_id: String,
    pub generation: u64,
    pub runner_id: String,
    pub group_id: String,
    pub agent_id: String,
    pub agent_version: String,
    pub project_id: String,
    pub destructive: bool,
    /// Decision grants cap every tool, host and credential at read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reach: Option<runbook_model::RunnerLabel>,
    pub network: Option<Vec<String>>,
    pub connections: Option<Vec<String>>,
    pub model_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    pub max_tokens: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub step_deadline_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
}

/// A person's (or their agent's) request through the runner proxy
/// (`typ: rb-user+jwt`, 60 s, single use, bound to method, path and body).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserAssertionClaims {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_mfa: Option<bool>,
    pub iss: String,
    /// `runner:<install id>`.
    pub aud: String,
    pub jti: String,
    pub iat: i64,
    pub exp: i64,
    pub org_id: String,
    pub user_id: String,
    /// `human` or `agent`.
    pub principal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sponsor_id: Option<String>,
    /// `owner`, `admin` or `member`.
    pub org_role: String,
    /// Project ID to `viewer`, `operator`, `editor` or `admin`.
    pub project_roles: BTreeMap<String, String>,
    /// `runner:read` and/or `runner:admin`.
    pub scopes: Vec<String>,
    /// HTTP method.
    pub htm: String,
    /// Path and query.
    pub htu: String,
    /// base64url SHA-256 of the body.
    pub bh: String,
}

/// base64url (no padding) SHA-256 of a request body: [`UserAssertionClaims::bh`].
pub fn body_hash(body: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(body))
}
