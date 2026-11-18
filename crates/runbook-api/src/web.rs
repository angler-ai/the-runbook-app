//! The web UI's place on the app origin (B2), shared by the native server
//! (`runbook-server/src/web_assets.rs`) and the Cloudflare edition (CF7):
//! the path prefixes the API owns, never served as files, and the headers
//! every web response carries.

/// Path prefixes the server owns, now or as planned routes. Matched as a
/// whole segment (`/v1` and `/v1/...`, not `/v1x`) on both the raw and the
/// decoded path.
pub const RESERVED: &[&str] = &[
    "/v1",
    "/.well-known",
    "/mcp",
    "/api",
    "/hooks",
    "/healthz",
    "/internal",
    "/bootstrap",
];

/// F96 and plan §3 B2. `'wasm-unsafe-eval'` lets the browser compiler (B3)
/// instantiate; `blob:` workers are Monaco's; inline styles are Radix's;
/// `data:` fonts are the small @fontsource subsets Vite inlines (under its
/// 4 KiB asset limit) into the stylesheet, refused otherwise (M6 container).
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self' data:; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Hashed `assets/*` files never change.
pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// Whether `path` (one form of it) starts with a reserved prefix.
pub fn is_reserved(path: &str) -> bool {
    RESERVED.iter().any(|prefix| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// A path decoded once, or `None` when an escape is malformed or the
/// result is not UTF-8.
pub fn percent_decode(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Whether the API answers `path` rather than the web UI: a reserved
/// prefix in the raw or the decoded path, or a path that does not decode.
pub fn api_owns(path: &str) -> bool {
    is_reserved(path) || percent_decode(path).is_none_or(|decoded| is_reserved(&decoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reserved_prefixes_match_whole_segments_raw_or_decoded() {
        assert!(api_owns("/v1") && api_owns("/v1/meta") && api_owns("/mcp"));
        assert!(api_owns("/api/auth/ok") && api_owns("/.well-known/runbook"));
        assert!(!api_owns("/v1x") && !api_owns("/") && !api_owns("/runs/exe_1"));
        assert!(api_owns("/%76%31/meta"), "decoded /v1/meta");
        assert!(api_owns("/%zz"), "an undecodable path is the API's 404");
    }
}
