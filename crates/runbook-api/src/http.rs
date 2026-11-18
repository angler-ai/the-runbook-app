//! CF0: HTTP shapes shared by every host. A host turns its own request
//! into [`Headers`] (and, for the API, a [`crate::Request`]), and turns an
//! [`HttpResponse`] back into its own response; nothing here knows axum,
//! hyper or the Workers runtime.

/// The largest JSON request body (`/v1` and `/mcp`): twice the compiler's
/// 1 MiB source ceiling plus room for the JSON around it, so a source at
/// the ceiling saves even when every byte is escaped. The native server's
/// default (`TransportLimits::json_body_bytes`) and the Workers edition's
/// cap (CF12) on every route.
pub const JSON_BODY_BYTES: u64 = 2 * 1_048_576 + 65_536;

/// The largest request body the sign-in proxy (`/api/auth/*` and the OAuth
/// routes it forwards) passes on: sign-in requests are small JSON and
/// OAuth callbacks are GETs. Both editions' proxies refuse more with 413
/// `request.body_too_large` (CF12 review round 1).
pub const AUTH_BODY_BYTES: u64 = 64 * 1024;

/// Request headers in arrival order. Names are compared case-insensitively
/// (stored lowercase); values are raw bytes, as on the wire.
#[derive(Clone, Debug, Default)]
pub struct Headers {
    entries: Vec<(String, Vec<u8>)>,
}

/// `http::HeaderValue::to_str`: visible ASCII (and tab) only.
fn visible(bytes: &[u8]) -> Option<&str> {
    bytes
        .iter()
        .all(|b| *b == b'\t' || (32..127).contains(b))
        .then(|| std::str::from_utf8(bytes).ok())
        .flatten()
}

impl Headers {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, name: &str, value: impl Into<Vec<u8>>) {
        self.entries.push((name.to_ascii_lowercase(), value.into()));
    }
    /// Every value of `name`, raw, in order.
    pub fn get_all<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a [u8]> + 'a {
        let name = name.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(move |(n, _)| *n == name)
            .map(|(_, v)| v.as_slice())
    }
    /// Every value of `name` that is visible ASCII, in order.
    pub fn get_all_str<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a str> + 'a {
        self.get_all(name).filter_map(visible)
    }
    /// The first value of `name` as text (`None` when absent or not
    /// visible ASCII).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.get_all(name).next().and_then(visible)
    }
    pub fn contains(&self, name: &str) -> bool {
        self.get_all(name).next().is_some()
    }
    /// Exactly one value of `name`, as text.
    pub fn one(&self, name: &str) -> Option<&str> {
        let mut values = self.get_all(name);
        match (values.next(), values.next()) {
            (Some(v), None) => visible(v),
            _ => None,
        }
    }
}

impl<N: AsRef<str>, V: Into<Vec<u8>>> FromIterator<(N, V)> for Headers {
    fn from_iter<I: IntoIterator<Item = (N, V)>>(iter: I) -> Self {
        let mut headers = Headers::new();
        for (name, value) in iter {
            headers.push(name.as_ref(), value);
        }
        headers
    }
}

/// One `Authorization: Bearer` header; duplicates or other schemes are none.
pub fn bearer(headers: &Headers) -> Option<String> {
    if headers.get_all("authorization").count() != 1 {
        return None;
    }
    headers
        .get("authorization")
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::to_owned)
}

/// `Content-Type: application/json` (parameters allowed).
pub fn is_json(headers: &Headers) -> bool {
    headers.get("content-type").is_some_and(|v| {
        v.split(';')
            .next()
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
    })
}

/// F45: what a request's headers say about its body, decided before any
/// body byte is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyPlan {
    /// A skill bundle upload (`POST /v1/projects/{p}/skills`): the large
    /// request limit applies instead of the JSON one.
    pub upload: bool,
    /// The declared `Content-Length`.
    pub declared: Option<u64>,
    /// A body is coming (a non-zero length, or chunked).
    pub has_body: bool,
    /// D47: the CLI's device-code request (the host rate-limits it per
    /// address before anything else).
    pub device_code: bool,
    /// A body from a caller with no credential is refused (401), except on
    /// the refresh route (its body authenticates, PRD §9) and the device
    /// grant's calls (D47). Internal routes are the host's to exempt.
    pub needs_credential: bool,
}

/// [`BodyPlan`] from the headers; `Err` is 400 `request.invalid_length`.
pub fn plan_body(
    method: &str,
    path: &str,
    headers: &Headers,
    id: &str,
) -> Result<BodyPlan, crate::problem::Response> {
    let upload =
        method == "POST" && crate::directory::match_path("/v1/projects/{p}/skills", path).is_some();
    let declared = match headers.get_all("content-length").next() {
        None => None,
        Some(_) => match headers
            .get("content-length")
            .and_then(|v| v.parse::<u64>().ok())
        {
            Some(n) => Some(n),
            None => {
                return Err(crate::problem::Response::error(
                    400,
                    "request.invalid_length",
                    id,
                ));
            }
        },
    };
    let has_body = declared.is_some_and(|n| n > 0) || headers.contains("transfer-encoding");
    let device_poll =
        method == "POST" && matches!(path, crate::device::CODE | crate::device::TOKEN);
    Ok(BodyPlan {
        upload,
        declared,
        has_body,
        device_code: device_poll && path == crate::device::CODE,
        needs_credential: has_body && anonymous_body_refused(method, path),
    })
}

/// Whether a body from a caller with no credential is refused (401): every
/// route but the refresh route (its body authenticates, PRD §9), the
/// device grant's calls (D47) and a webhook trigger (LANG9b).
pub fn anonymous_body_refused(method: &str, path: &str) -> bool {
    let refresh =
        crate::directory::match_path("/v1/executions/{exe}/tokens/refresh", path).is_some();
    let device_poll =
        method == "POST" && matches!(path, crate::device::CODE | crate::device::TOKEN);
    // LANG9b: a webhook trigger's token may be in its URL (`?token=`); the
    // executions router refuses the call without one, header or query.
    let trigger = method == "POST"
        && crate::directory::match_path(runbook_model::TRIGGER_PATH, path).is_some();
    // NOTIFY1: the one-click unsubscribe body is its credential.
    let unsubscribe = method == "POST" && path == "/v1/unsubscribe";
    !refresh && !device_poll && !trigger && !unsubscribe
}

/// The API request from a read body: `Content-Type: application/json`
/// for any body (415), valid JSON (422) and a valid query string (400).
#[allow(clippy::too_many_arguments)]
pub fn decode(
    method: &str,
    path: &str,
    query: Option<&str>,
    headers: &Headers,
    bytes: &[u8],
    credential: Option<crate::Credential>,
    request_id: &str,
    nonce: String,
) -> Result<crate::Request, crate::problem::Response> {
    use crate::problem::Response;
    if !bytes.is_empty() && !is_json(headers) {
        return Err(Response::error(415, "request.content_type", request_id));
    }
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(bytes)
            .map_err(|_| Response::error(422, "request.invalid_json", request_id))?
    };
    let query = serde_urlencoded::from_str(query.unwrap_or(""))
        .map_err(|_| Response::error(400, "request.invalid_query", request_id))?;
    Ok(crate::Request {
        method: method.to_owned(),
        path: path.to_owned(),
        query,
        body,
        credential,
        idempotency_key: headers.get("idempotency-key").map(str::to_owned),
        if_match: headers.get("if-match").map(if_match_tag),
        client_ip: None,
        request_id: request_id.into(),
        new_id: nonce,
    })
}

/// The entity tag an `If-Match` header names. A proxy that compresses a
/// response weakens its ETag (Cloudflare serves `"t"` as `W/"t"`), and
/// clients send back what they were given. Our tags name a revision, not a
/// byte representation, so the weak form names the same revision; without
/// this every draft save and publish behind Cloudflare failed with 412
/// `catalog.draft_conflict`.
fn if_match_tag(value: &str) -> String {
    let value = value.trim();
    value.strip_prefix("W/").unwrap_or(value).to_owned()
}

/// A response ready for the wire: status, headers in order, body bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: vec![],
            body: vec![],
        }
    }
    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }
    /// Replace every value of `name` with one value.
    pub fn set_header(&mut self, name: &str, value: impl Into<String>) {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self.headers.push((name.to_owned(), value.into()));
    }
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The API's response on the wire (F96 and D34/REP2 shapes): request ID,
/// rate-limit and cache headers, `Retry-After` on a busy server, the ETag,
/// and a JSON, problem, Markdown or attachment body. `X-Content-Type-Options`
/// is the transport's, on every response.
pub fn encode(response: crate::problem::Response) -> HttpResponse {
    let mut out = HttpResponse::new(response.status)
        .header("x-request-id", response.request_id)
        .header("x-ratelimit-limit", "unlimited")
        .header("x-ratelimit-remaining", "unlimited")
        .header("cache-control", "no-store");
    // A busy server asks for a retry rather than queueing the request.
    if response.status == 503 && response.body["code"] == "server.busy" {
        out = out.header("retry-after", "1");
    }
    if let Some(etag) = response.etag {
        out = out.header("etag", etag);
    }
    // ARC: a stale archive day asks for a retry after its rewrite.
    if let Some(seconds) = response.retry_after {
        out = out.header("retry-after", seconds.to_string());
    }
    if response.status == 204 {
        return out;
    }
    // REP2: report downloads. The id-derived file name is already safe.
    if let Some(location) = response.location {
        return out.header("location", *location);
    }
    if let Some(file) = response.attachment {
        out = out.header("content-type", file.content_type).header(
            "content-disposition",
            format!(
                "{}; filename=\"{}\"",
                if file.inline { "inline" } else { "attachment" },
                file.filename
            ),
        );
        if file.inline {
            // A picture changes rarely and its URL carries its version.
            out = out
                .header("cache-control", "private, max-age=300")
                .header("x-content-type-options", "nosniff");
        }
        out.body = file.bytes;
        return out;
    }
    if let Some(markdown) = response.markdown {
        out = out.header("content-type", "text/markdown; charset=utf-8");
        out.body = markdown.into_bytes();
        return out;
    }
    out = out.header(
        "content-type",
        if response.problem {
            "application/problem+json"
        } else {
            "application/json"
        },
    );
    out.body = serde_json::to_vec(&response.body).expect("JSON response");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headers_match_names_case_insensitively_and_keep_raw_values() {
        let headers: Headers = [
            ("Authorization", b"Bearer a".to_vec()),
            ("cookie", b"x=1".to_vec()),
            ("Cookie", vec![0xff]),
        ]
        .into_iter()
        .collect();
        assert_eq!(bearer(&headers).as_deref(), Some("a"));
        assert_eq!(headers.get_all("COOKIE").count(), 2);
        assert_eq!(headers.get_all_str("cookie").collect::<Vec<_>>(), ["x=1"]);
        assert_eq!(headers.one("cookie"), None);
        let twice: Headers = [("authorization", "Bearer a"), ("authorization", "Bearer b")]
            .into_iter()
            .collect();
        assert_eq!(bearer(&twice), None);
    }

    #[test]
    fn if_match_accepts_the_weak_form_a_compressing_proxy_returns() {
        // Cloudflare serves `"draft-rb_1-1"` as `W/"draft-rb_1-1"` once it
        // compresses the response, and clients echo what they were given.
        let tag = |value: &str| {
            let headers: Headers = [("If-Match", value)].into_iter().collect();
            decode(
                "POST",
                "/x",
                None,
                &headers,
                b"",
                None,
                "req",
                String::new(),
            )
            .unwrap()
            .if_match
        };
        assert_eq!(
            tag(r#"W/"draft-rb_1-1""#).as_deref(),
            Some(r#""draft-rb_1-1""#)
        );
        assert_eq!(
            tag(r#""draft-rb_1-1""#).as_deref(),
            Some(r#""draft-rb_1-1""#)
        );
        assert_eq!(
            tag(r#" "draft-rb_1-1" "#).as_deref(),
            Some(r#""draft-rb_1-1""#)
        );
    }
}
