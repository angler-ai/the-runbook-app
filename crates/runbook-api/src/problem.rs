use runbook_model::Problem;
use serde_json::Value;
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub body: Value,
    pub request_id: String,
    pub problem: bool,
    pub etag: Option<String>,
    /// A `text/markdown` body instead of JSON (the document export, D34).
    /// Boxed: rare, and `Response` is an error type.
    pub markdown: Option<Box<String>>,
    /// A file download instead of JSON (the JUnit and PDF reports, REP2).
    /// Boxed: most responses have none, and `Response` is an error type.
    pub attachment: Option<Box<Attachment>>,
    /// `Retry-After` in seconds (ARC3: a stale archive day).
    pub retry_after: Option<u16>,
    /// A `302 Found` target (a person's Google photo, served from Google).
    /// Boxed: rare, and `Response` is an error type.
    pub location: Option<Box<String>>,
}
/// A body served with `Content-Disposition: attachment`.
#[derive(Clone, Debug)]
pub struct Attachment {
    pub content_type: &'static str,
    /// A safe file name: ASCII letters, digits, `.`, `_` and `-` only.
    pub filename: String,
    pub bytes: Vec<u8>,
    /// Shown in place (an image an `<img>` loads), not downloaded.
    pub inline: bool,
}
impl Response {
    pub fn json(status: u16, body: Value, id: &str) -> Self {
        Self {
            status,
            body,
            request_id: id.into(),
            problem: false,
            etag: None,
            markdown: None,
            attachment: None,
            retry_after: None,
            location: None,
        }
    }
    pub fn markdown(status: u16, text: String, id: &str) -> Self {
        Self {
            markdown: Some(Box::new(text)),
            ..Self::json(status, Value::Null, id)
        }
    }
    pub fn attachment(status: u16, attachment: Attachment, id: &str) -> Self {
        Self {
            attachment: Some(Box::new(attachment)),
            ..Self::json(status, Value::Null, id)
        }
    }
    pub fn error(status: u16, code: &str, id: &str) -> Self {
        let p = Problem {
            type_uri: format!("urn:runbook:problem:{code}"),
            title: code.into(),
            status,
            detail: code.into(),
            instance: format!("urn:request:{id}"),
            code: code.into(),
            errors: vec![],
            next_action: None,
        };
        Self {
            status,
            body: serde_json::to_value(p).expect("problem serialization"),
            request_id: id.into(),
            problem: true,
            etag: None,
            markdown: None,
            attachment: None,
            retry_after: None,
            location: None,
        }
    }
}
