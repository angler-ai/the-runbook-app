//! MCP2: OAuth 2.1 access tokens on `/mcp` (PRD §11), the portable half.
//!
//! The authorization server is the auth service (Better Auth's OAuth
//! provider plugin, `auth/src/oauth.ts`). This side is the resource server,
//! kept small for the api Worker's size ceiling:
//!
//! - **Discovery.** The RFC 9728 protected resource metadata and the RFC 8414
//!   authorization server metadata are both the auth service's; the host
//!   proxies [`PROXIED_PATHS`] to it like `/api/auth/*`. A 401 on `/mcp`
//!   names the former in `WWW-Authenticate` ([`challenge`]) when sign-in is
//!   configured.
//! - **Validation.** An access token (`trb_oat_`) is opaque. The host asks
//!   the auth service's internal introspection route (the D44 call, cached
//!   30 s by the token's SHA-256 in the session cache, [`TokenIntrospector`]),
//!   so native and Workers share one path and Rust has no JWT stack. The
//!   answer has the `/internal/session` shape plus the granted `scope`.
//! - **Audience (RFC 8707).** The host names this server's `/mcp`
//!   ([`resource`]) in the call; the auth service answers only for a token
//!   whose audience includes it, so a token minted for another resource is
//!   refused.
//! - **Principal.** The answer is a [`ResolvedSession`] with `scopes` set.
//!   `/mcp` passes it to its tools' calls as a session credential, so it is
//!   linked to a directory user exactly as a browser session is (D45,
//!   [`crate::session::link`]) and follows the session rules (D46: never
//!   mints execution credentials or approves device codes); with `scopes`
//!   set, `link` makes it an agent sponsored by that user, with the granted
//!   scopes intersected with the user's own, so never wider than the role.
use crate::{ResolvedSession, session::Unavailable};
use std::future::Future;

/// The access token prefix the auth service issues (`trb_ort_` is its
/// refresh token, never a bearer here).
pub const ACCESS_PREFIX: &str = "trb_oat_";
/// The discovery documents the auth service answers through the host's
/// proxy: RFC 9728 for `/mcp` (root and path-inserted forms) and RFC 8414
/// for the issuer `{origin}/api/auth`.
pub const PROXIED_PATHS: [&str; 4] = [
    "/.well-known/oauth-protected-resource",
    "/.well-known/oauth-protected-resource/mcp",
    "/.well-known/oauth-authorization-server",
    "/.well-known/oauth-authorization-server/api/auth",
];

/// MCP2 review 6: how long a host remembers that a token is not active, so a
/// client retrying a dead token does not reach the auth service each time.
pub const NEGATIVE_CACHE_MS: u64 = 5_000;

/// Whether the host proxies `path` to the auth service.
pub fn proxied(path: &str) -> bool {
    PROXIED_PATHS.contains(&path)
}

/// This server's `/mcp` as an RFC 8707 resource identifier.
pub fn resource(origin: &str) -> String {
    format!("{origin}{}", crate::mcp::PATH)
}

/// Whether a bearer is an OAuth access token.
pub fn is_access_token(secret: &str) -> bool {
    secret.starts_with(ACCESS_PREFIX)
}

/// `WWW-Authenticate` for a 401 on `/mcp` (RFC 6750, RFC 9728 §5.1). With an
/// authorization server it names the metadata; `invalid` adds
/// `error="invalid_token"` for a presented token that does not work.
pub fn challenge(origin: &str, enabled: bool, invalid: bool) -> String {
    let (open, close) = if enabled {
        (
            " resource_metadata=\"",
            "/.well-known/oauth-protected-resource/mcp\"",
        )
    } else {
        ("", "")
    };
    format!(
        "Bearer{open}{}{close}{}",
        if enabled { origin } else { "" },
        match (enabled, invalid) {
            (true, true) => ", error=\"invalid_token\"",
            (false, true) => " error=\"invalid_token\"",
            _ => "",
        }
    )
}

/// D44 for OAuth: what an access token is. The answer is a
/// [`ResolvedSession`] whose `scopes` are the grant's; `None` for an unknown,
/// expired or revoked token, or one minted for another resource. A host
/// caches answers 30 s by the token's SHA-256 (never the token).
pub trait TokenIntrospector {
    fn introspect(
        &self,
        token: &str,
        resource: &str,
    ) -> impl Future<Output = Result<Option<ResolvedSession>, Unavailable>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn challenges_name_the_metadata_when_there_is_a_server() {
        let o = "https://runbooks.example";
        assert_eq!(challenge(o, false, false), "Bearer");
        assert_eq!(challenge(o, false, true), "Bearer error=\"invalid_token\"");
        assert_eq!(
            challenge(o, true, false),
            "Bearer resource_metadata=\"https://runbooks.example/.well-known/oauth-protected-resource/mcp\""
        );
        assert_eq!(
            challenge(o, true, true),
            "Bearer resource_metadata=\"https://runbooks.example/.well-known/oauth-protected-resource/mcp\", error=\"invalid_token\""
        );
        assert!(proxied("/.well-known/oauth-protected-resource/mcp"));
        assert!(!proxied("/.well-known/oauth-protected-resource/x"));
        assert!(is_access_token("trb_oat_abc") && !is_access_token("trb_pat_abc"));
        assert_eq!(resource(o), "https://runbooks.example/mcp");
    }
}
