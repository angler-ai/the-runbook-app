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

/// Whether a request is a person opening an address in the browser (from
/// an email or a pasted link) rather than a program calling the API: a `GET`
/// with `Sec-Fetch-Dest: document`, or, from a browser without Fetch
/// Metadata, an `Accept` header that lists `text/html`.
pub fn is_navigation(method: &str, sec_fetch_dest: Option<&str>, accept: Option<&str>) -> bool {
    method.eq_ignore_ascii_case("GET")
        && match sec_fetch_dest {
            Some(dest) => dest.trim().eq_ignore_ascii_case("document"),
            None => accept.is_some_and(|accept| {
                accept.split(',').any(|item| {
                    item.split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .eq_ignore_ascii_case("text/html")
                })
            }),
        }
}

/// Where to send a browser navigation that the API refused under `/v1`,
/// instead of showing it the problem JSON: signed out (401) to sign-in,
/// refused or missing (403, 404, 405, 410) to the web app's not-found page.
/// Other answers, and every program's request, keep the problem as it is.
pub fn navigation_redirect(path: &str, status: u16) -> Option<&'static str> {
    if !(path == "/v1" || path.starts_with("/v1/")) {
        return None;
    }
    match status {
        401 => Some("/sign-in"),
        403 | 404 | 405 | 410 => Some("/not-found"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_navigations_to_refused_api_addresses_redirect() {
        assert!(is_navigation("GET", Some("document"), None));
        assert!(
            !is_navigation("GET", Some("empty"), Some("text/html")),
            "fetch() from the app"
        );
        assert!(!is_navigation("POST", Some("document"), None));
        assert!(is_navigation(
            "GET",
            None,
            Some("text/html,application/xhtml+xml,*/*;q=0.8")
        ));
        assert!(
            !is_navigation("GET", None, Some("application/json")),
            "the CLI"
        );
        assert!(!is_navigation("GET", None, None));
        assert_eq!(navigation_redirect("/v1/projects", 401), Some("/sign-in"));
        assert_eq!(
            navigation_redirect("/v1/executions/exe_1", 404),
            Some("/not-found")
        );
        assert_eq!(navigation_redirect("/v1/projects", 500), None);
        assert_eq!(navigation_redirect("/api/auth/session", 401), None);
        assert_eq!(navigation_redirect("/mcp", 401), None);
    }
    #[test]
    fn reserved_prefixes_match_whole_segments_raw_or_decoded() {
        assert!(api_owns("/v1") && api_owns("/v1/meta") && api_owns("/mcp"));
        assert!(api_owns("/api/auth/ok") && api_owns("/.well-known/runbook"));
        assert!(!api_owns("/v1x") && !api_owns("/") && !api_owns("/runs/exe_1"));
        assert!(api_owns("/%76%31/meta"), "decoded /v1/meta");
        assert!(api_owns("/%zz"), "an undecodable path is the API's 404");
    }
}
