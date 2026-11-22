use std::process::Command;

#[test]
fn mfa_reset_help_is_available_without_starting_auth() {
    let output = Command::new(env!("CARGO_BIN_EXE_runbook-server"))
        .args(["admin", "mfa", "reset", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(
        help.contains("runbook-server admin mfa reset <EMAIL>"),
        "{help}"
    );
}

#[test]
fn mfa_reset_rejects_invalid_arguments_before_starting_auth() {
    for args in [
        vec![],
        vec!["--bogus"],
        vec!["not-an-email"],
        vec!["member@bad..domain"],
        vec!["member@example.test", "extra"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_runbook-server"))
            .args(["admin", "mfa", "reset"])
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("error:") && error.contains("--help"),
            "{error}"
        );
        if matches!(args.as_slice(), ["not-an-email" | "member@bad..domain"]) {
            assert!(error.contains("expected an email address"), "{error}");
        }
    }
}
