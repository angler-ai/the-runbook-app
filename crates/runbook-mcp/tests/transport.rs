//! F72: one malformed input line (not UTF-8) is a parse error for that
//! line, not the end of the session.
use runbook_mcp::backend::{Backend, BackendError, HttpCall, HttpReply};
use runbook_mcp::serve;
use serde_json::Value;

struct NoCalls;
impl Backend for NoCalls {
    fn call(&mut self, call: HttpCall) -> Result<HttpReply, BackendError> {
        panic!("unexpected call {call:?}")
    }
    fn random_token(&mut self) -> Result<String, BackendError> {
        Ok("n".into())
    }
}

#[test]
fn a_non_utf8_line_is_a_parse_error_and_the_session_continues() {
    let mut input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".to_vec();
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"\xff\xfe\"}\n");
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n");
    let mut out = vec![];
    serve(&mut NoCalls, input.as_slice(), &mut out).expect("serve keeps going");
    let lines: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[0]["id"], 1);
    assert_eq!(lines[1]["error"]["code"], -32700);
    assert_eq!(lines[2]["id"], 3);
}
