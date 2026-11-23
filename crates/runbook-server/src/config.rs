//! RUNBOOK_ORIGIN is required. The listen address is RUNBOOK_LISTEN_ADDR;
//! without it, `0.0.0.0:$PORT` when RUNBOOK_LISTEN_PORT_FROM_ENV=1 (the
//! image sets it, so Cloud Run, Fly and Render's PORT is followed);
//! otherwise 127.0.0.1:8080.
//! DATABASE_URL and DATABASE_URL_DIRECT use the shared native adapter parser.
//! RUNBOOK_WEB_DIR (optional) is the built web UI that `serve` hosts (B2).
use crate::Error;
use runbook_store_postgres::DatabaseConfig;
use std::{fmt, net::SocketAddr, time::Duration};

/// An environment value, with empty (or blank) meaning unset (D48).
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}
#[derive(Clone)]
pub struct ServerConfig {
    pub database: DatabaseConfig,
    pub origin: String,
    pub listen: SocketAddr,
    /// Largest body accepted on an upload route (skill bundles).
    pub max_request_bytes: usize,
    pub authoring: runbook_api::authoring::AuthoringLimits,
    pub webhooks: WebhookConfig,
    pub transport: TransportLimits,
    /// B2: the built web UI (`web/dist`), served on the API's origin.
    /// None (RUNBOOK_WEB_DIR unset or empty) serves no files.
    pub web_dir: Option<std::path::PathBuf>,
    /// AUTH2 (D43, D48): the Better Auth child process `serve` supervises.
    /// None unless RUNBOOK_AUTH_SECRET is set; then sessions, `/api/auth`
    /// and the internal routes exist.
    pub auth: Option<AuthConfig>,
    /// UP1 evidence uploads (D49).
    pub uploads: UploadConfig,
    /// ARC2 daily archives and retention (D53–D54).
    pub retention: RetentionConfig,
    /// HOST1: how long `serve` takes, at most, from SIGTERM to exit
    /// (RUNBOOK_SHUTDOWN_TIMEOUT_SECS, default 8: inside Cloud Run's 10 s).
    pub shutdown_timeout: Duration,
    /// HOST1: RUNBOOK_LOG_FORMAT, `text` (default) or `json` (one object
    /// per line on stdout, for Cloud Logging and other collectors).
    pub log_format: crate::log::Format,
    /// UI-1: the theme colour override `/v1/meta` reports
    /// (`RUNBOOK_THEME_*`, [`runbook_api::discovery::theme_from_vars`]).
    pub theme: Option<runbook_model::ThemeView>,
    /// UI-1: low-contrast warnings for those colours, logged at startup.
    pub theme_warnings: Vec<String>,
    /// UPD1: the database's schema version, which `/v1/meta` reports.
    /// This code's ([`runbook_store_postgres::SCHEMA_VERSION`]) until
    /// `serve` migrates and records what the database holds.
    pub schema_version: u32,
    /// UPD1: apply pending migrations while starting `serve`.
    /// `RUNBOOK_MIGRATE_ON_START=false` checks the schema instead, for
    /// deployments that run `runbook-server migrate` first (a Kubernetes
    /// Job). The default stays true for Docker and single instances.
    pub migrate_on_start: bool,
    /// NOTIFY1: notification mail transport (SMTP or the test capture
    /// file). None: mail stays queued; the drain says so once.
    pub email: Option<crate::email::EmailConfig>,
    /// STO1: optional object storage for blobs (GCS or S3).
    pub blob_store: runbook_store_objects::BlobStoreConfig,
    /// SLACK1 stage 2: the operator's Slack app (`RUNBOOK_SLACK_BOT_TOKEN`,
    /// `RUNBOOK_SLACK_SIGNING_SECRET`, `RUNBOOK_SLACK_DEFAULT_CHANNEL`).
    pub slack: Option<runbook_core::slack::SlackApp>,
    /// R0: the runner (registry URLs and the proxy to it).
    pub runner: RunnerConfig,
}

/// R0: how this server reaches its organizations' runners.
#[derive(Clone, Default)]
pub struct RunnerConfig {
    /// `RUNBOOK_RUNNER_PRIVATE_NETWORKS=true`: a runner URL may be a
    /// private or loopback address (plain http included), as for
    /// self-hosted webhook targets. Link-local and metadata addresses stay
    /// refused.
    pub private_networks: bool,
    /// Optional exact base URL for private-network proxy requests.
    pub private_url: Option<String>,
    /// `RUNBOOK_RUNNER_CLIENT_CERT`: a PEM file holding the client
    /// certificate and its key, presented to the runner over mutual TLS.
    pub client_cert_pem: Option<Vec<u8>>,
    /// Tests: resolve runner hosts with this instead of the system resolver.
    pub resolver: Option<crate::webhooks::Resolve>,
}
impl std::fmt::Debug for RunnerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerConfig")
            .field("private_networks", &self.private_networks)
            .field(
                "client_cert",
                &self.client_cert_pem.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

/// HOST1: the listen address: RUNBOOK_LISTEN_ADDR when set (empty means
/// unset). Otherwise, when the image opts in with
/// RUNBOOK_LISTEN_PORT_FROM_ENV=1 (HOST1 review: a stray PORT in a
/// developer's shell must not bind every interface), every interface on
/// PORT, or 8080 without it; else loopback 8080. Never echoes a value.
pub fn listen_address(
    listen: Option<&str>,
    port: Option<&str>,
    port_from_env: Option<&str>,
) -> Result<String, Error> {
    if let Some(listen) = listen.map(str::trim).filter(|v| !v.is_empty()) {
        return Ok(listen.to_owned());
    }
    let opted_in = match port_from_env.map(str::trim).unwrap_or("") {
        "" | "0" | "false" => false,
        "1" | "true" => true,
        _ => return Err(Error("config.invalid_listen_port_from_env")),
    };
    if !opted_in {
        return Ok("127.0.0.1:8080".into());
    }
    match port.map(str::trim).filter(|v| !v.is_empty()) {
        Some(port) => {
            let port: u16 = port
                .parse()
                .ok()
                .filter(|p| *p != 0)
                .ok_or(Error("config.invalid_port"))?;
            Ok(format!("0.0.0.0:{port}"))
        }
        None => Ok("0.0.0.0:8080".into()),
    }
}

/// ARC2 (D54): retention for self-hosted installs. Daily archives are always
/// written; nothing is deleted unless a window is set (default pending user
/// confirmation: retention is off until the operator sets it).
///
/// - `RUNBOOK_RETENTION_LIVE_DAYS`: live detail is deleted this many days
///   after a run finishes, once its daily file exists (90 is the PRD
///   default).
/// - `RUNBOOK_RETENTION_ARCHIVE_DAYS`: whole daily files are deleted this
///   many days after their day ends; needs a live window no longer than it.
/// - `RUNBOOK_RETENTION_SUMMARY_DAYS`: summary rows of deleted runs go this
///   many days after the run finished.
/// - `RUNBOOK_RETENTION_PROJECT_OVERRIDES`: per-project policies,
///   `prj_…:live=30,archive=365;prj_…:live=off` (`off` keeps forever; an
///   omitted field inherits the default).
///
/// Empty variables count as unset.
#[derive(Clone, Debug)]
pub struct RetentionConfig {
    pub defaults: runbook_core::archives::job::RetentionPolicy,
    pub projects: std::collections::BTreeMap<runbook_model::ProjectId, PolicyOverride>,
    /// How often the job runs while the UTC day stays the same (it always
    /// runs once the day changes).
    pub interval: Duration,
    /// TESTS ONLY: added to the store clock when the job decides what is
    /// due, so a smoke test can jump days ahead. Debug builds only:
    /// `RetentionConfig::check` refuses it in release builds
    /// (`config.test_clock_refused`). RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS.
    pub test_clock_offset_ms: i64,
}
impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            defaults: Default::default(),
            projects: Default::default(),
            interval: Duration::from_secs(15 * 60),
            test_clock_offset_ms: 0,
        }
    }
}
/// One project's override: `None` inherits the default; `Some(None)` keeps
/// forever; `Some(Some(n))` is `n` days.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PolicyOverride {
    pub live_days: Option<Option<u32>>,
    pub archive_days: Option<Option<u32>>,
    pub summary_days: Option<Option<u32>>,
}
impl RetentionConfig {
    pub fn policy_for(
        &self,
        project: &runbook_model::ProjectId,
    ) -> runbook_core::archives::job::RetentionPolicy {
        let mut policy = self.defaults;
        if let Some(o) = self.projects.get(project) {
            policy.live_days = o.live_days.unwrap_or(policy.live_days);
            policy.archive_days = o.archive_days.unwrap_or(policy.archive_days);
            policy.summary_days = o.summary_days.unwrap_or(policy.summary_days);
        }
        policy
    }
    /// Every effective policy must be coherent, and the test clock exists in
    /// debug builds only.
    pub fn check(&self) -> Result<(), Error> {
        if self.test_clock_offset_ms != 0 && !cfg!(debug_assertions) {
            return Err(Error("config.test_clock_refused"));
        }
        self.defaults.check().map_err(Error)?;
        for project in self.projects.keys() {
            self.policy_for(project).check().map_err(Error)?;
        }
        Ok(())
    }
    fn days(value: &str) -> Result<u32, Error> {
        value
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|d| (1..=36_500).contains(d))
            .ok_or(Error("config.invalid_retention_days"))
    }
    /// Parse RUNBOOK_RETENTION_PROJECT_OVERRIDES.
    pub fn parse_overrides(
        value: &str,
    ) -> Result<std::collections::BTreeMap<runbook_model::ProjectId, PolicyOverride>, Error> {
        let bad = || Error("config.invalid_retention_overrides");
        let mut out = std::collections::BTreeMap::new();
        for entry in value.split(';').map(str::trim).filter(|e| !e.is_empty()) {
            let (project, fields) = entry.split_once(':').ok_or_else(bad)?;
            let project: runbook_model::ProjectId = project.trim().parse().map_err(|_| bad())?;
            let mut o = PolicyOverride::default();
            for field in fields.split(',').map(str::trim).filter(|f| !f.is_empty()) {
                let (name, value) = field.split_once('=').ok_or_else(bad)?;
                let days = match value.trim() {
                    "off" => None,
                    v => Some(Self::days(v)?),
                };
                let slot = match name.trim() {
                    "live" => &mut o.live_days,
                    "archive" => &mut o.archive_days,
                    "summary" => &mut o.summary_days,
                    _ => return Err(bad()),
                };
                if slot.replace(days).is_some() {
                    return Err(bad());
                }
            }
            if out.insert(project, o).is_some() {
                return Err(bad());
            }
        }
        Ok(out)
    }
}

/// Evidence uploads (D49–D51). Slots are issued only when the server has
/// all of: a signing key, a user-content origin other than RUNBOOK_ORIGIN,
/// and the magic-byte table; `/v1/meta` reports `capabilities.uploads`
/// accordingly. Environment (empty means unset, as in D48):
/// - RUNBOOK_UPLOAD_SIGNING_KEY: base64, 32 bytes;
/// - RUNBOOK_USERCONTENT_ORIGIN: the origin of upload and file URLs (D50);
/// - RUNBOOK_USERCONTENT_LISTEN_ADDR: a second listener for that origin,
///   serving only `/u/*`. HOST1: when it is unset, the main listener
///   routes requests whose `Host` is the user-content origin's to the same
///   `/u/*` handler (single-port mode, for platforms with one port).
#[derive(Clone)]
pub struct UploadConfig {
    pub signing_key: Option<runbook_core::uploads::signing::UploadSigningKey>,
    /// Origin of signed upload URLs (D50: never the app origin).
    pub usercontent_origin: Option<String>,
    /// UP2: the user-content listener's address. `serve` starts it only
    /// with a user-content origin; with an origin and no address the main
    /// listener routes by host (HOST1, [`Self::shared_port`]).
    pub usercontent_listen: Option<SocketAddr>,
    /// The PRD §14 self-hosted column by default: no size or count limits.
    pub limits: runbook_model::UploadLimits,
    pub sniff: Option<runbook_core::uploads::verify::Sniff>,
}
impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            signing_key: None,
            usercontent_origin: None,
            usercontent_listen: None,
            limits: runbook_api::discovery::self_hosted_upload_limits(),
            sniff: Some(runbook_core::uploads::sniff::sniff),
        }
    }
}
impl UploadConfig {
    /// Whether slots can be issued and finalized (`capabilities.uploads`).
    pub fn enabled(&self, origin: &str) -> bool {
        self.signing_key.is_some()
            && self.sniff.is_some()
            && self
                .usercontent_origin
                .as_deref()
                .is_some_and(|u| !same_origin(u, origin))
    }
    /// CF0: what the portable upload routes need, for the app `origin`.
    pub fn settings(&self, origin: &str) -> runbook_api::evidence::UploadSettings {
        runbook_api::evidence::UploadSettings {
            enabled: self.enabled(origin),
            signing_key: self.signing_key.clone(),
            usercontent_origin: self.usercontent_origin.clone(),
            limits: self.limits.clone(),
            sniff: self.sniff,
        }
    }
    /// The widest image side the native pipeline accepts (D51): the
    /// configured `image_max_dimension`, never above `IMAGE_MAX_DIMENSION`,
    /// because every accepted image is decoded to strip its metadata.
    pub fn image_max_dimension(&self) -> u32 {
        let cap = runbook_core::uploads::limits::IMAGE_MAX_DIMENSION;
        self.limits.image_max_dimension.map_or(cap, |d| d.min(cap))
    }
    /// The largest width × height the native pipeline accepts: the
    /// configured `image_max_pixels`, never above
    /// `images::IMAGE_MAX_PIXELS` (its memory budget).
    pub fn image_max_pixels(&self) -> u64 {
        let cap = crate::images::IMAGE_MAX_PIXELS;
        self.limits.image_max_pixels.map_or(cap, |p| p.min(cap))
    }
    /// The pipeline's caps for one image.
    pub fn image_caps(&self) -> crate::images::Caps {
        crate::images::Caps {
            max_dimension: self.image_max_dimension(),
            max_pixels: self.image_max_pixels(),
        }
    }
    /// The limits `/v1/meta` advertises: the configured ones, with the
    /// dimension and pixel caps the pipeline enforces, so clients shrink
    /// first.
    pub fn advertised_limits(&self) -> runbook_model::UploadLimits {
        let mut limits = self.limits.clone();
        limits.image_max_dimension = Some(self.image_max_dimension());
        limits.image_max_pixels = Some(self.image_max_pixels());
        limits
    }
    /// RUNBOOK_USERCONTENT_ORIGIN and RUNBOOK_USERCONTENT_LISTEN_ADDR (empty
    /// already dropped): with an origin, a listen address other than the
    /// main one starts a second listener, and no address shares the main
    /// port, routed by host (HOST1); then the user-content host must differ
    /// from the app's, or nothing could tell them apart. A listen address
    /// without an origin starts nothing (the compose file always sets one).
    /// Never echoes a value.
    pub fn usercontent(
        origin: Option<&str>,
        listen: Option<&str>,
        main: SocketAddr,
        app_origin: &str,
    ) -> Result<(Option<String>, Option<SocketAddr>), Error> {
        let Some(origin) = origin else {
            return Ok((None, None));
        };
        let origin =
            parse_origin(origin.trim()).map_err(|_| Error("config.invalid_usercontent_origin"))?;
        let Some(listen) = listen else {
            let files = HostMatch::of(&origin).ok_or(Error("config.invalid_usercontent_origin"))?;
            if HostMatch::of(app_origin).is_some_and(|app| app.same_authority(&files)) {
                return Err(Error("config.usercontent_host_conflict"));
            }
            return Ok((Some(origin), None));
        };
        let listen: SocketAddr = listen
            .trim()
            .parse()
            .map_err(|_| Error("config.invalid_usercontent_listen"))?;
        if listen == main {
            return Err(Error("config.usercontent_listen_conflict"));
        }
        Ok((Some(origin), Some(listen)))
    }
    /// HOST1: user content shares the main port, routed by `Host`.
    pub fn shared_port(&self) -> bool {
        self.usercontent_origin.is_some() && self.usercontent_listen.is_none()
    }
    /// HOST1 review: a startup warning when the user-content origin has the
    /// app's host name on another port. Ports make separate origins, but
    /// cookies ignore ports, so the app's cookies would also reach user
    /// content. Never echoes a value.
    pub fn cookie_warning(&self, origin: &str) -> Option<&'static str> {
        let files = HostMatch::of(self.usercontent_origin.as_deref()?)?;
        let app = HostMatch::of(origin)?;
        (files.host == app.host && !same_origin(self.usercontent_origin.as_deref()?, origin))
            .then_some(
                "RUNBOOK_USERCONTENT_ORIGIN has the same host name as RUNBOOK_ORIGIN on another port; cookies ignore ports, so use a different host name (ideally another domain) outside local testing",
            )
    }
    /// Why uploads stay off although a user-content origin is set, for a
    /// startup warning. Never echoes a value.
    pub fn warning(&self, origin: &str) -> Option<&'static str> {
        let usercontent = self.usercontent_origin.as_deref()?;
        if same_origin(usercontent, origin) {
            return Some(
                "RUNBOOK_USERCONTENT_ORIGIN equals RUNBOOK_ORIGIN, so uploads stay off; serve user content from a separate origin (ideally a separate domain)",
            );
        }
        if self.signing_key.is_none() {
            return Some(
                "RUNBOOK_USERCONTENT_ORIGIN is set but RUNBOOK_UPLOAD_SIGNING_KEY is not, so uploads stay off",
            );
        }
        None
    }
}
impl fmt::Debug for UploadConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadConfig")
            .field(
                "signing_key",
                &self.signing_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("usercontent_origin", &self.usercontent_origin.is_some())
            .field("usercontent_listen", &self.usercontent_listen)
            .field("shared_port", &self.shared_port())
            .field("sniff", &self.sniff.is_some())
            .finish()
    }
}

/// AUTH2 sign-in settings (D48). The child inherits only the allowlisted
/// server variables ([`crate::auth_child::INHERITED_ENV`]: RUNBOOK_AUTH_SECRET,
/// RUNBOOK_SMTP_*, RUNBOOK_GOOGLE_*, DATABASE_URL and runtime basics) and
/// also gets the contract variables in
/// [`crate::auth_child`]: `RUNBOOK_AUTH_PORT`, `RUNBOOK_AUTH_INTERNAL_SECRET`,
/// `RUNBOOK_AUTH_CAN_REGISTER_URL` and the normalized `RUNBOOK_ORIGIN`.
#[derive(Clone, Debug)]
pub struct AuthConfig {
    /// RUNBOOK_AUTH_COMMAND, split on whitespace; default
    /// `bun auth/dist/server.mjs` (the image sets its own absolute path).
    pub command: Vec<String>,
    /// RUNBOOK_AUTH_METHODS (`email`, `google`), listed in discovery.
    pub methods: Vec<String>,
    /// RUNBOOK_AUTH_ALLOWED_DOMAINS, lowercased; empty allows any domain.
    pub allowed_domains: Vec<String>,
    /// D44: how long a resolved session is trusted without asking the child.
    pub session_cache_ttl: Duration,
    /// Restart backoff after the child exits: doubles from `min` to `max`,
    /// and starts over once a child has stayed up for a minute.
    pub restart_min: Duration,
    pub restart_max: Duration,
    /// Extra environment for the child (tests).
    pub child_env: Vec<(String, String)>,
}
impl AuthConfig {
    pub fn new(command: Vec<String>) -> Self {
        Self {
            command,
            methods: Vec::new(),
            allowed_domains: Vec::new(),
            session_cache_ttl: Duration::from_secs(30),
            restart_min: Duration::from_millis(250),
            restart_max: Duration::from_secs(30),
            child_env: Vec::new(),
        }
    }
    /// D45: an email's domain passes when no domains are configured or it
    /// equals one of them (case-insensitive, no subdomain matching).
    pub fn domain_allowed(&self, email: &str) -> bool {
        runbook_api::session::domain_allowed(&self.allowed_domains, email)
    }
}
/// F45/F48 transport bounds. Anonymous clients cannot make the server
/// buffer a body, hold a socket with half-sent headers, or hold streams.
/// RUNBOOK_MAX_CONNECTIONS overrides `max_connections`.
#[derive(Clone, Debug)]
pub struct TransportLimits {
    /// Open TCP connections; further connections wait in the backlog.
    pub max_connections: usize,
    /// Time allowed to send a request's headers.
    pub header_read_timeout: Duration,
    /// Time from complete headers to the response, body read included.
    pub request_timeout: Duration,
    /// UP2: the same for one signed upload PUT on the user-content listener,
    /// which may carry a large file over a slow link.
    pub upload_timeout: Duration,
    /// Body cap for every route except uploads. Twice the largest source
    /// plus room for the JSON envelope, so any valid draft fits.
    pub json_body_bytes: usize,
    /// Body bytes buffered at once across all requests; beyond it 503.
    pub in_flight_body_bytes: usize,
    /// Requests handled at once by the store executor, and queued behind.
    pub max_in_flight: usize,
    pub queue_depth: usize,
    /// Live event streams in total and per credential, and their lifetime
    /// (clients reconnect with Last-Event-ID).
    pub max_streams: usize,
    pub streams_per_credential: usize,
    pub stream_lifetime: Duration,
    /// D32: an open stream rechecks its credential at least this often.
    /// With the 30 s session cache (D44), a revoked session's stream closes
    /// within 60 s.
    pub stream_recheck: Duration,
    /// MCP1: `/mcp` messages handled at once; beyond it 503 `server.busy`
    /// at once (each message holds a blocking-pool thread).
    pub mcp_in_flight: usize,
    /// MCP1: API calls one `/mcp` message may make (a `resources/list`
    /// walk, a slug lookup); beyond it the message ends with
    /// `limit.mcp_calls`.
    pub mcp_calls_per_message: usize,
    /// Sign-in gate fix 1: RUNBOOK_TRUSTED_PROXIES, the reverse proxies
    /// whose `X-Forwarded-For` / `Forwarded` name the client
    /// ([`crate::client_ip`]). Empty trusts none.
    pub trusted_proxies: crate::client_ip::TrustedProxies,
}
impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            max_connections: 512,
            header_read_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            upload_timeout: Duration::from_secs(600),
            json_body_bytes: runbook_api::http::JSON_BODY_BYTES as usize,
            in_flight_body_bytes: 128 * 1_048_576,
            max_in_flight: 8,
            queue_depth: 32,
            max_streams: 32,
            streams_per_credential: 4,
            stream_lifetime: Duration::from_secs(3600),
            stream_recheck: Duration::from_secs(30),
            mcp_in_flight: 16,
            mcp_calls_per_message: 200,
            trusted_proxies: Default::default(),
        }
    }
}
/// C1 webhook settings. RUNBOOK_WEBHOOK_SECRET_KEY (base64, 32 bytes) seals
/// subscription secrets at rest; without it webhook routes answer 503
/// `webhooks.not_configured` and deliveries wait. Self-hosted operators set
/// RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS=true to allow private, loopback and
/// plain-http targets; link-local targets (cloud metadata) additionally need
/// RUNBOOK_ALLOW_LINK_LOCAL_WEBHOOK_TARGETS=true.
#[derive(Clone, Default)]
pub struct WebhookConfig {
    pub allow_private_targets: bool,
    pub allow_link_local_targets: bool,
    pub secret_key: Option<[u8; 32]>,
    /// F101: keys that sealed earlier secrets; they open, never seal.
    /// RUNBOOK_WEBHOOK_PREVIOUS_SECRET_KEYS, comma-separated base64.
    pub previous_secret_keys: Vec<[u8; 32]>,
    /// Test seam for DNS; None uses the system resolver.
    pub resolver: Option<crate::webhooks::Resolve>,
}
impl WebhookConfig {
    pub fn policy(&self) -> runbook_core::webhooks::TargetPolicy {
        runbook_core::webhooks::TargetPolicy {
            allow_private: self.allow_private_targets,
            allow_link_local: self.allow_link_local_targets,
        }
    }
}
impl fmt::Debug for WebhookConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebhookConfig")
            .field("allow_private_targets", &self.allow_private_targets)
            .field("allow_link_local_targets", &self.allow_link_local_targets)
            .field("secret_key", &self.secret_key.map(|_| "[REDACTED]"))
            .field("previous_secret_keys", &self.previous_secret_keys.len())
            .finish()
    }
}
impl fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerConfig")
            .field("urls", &"[REDACTED]")
            .field("listen", &self.listen)
            .finish()
    }
}
impl ServerConfig {
    pub fn from_env() -> Result<Self, Error> {
        let origin =
            std::env::var("RUNBOOK_ORIGIN").map_err(|_| Error("config.origin_required"))?;
        let read = |name: &str| match std::env::var(name) {
            Ok(v) => Ok(Some(v)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(Error("config.invalid_listen")),
        };
        let listen = listen_address(
            read("RUNBOOK_LISTEN_ADDR")?.as_deref(),
            read("PORT")?.as_deref(),
            read("RUNBOOK_LISTEN_PORT_FROM_ENV")?.as_deref(),
        )?;
        let mut config = Self::new(
            DatabaseConfig::from_env().map_err(|e| Error(e.0))?,
            &origin,
            &listen,
        )?;
        config.migrate_on_start =
            parse_migrate_on_start(read("RUNBOOK_MIGRATE_ON_START")?.as_deref())?;
        config.email = crate::email::email_config(|name| read(name).ok().flatten())?;
        if let Some(value) = std::env::var_os("RUNBOOK_WEB_DIR").filter(|v| !v.is_empty()) {
            config.web_dir = Some(value.into());
        }
        if let Ok(value) = std::env::var("RUNBOOK_MAX_REQUEST_BYTES") {
            config.max_request_bytes = value
                .parse()
                .map_err(|_| Error("config.invalid_request_limit"))?;
            if config.max_request_bytes == 0 {
                return Err(Error("config.invalid_request_limit"));
            }
        }
        if let Ok(value) = std::env::var("RUNBOOK_TRUSTED_PROXIES") {
            config.transport.trusted_proxies = crate::client_ip::TrustedProxies::parse(&value)
                .ok_or(Error("config.invalid_trusted_proxies"))?;
        }
        if let Ok(value) = std::env::var("RUNBOOK_MAX_CONNECTIONS") {
            config.transport.max_connections = value
                .parse()
                .ok()
                .filter(|n| *n > 0)
                .ok_or(Error("config.invalid_connection_limit"))?;
        }
        if let Ok(value) = std::env::var("RUNBOOK_MAX_SOURCE_BYTES") {
            config.authoring.source_bytes = value
                .parse()
                .map_err(|_| Error("config.invalid_source_limit"))?;
            if config.authoring.source_bytes == 0 || config.authoring.source_bytes > 1_048_576 {
                return Err(Error("config.invalid_source_limit"));
            }
        }
        if let Ok(value) = std::env::var("RUNBOOK_MAX_SKILL_BYTES") {
            config.authoring.skill_bytes = value
                .parse()
                .map_err(|_| Error("config.invalid_skill_limit"))?;
        }
        if let Ok(value) = std::env::var("RUNBOOK_WEBHOOK_SECRET_KEY") {
            use base64::Engine;
            let key = base64::engine::general_purpose::STANDARD
                .decode(value.trim())
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                .ok_or(Error("config.invalid_webhook_key"))?;
            config.webhooks.secret_key = Some(key);
        }
        if let Ok(value) = std::env::var("RUNBOOK_WEBHOOK_PREVIOUS_SECRET_KEYS") {
            use base64::Engine;
            for part in value.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let key = base64::engine::general_purpose::STANDARD
                    .decode(part)
                    .ok()
                    .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                    .ok_or(Error("config.invalid_webhook_key"))?;
                config.webhooks.previous_secret_keys.push(key);
            }
        }
        if let Some(value) = env_value("RUNBOOK_UPLOAD_SIGNING_KEY") {
            use base64::Engine;
            let key = base64::engine::general_purpose::STANDARD
                .decode(value.trim())
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                .ok_or(Error("config.invalid_upload_key"))?;
            config.uploads.signing_key =
                Some(runbook_core::uploads::signing::UploadSigningKey::new(key));
        }
        // UP2 (D50): the user-content origin and its listener.
        (
            config.uploads.usercontent_origin,
            config.uploads.usercontent_listen,
        ) = UploadConfig::usercontent(
            env_value("RUNBOOK_USERCONTENT_ORIGIN").as_deref(),
            env_value("RUNBOOK_USERCONTENT_LISTEN_ADDR").as_deref(),
            config.listen,
            &config.origin,
        )?;
        if let Some(value) = env_value("RUNBOOK_SHUTDOWN_TIMEOUT_SECS") {
            config.shutdown_timeout = value
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|s| (1..=300).contains(s))
                .map(Duration::from_secs)
                .ok_or(Error("config.invalid_shutdown_timeout"))?;
        }
        if let Some(value) = env_value("RUNBOOK_LOG_FORMAT") {
            config.log_format = crate::log::Format::parse(value.trim())
                .ok_or(Error("config.invalid_log_format"))?;
        }
        if let Ok(value) = std::env::var("RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS") {
            config.webhooks.allow_private_targets = match value.as_str() {
                "true" | "1" => true,
                "false" | "0" => false,
                _ => return Err(Error("config.invalid_private_webhooks")),
            };
        }
        if std::env::var("RUNBOOK_AUTH_SECRET").is_ok_and(|v| !v.trim().is_empty()) {
            let command = match std::env::var("RUNBOOK_AUTH_COMMAND") {
                Ok(v) => v.split_whitespace().map(str::to_owned).collect(),
                Err(_) => crate::auth_child::DEFAULT_COMMAND
                    .iter()
                    .map(|part| (*part).to_owned())
                    .collect(),
            };
            if Vec::<String>::is_empty(&command) {
                return Err(Error("config.invalid_auth_command"));
            }
            let mut auth = AuthConfig::new(command);
            if let Ok(value) = std::env::var("RUNBOOK_AUTH_METHODS") {
                for method in value.split(',').map(str::trim).filter(|m| !m.is_empty()) {
                    if !matches!(method, "email" | "google") {
                        return Err(Error("config.invalid_auth_methods"));
                    }
                    if !auth.methods.iter().any(|m| m == method) {
                        auth.methods.push(method.to_owned());
                    }
                }
            }
            if let Ok(value) = std::env::var("RUNBOOK_AUTH_ALLOWED_DOMAINS") {
                auth.allowed_domains = value
                    .split(',')
                    .map(|d| d.trim().trim_start_matches('@').to_lowercase())
                    .filter(|d| !d.is_empty())
                    .collect();
            }
            config.auth = Some(auth);
        }
        let set = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        if let Some(value) = set("RUNBOOK_RETENTION_LIVE_DAYS") {
            config.retention.defaults.live_days = Some(RetentionConfig::days(&value)?);
        }
        if let Some(value) = set("RUNBOOK_RETENTION_ARCHIVE_DAYS") {
            config.retention.defaults.archive_days = Some(RetentionConfig::days(&value)?);
        }
        if let Some(value) = set("RUNBOOK_RETENTION_SUMMARY_DAYS") {
            config.retention.defaults.summary_days = Some(RetentionConfig::days(&value)?);
        }
        if let Some(value) = set("RUNBOOK_RETENTION_PROJECT_OVERRIDES") {
            config.retention.projects = RetentionConfig::parse_overrides(&value)?;
        }
        if let Some(value) = set("RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS") {
            config.retention.test_clock_offset_ms = value
                .trim()
                .parse()
                .map_err(|_| Error("config.invalid_test_clock"))?;
        }
        config.retention.check()?;
        (config.theme, config.theme_warnings) = theme_config(env_value)?;
        config.slack = runbook_core::slack::SlackApp::from_vars(env_value);
        if let Ok(value) = std::env::var("RUNBOOK_RUNNER_PRIVATE_NETWORKS") {
            config.runner.private_networks = match value.as_str() {
                "true" | "1" => true,
                "false" | "0" => false,
                _ => return Err(Error("config.invalid_runner_private_networks")),
            };
        }
        if let Some(value) = env_value("RUNBOOK_RUNNER_PRIVATE_URL") {
            let base = value.trim().trim_end_matches('/');
            if !config.runner.private_networks {
                return Err(Error("config.invalid_runner_private_url"));
            }
            runbook_core::runners::validate_url(base, true)
                .map_err(|_| Error("config.invalid_runner_private_url"))?;
            config.runner.private_url = Some(base.to_owned());
        }
        if let Some(path) = env_value("RUNBOOK_RUNNER_CLIENT_CERT") {
            let pem = std::fs::read(path.trim())
                .map_err(|_| Error("config.invalid_runner_client_cert"))?;
            // Refuse a file that does not hold a certificate and key at start.
            crate::runner_proxy::client_tls(&pem).map_err(Error)?;
            config.runner.client_cert_pem = Some(pem);
        }
        if let Ok(value) = std::env::var("RUNBOOK_ALLOW_LINK_LOCAL_WEBHOOK_TARGETS") {
            config.webhooks.allow_link_local_targets = match value.as_str() {
                "true" | "1" => true,
                "false" | "0" => false,
                _ => return Err(Error("config.invalid_link_local_webhooks")),
            };
        }
        config.blob_store =
            runbook_store_objects::BlobStoreConfig::from_env().map_err(|e| Error(e.0))?;
        config.blob_store.build_store().map_err(|e| Error(e.0))?;
        Ok(config)
    }
    /// F84: a startup warning when bearer tokens would cross the network
    /// in clear text: a plain `http://` origin whose host is not loopback.
    /// (A loopback `http://` origin is the local pilot, including the
    /// container that listens on 0.0.0.0 behind a loopback-published port.)
    /// Never echoes the origin.
    pub fn insecure_origin_warning(&self) -> Option<&'static str> {
        let uri: http::Uri = self.origin.parse().ok()?;
        if uri.scheme_str() != Some("http") {
            return None;
        }
        let host = uri.host()?.trim_start_matches('[').trim_end_matches(']');
        // The normalized origin has a lowercase host. RFC 6761 also reserves
        // names below localhost, including their absolute DNS spelling.
        let dns_host = host.strip_suffix('.').unwrap_or(host);
        let loopback = dns_host == "localhost"
            || dns_host.ends_with(".localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        (!loopback).then_some(
            "RUNBOOK_ORIGIN is plain http:// on a non-loopback host, so bearer tokens cross the network unencrypted; put a TLS reverse proxy in front and set RUNBOOK_ORIGIN to its https:// origin",
        )
    }
    pub fn new(database: DatabaseConfig, origin: &str, listen: &str) -> Result<Self, Error> {
        let origin = parse_origin(origin)?;
        Ok(Self {
            max_request_bytes: 67_108_864,
            authoring: Default::default(),
            webhooks: Default::default(),
            transport: Default::default(),
            web_dir: None,
            auth: None,
            uploads: UploadConfig::default(),
            retention: RetentionConfig::default(),
            shutdown_timeout: Duration::from_secs(8),
            log_format: Default::default(),
            theme: None,
            theme_warnings: vec![],
            schema_version: runbook_store_postgres::SCHEMA_VERSION,
            migrate_on_start: true,
            email: None,
            blob_store: Default::default(),
            slack: None,
            runner: RunnerConfig::default(),
            database,
            origin,
            listen: listen.parse().map_err(|_| Error("config.invalid_listen"))?,
        })
    }
}
/// UI-1: the theme override and its low-contrast warnings from the
/// `RUNBOOK_THEME_*` variables (`get` reads one; empty means unset). A
/// colour that is not `#rrggbb` refuses startup, naming the variable.
fn theme_config(
    get: impl Fn(&str) -> Option<String>,
) -> Result<(Option<runbook_model::ThemeView>, Vec<String>), Error> {
    let theme = runbook_api::discovery::theme_from_vars(&get).map_err(|e| Error(e.message()))?;
    Ok((theme, runbook_api::discovery::theme_warnings(&get)))
}
/// UPD1: `RUNBOOK_MIGRATE_ON_START`, defaulting to migrating on start.
fn parse_migrate_on_start(value: Option<&str>) -> Result<bool, Error> {
    match value {
        None | Some("true" | "1") => Ok(true),
        Some("false" | "0") => Ok(false),
        Some(_) => Err(Error("config.invalid_migrate_on_start")),
    }
}

/// `scheme://host[:port]` with http or https, no credentials, path or
/// query; returned normalized ([`normalize_origin`]).
fn parse_origin(origin: &str) -> Result<String, Error> {
    let uri: http::Uri = origin.parse().map_err(|_| Error("config.invalid_origin"))?;
    if !matches!(
        uri.scheme_str().map(str::to_ascii_lowercase).as_deref(),
        Some("http" | "https")
    ) || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || uri.query().is_some()
        || !matches!(uri.path(), "" | "/")
    {
        return Err(Error("config.invalid_origin"));
    }
    normalize_origin(origin).ok_or(Error("config.invalid_origin"))
}

/// An origin as a browser serializes it: lowercase scheme and host, the
/// port only when it is not the scheme's default, no trailing slash.
pub fn normalize_origin(origin: &str) -> Option<String> {
    let uri: http::Uri = origin.trim().parse().ok()?;
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    let host = uri.host()?.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    Some(match uri.port_u16() {
        Some(port) if port != default => format!("{scheme}://{host}:{port}"),
        _ => format!("{scheme}://{host}"),
    })
}

/// HOST1: an origin's host and effective port, to match a request's `Host`
/// header in single-port mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostMatch {
    host: String,
    port: u16,
    default_port: bool,
}
impl HostMatch {
    pub fn of(origin: &str) -> Option<Self> {
        let uri: http::Uri = origin.trim().parse().ok()?;
        let default = match uri.scheme_str()?.to_ascii_lowercase().as_str() {
            "http" => 80,
            "https" => 443,
            _ => return None,
        };
        let port = uri.port_u16().unwrap_or(default);
        Some(Self {
            host: uri.host()?.to_ascii_lowercase(),
            port,
            default_port: port == default,
        })
    }
    /// Whether a `Host` header (`name[:port]`) names this origin: the same
    /// host, and the origin's port, or no port when that port is the
    /// scheme's default (a TLS proxy forwards `files.example`, not `:443`).
    pub fn matches(&self, header: &str) -> bool {
        let Ok(authority) = header.trim().parse::<http::uri::Authority>() else {
            return false;
        };
        authority.host().eq_ignore_ascii_case(&self.host)
            && match authority.port_u16() {
                Some(port) => port == self.port,
                None => self.default_port,
            }
    }
    /// Whether no `Host` header could tell the two apart.
    fn same_authority(&self, other: &Self) -> bool {
        self.host == other.host
            && (self.port == other.port || (self.default_port && other.default_port))
    }
}

/// Whether two configured origins are the same origin (D50 compares
/// scheme, host and effective port, never the spelling).
fn same_origin(a: &str, b: &str) -> bool {
    match (normalize_origin(a), normalize_origin(b)) {
        (Some(a), Some(b)) => a == b,
        // Unparsable: never treat as separate.
        _ => true,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migrate_on_start_defaults_on_and_accepts_only_booleans() {
        assert!(parse_migrate_on_start(None).unwrap());
        assert!(parse_migrate_on_start(Some("true")).unwrap());
        assert!(parse_migrate_on_start(Some("1")).unwrap());
        assert!(!parse_migrate_on_start(Some("false")).unwrap());
        assert!(!parse_migrate_on_start(Some("0")).unwrap());
        for bad in ["", "yes", "check", "2"] {
            assert_eq!(
                parse_migrate_on_start(Some(bad)).unwrap_err().0,
                "config.invalid_migrate_on_start",
                "{bad}"
            );
        }
    }
    #[test]
    fn retention_overrides_parse_and_merge() {
        let p: runbook_model::ProjectId = "prj_00000000000000000000000001".parse().unwrap();
        let q: runbook_model::ProjectId = "prj_00000000000000000000000002".parse().unwrap();
        let projects = RetentionConfig::parse_overrides(&format!(
            "{p}: live=30, archive=365 ; {q}:live=off,archive=off,summary=off"
        ))
        .unwrap();
        let config = RetentionConfig {
            defaults: runbook_core::archives::job::RetentionPolicy {
                live_days: Some(90),
                archive_days: Some(365),
                summary_days: Some(400),
            },
            projects,
            ..Default::default()
        };
        let mine = config.policy_for(&p);
        assert_eq!(
            (mine.live_days, mine.archive_days, mine.summary_days),
            (Some(30), Some(365), Some(400))
        );
        assert_eq!(config.policy_for(&q).live_days, None);
        config.check().unwrap();
        // Summary rows must outlive files: none while files are kept forever.
        let forever = RetentionConfig {
            projects: RetentionConfig::parse_overrides(&format!("{p}:archive=off")).unwrap(),
            ..config.clone()
        };
        assert_eq!(
            forever.check().unwrap_err().0,
            "config.retention_summary_before_archive"
        );
        for bad in [
            "prj_00000000000000000000000001",
            "prj_00000000000000000000000001:live=0",
            "prj_00000000000000000000000001:live=30,live=40",
            "prj_00000000000000000000000001:keep=3",
            "nope:live=3",
            "prj_00000000000000000000000001:live=-1",
        ] {
            assert!(RetentionConfig::parse_overrides(bad).is_err(), "{bad}");
        }
        // An override that would expire files before live copies is refused.
        let incoherent = RetentionConfig {
            projects: RetentionConfig::parse_overrides(&format!("{p}:archive=10")).unwrap(),
            ..Default::default()
        };
        assert_eq!(
            incoherent.check().unwrap_err().0,
            "config.retention_archive_before_live"
        );
    }
    #[test]
    fn the_test_clock_exists_in_debug_builds_only() {
        let config = RetentionConfig {
            test_clock_offset_ms: 91 * 86_400_000,
            ..Default::default()
        };
        assert_eq!(config.check().is_ok(), cfg!(debug_assertions));
    }
    /// UI-1: the theme variables, as `from_env` reads them: an invalid
    /// colour refuses startup naming the variable; low contrast warns.
    #[test]
    fn theme_colours_are_validated_and_low_contrast_warns() {
        let env = |pairs: Vec<(&'static str, &'static str)>| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(theme_config(env(vec![])).unwrap(), (None, vec![]));
        assert_eq!(
            theme_config(env(vec![("RUNBOOK_THEME_ACCENT", "  ")])).unwrap(),
            (None, vec![])
        );
        let (theme, warnings) = theme_config(env(vec![
            ("RUNBOOK_THEME_ACCENT", "#4338CA"),
            ("RUNBOOK_THEME_DARK_ACCENT", "#a5b4fc"),
        ]))
        .unwrap();
        let theme = theme.unwrap();
        assert_eq!(theme.light.accent.as_deref(), Some("#4338ca"));
        assert_eq!(theme.dark.accent.as_deref(), Some("#a5b4fc"));
        assert!(warnings.is_empty(), "{warnings:?}");
        let (_, warnings) =
            theme_config(env(vec![("RUNBOOK_THEME_ACCENT_SECONDARY", "#00cc99")])).unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with(
            "theme: RUNBOOK_THEME_ACCENT_SECONDARY #00cc99 has contrast 2.0:1 against #ffffff"
        ));
        for (name, value) in [
            ("RUNBOOK_THEME_ACCENT", "#fff"),
            ("RUNBOOK_THEME_ACCENT_SECONDARY", "teal"),
            ("RUNBOOK_THEME_DARK_ACCENT", "4338ca"),
            ("RUNBOOK_THEME_DARK_ACCENT_SECONDARY", "#12345g"),
        ] {
            assert_eq!(
                theme_config(env(vec![(name, value)])).unwrap_err().0,
                format!("config.invalid_theme_color: {name} must be #rrggbb")
            );
        }
    }
    #[test]
    fn plain_http_origins_off_loopback_warn_at_startup() {
        let db = DatabaseConfig::from_values(
            Some("postgres://unused:secret@localhost/test".into()),
            None,
        )
        .unwrap();
        let warns = |origin: &str, listen: &str| {
            ServerConfig::new(db.clone(), origin, listen)
                .unwrap()
                .insecure_origin_warning()
        };
        for origin in [
            "http://runbook.internal:8080",
            "http://runbook.localhost.example",
            "http://notlocalhost",
            "http://10.0.0.5:8080",
            "http://192.168.1.20",
            "http://[2001:db8::1]:8080",
        ] {
            let warning = warns(origin, "0.0.0.0:8080").unwrap_or_else(|| panic!("{origin}"));
            assert!(warning.contains("https://") && !warning.contains("runbook.internal"));
            assert!(warns(origin, "127.0.0.1:8080").is_some(), "{origin}");
        }
        for origin in [
            "https://runbooks.example",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://127.1.2.3",
            "http://[::1]:8080",
            "http://LOCALHOST",
            "http://runbook.localhost:8080",
            "http://nested.RUNBOOK.localhost.",
        ] {
            assert_eq!(warns(origin, "0.0.0.0:8080"), None, "{origin}");
        }
    }
    #[test]
    fn the_usercontent_origin_gets_its_own_listener_or_shares_the_port_by_host() {
        let main: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let app = "https://runbooks.example.test";
        let parse = |o: Option<&str>, l: Option<&str>| UploadConfig::usercontent(o, l, main, app);
        assert_eq!(parse(None, None).unwrap(), (None, None));
        // The compose file always sets the address; alone it starts nothing.
        assert_eq!(parse(None, Some("0.0.0.0:8081")).unwrap(), (None, None));
        assert_eq!(
            parse(Some("https://files.example.test/"), Some(" 0.0.0.0:8081 ")).unwrap(),
            (
                Some("https://files.example.test".into()),
                Some("0.0.0.0:8081".parse().unwrap())
            )
        );
        // HOST1: no address shares the main port, routed by host.
        assert_eq!(
            parse(Some("https://files.example.test"), None).unwrap(),
            (Some("https://files.example.test".into()), None)
        );
        let shared = UploadConfig {
            usercontent_origin: Some("https://files.example.test".into()),
            ..Default::default()
        };
        assert!(shared.shared_port());
        for (origin, listen, code) in [
            (
                "https://files.example.test",
                Some("nope"),
                "config.invalid_usercontent_listen",
            ),
            (
                "https://files.example.test",
                Some("127.0.0.1:8080"),
                "config.usercontent_listen_conflict",
            ),
            // One port cannot tell the app's host from itself.
            (
                "http://runbooks.example.test",
                None,
                "config.usercontent_host_conflict",
            ),
            (
                "https://RUNBOOKS.example.test:443",
                None,
                "config.usercontent_host_conflict",
            ),
            (
                "https://files.example.test/x",
                Some("0.0.0.0:8081"),
                "config.invalid_usercontent_origin",
            ),
            (
                "https://user:secret@files.example.test",
                Some("0.0.0.0:8081"),
                "config.invalid_usercontent_origin",
            ),
            (
                "ftp://files.example.test",
                Some("0.0.0.0:8081"),
                "config.invalid_usercontent_origin",
            ),
        ] {
            let error = parse(Some(origin), listen).unwrap_err();
            assert_eq!(error.0, code, "{origin} {listen:?}");
        }
        // Another port of the same host is a separate origin and host.
        assert!(
            UploadConfig::usercontent(
                Some("http://localhost:8081"),
                None,
                main,
                "http://localhost:8080"
            )
            .is_ok()
        );
    }
    #[test]
    fn a_usercontent_origin_on_the_apps_host_name_warns_about_cookies() {
        let uploads = |origin: &str| UploadConfig {
            usercontent_origin: Some(origin.into()),
            ..Default::default()
        };
        // Cookies ignore ports: another port of the same name shares them.
        let warning = uploads("http://localhost:8081")
            .cookie_warning("http://localhost:8080")
            .unwrap();
        assert!(warning.contains("host name") && !warning.contains("localhost"));
        assert!(
            uploads("https://RUNBOOKS.example:8443")
                .cookie_warning("https://runbooks.example")
                .is_some()
        );
        for other in ["https://files.example", "https://files.runbooks.example"] {
            assert_eq!(
                uploads(other).cookie_warning("https://runbooks.example"),
                None,
                "{other}"
            );
        }
        assert_eq!(
            UploadConfig::default().cookie_warning("https://runbooks.example"),
            None
        );
    }
    #[test]
    fn host_headers_match_the_usercontent_origin_exactly() {
        let files = HostMatch::of("https://files.example.test").unwrap();
        for yes in [
            "files.example.test",
            "FILES.example.test",
            "files.example.test:443",
        ] {
            assert!(files.matches(yes), "{yes}");
        }
        for no in [
            "files.example.test:8443",
            "example.test",
            "evil-files.example.test",
            "files.example.test.evil",
            "runbooks.example.test",
            "",
            "files.example.test/u",
        ] {
            assert!(!files.matches(no), "{no}");
        }
        let local = HostMatch::of("http://localhost:8081").unwrap();
        assert!(local.matches("localhost:8081"));
        assert!(!local.matches("localhost:8080") && !local.matches("localhost"));
    }
    #[test]
    fn the_listen_address_honours_port_only_when_the_image_opts_in() {
        let on = Some("1");
        assert_eq!(listen_address(None, None, None).unwrap(), "127.0.0.1:8080");
        // HOST1 review: a stray PORT in a developer's shell binds nothing
        // public; only the image (RUNBOOK_LISTEN_PORT_FROM_ENV=1) follows it.
        for off in [None, Some(""), Some("0"), Some("false")] {
            assert_eq!(
                listen_address(None, Some("9090"), off).unwrap(),
                "127.0.0.1:8080",
                "{off:?}"
            );
        }
        for opt_in in [on, Some("true"), Some(" 1 ")] {
            assert_eq!(
                listen_address(None, Some("9090"), opt_in).unwrap(),
                "0.0.0.0:9090"
            );
        }
        assert_eq!(
            listen_address(Some(""), Some(" 9090 "), on).unwrap(),
            "0.0.0.0:9090"
        );
        // Opted in without PORT: the image's own default.
        assert_eq!(listen_address(None, None, on).unwrap(), "0.0.0.0:8080");
        // An explicit address wins over the platform's PORT.
        assert_eq!(
            listen_address(Some("127.0.0.1:7000"), Some("9090"), on).unwrap(),
            "127.0.0.1:7000"
        );
        for bad in ["0", "65536", "http", "-1"] {
            assert_eq!(
                listen_address(None, Some(bad), on).unwrap_err().0,
                "config.invalid_port"
            );
        }
        assert_eq!(
            listen_address(None, Some("9090"), Some("yes"))
                .unwrap_err()
                .0,
            "config.invalid_listen_port_from_env"
        );
    }
    #[test]
    fn origins_compare_by_scheme_host_and_effective_port() {
        let db = DatabaseConfig::from_values(
            Some("postgres://unused:secret@localhost/test".into()),
            None,
        )
        .unwrap();
        // RUNBOOK_ORIGIN is normalized, so the CORS echo and the D50
        // comparison see what a browser sends.
        let config =
            ServerConfig::new(db, "HTTPS://Runbooks.Example:443/", "127.0.0.1:8080").unwrap();
        assert_eq!(config.origin, "https://runbooks.example");
        assert_eq!(
            normalize_origin("http://A.test:80").as_deref(),
            Some("http://a.test")
        );
        assert_eq!(
            normalize_origin("http://a.test:8080").as_deref(),
            Some("http://a.test:8080")
        );
        assert_eq!(
            normalize_origin("https://[::1]:443").as_deref(),
            Some("https://[::1]")
        );
        let main: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let (usercontent, _) = UploadConfig::usercontent(
            Some("https://FILES.example:443"),
            Some("0.0.0.0:8081"),
            main,
            "https://runbooks.example",
        )
        .unwrap();
        assert_eq!(usercontent.as_deref(), Some("https://files.example"));
        let mut uploads = UploadConfig {
            signing_key: Some(runbook_core::uploads::signing::UploadSigningKey::new(
                [3; 32],
            )),
            ..Default::default()
        };
        for same in [
            "https://runbooks.example:443",
            "HTTPS://RUNBOOKS.EXAMPLE",
            "https://runbooks.example/",
        ] {
            uploads.usercontent_origin = Some(same.into());
            assert!(!uploads.enabled(&config.origin), "{same}");
            assert!(uploads.warning(&config.origin).is_some(), "{same}");
        }
        for other in [
            "http://runbooks.example",
            "https://runbooks.example:8443",
            "https://files.example",
        ] {
            uploads.usercontent_origin = Some(other.into());
            assert!(uploads.enabled(&config.origin), "{other}");
        }
    }
    #[test]
    fn uploads_need_a_key_and_a_separate_origin_and_cap_dimensions() {
        let mut uploads = UploadConfig {
            usercontent_origin: Some("https://runbooks.example".into()),
            ..Default::default()
        };
        assert!(!uploads.enabled("https://runbooks.example"));
        assert!(
            uploads
                .warning("https://runbooks.example")
                .unwrap()
                .contains("equals")
        );
        uploads.usercontent_origin = Some("https://files.example".into());
        assert!(!uploads.enabled("https://runbooks.example"));
        assert!(
            uploads
                .warning("https://runbooks.example")
                .unwrap()
                .contains("SIGNING_KEY")
        );
        uploads.signing_key = Some(runbook_core::uploads::signing::UploadSigningKey::new(
            [3; 32],
        ));
        assert!(uploads.enabled("https://runbooks.example"));
        assert_eq!(uploads.warning("https://runbooks.example"), None);
        assert!(!format!("{uploads:?}").contains("files.example"));
        // Self-hosted has no dimension limit configured; every accepted
        // image is decoded, so the pipeline's cap is advertised.
        assert_eq!(uploads.limits.image_max_dimension, None);
        assert_eq!(uploads.image_max_dimension(), 8_000);
        assert_eq!(uploads.advertised_limits().image_max_dimension, Some(8_000));
        // UP2 fixes: the pipeline's pixel cap is advertised too.
        assert_eq!(uploads.image_max_pixels(), 4_096 * 4_096);
        assert_eq!(
            uploads.advertised_limits().image_max_pixels,
            Some(4_096 * 4_096)
        );
        uploads.limits.image_max_dimension = Some(4_096);
        assert_eq!(uploads.image_max_dimension(), 4_096);
        uploads.limits.image_max_dimension = Some(20_000);
        assert_eq!(uploads.image_max_dimension(), 8_000);
    }
    #[test]
    fn configuration_rejects_non_origins_without_echoing_urls() {
        let db = DatabaseConfig::from_values(
            Some("postgres://unused:secret@localhost/test".into()),
            Some("postgres://unused:other@localhost/direct".into()),
        )
        .unwrap();
        for origin in [
            "https://user:secret@example.test",
            "https://example.test/path",
            "https://example.test?secret=yes",
            "file:///secret",
            "not-an-origin",
        ] {
            let e = ServerConfig::new(db.clone(), origin, "127.0.0.1:8080").unwrap_err();
            assert_eq!(e.0, "config.invalid_origin");
        }
        let config = ServerConfig::new(db, "https://example.test/", "[::1]:8080").unwrap();
        assert_eq!(config.origin, "https://example.test");
        let debug = format!("{config:?}");
        assert!(
            !debug.contains("secret")
                && !debug.contains("postgres://")
                && !debug.contains("https://")
        );
    }

    #[test]
    fn blob_store_config_validation() {
        let config = runbook_store_objects::BlobStoreConfig::default();
        assert_eq!(config.kind, runbook_store_objects::BlobStoreKind::Postgres);

        let gcs = runbook_store_objects::BlobStoreConfig {
            kind: runbook_store_objects::BlobStoreKind::Gcs,
            bucket: None,
            ..Default::default()
        };
        assert_eq!(
            gcs.build_store().unwrap_err().0,
            "config.blob_bucket_required"
        );

        let s3 = runbook_store_objects::BlobStoreConfig {
            kind: runbook_store_objects::BlobStoreKind::S3,
            bucket: None,
            ..Default::default()
        };
        assert_eq!(
            s3.build_store().unwrap_err().0,
            "config.blob_bucket_required"
        );
    }
}
