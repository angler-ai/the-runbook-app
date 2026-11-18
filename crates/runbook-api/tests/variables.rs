#[test]
fn project_variables_reject_d34_secret_patterns() {
    use runbook_api::variables::secret_like;
    use serde_json::json;
    for value in [
        "Authorization: Basic abc",
        "Bearer abc",
        "AKIA0123456789ABCDEF",
        "ASIA0123456789ABCDEF",
        "ghp_abc",
        "gho_abc",
        "ghs_abc",
        "github_pat_abc",
        "xoxb-abc",
        "-----BEGIN RSA PRIVATE KEY-----",
        "-----BEGIN PRIVATE KEY-----",
        "password=abc",
        "secret=abc",
        "token=abc",
    ] {
        assert!(
            secret_like(&json!({"nested":[{"value":value}]})),
            "pattern was missed"
        );
    }
    for kind in [
        runbook_core::credentials::TokenKind::Personal,
        runbook_core::credentials::TokenKind::Service,
        runbook_core::credentials::TokenKind::Execution,
        runbook_core::credentials::TokenKind::Refresh,
    ] {
        assert!(secret_like(&json!(
            runbook_core::credentials::encode_secret(kind, [9; 32])
        )));
    }
    assert!(!secret_like(
        &json!({"region":"us-east","attempts":3,"enabled":true})
    ));
}
