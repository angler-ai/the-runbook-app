use runbook_store_postgres::DatabaseConfig;

#[test]
fn database_configuration_is_required_and_redacted() {
    let error = DatabaseConfig::from_values(None, None).unwrap_err();
    assert_eq!(error.to_string(), "postgres.missing_database_url");
    let config = DatabaseConfig::from_values(
        Some("postgres://user:secret@localhost/db".into()),
        Some("postgres://user:other_secret@localhost/direct".into()),
    )
    .unwrap();
    assert!(!format!("{config:?}").contains("secret"));
    let error =
        DatabaseConfig::from_values(Some("password=secret invalid".into()), None).unwrap_err();
    assert_eq!(error.to_string(), "postgres.invalid_database_url");
    assert!(!format!("{error:?}").contains("secret"));
}
