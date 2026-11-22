use runbook_api::discovery::*;
#[test]
fn discovery_uses_configured_origin() {
    for origin in ["https://one.example.test", "https://two.example.test"] {
        let config = DiscoveryConfig {
            origin: origin.into(),
            minimum_cli_version: "0.1.0".into(),
            max_request_bytes: 67_108_864,
            version: runbook_api::VERSION.into(),
            schema: [("postgres".to_owned(), 30)].into(),
            theme: None,
            step_runs: None,
        };
        let d = config.discovery();
        assert_eq!(d.api_base_url, format!("{origin}/v1"));
        assert_eq!(d.auth_base_url, format!("{origin}/api/auth"));
        assert_eq!(d.minimum_cli_version, "0.1.0");
        let meta = config.meta();
        assert!(!meta.capabilities["uploads"]);
        assert!(meta.capabilities["memory"]);
        assert_eq!(meta.limits.max_request_bytes, 67_108_864);
        // T0: the PRD self-hosted limits are advertised while uploads are off.
        let uploads = meta.limits.uploads.unwrap();
        assert_eq!(uploads.image_bytes, None);
        assert_eq!(uploads.thumbnail_max_dimension, Some(8_000));
        assert!(!uploads.image_types.iter().any(|t| t.contains("svg")));
        assert!(d.auth.methods.is_empty());
        assert_eq!(d.auth.device_authorization_endpoint, None);
    }
}

/// UPD1: `/v1/meta` reports the release (the workspace version, the same
/// number `runbook-server --version` and `runbook --version` print) and the
/// storage schema the host runs on, so an update can be verified.
#[test]
fn meta_reports_the_release_version_and_schema() {
    let config = DiscoveryConfig {
        origin: "https://one.example.test".into(),
        minimum_cli_version: "0.1.0".into(),
        max_request_bytes: 1,
        version: runbook_api::VERSION.into(),
        schema: [("d1".to_owned(), 1014), ("execution_do".to_owned(), 5)].into(),
        theme: None,
        step_runs: None,
    };
    let meta = config.meta();
    assert_eq!(meta.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(runbook_api::VERSION, env!("CARGO_PKG_VERSION"));
    assert_eq!(meta.schema["d1"], 1014);
    assert_eq!(meta.schema["execution_do"], 5);
    let body = serde_json::to_value(&meta).unwrap();
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["runner_contract_version"], 4);
    assert_eq!(body["schema"]["d1"], 1014);
}

fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: std::collections::BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name| map.get(name).cloned()
}

/// UI-1: the theme override. The base variables apply to both themes;
/// the dark ones replace them in the dark theme only. Colours are
/// `#rrggbb`, stored lowercase; empty or blank means unset; nothing set
/// means no theme.
#[test]
fn theme_variables_resolve_per_theme() {
    assert_eq!(theme_from_vars(vars(&[])).unwrap(), None);
    assert_eq!(
        theme_from_vars(vars(&[
            (THEME_ACCENT, ""),
            (THEME_DARK_ACCENT_SECONDARY, "  ")
        ]))
        .unwrap(),
        None
    );
    let theme = theme_from_vars(vars(&[
        (THEME_ACCENT, "#4338CA"),
        (THEME_ACCENT_SECONDARY, " #0e7490 "),
        (THEME_DARK_ACCENT, "#A5B4FC"),
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(theme.light.accent.as_deref(), Some("#4338ca"));
    assert_eq!(theme.light.accent_secondary.as_deref(), Some("#0e7490"));
    assert_eq!(theme.dark.accent.as_deref(), Some("#a5b4fc"));
    assert_eq!(theme.dark.accent_secondary.as_deref(), Some("#0e7490"));
    // A dark value alone: the light theme keeps the app's own colours.
    let theme = theme_from_vars(vars(&[(THEME_DARK_ACCENT_SECONDARY, "#ffffff")]))
        .unwrap()
        .unwrap();
    assert_eq!(theme.light, runbook_model::ThemeColors::default());
    assert_eq!(theme.dark.accent, None);
    assert_eq!(theme.dark.accent_secondary.as_deref(), Some("#ffffff"));
    // `/v1/meta` reports it; absent when unset.
    let mut config = DiscoveryConfig {
        origin: "https://one.example.test".into(),
        minimum_cli_version: "0.1.0".into(),
        max_request_bytes: 1,
        version: runbook_api::VERSION.into(),
        schema: Default::default(),
        theme: None,
        step_runs: None,
    };
    assert!(
        serde_json::to_value(config.meta())
            .unwrap()
            .get("theme")
            .is_none()
    );
    config.theme = Some(theme);
    assert_eq!(
        serde_json::to_value(config.meta()).unwrap()["theme"],
        serde_json::json!({"light": {}, "dark": {"accent_secondary": "#ffffff"}})
    );
}

#[test]
fn invalid_theme_colours_name_the_variable() {
    for name in [
        THEME_ACCENT,
        THEME_ACCENT_SECONDARY,
        THEME_DARK_ACCENT,
        THEME_DARK_ACCENT_SECONDARY,
    ] {
        for value in [
            "#fff",
            "4338ca",
            "indigo",
            "#4338cg",
            "#4338ca0",
            "# 4338c",
            "#４３３８ca",
        ] {
            let error = theme_from_vars(vars(&[(name, value)])).unwrap_err();
            assert_eq!(error.variable, name);
            assert_eq!(ThemeError::CODE, "config.invalid_theme_color");
            assert_eq!(
                error.message(),
                format!("config.invalid_theme_color: {name} must be #rrggbb")
            );
            assert!(error.to_string().starts_with(error.message()), "{error}");
        }
    }
    // A long value is echoed cut to 32 characters.
    let long = "x".repeat(500);
    let error = theme_from_vars(vars(&[(THEME_ACCENT, &long)])).unwrap_err();
    assert!(error.to_string().len() < 200, "{error}");
    assert!(error.to_string().contains(&"x".repeat(32)));
    assert!(!error.to_string().contains(&"x".repeat(33)));
}

/// UI-1: WCAG 2 contrast. Low contrast warns (it never refuses): the light
/// theme against white, the dark theme against its near-black ground (#0e0f14).
#[test]
fn theme_contrast_warns_below_wcag_aa() {
    let ratio = |a: &str, b: &str| (contrast_ratio(a, b).unwrap() * 100.0).round() / 100.0;
    assert_eq!(ratio("#000000", "#ffffff"), 21.0);
    assert_eq!(ratio("#ffffff", "#ffffff"), 1.0);
    assert_eq!(ratio("#4338ca", "#ffffff"), 7.90);
    assert_eq!(ratio("#ffffff", "#4338ca"), 7.90);
    assert!(contrast_ratio("#00cc99", "#ffffff").unwrap() < 4.5);
    assert_eq!(contrast_ratio("#fff", "#ffffff"), None);
    // Good colours in both themes: no warning.
    assert!(
        theme_warnings(vars(&[
            (THEME_ACCENT, "#4338ca"),
            (THEME_DARK_ACCENT, "#a5b4fc")
        ]))
        .is_empty()
    );
    // The base accent is used in both themes, and reads poorly in the
    // light one; the secondary is poor against Ink.
    let warnings = theme_warnings(vars(&[
        (THEME_ACCENT, "#a6a2e5"),
        (THEME_ACCENT_SECONDARY, "#4338CA"),
    ]));
    assert_eq!(warnings.len(), 2, "{warnings:#?}");
    assert_eq!(
        warnings[0],
        "theme: RUNBOOK_THEME_ACCENT #a6a2e5 has contrast 2.3:1 against #ffffff (light theme; below WCAG AA 4.5:1); the web app adjusts text shades but buttons may be hard to read"
    );
    assert!(
        warnings[1].starts_with("theme: RUNBOOK_THEME_ACCENT_SECONDARY #4338ca has contrast ")
            && warnings[1].contains("against #0e0f14 (dark theme;"),
        "{}",
        warnings[1]
    );
    // A dark override is checked against Ink, under its own name.
    let warnings = theme_warnings(vars(&[(THEME_DARK_ACCENT, "#0e0f14")]));
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].starts_with("theme: RUNBOOK_THEME_DARK_ACCENT #0e0f14 has contrast 1.0:1"));
    // Invalid values are the startup error's business, not a warning's.
    assert!(theme_warnings(vars(&[(THEME_ACCENT, "nope")])).is_empty());
}
