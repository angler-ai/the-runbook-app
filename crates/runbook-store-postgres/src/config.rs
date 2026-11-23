use crate::{
    Error,
    pool::{DEFAULT_POOL_SIZE, Shared},
};
use std::{fmt, sync::Arc, time::Duration};
use tokio_postgres::Config;

#[derive(Clone)]
pub struct DatabaseConfig {
    primary: Config,
    direct: Option<Config>,
    /// HOST1: how each endpoint's TLS is checked (libpq's `sslmode` and
    /// `sslrootcert`, which tokio-postgres does not parse itself).
    primary_tls: TlsPolicy,
    direct_tls: Option<TlsPolicy>,
    lock_timeout: Duration,
    idle_transaction_timeout: Duration,
    /// The bounded pool every clone of this config shares (F32). Settings
    /// that change how connections open start a new pool.
    pool: Arc<Shared>,
    /// CF6 quota limits (`RUNBOOK_LIMIT_EXECUTIONS_MONTHLY`; off by default).
    quota: runbook_core::quotas::QuotaLimits,
    /// CF13 review round 1: the run budget (`RUNBOOK_LIMIT_RUN_BYTES`;
    /// `runbook_core::run_budget::DEFAULT_MAX_RUN_BYTES` by default).
    run_budget: u64,
    /// The per-run step cap (`RUNBOOK_LIMIT_STEP_RUNS`; none by default).
    step_cap: Option<u64>,
    /// UPD1: run on a schema a newer release migrated
    /// (`RUNBOOK_ALLOW_NEWER_SCHEMA`; off by default, which refuses it with
    /// `postgres.schema_newer_than_code`).
    allow_newer_schema: bool,
}
/// Bounded waits: a writer gives up on a held execution lock after this long.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
/// A session left open and idle is ended by the server, freeing its lock.
pub const DEFAULT_IDLE_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(30);
impl fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabaseConfig")
            .field("urls", &"[REDACTED]")
            .finish()
    }
}
impl DatabaseConfig {
    pub fn from_env() -> Result<Self, Error> {
        let read = |key| match std::env::var(key) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(Error("postgres.invalid_environment")),
        };
        let mut config = Self::from_values(read("DATABASE_URL")?, read("DATABASE_URL_DIRECT")?)?;
        config.quota.executions_monthly =
            runbook_core::quotas::parse_limit(read("RUNBOOK_LIMIT_EXECUTIONS_MONTHLY")?.as_deref())
                .map_err(Error)?;
        config.run_budget =
            runbook_core::run_budget::parse_run_budget(read("RUNBOOK_LIMIT_RUN_BYTES")?.as_deref())
                .map_err(Error)?;
        config.step_cap =
            runbook_core::run_budget::parse_step_cap(read("RUNBOOK_LIMIT_STEP_RUNS")?.as_deref())
                .map_err(Error)?;
        config.allow_newer_schema =
            parse_allow_newer_schema(read("RUNBOOK_ALLOW_NEWER_SCHEMA")?.as_deref())?;
        match read("RUNBOOK_DATABASE_POOL_SIZE")? {
            None => Ok(config),
            Some(value) => Ok(config.with_pool_size(parse_pool_size(&value)?)),
        }
    }
    pub fn from_values(primary: Option<String>, direct: Option<String>) -> Result<Self, Error> {
        fn parse(value: &str, code: &'static str) -> Result<(Config, TlsPolicy), Error> {
            if value.trim().is_empty() {
                return Err(Error(code));
            }
            let (rest, tls) = TlsPolicy::split(value).map_err(|_| Error(code))?;
            let mut config: Config = rest.parse().map_err(|_| Error(code))?;
            config.connect_timeout(Duration::from_secs(10));
            // libpq ignores sslmode on a Unix socket (Cloud SQL's
            // `host=/cloudsql/...`); the server never offers TLS there.
            config.ssl_mode(if all_unix(&config) {
                tokio_postgres::config::SslMode::Disable
            } else {
                tls.mode.wire()
            });
            Ok((config, tls))
        }
        let primary = primary.ok_or(Error("postgres.missing_database_url"))?;
        let (primary, primary_tls) = parse(&primary, "postgres.invalid_database_url")?;
        let direct = direct
            .map(|value| parse(&value, "postgres.invalid_direct_url"))
            .transpose()?;
        let (direct, direct_tls) = match direct {
            Some((config, tls)) => (Some(config), Some(tls)),
            None => (None, None),
        };
        Ok(Self {
            primary,
            direct,
            primary_tls,
            direct_tls,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            idle_transaction_timeout: DEFAULT_IDLE_TRANSACTION_TIMEOUT,
            pool: Shared::new(DEFAULT_POOL_SIZE),
            quota: Default::default(),
            run_budget: runbook_core::run_budget::DEFAULT_MAX_RUN_BYTES,
            step_cap: None,
            allow_newer_schema: false,
        })
    }
    /// UPD1: whether stores built from this config run on a schema a newer
    /// release migrated (the rollback path) instead of refusing it.
    pub fn allow_newer_schema(mut self, allow: bool) -> Self {
        self.allow_newer_schema = allow;
        self
    }
    pub fn allows_newer_schema(&self) -> bool {
        self.allow_newer_schema
    }
    /// CF6: the quota limits every store built from this config enforces.
    pub fn with_quota_limits(mut self, limits: runbook_core::quotas::QuotaLimits) -> Self {
        self.quota = limits;
        self
    }
    pub fn quota_limits(&self) -> runbook_core::quotas::QuotaLimits {
        self.quota
    }
    /// CF13 review round 1: the run budget every store built from this
    /// config enforces.
    pub fn with_run_budget(mut self, bytes: u64) -> Self {
        self.run_budget = bytes;
        self
    }
    pub fn with_step_cap(mut self, cap: Option<u64>) -> Self {
        self.step_cap = cap;
        self
    }
    /// The most step runs a run may open (none: no cap).
    pub fn step_cap(&self) -> Option<u64> {
        self.step_cap
    }
    pub fn run_budget(&self) -> u64 {
        self.run_budget
    }
    /// Override the server-side lock wait and idle-transaction limits.
    /// Connections open with these settings, so this starts a new pool.
    pub fn with_lock_timeouts(mut self, lock: Duration, idle_transaction: Duration) -> Self {
        self.lock_timeout = lock;
        self.idle_transaction_timeout = idle_transaction;
        self.fresh_pool()
    }
    /// At most `size` pooled connections (at least one) for every store
    /// built from this config or its clones. Starts a new pool.
    pub fn with_pool_size(mut self, size: usize) -> Self {
        self.pool = Shared::new(size.max(1));
        self
    }
    pub fn pool_size(&self) -> usize {
        self.pool.size()
    }
    pub fn pool_stats(&self) -> crate::PoolStats {
        self.pool.stats()
    }
    pub(crate) fn pool(&self) -> &Shared {
        &self.pool
    }
    fn fresh_pool(mut self) -> Self {
        self.pool = Shared::new(self.pool.size());
        self
    }
    pub(crate) fn session_settings(&self) -> String {
        format!(
            "SET lock_timeout = '{}ms'; SET idle_in_transaction_session_timeout = '{}ms'",
            self.lock_timeout.as_millis().max(1),
            self.idle_transaction_timeout.as_millis().max(1)
        )
    }
    pub(crate) fn primary(&self) -> &Config {
        &self.primary
    }
    /// The TLS policy of the endpoint [`Self::primary`] names.
    pub fn tls(&self) -> &TlsPolicy {
        &self.primary_tls
    }
    /// Whether every host of the primary endpoint is a Unix socket.
    pub fn unix_socket(&self) -> bool {
        all_unix(&self.primary)
    }
    /// Startup warnings, one per endpoint (`DATABASE_URL`, and
    /// `DATABASE_URL_DIRECT` when set), whose TLS is requested but whose
    /// certificate is not verified (`sslmode=require` or `prefer` set
    /// explicitly with no `sslrootcert`). Never echo a URL.
    pub fn tls_warnings(&self) -> Vec<&'static str> {
        let unverified = |config: &Config, tls: &TlsPolicy| {
            tls.explicit
                && !all_unix(config)
                && matches!(tls.mode, SslMode::Prefer | SslMode::Require)
                && tls.root_cert.is_none()
        };
        let mut warnings = Vec::new();
        if unverified(&self.primary, &self.primary_tls) {
            warnings.push(
                "DATABASE_URL sets sslmode=require (or prefer): the connection is encrypted but the database's certificate is not verified; use sslmode=verify-full, with sslrootcert=<CA file> when the provider's CA is not a public one",
            );
        }
        if let (Some(direct), Some(tls)) = (&self.direct, &self.direct_tls)
            && unverified(direct, tls)
        {
            warnings.push(
                "DATABASE_URL_DIRECT sets sslmode=require (or prefer): the connection is encrypted but the database's certificate is not verified; use sslmode=verify-full, with sslrootcert=<CA file> when the provider's CA is not a public one",
            );
        }
        warnings
    }
    /// Direct endpoint for LISTEN (B2.3); falls back to the ordinary endpoint.
    pub fn direct_connection(&self) -> Self {
        Self {
            primary: self.direct.clone().unwrap_or_else(|| self.primary.clone()),
            primary_tls: self
                .direct_tls
                .clone()
                .unwrap_or_else(|| self.primary_tls.clone()),
            direct: None,
            direct_tls: None,
            ..self.clone()
        }
        .fresh_pool()
    }
    /// Point the direct (LISTEN) endpoint at a named database, creating the
    /// direct endpoint from the primary when none was configured.
    pub fn with_direct_database(&self, name: &str) -> Self {
        let mut config = self.clone();
        match &mut config.direct {
            Some(direct) => {
                direct.dbname(name);
            }
            None => {
                let mut direct = config.primary.clone();
                direct.dbname(name);
                config.direct = Some(direct);
                config.direct_tls = Some(config.primary_tls.clone());
            }
        }
        config
    }
    /// Select an explicitly named database without string manipulation of credentials.
    pub fn with_database(&self, name: &str) -> Self {
        let mut config = self.clone();
        config.primary.dbname(name);
        if let Some(direct) = &mut config.direct {
            direct.dbname(name);
        }
        config.fresh_pool()
    }
}
/// Whether every host of `config` is a Unix socket (TLS never applies).
fn all_unix(config: &Config) -> bool {
    let hosts = config.get_hosts();
    !hosts.is_empty()
        && hosts
            .iter()
            .all(|h| !matches!(h, tokio_postgres::config::Host::Tcp(_)))
}

/// HOST1: libpq's `sslmode` values. `prefer` is the default, as in libpq.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SslMode {
    Disable,
    /// libpq's `allow` and `prefer`: TLS when the server offers it, with no
    /// certificate check (unless `sslrootcert` is set).
    #[default]
    Prefer,
    /// TLS or no connection; the certificate is checked only when
    /// `sslrootcert` is set (then as `verify-ca`), as in libpq.
    Require,
    /// TLS, and the certificate chains to `sslrootcert` (or the system's
    /// roots); the host name is not checked.
    VerifyCa,
    /// TLS, the chain is checked and the certificate names the host.
    VerifyFull,
}
impl SslMode {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "disable" => Self::Disable,
            "allow" | "prefer" => Self::Prefer,
            "require" => Self::Require,
            "verify-ca" => Self::VerifyCa,
            "verify-full" => Self::VerifyFull,
            _ => return None,
        })
    }
    fn wire(self) -> tokio_postgres::config::SslMode {
        use tokio_postgres::config::SslMode as Wire;
        match self {
            Self::Disable => Wire::Disable,
            Self::Prefer => Wire::Prefer,
            Self::Require | Self::VerifyCa | Self::VerifyFull => Wire::Require,
        }
    }
}

/// HOST1: one endpoint's TLS settings, taken out of its URL (URL query or
/// `key=value` form) before tokio-postgres parses the rest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsPolicy {
    pub mode: SslMode,
    /// `sslrootcert=<file>`: PEM roots that replace the system's.
    /// `sslrootcert=system` (libpq 16) is the system's roots, as when unset.
    pub root_cert: Option<std::path::PathBuf>,
    /// `sslmode` was given (for the startup warning).
    pub explicit: bool,
}
impl TlsPolicy {
    /// Split `sslmode` and `sslrootcert` off a connection string. The
    /// error is `postgres.invalid_tls_settings`; it names no value.
    pub fn split(value: &str) -> Result<(String, Self), Error> {
        Self::split_inner(value).map_err(|()| Error("postgres.invalid_tls_settings"))
    }
    fn split_inner(value: &str) -> Result<(String, Self), ()> {
        let mut policy = Self::default();
        if !value.contains("sslmode") && !value.contains("sslrootcert") {
            return Ok((value.to_owned(), policy));
        }
        let mut take = |key: &str, raw: &str| -> Result<bool, ()> {
            match key {
                "sslmode" => {
                    policy.mode = SslMode::parse(raw).ok_or(())?;
                    policy.explicit = true;
                }
                "sslrootcert" if raw.is_empty() => return Err(()),
                "sslrootcert" if raw == "system" => policy.root_cert = None,
                "sslrootcert" => policy.root_cert = Some(raw.into()),
                _ => return Ok(false),
            }
            Ok(true)
        };
        let trimmed = value.trim();
        let rest = if trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://") {
            match trimmed.split_once('?') {
                None => trimmed.to_owned(),
                Some((base, query)) => {
                    let mut kept = Vec::new();
                    for pair in query.split('&').filter(|p| !p.is_empty()) {
                        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                        if !take(&percent_decode(k)?, &percent_decode(v)?)? {
                            kept.push(pair);
                        }
                    }
                    if kept.is_empty() {
                        base.to_owned()
                    } else {
                        format!("{base}?{}", kept.join("&"))
                    }
                }
            }
        } else {
            let mut kept = Vec::new();
            for token in key_value_tokens(trimmed)? {
                let (k, v) = token.split_once('=').ok_or(())?;
                let v = v.trim();
                let unquoted = v
                    .strip_prefix('\'')
                    .and_then(|v| v.strip_suffix('\''))
                    .map(|v| v.replace("\\'", "'"))
                    .unwrap_or_else(|| v.to_owned());
                if !take(k.trim(), &unquoted)? {
                    kept.push(token);
                }
            }
            kept.join(" ")
        };
        Ok((rest, policy))
    }
    /// The connector for this policy. Errors never name the file.
    pub(crate) fn connector(&self) -> Result<native_tls::TlsConnector, Error> {
        let mut builder = native_tls::TlsConnector::builder();
        if let Some(path) = &self.root_cert {
            let pem = std::fs::read(path).map_err(|_| Error("postgres.tls_root_certificate"))?;
            let roots = native_tls::Certificate::stack_from_pem(&pem)
                .ok()
                .filter(|roots| !roots.is_empty())
                .ok_or(Error("postgres.tls_root_certificate"))?;
            for root in roots {
                builder.add_root_certificate(root);
            }
            builder.disable_built_in_roots(true);
        }
        let (chain, host) = match (self.mode, self.root_cert.is_some()) {
            (SslMode::Disable, _) => (false, false),
            (SslMode::Prefer | SslMode::Require, false) => (false, false),
            (SslMode::Prefer | SslMode::Require | SslMode::VerifyCa, _) => (true, false),
            (SslMode::VerifyFull, _) => (true, true),
        };
        builder
            .danger_accept_invalid_certs(!chain)
            .danger_accept_invalid_hostnames(!host)
            .build()
            .map_err(|_| Error("postgres.tls_configuration"))
    }
}

/// `%XX` escapes in a URL query part (`+` stays itself, as libpq reads it).
fn percent_decode(value: &str) -> Result<String, ()> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value.get(i + 1..i + 3).ok_or(())?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| ())?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

/// `key=value` tokens of a libpq keyword string, single quotes respected.
fn key_value_tokens(value: &str) -> Result<Vec<&str>, ()> {
    let mut tokens = Vec::new();
    let (mut start, mut quoted, mut escaped) = (None, false, false);
    for (i, ch) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quoted => escaped = true,
            '\'' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if let Some(s) = start.take() {
                    tokens.push(&value[s..i]);
                }
                continue;
            }
            _ => {}
        }
        start.get_or_insert(i);
    }
    if quoted {
        return Err(());
    }
    if let Some(s) = start {
        tokens.push(&value[s..]);
    }
    Ok(tokens)
}

fn parse_pool_size(value: &str) -> Result<usize, Error> {
    match value.trim().parse::<usize>() {
        Ok(size) if (1..=1_000).contains(&size) => Ok(size),
        _ => Err(Error("postgres.invalid_pool_size")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn libpq_tls_settings_are_split_off_before_tokio_postgres_parses() {
        let split = |v: &str| TlsPolicy::split(v).unwrap();
        // Untouched without TLS keys.
        let (rest, tls) = split("postgres://u:p@db.example/rb?application_name=x");
        assert_eq!(rest, "postgres://u:p@db.example/rb?application_name=x");
        assert_eq!(tls, TlsPolicy::default());
        assert_eq!(tls.mode, SslMode::Prefer);
        let (rest, tls) = split(
            "postgres://u:p@db.example/rb?sslmode=verify-full&connect_timeout=5&sslrootcert=%2Fetc%2Fca.pem",
        );
        assert_eq!(rest, "postgres://u:p@db.example/rb?connect_timeout=5");
        assert_eq!(
            tls,
            TlsPolicy {
                mode: SslMode::VerifyFull,
                root_cert: Some("/etc/ca.pem".into()),
                explicit: true,
            }
        );
        let (rest, tls) = split("postgresql://u@h/rb?sslmode=require");
        assert_eq!(
            (rest.as_str(), tls.mode),
            ("postgresql://u@h/rb", SslMode::Require)
        );
        let (_, tls) = split("postgres://u@h/rb?sslmode=verify-ca&sslrootcert=system");
        assert_eq!((tls.mode, tls.root_cert), (SslMode::VerifyCa, None));
        assert_eq!(
            split("postgres://u@h/rb?sslmode=allow").1.mode,
            SslMode::Prefer
        );
        assert_eq!(
            split("postgres://u@h/rb?sslmode=disable").1.mode,
            SslMode::Disable
        );
        // The keyword form, quotes respected.
        let (rest, tls) = split(
            "host=db.example user=u password='a b\\'c' sslmode=verify-full sslrootcert='/run/ca dir/ca.pem' dbname=rb",
        );
        assert_eq!(rest, "host=db.example user=u password='a b\\'c' dbname=rb");
        assert_eq!(tls.root_cert, Some("/run/ca dir/ca.pem".into()));
        assert_eq!(tls.mode, SslMode::VerifyFull);
        for bad in [
            "postgres://u@h/rb?sslmode=bogus",
            "postgres://u@h/rb?sslrootcert=",
            "postgres://u@h/rb?sslmode=%zz",
            "host=h sslmode='verify-full",
        ] {
            assert!(TlsPolicy::split(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn database_urls_accept_every_sslmode_and_unix_sockets() {
        for mode in [
            "disable",
            "allow",
            "prefer",
            "require",
            "verify-ca",
            "verify-full",
        ] {
            let url = format!("postgres://u:secret@db.example/rb?sslmode={mode}");
            let config = DatabaseConfig::from_values(Some(url), None).unwrap();
            assert!(!config.unix_socket());
            config.tls().connector().unwrap();
        }
        let bad = DatabaseConfig::from_values(
            Some("postgres://u:secret@db.example/rb?sslmode=verify-everything".into()),
            None,
        );
        assert_eq!(
            bad.err().map(|e| e.0),
            Some("postgres.invalid_database_url")
        );
        // Cloud SQL's socket: TLS never applies there (libpq ignores sslmode).
        let config = DatabaseConfig::from_values(
            Some("postgres://u:secret@/rb?host=/cloudsql/proj:region:inst&sslmode=require".into()),
            None,
        )
        .unwrap();
        assert!(config.unix_socket());
        assert_eq!(
            config.primary().get_ssl_mode(),
            tokio_postgres::config::SslMode::Disable
        );
        assert!(config.tls_warnings().is_empty());
        // The direct endpoint keeps its own policy.
        let config = DatabaseConfig::from_values(
            Some("postgres://u@pooler.example/rb?sslmode=require".into()),
            Some("postgres://u@db.example/rb?sslmode=verify-full".into()),
        )
        .unwrap();
        assert_eq!(config.tls().mode, SslMode::Require);
        assert_eq!(config.direct_connection().tls().mode, SslMode::VerifyFull);
        assert_eq!(
            config.primary().get_ssl_mode(),
            tokio_postgres::config::SslMode::Require
        );
        // Encrypted but unverified gets a startup warning that names the
        // variable, never the URL; each endpoint is checked (HOST1 review).
        let warnings = config.tls_warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with("DATABASE_URL sets"));
        assert!(warnings[0].contains("verify-full") && !warnings[0].contains("pooler"));
        let direct_only = DatabaseConfig::from_values(
            Some("postgres://u@db.example/rb?sslmode=verify-full".into()),
            Some("postgres://u@db.example/rb?sslmode=require".into()),
        )
        .unwrap();
        let warnings = direct_only.tls_warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with("DATABASE_URL_DIRECT sets"));
        let both = DatabaseConfig::from_values(
            Some("postgres://u@pooler.example/rb?sslmode=prefer".into()),
            Some("postgres://u@db.example/rb?sslmode=require".into()),
        )
        .unwrap();
        assert_eq!(both.tls_warnings().len(), 2);
        // A direct endpoint on a socket, or one with a root file, is quiet.
        for direct in [
            "postgres://u@/rb?host=/cloudsql/p:r:i&sslmode=require",
            "postgres://u@db.example/rb?sslmode=require&sslrootcert=/ca.pem",
        ] {
            let config = DatabaseConfig::from_values(
                Some("postgres://u@db.example/rb?sslmode=verify-full".into()),
                Some(direct.into()),
            )
            .unwrap();
            assert!(config.tls_warnings().is_empty(), "{direct}");
        }
        let quiet = DatabaseConfig::from_values(Some("postgres://u@h/rb".into()), None).unwrap();
        assert!(quiet.tls_warnings().is_empty());
    }
    #[test]
    fn a_root_certificate_file_must_hold_pem_roots() {
        let dir = std::env::temp_dir().join(format!("rb-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let junk = dir.join("junk.pem");
        std::fs::write(&junk, "not a certificate").unwrap();
        for path in [junk.clone(), dir.join("missing.pem")] {
            let policy = TlsPolicy {
                mode: SslMode::VerifyFull,
                root_cert: Some(path),
                explicit: true,
            };
            let error = policy.connector().err().unwrap();
            assert_eq!(error.0, "postgres.tls_root_certificate");
            assert!(!error.0.contains("rb-tls"));
        }
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn pool_size_is_bounded_and_clones_share_one_pool() {
        assert_eq!(parse_pool_size(" 8 "), Ok(8));
        for bad in ["0", "-1", "1001", "eight", ""] {
            assert_eq!(
                parse_pool_size(bad),
                Err(Error("postgres.invalid_pool_size"))
            );
        }
        let config = DatabaseConfig::from_values(Some("postgres://u@localhost/a".into()), None)
            .unwrap()
            .with_pool_size(4);
        assert_eq!(config.pool_size(), 4);
        let clone = config.clone();
        assert!(std::ptr::eq(config.pool(), clone.pool()));
        // Another database or other session settings never share connections.
        let other = config.with_database("b");
        assert!(!std::ptr::eq(config.pool(), other.pool()));
        assert_eq!(other.pool_size(), 4);
        assert!(!std::ptr::eq(
            config.pool(),
            config.direct_connection().pool()
        ));
    }
}

/// `RUNBOOK_ALLOW_NEWER_SCHEMA`: `true`/`1` allows, `false`/`0`/empty/unset
/// refuses; anything else is a configuration error.
fn parse_allow_newer_schema(value: Option<&str>) -> Result<bool, Error> {
    match value.map(str::trim) {
        None | Some("") | Some("false") | Some("0") => Ok(false),
        Some("true") | Some("1") => Ok(true),
        Some(_) => Err(Error("postgres.invalid_allow_newer_schema")),
    }
}

#[cfg(test)]
mod upd1_tests {
    #[test]
    fn allow_newer_schema_parses_strictly() {
        use super::parse_allow_newer_schema as parse;
        for (value, expected) in [
            (None, Ok(false)),
            (Some(""), Ok(false)),
            (Some("false"), Ok(false)),
            (Some("0"), Ok(false)),
            (Some("true"), Ok(true)),
            (Some(" 1 "), Ok(true)),
        ] {
            assert_eq!(parse(value), expected, "{value:?}");
        }
        assert_eq!(
            parse(Some("yes")).unwrap_err().0,
            "postgres.invalid_allow_newer_schema"
        );
    }
}
