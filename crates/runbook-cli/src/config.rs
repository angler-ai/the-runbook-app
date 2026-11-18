use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSource {
    FlagHost,
    EnvHost,
    FlagContext,
    CurrentContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHost {
    pub host: String,
    pub source: HostSource,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    InvalidContextName(String),
    InvalidHost(String),
    UnknownContext(String),
    StaleCurrentContext(String),
    /// No --host, RUNBOOK_HOST, --context or current context: the CLI has
    /// no built-in server, so it says how to set one.
    NoHost,
    Io(String),
    Serialization(String),
    Deserialization(String),
}

impl std::error::Error for ConfigError {}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::InvalidContextName(name) => write!(f, "invalid context name: '{name}'"),
            ConfigError::InvalidHost(host) => write!(f, "invalid host URL: '{host}'"),
            ConfigError::UnknownContext(name) => write!(f, "unknown context: '{name}'"),
            ConfigError::StaleCurrentContext(name) => {
                write!(f, "current context '{name}' does not exist")
            }
            ConfigError::NoHost => write!(
                f,
                "no server set: run `runbook login <server-url>` (for example \
                 `runbook login https://runbook.example.com`), add one with \
                 `runbook context add <name> <server-url>`, or set RUNBOOK_HOST"
            ),
            ConfigError::Io(err) => write!(f, "I/O error: {err}"),
            ConfigError::Serialization(err) => write!(f, "failed to serialize config: {err}"),
            ConfigError::Deserialization(err) => write!(f, "failed to parse config: {err}"),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_context: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contexts: BTreeMap<String, Context>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub host: String,
    /// The project commands run in this context use when they name none
    /// (`--project` and RUNBOOK_PROJECT win).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

impl Context {
    /// The host in the form commands resolve it to (`context add` stores
    /// it so; a hand-edited config may not), or as written when it is not
    /// a valid host.
    pub fn normalized_host(&self) -> String {
        normalize_and_validate_host(&self.host).unwrap_or_else(|_| self.host.clone())
    }
}

pub fn validate_context_name(name: &str) -> Result<(), ConfigError> {
    // 1-63 chars, lowercase letters, digits and hyphens, starting with a letter.
    if name.is_empty() || name.len() > 63 {
        return Err(ConfigError::InvalidContextName(name.to_string()));
    }
    let first = name.chars().next().unwrap();
    if !first.is_ascii_lowercase() {
        return Err(ConfigError::InvalidContextName(name.to_string()));
    }
    for c in name.chars() {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return Err(ConfigError::InvalidContextName(name.to_string()));
        }
    }
    Ok(())
}

pub fn normalize_and_validate_host(host: &str) -> Result<String, ConfigError> {
    // A query or fragment is no part of a host, and `#` separates the host
    // from the context in keychain accounts (`<host>#<context>`).
    if host.contains(['#', '?']) {
        return Err(ConfigError::InvalidHost(host.to_string()));
    }
    // Strip trailing slashes
    let trimmed = host.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(ConfigError::InvalidHost(host.to_string()));
    }

    let (scheme, rest) = if let Some(rest) = trimmed.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        ("http", rest)
    } else {
        return Err(ConfigError::InvalidHost(host.to_string()));
    };

    if rest.is_empty() {
        return Err(ConfigError::InvalidHost(host.to_string()));
    }

    // Authority part is the text before the first '/'
    let authority = match rest.find('/') {
        Some(idx) => &rest[..idx],
        None => rest,
    };

    if authority.is_empty() || authority.contains('@') {
        return Err(ConfigError::InvalidHost(host.to_string()));
    }

    if scheme == "http" {
        // http is allowed ONLY for localhost and 127.0.0.1
        // For the http:// localhost check, take the host from that authority part only:
        // the authority, cut at the first ':'
        let host_part = match authority.find(':') {
            Some(idx) => &authority[..idx],
            None => authority,
        };
        if host_part != "localhost" && host_part != "127.0.0.1" {
            return Err(ConfigError::InvalidHost(host.to_string()));
        }
    }

    Ok(trimmed.to_string())
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let content = fs::read_to_string(path).map_err(|e| ConfigError::Io(e.to_string()))?;
        let config: Config =
            toml::from_str(&content).map_err(|e| ConfigError::Deserialization(e.to_string()))?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| ConfigError::Io(e.to_string()))?;
        }
        let content =
            toml::to_string_pretty(self).map_err(|e| ConfigError::Serialization(e.to_string()))?;

        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("config.toml");

        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();

        // Try a few times with distinct temp file names
        let mut written = false;
        for i in 0..10 {
            let temp_name = format!(".{filename}.tmp-{pid}-{nanos}-{i}");
            let temp_path = parent.join(temp_name);

            let mut open_options = fs::OpenOptions::new();
            open_options.write(true).create_new(true);

            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                open_options.mode(0o600);
            }

            match open_options.open(&temp_path) {
                Ok(mut file) => {
                    if let Err(e) = file.write_all(content.as_bytes()) {
                        let _ = fs::remove_file(&temp_path);
                        return Err(ConfigError::Io(e.to_string()));
                    }
                    if let Err(e) = file.flush() {
                        let _ = fs::remove_file(&temp_path);
                        return Err(ConfigError::Io(e.to_string()));
                    }

                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600));
                    }

                    if let Err(e) = fs::rename(&temp_path, path) {
                        let _ = fs::remove_file(&temp_path);
                        return Err(ConfigError::Io(e.to_string()));
                    }
                    written = true;
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    continue;
                }
                Err(e) => return Err(ConfigError::Io(e.to_string())),
            }
        }

        if !written {
            return Err(ConfigError::Io(
                "failed to create temporary file for atomic write".to_string(),
            ));
        }

        Ok(())
    }

    pub fn add_context(&mut self, name: &str, host: &str) -> Result<(), ConfigError> {
        validate_context_name(name)?;
        let normalized_host = normalize_and_validate_host(host)?;
        self.contexts.insert(
            name.to_string(),
            Context {
                host: normalized_host,
                project: None,
            },
        );
        Ok(())
    }

    /// Set or clear the default project of an existing context.
    pub fn set_project(&mut self, name: &str, project: Option<&str>) -> Result<(), ConfigError> {
        let context = self
            .contexts
            .get_mut(name)
            .ok_or_else(|| ConfigError::UnknownContext(name.to_string()))?;
        context.project = project.map(str::to_owned).filter(|p| !p.is_empty());
        Ok(())
    }

    pub fn use_context(&mut self, name: &str) -> Result<(), ConfigError> {
        if !self.contexts.contains_key(name) {
            return Err(ConfigError::UnknownContext(name.to_string()));
        }
        self.current_context = Some(name.to_string());
        Ok(())
    }

    pub fn list(&self) -> Vec<(String, bool)> {
        self.contexts
            .keys()
            .map(|name| {
                let is_current = self
                    .current_context
                    .as_deref()
                    .map(|curr| curr == name)
                    .unwrap_or(false);
                (name.clone(), is_current)
            })
            .collect()
    }
}

pub fn config_path(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(xdg) = xdg_config_home {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join("runbook").join("config.toml"));
        }
    }
    if let Some(h) = home {
        let trimmed = h.trim();
        if !trimmed.is_empty() {
            return Some(
                PathBuf::from(trimmed)
                    .join(".config")
                    .join("runbook")
                    .join("config.toml"),
            );
        }
    }
    None
}

pub fn resolve_host(
    flag_host: Option<&str>,
    env_host: Option<&str>,
    flag_context: Option<&str>,
    config: &Config,
) -> Result<ResolvedHost, ConfigError> {
    // 1. --host flag
    if let Some(h) = flag_host {
        let normalized = normalize_and_validate_host(h)?;
        return Ok(ResolvedHost {
            host: normalized,
            source: HostSource::FlagHost,
        });
    }

    // 2. RUNBOOK_HOST env
    if let Some(h) = env_host {
        let normalized = normalize_and_validate_host(h)?;
        return Ok(ResolvedHost {
            host: normalized,
            source: HostSource::EnvHost,
        });
    }

    // 3. --context flag
    if let Some(ctx_name) = flag_context {
        match config.contexts.get(ctx_name) {
            Some(ctx) => {
                let normalized = normalize_and_validate_host(&ctx.host)?;
                return Ok(ResolvedHost {
                    host: normalized,
                    source: HostSource::FlagContext,
                });
            }
            None => return Err(ConfigError::UnknownContext(ctx_name.to_string())),
        }
    }

    // 4. Config's current context
    if let Some(ref curr_name) = config.current_context {
        match config.contexts.get(curr_name) {
            Some(ctx) => {
                let normalized = normalize_and_validate_host(&ctx.host)?;
                return Ok(ResolvedHost {
                    host: normalized,
                    source: HostSource::CurrentContext,
                });
            }
            None => return Err(ConfigError::StaleCurrentContext(curr_name.clone())),
        }
    }

    // 5. Nothing: there is no built-in server.
    Err(ConfigError::NoHost)
}

/// A context name for a server's URL: the host's first label, as a valid
/// name (`https://runbook.getangler.ai` → `runbook`), else `default`.
pub fn context_name_for(host: &str) -> String {
    let authority = host
        .split_once("://")
        .map_or(host, |(_, rest)| rest)
        .split(['/', ':'])
        .next()
        .unwrap_or_default();
    let label: String = authority
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let label = label.trim_matches('-').chars().take(63).collect::<String>();
    if validate_context_name(&label).is_ok() {
        label
    } else {
        "default".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review (low): `#` and `?` are refused in a host, so the keychain
    /// account `<host>#<context>` of one login can never equal another's.
    #[test]
    fn hosts_with_a_fragment_or_query_are_refused() {
        for bad in [
            "https://h.example#bot",
            "https://h.example?x=1",
            "https://h.example/a#b",
            "http://localhost:8080/?",
        ] {
            assert!(normalize_and_validate_host(bad).is_err(), "{bad}");
        }
        assert_eq!(
            normalize_and_validate_host("https://h.example/").unwrap(),
            "https://h.example"
        );
    }
}
