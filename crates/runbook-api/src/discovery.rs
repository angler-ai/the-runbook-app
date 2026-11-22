use runbook_model::{
    ApiLimits, AuthDiscovery, DiscoveryView, MetaView, ThemeColors, ThemeView, UploadLimits,
};
use std::collections::BTreeMap;
#[derive(Clone)]
pub struct DiscoveryConfig {
    pub origin: String,
    pub minimum_cli_version: String,
    pub max_request_bytes: u64,
    /// UPD1: the release `/v1/meta` reports: [`crate::VERSION`] on every
    /// host (a test build's seam may set another to stand in for one).
    pub version: String,
    /// UPD1: the storage schema `/v1/meta` reports (see [`MetaView::schema`]).
    pub schema: BTreeMap<String, u64>,
    /// UI-1: the theme override `/v1/meta` reports ([`theme_from_vars`]).
    pub theme: Option<ThemeView>,
    /// The per-run step cap `/v1/meta` reports (none: no cap).
    pub step_runs: Option<u64>,
}
impl DiscoveryConfig {
    pub fn discovery(&self) -> DiscoveryView {
        DiscoveryView {
            api_base_url: format!("{}/v1", self.origin),
            auth_base_url: format!("{}/api/auth", self.origin),
            edition: "self_hosted".into(),
            minimum_cli_version: self.minimum_cli_version.clone(),
            // D48: no sign-in method is configured until AUTH2/AUTH5.
            auth: AuthDiscovery::default(),
            jwks_uri: Some(format!("{}{}", self.origin, crate::runners::JWKS_PATH)),
        }
    }
    pub fn meta(&self) -> MetaView {
        MetaView {
            edition: "self_hosted".into(),
            version: self.version.clone(),
            runner_contract_version: runbook_model::RUNNER_CONTRACT_VERSIONS.last().copied(),
            schema: self.schema.clone(),
            capabilities: [
                ("directory".into(), true),
                ("project_variables".into(), true),
                ("uploads".into(), false),
                ("memory".into(), true),
                ("executions".into(), false),
            ]
            .into(),
            limits: ApiLimits {
                max_request_bytes: self.max_request_bytes,
                source_bytes: None,
                skill_bundle_bytes: None,
                uploads: Some(self_hosted_upload_limits()),
                step_runs: self.step_runs,
            },
            usage: None,
            theme: self.theme.clone(),
        }
    }
}

/// PRD §14 upload table, self-hosted column: no size or count limits by
/// default; images over 8,000 px are stored without a thumbnail. Type checks
/// still apply (HEIC refused for now, D51).
pub fn self_hosted_upload_limits() -> UploadLimits {
    runbook_core::uploads::limits::self_hosted()
}

// ---------------------------------------------------------------------------
// UI-1: the theme colour override, read the same way on both editions.

/// Accent colour for both themes.
pub const THEME_ACCENT: &str = "RUNBOOK_THEME_ACCENT";
/// Secondary accent colour for both themes.
pub const THEME_ACCENT_SECONDARY: &str = "RUNBOOK_THEME_ACCENT_SECONDARY";
/// Accent colour for the dark theme only (replaces [`THEME_ACCENT`] there).
pub const THEME_DARK_ACCENT: &str = "RUNBOOK_THEME_DARK_ACCENT";
/// Secondary accent for the dark theme only (replaces
/// [`THEME_ACCENT_SECONDARY`] there).
pub const THEME_DARK_ACCENT_SECONDARY: &str = "RUNBOOK_THEME_DARK_ACCENT_SECONDARY";
/// The light theme's page background, which its colours are checked against.
pub const THEME_LIGHT_BACKGROUND: &str = "#ffffff";
/// The dark theme's page background (Ink), which its colours are checked
/// against.
pub const THEME_DARK_BACKGROUND: &str = "#0e0f14";
/// WCAG 2 AA contrast for normal text.
const WCAG_AA: f64 = 4.5;

/// A theme variable that is set but not `#rrggbb`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeError {
    /// The variable's name.
    pub variable: &'static str,
    /// The value, cut to 32 characters (a colour is not secret).
    pub value: String,
}
impl ThemeError {
    pub const CODE: &'static str = "config.invalid_theme_color";
    /// `config.invalid_theme_color: <VARIABLE> must be #rrggbb`, without
    /// the value (the native server's startup error).
    pub fn message(&self) -> &'static str {
        match self.variable {
            THEME_ACCENT => "config.invalid_theme_color: RUNBOOK_THEME_ACCENT must be #rrggbb",
            THEME_ACCENT_SECONDARY => {
                "config.invalid_theme_color: RUNBOOK_THEME_ACCENT_SECONDARY must be #rrggbb"
            }
            THEME_DARK_ACCENT => {
                "config.invalid_theme_color: RUNBOOK_THEME_DARK_ACCENT must be #rrggbb"
            }
            _ => "config.invalid_theme_color: RUNBOOK_THEME_DARK_ACCENT_SECONDARY must be #rrggbb",
        }
    }
}
impl std::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (got {:?})", self.message(), self.value)
    }
}
impl std::error::Error for ThemeError {}

/// One configured colour and the variable it came from.
#[derive(Clone)]
struct Colour {
    variable: &'static str,
    hex: String,
}
/// Both themes' colours, each with its source variable.
struct Resolved {
    light: [Option<Colour>; 2],
    dark: [Option<Colour>; 2],
}
fn resolve(get: impl Fn(&str) -> Option<String>) -> Result<Option<Resolved>, ThemeError> {
    let read = |variable: &'static str| -> Result<Option<Colour>, ThemeError> {
        let Some(value) = get(variable).filter(|v| !v.trim().is_empty()) else {
            return Ok(None);
        };
        match parse_colour(value.trim()) {
            Some(hex) => Ok(Some(Colour { variable, hex })),
            None => Err(ThemeError {
                variable,
                value: value.chars().take(32).collect(),
            }),
        }
    };
    let accent = read(THEME_ACCENT)?;
    let secondary = read(THEME_ACCENT_SECONDARY)?;
    let dark_accent = read(THEME_DARK_ACCENT)?;
    let dark_secondary = read(THEME_DARK_ACCENT_SECONDARY)?;
    if accent.is_none() && secondary.is_none() && dark_accent.is_none() && dark_secondary.is_none()
    {
        return Ok(None);
    }
    Ok(Some(Resolved {
        dark: [
            dark_accent.or_else(|| accent.clone()),
            dark_secondary.or_else(|| secondary.clone()),
        ],
        light: [accent, secondary],
    }))
}
/// `#rrggbb` in either case, lowercased; anything else is `None`.
fn parse_colour(value: &str) -> Option<String> {
    let hex = value.strip_prefix('#')?;
    (hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("#{}", hex.to_ascii_lowercase()))
}
fn colours(pair: &[Option<Colour>; 2]) -> ThemeColors {
    ThemeColors {
        accent: pair[0].as_ref().map(|c| c.hex.clone()),
        accent_secondary: pair[1].as_ref().map(|c| c.hex.clone()),
    }
}

/// UI-1: the theme override from the `RUNBOOK_THEME_*` variables, `get`
/// reading one by name (both editions: the environment natively, the
/// Worker's vars on Workers). The base variables apply to both themes;
/// each dark variable, when set, replaces its base value in the dark theme
/// only. Empty or blank means unset; nothing set is `None`. A set value
/// that is not `#rrggbb` (either case; `#rgb`, names and a missing `#` are
/// refused) is a [`ThemeError`] naming the variable.
pub fn theme_from_vars(
    get: impl Fn(&str) -> Option<String>,
) -> Result<Option<ThemeView>, ThemeError> {
    Ok(resolve(get)?.map(|r| ThemeView {
        light: colours(&r.light),
        dark: colours(&r.dark),
    }))
}

/// UI-1: a warning per configured colour whose WCAG 2 contrast against its
/// theme's background ([`THEME_LIGHT_BACKGROUND`], [`THEME_DARK_BACKGROUND`])
/// is below AA's 4.5:1. Low contrast is allowed (the web app adjusts its
/// text shades); invalid values give none (they are refused instead).
pub fn theme_warnings(get: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let Ok(Some(resolved)) = resolve(get) else {
        return vec![];
    };
    let mut warnings = vec![];
    for (pair, background, name) in [
        (&resolved.light, THEME_LIGHT_BACKGROUND, "light"),
        (&resolved.dark, THEME_DARK_BACKGROUND, "dark"),
    ] {
        for colour in pair.iter().flatten() {
            let Some(ratio) = contrast_ratio(&colour.hex, background) else {
                continue;
            };
            if ratio < WCAG_AA {
                // Cut, not rounded: 4.46 never reads as 4.5.
                let shown = (ratio * 10.0).floor() / 10.0;
                warnings.push(format!(
                    "theme: {} {} has contrast {shown:.1}:1 against {background} ({name} theme; below WCAG AA 4.5:1); the web app adjusts text shades but buttons may be hard to read",
                    colour.variable, colour.hex
                ));
            }
        }
    }
    warnings
}

/// WCAG 2 contrast ratio of two `#rrggbb` colours (1 to 21), from their
/// relative luminance; `None` when either is not `#rrggbb`.
pub fn contrast_ratio(a: &str, b: &str) -> Option<f64> {
    let (a, b) = (luminance(a)?, luminance(b)?);
    let (light, dark) = if a >= b { (a, b) } else { (b, a) };
    Some((light + 0.05) / (dark + 0.05))
}
fn luminance(colour: &str) -> Option<f64> {
    let hex = parse_colour(colour)?;
    let channel = |at: usize| -> Option<f64> {
        let c = f64::from(u8::from_str_radix(&hex[at..at + 2], 16).ok()?) / 255.0;
        Some(if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        })
    };
    Some(0.2126 * channel(1)? + 0.7152 * channel(3)? + 0.0722 * channel(5)?)
}
