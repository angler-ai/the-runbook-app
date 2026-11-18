use runbook_cli::config::{
    Config, ConfigError, HostSource, config_path, context_name_for, resolve_host,
};
use std::path::PathBuf;

#[test]
fn host_precedence_is_flag_env_context_current_default() {
    let mut config = Config::default();
    config
        .add_context("prod", "https://prod.example.com")
        .unwrap();
    config
        .add_context("staging", "https://staging.example.com")
        .unwrap();
    config.use_context("prod").unwrap();

    // 1. All present: --host wins
    let res = resolve_host(
        Some("https://flag.example.com"),
        Some("https://env.example.com"),
        Some("staging"),
        &config,
    )
    .unwrap();
    assert_eq!(res.host, "https://flag.example.com");
    assert_eq!(res.source, HostSource::FlagHost);

    // 2. No --host: RUNBOOK_HOST wins
    let res = resolve_host(
        None,
        Some("https://env.example.com"),
        Some("staging"),
        &config,
    )
    .unwrap();
    assert_eq!(res.host, "https://env.example.com");
    assert_eq!(res.source, HostSource::EnvHost);

    // 3. No --host and no RUNBOOK_HOST: --context wins
    let res = resolve_host(None, None, Some("staging"), &config).unwrap();
    assert_eq!(res.host, "https://staging.example.com");
    assert_eq!(res.source, HostSource::FlagContext);

    // 4. No flags and no env: current context wins
    let res = resolve_host(None, None, None, &config).unwrap();
    assert_eq!(res.host, "https://prod.example.com");
    assert_eq!(res.source, HostSource::CurrentContext);

    // 5. Empty config: there is no built-in server; the error says how to set one.
    let empty_config = Config::default();
    let err = resolve_host(None, None, None, &empty_config).unwrap_err();
    assert_eq!(err, ConfigError::NoHost);
    assert!(err.to_string().contains("runbook login <server-url>"));
}

#[test]
fn a_first_login_names_its_context_after_the_host() {
    assert_eq!(context_name_for("https://runbook.getangler.ai"), "runbook");
    assert_eq!(
        context_name_for("https://Ops-Runbooks.example.com:8443/x"),
        "ops-runbooks"
    );
    assert_eq!(context_name_for("http://localhost:8080"), "localhost");
    assert_eq!(context_name_for("http://127.0.0.1:8080"), "default");
}

#[test]
fn unknown_context_is_an_error() {
    let mut config = Config::default();
    config
        .add_context("angler", "https://runbooks.getangler.ai")
        .unwrap();

    // Unknown --context
    let err = resolve_host(None, None, Some("nonexistent"), &config).unwrap_err();
    assert_eq!(err, ConfigError::UnknownContext("nonexistent".to_string()));

    // use_context on unknown context
    let err = config.use_context("nonexistent").unwrap_err();
    assert_eq!(err, ConfigError::UnknownContext("nonexistent".to_string()));

    // Stale current context (e.g. current_context points to removed/unknown context)
    let stale_config = Config {
        current_context: Some("ghost".to_string()),
        ..Default::default()
    };
    let err = resolve_host(None, None, None, &stale_config).unwrap_err();
    assert_eq!(err, ConfigError::StaleCurrentContext("ghost".to_string()));
}

#[test]
fn context_names_and_hosts_are_validated() {
    let mut config = Config::default();

    // Valid names
    assert!(config.add_context("a", "https://example.com").is_ok());
    assert!(config.add_context("a1", "https://example.com").is_ok());
    assert!(config.add_context("a-b-c", "https://example.com").is_ok());
    let long_name = "a".repeat(63);
    assert!(
        config
            .add_context(&long_name, "https://example.com")
            .is_ok()
    );

    // Invalid names
    // empty
    assert!(matches!(
        config.add_context("", "https://example.com"),
        Err(ConfigError::InvalidContextName(_))
    ));
    // starts with digit
    assert!(matches!(
        config.add_context("1a", "https://example.com"),
        Err(ConfigError::InvalidContextName(_))
    ));
    // starts with hyphen
    assert!(matches!(
        config.add_context("-a", "https://example.com"),
        Err(ConfigError::InvalidContextName(_))
    ));
    // contains uppercase
    assert!(matches!(
        config.add_context("Angler", "https://example.com"),
        Err(ConfigError::InvalidContextName(_))
    ));
    // contains underscore or special char
    assert!(matches!(
        config.add_context("a_b", "https://example.com"),
        Err(ConfigError::InvalidContextName(_))
    ));
    assert!(matches!(
        config.add_context("a.b", "https://example.com"),
        Err(ConfigError::InvalidContextName(_))
    ));
    // exceeds 63 chars
    let too_long = "a".repeat(64);
    assert!(matches!(
        config.add_context(&too_long, "https://example.com"),
        Err(ConfigError::InvalidContextName(_))
    ));

    // Valid hosts
    assert!(config.add_context("c1", "https://example.com").is_ok());
    assert!(
        config
            .add_context("c2", "https://example.com:8443/api")
            .is_ok()
    );
    assert!(config.add_context("c3", "http://localhost").is_ok());
    assert!(config.add_context("c4", "http://localhost:8080").is_ok());
    assert!(
        config
            .add_context("c4-path", "http://localhost:8080/path")
            .is_ok()
    );
    assert!(config.add_context("c5", "http://127.0.0.1").is_ok());
    assert!(config.add_context("c6", "http://127.0.0.1:3000").is_ok());

    // Trailing slash normalization
    config.add_context("c7", "https://example.com/").unwrap();
    assert_eq!(
        config.contexts.get("c7").unwrap().host,
        "https://example.com"
    );

    config
        .add_context("c8", "https://example.com/subpath///")
        .unwrap();
    assert_eq!(
        config.contexts.get("c8").unwrap().host,
        "https://example.com/subpath"
    );

    // Invalid hosts
    // userinfo / credentials in host
    assert!(matches!(
        config.add_context("bad-cred1", "http://localhost:80@evil.com"),
        Err(ConfigError::InvalidHost(_))
    ));
    assert!(matches!(
        config.add_context("bad-cred2", "https://user:pass@example.com"),
        Err(ConfigError::InvalidHost(_))
    ));
    // http non-localhost
    assert!(matches!(
        config.add_context("bad1", "http://example.com"),
        Err(ConfigError::InvalidHost(_))
    ));
    assert!(matches!(
        config.add_context("bad2", "http://192.168.1.1"),
        Err(ConfigError::InvalidHost(_))
    ));
    // ftp or other scheme
    assert!(matches!(
        config.add_context("bad3", "ftp://localhost"),
        Err(ConfigError::InvalidHost(_))
    ));
    // missing scheme
    assert!(matches!(
        config.add_context("bad4", "example.com"),
        Err(ConfigError::InvalidHost(_))
    ));

    // Validation applies in resolve_host as well
    assert!(matches!(
        resolve_host(Some("http://remote.com"), None, None, &config),
        Err(ConfigError::InvalidHost(_))
    ));
    assert!(matches!(
        resolve_host(None, Some("http://remote.com"), None, &config),
        Err(ConfigError::InvalidHost(_))
    ));
    let res = resolve_host(Some("https://remote.com///"), None, None, &config).unwrap();
    assert_eq!(res.host, "https://remote.com");
}

#[test]
fn config_round_trips_and_is_private() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_file = temp_dir.path().join("runbook").join("config.toml");

    // Missing file loads as empty default
    let loaded = Config::load(&config_file).unwrap();
    assert_eq!(loaded, Config::default());

    // Build config
    let mut config = Config::default();
    config
        .add_context("angler", "https://runbooks.getangler.ai")
        .unwrap();
    config
        .add_context("local", "http://localhost:8080")
        .unwrap();
    config.use_context("angler").unwrap();

    // Save
    config.save(&config_file).unwrap();

    // Verify file mode on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&config_file).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600 file mode, got {:o}", mode);
    }

    // Load and compare
    let reloaded = Config::load(&config_file).unwrap();
    assert_eq!(reloaded, config);

    // List reflects contexts and current
    let list = reloaded.list();
    assert_eq!(
        list,
        vec![("angler".to_string(), true), ("local".to_string(), false),]
    );
}

#[test]
fn config_path_prefers_xdg_then_home() {
    // Both XDG and HOME set -> XDG wins
    let path = config_path(Some("/custom/xdg"), Some("/home/user")).unwrap();
    assert_eq!(path, PathBuf::from("/custom/xdg/runbook/config.toml"));

    // Only XDG set
    let path = config_path(Some("/custom/xdg"), None).unwrap();
    assert_eq!(path, PathBuf::from("/custom/xdg/runbook/config.toml"));

    // XDG empty or whitespace -> falls back to HOME if empty is treated as unset
    // Or XDG None -> HOME used: $HOME/.config/runbook/config.toml
    let path = config_path(None, Some("/home/user")).unwrap();
    assert_eq!(
        path,
        PathBuf::from("/home/user/.config/runbook/config.toml")
    );

    // If XDG is empty string ""
    let path = config_path(Some(""), Some("/home/user")).unwrap();
    assert_eq!(
        path,
        PathBuf::from("/home/user/.config/runbook/config.toml")
    );

    // Neither set -> None
    assert_eq!(config_path(None, None), None);
    assert_eq!(config_path(Some(""), None), None);
}
