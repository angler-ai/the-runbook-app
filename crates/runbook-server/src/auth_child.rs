//! AUTH2 (D43): `serve` runs the Better Auth service as a child process on a
//! loopback port and restarts it with backoff when it exits. One origin, one
//! exposed port and no second supervisor in the image.
//!
//! AUTH5: the contract is `auth/README.md` ("Contract for the Rust host");
//! the constants below are its Rust half, and `tests/auth_service.rs` runs
//! the real service against them. The child inherits only the server
//! variables in [`INHERITED_ENV`] (gate fix 7); on top of them it gets `RUNBOOK_AUTH_PORT=0`, a per-boot
//! `RUNBOOK_AUTH_INTERNAL_SECRET`, `RUNBOOK_AUTH_CAN_REGISTER_URL` and the
//! server's normalized `RUNBOOK_ORIGIN`. It binds a free loopback port
//! itself and prints `{"event":"ready","port":N,...}` on stdout once it
//! listens; the port is read again from every ready line, so nothing that
//! took a previous child's port is ever called (sign-in gate fix 2). Every
//! call to the child carries a fresh nonce ([`NONCE_HEADER`]) and its answer
//! must carry the HMAC of that nonce under the secret ([`PROOF_HEADER`]);
//! an answer without it is refused. Exit status 78 is a configuration
//! error, which no restart fixes. Sessions live in the auth service's
//! database, so a restart loses none.
use crate::{Error, config::AuthConfig};
use std::{
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// The loopback port the child listens on.
pub const PORT_ENV: &str = "RUNBOOK_AUTH_PORT";
/// The per-boot shared secret, both directions.
pub const SECRET_ENV: &str = "RUNBOOK_AUTH_INTERNAL_SECRET";
/// Where the child asks Rust whether an address may register (D45).
pub const CAN_REGISTER_URL_ENV: &str = "RUNBOOK_AUTH_CAN_REGISTER_URL";
/// The header carrying the per-boot secret, both directions.
pub const SECRET_HEADER: &str = "x-runbook-internal-secret";
/// The client address the proxy reports to the child (rate limits).
pub const CLIENT_IP_HEADER: &str = "x-runbook-client-ip";
/// Gate fix 2: a fresh random value on every call to the child.
pub const NONCE_HEADER: &str = "x-runbook-auth-nonce";
/// Gate fix 2: the child's answer, lowercase hex of
/// HMAC-SHA-256(secret, `runbook-auth-proof\n` + nonce).
pub const PROOF_HEADER: &str = "x-runbook-auth-proof";
/// The child's exit status for a bad configuration (`EX_CONFIG`).
pub const EX_CONFIG: i32 = 78;
/// RUNBOOK_AUTH_COMMAND's default, relative to the working directory (a
/// source checkout after `npm run build` in `auth/`). The image sets its own.
pub const DEFAULT_COMMAND: [&str; 2] = ["bun", "auth/dist/server.mjs"];

/// Sign-in gate fix 7: the only server variables the child inherits (the
/// D48 settings, its database, and what Bun or Node needs to run, reach TLS
/// and an outbound proxy), so no other server secret (webhook or upload
/// keys, bootstrap tokens, cloud credentials) reaches it. The contract
/// variables and `child_env` are set on top.
pub const INHERITED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TZ",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "DATABASE_URL",
    "PGHOST",
    "PGPORT",
    "PGUSER",
    "PGPASSWORD",
    "PGDATABASE",
    "PGSSLMODE",
    "PGSSLROOTCERT",
    "PGSSLCERT",
    "PGSSLKEY",
    "PGCONNECT_TIMEOUT",
    "RUNBOOK_AUTH_SECRET",
    "RUNBOOK_AUTH_METHODS",
    "RUNBOOK_AUTH_ALLOWED_DOMAINS",
    "RUNBOOK_SMTP_URL",
    "RUNBOOK_SMTP_FROM",
    "RUNBOOK_GOOGLE_CLIENT_ID",
    "RUNBOOK_GOOGLE_CLIENT_SECRET",
    "RUNBOOK_AUTH_EMAIL_TRANSPORT",
    "RUNBOOK_AUTH_CAPTURE_FILE",
    "RUNBOOK_AUTH_MIGRATIONS_DIR",
    // HOST1: the child's own Postgres pool size (small managed tiers).
    "RUNBOOK_AUTH_DATABASE_POOL_SIZE",
];

/// The server variables in [`INHERITED_ENV`], in their original order.
pub fn inherited_env(
    vars: impl Iterator<Item = (String, String)>,
) -> impl Iterator<Item = (String, String)> {
    vars.filter(|(k, _)| INHERITED_ENV.contains(&k.as_str()))
}

/// A child that stays up this long resets the backoff.
const STABLE_AFTER: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(50);

struct Inner {
    /// The port the running child's ready line named; 0 while none is up.
    port: AtomicU16,
    secret: String,
    stop: AtomicBool,
    restarts: AtomicU64,
    /// The running child printed its ready line.
    ready: AtomicBool,
    /// The child exited with EX_CONFIG; the supervisor gave up.
    config_error: AtomicBool,
    /// The sign-in methods the last ready line listed: the child resolves
    /// RUNBOOK_AUTH_METHODS and what is configured, so it is the authority.
    methods: Mutex<Option<Vec<String>>>,
    child: Mutex<Option<Child>>,
    /// Tests only (zero otherwise): a pause between spawning a child and
    /// recording it, to hold open the window a concurrent `stop()` used to
    /// miss.
    store_delay: Duration,
}

/// The supervised child. Dropping it stops the supervisor and kills the
/// child.
pub struct AuthChild {
    inner: Arc<Inner>,
    thread: Option<JoinHandle<()>>,
}

impl AuthChild {
    /// Mint the per-boot secret and start the supervisor; the child picks
    /// its own loopback port. `internal_url` is where the child reaches Rust's
    /// `/internal/*` routes; `origin` is the server's RUNBOOK_ORIGIN, passed
    /// on so both sides derive the same cookie name.
    pub fn start(config: &AuthConfig, internal_url: &str, origin: &str) -> Result<Self, Error> {
        Self::start_with(config, internal_url, origin, Duration::ZERO)
    }
    fn start_with(
        config: &AuthConfig,
        internal_url: &str,
        origin: &str,
        store_delay: Duration,
    ) -> Result<Self, Error> {
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|_| Error("server.entropy"))?;
        let secret = random.iter().map(|b| format!("{b:02x}")).collect();
        let inner = Arc::new(Inner {
            port: AtomicU16::new(0),
            secret,
            stop: AtomicBool::new(false),
            restarts: AtomicU64::new(0),
            ready: AtomicBool::new(false),
            config_error: AtomicBool::new(false),
            methods: Mutex::new(None),
            child: Mutex::new(None),
            store_delay,
        });
        let supervised = inner.clone();
        let config = config.clone();
        let env = vec![
            (
                CAN_REGISTER_URL_ENV,
                format!(
                    "{}{}",
                    internal_url.trim_end_matches('/'),
                    crate::session::CAN_REGISTER
                ),
            ),
            ("RUNBOOK_ORIGIN", origin.to_owned()),
        ];
        let thread = std::thread::Builder::new()
            .name("runbook-auth-supervisor".into())
            .spawn(move || supervise(&supervised, &config, &env))
            .map_err(|_| Error("auth.child_supervisor"))?;
        Ok(Self {
            inner,
            thread: Some(thread),
        })
    }
    /// Tests only: [`Self::start`] with a pause between spawning a child and
    /// recording it.
    #[cfg(test)]
    fn start_paused_store(
        config: &AuthConfig,
        internal_url: &str,
        origin: &str,
        delay: Duration,
    ) -> Result<Self, Error> {
        Self::start_with(config, internal_url, origin, delay)
    }
    /// The port the running child reported, once it is ready.
    pub fn port(&self) -> Option<u16> {
        Some(self.inner.port.load(Ordering::SeqCst)).filter(|p| *p != 0 && self.ready())
    }
    /// `http://127.0.0.1:<port>`, the running child's loopback address, or
    /// None while no child is ready (callers answer "unavailable").
    pub fn base_url(&self) -> Option<String> {
        self.port().map(|port| format!("http://127.0.0.1:{port}"))
    }
    /// A fresh nonce for one call ([`NONCE_HEADER`]).
    pub fn nonce(&self) -> Option<String> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).ok()?;
        Some(random.iter().map(|b| format!("{b:02x}")).collect())
    }
    /// The answer to `nonce` carries the right [`PROOF_HEADER`].
    pub fn proof_ok(&self, nonce: &str, proof: Option<&str>) -> bool {
        let Some(proof) = proof else {
            return false;
        };
        // Compare digests so the comparison leaks nothing.
        runbook_core::credentials::secret_hash(proof.trim())
            == runbook_core::credentials::secret_hash(&proof_for(&self.inner.secret, nonce))
    }
    /// The per-boot secret both sides send as [`SECRET_HEADER`].
    pub fn secret(&self) -> &str {
        &self.inner.secret
    }
    /// The running child's process ID, if one is up.
    pub fn pid(&self) -> Option<u32> {
        lock(&self.inner.child).as_ref().map(Child::id)
    }
    /// How many times the child has been started after the first.
    pub fn restarts(&self) -> u64 {
        self.inner.restarts.load(Ordering::SeqCst)
    }
    /// The running child has printed its ready line for our port.
    pub fn ready(&self) -> bool {
        self.inner.ready.load(Ordering::SeqCst)
    }
    /// The child exited with [`EX_CONFIG`] and is not restarted.
    pub fn config_error(&self) -> bool {
        self.inner.config_error.load(Ordering::SeqCst)
    }
    /// The methods the child's ready line listed, once it has printed one.
    pub fn methods(&self) -> Option<Vec<String>> {
        self.inner
            .methods
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    /// HOST1: stop for good: no restart, SIGTERM (the service closes its
    /// listener and database pool), then SIGKILL if it is still up after
    /// `grace`. Returns whether it exited on SIGTERM.
    pub async fn stop(&self, grace: Duration) -> bool {
        // The flag is set before the lock is taken: a child the supervisor
        // records after this sees it and is ended there.
        self.inner.stop.store(true, Ordering::SeqCst);
        self.inner.ready.store(false, Ordering::SeqCst);
        let Some(mut child) = lock(&self.inner.child).take() else {
            return true;
        };
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: kill(2) with our own child's PID; no memory involved.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
        let until = Instant::now() + grace;
        while Instant::now() < until {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return true;
            }
            tokio::time::sleep(POLL).await;
        }
        // Reap off the async runtime: wait(2) blocks.
        let _ = child.kill();
        let _ = tokio::task::spawn_blocking(move || child.wait()).await;
        false
    }
    /// Wait up to `timeout` for the ready line; false on timeout or a
    /// configuration error.
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        while Instant::now() < until && !self.config_error() {
            if self.ready() {
                return true;
            }
            std::thread::sleep(POLL);
        }
        self.ready()
    }
}

impl Drop for AuthChild {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        if let Some(mut child) = lock(&self.inner.child).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn lock(child: &Mutex<Option<Child>>) -> std::sync::MutexGuard<'_, Option<Child>> {
    child.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sleep in short slices so a stop request is seen promptly.
fn pause(inner: &Inner, total: Duration) {
    let until = Instant::now() + total;
    while !inner.stop.load(Ordering::SeqCst) && Instant::now() < until {
        std::thread::sleep(POLL);
    }
}

/// The proof a child computes for `nonce` ([`PROOF_HEADER`]).
pub fn proof_for(secret: &str, nonce: &str) -> String {
    runbook_core::authoring::hex(&runbook_core::webhooks::hmac_sha256(
        secret.as_bytes(),
        &[b"runbook-auth-proof\n", nonce.as_bytes()],
    ))
}

/// One stdout line: the ready signal with the port the child bound and the
/// methods it lists, or None for anything else, which is passed through.
fn ready_line(line: &str) -> Option<(u16, Vec<String>)> {
    let v = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if v.get("event")?.as_str()? != "ready" {
        return None;
    }
    let port = u16::try_from(v.get("port")?.as_u64()?)
        .ok()
        .filter(|p| *p != 0)?;
    let methods = v
        .get("methods")
        .and_then(serde_json::Value::as_array)
        .map(|m| {
            m.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some((port, methods))
}

fn watch_stdout(inner: &Arc<Inner>, stdout: impl Read + Send + 'static) {
    let inner = inner.clone();
    let _ = std::thread::Builder::new()
        .name("runbook-auth-stdout".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some((port, methods)) = ready_line(&line) {
                    *inner.methods.lock().unwrap_or_else(PoisonError::into_inner) = Some(methods);
                    inner.port.store(port, Ordering::SeqCst);
                    inner.ready.store(true, Ordering::SeqCst);
                    crate::log::info("auth", &format!("auth service ready on 127.0.0.1:{port}"));
                } else {
                    crate::log::passthrough("auth", &line, false);
                }
            }
        });
}

/// HOST1: the child's stderr, re-emitted in the server's log format.
fn watch_stderr(stderr: impl Read + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name("runbook-auth-stderr".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                crate::log::passthrough("auth", &line, true);
            }
        });
}

fn supervise(inner: &Arc<Inner>, config: &AuthConfig, env: &[(&str, String)]) {
    let mut backoff = config.restart_min;
    let mut first = true;
    let mut reported = false;
    while !inner.stop.load(Ordering::SeqCst) {
        if !first {
            inner.restarts.fetch_add(1, Ordering::SeqCst);
        }
        first = false;
        inner.ready.store(false, Ordering::SeqCst);
        inner.port.store(0, Ordering::SeqCst);
        let mut command = Command::new(&config.command[0]);
        command
            .args(&config.command[1..])
            .env_clear()
            .envs(inherited_env(std::env::vars()))
            .envs(config.child_env.iter().map(|(k, v)| (k, v)))
            .env(PORT_ENV, "0")
            .env(SECRET_ENV, &inner.secret)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started = Instant::now();
        match command.spawn() {
            Ok(mut child) => {
                if let Some(stdout) = child.stdout.take() {
                    watch_stdout(inner, stdout);
                }
                if let Some(stderr) = child.stderr.take() {
                    watch_stderr(stderr);
                }
                if !inner.store_delay.is_zero() {
                    std::thread::sleep(inner.store_delay);
                }
                // HOST1 review: record the child and check for a stop under
                // the same lock `stop()` takes it with, so a child spawned
                // as a stop began is ended here instead of left running.
                {
                    let mut slot = lock(&inner.child);
                    if inner.stop.load(Ordering::SeqCst) {
                        drop(slot);
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                    *slot = Some(child);
                }
                // Wait for exit or stop, without holding the lock while asleep.
                let status = loop {
                    if inner.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let exited = match lock(&inner.child).as_mut() {
                        Some(child) => match child.try_wait() {
                            Ok(None) => None,
                            Ok(Some(status)) => Some(status.code()),
                            Err(_) => Some(None),
                        },
                        None => Some(None),
                    };
                    if let Some(code) = exited {
                        lock(&inner.child).take();
                        break code;
                    }
                    std::thread::sleep(POLL);
                };
                inner.ready.store(false, Ordering::SeqCst);
                inner.port.store(0, Ordering::SeqCst);
                if status == Some(EX_CONFIG) {
                    inner.config_error.store(true, Ordering::SeqCst);
                    crate::log::error(
                        "auth",
                        "the auth service's configuration is invalid (see above); sign-in is unavailable until it is fixed and serve restarts",
                    );
                    return;
                }
                if started.elapsed() >= STABLE_AFTER {
                    backoff = config.restart_min;
                }
                // Never echo the command or its environment (secrets).
                crate::log::warning(
                    "auth",
                    &format!("auth service exited; restarting in {backoff:?}"),
                );
            }
            Err(_) => {
                if !reported {
                    crate::log::warning(
                        "auth",
                        "auth service did not start (RUNBOOK_AUTH_COMMAND); retrying with backoff",
                    );
                    reported = true;
                }
            }
        }
        pause(inner, backoff);
        backoff = (backoff * 2).min(config.restart_max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_child_inherits_only_the_allowlist() {
        let vars = [
            ("PATH", "/bin"),
            ("DATABASE_URL", "postgres://x"),
            ("RUNBOOK_AUTH_SECRET", "s"),
            ("RUNBOOK_SMTP_URL", "smtp://x"),
            ("RUNBOOK_GOOGLE_CLIENT_SECRET", "g"),
            ("NODE_EXTRA_CA_CERTS", "/ca.pem"),
            ("PGSSLROOTCERT", "/root.crt"),
            ("RUNBOOK_WEBHOOK_SECRET_KEY", "w"),
            ("RUNBOOK_UPLOAD_SIGNING_KEY", "u"),
            ("RUNBOOK_BOOTSTRAP_TOKEN", "b"),
            ("RUNBOOK_AUTH_PORT", "1234"),
            ("RUNBOOK_AUTH_INTERNAL_SECRET", "stale"),
            ("AWS_SECRET_ACCESS_KEY", "a"),
            ("NODE_OPTIONS", "--require /tmp/x.js"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()));
        let kept: Vec<String> = inherited_env(vars.into_iter()).map(|(k, _)| k).collect();
        assert_eq!(
            kept,
            [
                "PATH",
                "DATABASE_URL",
                "RUNBOOK_AUTH_SECRET",
                "RUNBOOK_SMTP_URL",
                "RUNBOOK_GOOGLE_CLIENT_SECRET",
                "NODE_EXTRA_CA_CERTS",
                "PGSSLROOTCERT"
            ]
        );
    }
    #[test]
    fn the_ready_line_names_the_port() {
        assert_eq!(
            ready_line(r#"{"event":"ready","port":4100,"methods":["email","google"]}"#),
            Some((4100, vec!["email".to_owned(), "google".to_owned()]))
        );
        assert_eq!(
            ready_line(r#"{"event":"ready","port":4101}"#),
            Some((4101, vec![]))
        );
        assert_eq!(ready_line(r#"{"event":"ready","port":0}"#), None);
        assert_eq!(ready_line(r#"{"event":"ready","port":70000}"#), None);
        assert_eq!(ready_line(r#"{"event":"started","port":4100}"#), None);
        assert_eq!(ready_line("ready 4100"), None);
    }
    #[test]
    fn the_port_follows_each_ready_line() {
        // The first child reports 41001 and exits; the next reports 41002.
        let dir = std::env::temp_dir().join(format!("rb-auth-child-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let counter = dir.join("n");
        let _ = std::fs::remove_file(&counter);
        let script = format!(
            r#"test "$RUNBOOK_AUTH_PORT" = 0 || exit 1
            if [ -e {c} ]; then printf '{{"event":"ready","port":41002}}\n'; exec sleep 30; fi
            touch {c}; printf '{{"event":"ready","port":41001}}\n'; sleep 0.3; exit 1"#,
            c = counter.display()
        );
        let mut config = AuthConfig::new(vec!["sh".into(), "-c".into(), script]);
        config.restart_min = Duration::from_millis(10);
        let child = AuthChild::start(&config, "http://127.0.0.1:1", "http://127.0.0.1:2").unwrap();
        assert!(child.wait_ready(Duration::from_secs(10)));
        assert_eq!(child.base_url().as_deref(), Some("http://127.0.0.1:41001"));
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until && child.port() != Some(41002) {
            std::thread::sleep(POLL);
        }
        assert_eq!(child.base_url().as_deref(), Some("http://127.0.0.1:41002"));
        assert!(child.restarts() >= 1);
        drop(child);
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn the_proof_binds_the_secret_and_the_nonce() {
        let config = AuthConfig::new(vec!["sh".into(), "-c".into(), "exit 78".into()]);
        let child = AuthChild::start(&config, "http://127.0.0.1:1", "http://127.0.0.1:2").unwrap();
        let nonce = child.nonce().unwrap();
        assert_eq!(nonce.len(), 32);
        assert_ne!(child.nonce().unwrap(), nonce);
        let proof = proof_for(child.secret(), &nonce);
        assert!(child.proof_ok(&nonce, Some(&proof)));
        assert!(!child.proof_ok(&nonce, None));
        assert!(!child.proof_ok(&nonce, Some("")));
        assert!(!child.proof_ok(&nonce, Some(&proof_for("another secret", &nonce))));
        assert!(!child.proof_ok("another nonce", Some(&proof)));
        // Known answer, shared with auth/tests (HMAC-SHA-256).
        assert_eq!(
            proof_for("s3cret", "n0nce"),
            runbook_core::authoring::hex(&runbook_core::webhooks::hmac_sha256(
                b"s3cret",
                &[b"runbook-auth-proof\nn0nce"]
            ))
        );
    }
    /// HOST1 review: a stop that lands while the supervisor has spawned a
    /// child but not yet recorded it must still end that child (it used to
    /// be left running until the handle was dropped).
    #[tokio::test]
    async fn a_stop_during_a_restart_leaves_no_child_running() {
        let dir = std::env::temp_dir().join(format!("rb-auth-race-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("pid");
        let _ = std::fs::remove_file(&pid_file);
        let script = format!("echo $$ > {}; exec sleep 30", pid_file.display());
        let config = AuthConfig::new(vec!["sh".into(), "-c".into(), script]);
        let child = AuthChild::start_paused_store(
            &config,
            "http://127.0.0.1:1",
            "http://127.0.0.1:2",
            Duration::from_millis(400),
        )
        .unwrap();
        // The child is up, and not yet recorded by the supervisor.
        let until = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
            {
                break pid;
            }
            assert!(Instant::now() < until, "child never started");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        child.stop(Duration::from_millis(200)).await;
        let alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;
        let until = Instant::now() + Duration::from_secs(3);
        while alive(pid) && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let survived = alive(pid);
        // The handle is still held: the stop alone must have ended it.
        assert!(
            !survived,
            "the child spawned during the stop is still running"
        );
        drop(child);
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn a_configuration_error_is_not_restarted() {
        let mut config = AuthConfig::new(vec!["sh".into(), "-c".into(), "exit 78".into()]);
        config.restart_min = Duration::from_millis(10);
        let child = AuthChild::start(&config, "http://127.0.0.1:1", "http://127.0.0.1:2").unwrap();
        assert!(!child.wait_ready(Duration::from_secs(5)));
        assert!(child.config_error());
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(child.restarts(), 0);
    }
    #[test]
    fn the_child_gets_the_contract_variables_and_reports_ready() {
        let script = r#"test -n "$RUNBOOK_AUTH_INTERNAL_SECRET" &&
            test "$RUNBOOK_AUTH_CAN_REGISTER_URL" = http://127.0.0.1:1/internal/can-register &&
            test "$RUNBOOK_ORIGIN" = http://127.0.0.1:2 &&
            test "$RUNBOOK_AUTH_PORT" = 0 &&
            test -n "$PATH" && test -z "$CARGO_PKG_NAME" &&
            echo hello &&
            printf '{"event":"ready","port":4100,"methods":["email"]}\n' &&
            exec sleep 30"#
            .to_owned();
        let config = AuthConfig::new(vec!["sh".into(), "-c".into(), script]);
        let child = AuthChild::start(&config, "http://127.0.0.1:1/", "http://127.0.0.1:2").unwrap();
        assert!(child.wait_ready(Duration::from_secs(10)), "no ready line");
        assert!(!child.config_error());
        assert_eq!(child.methods(), Some(vec!["email".to_owned()]));
        assert_eq!(child.base_url().as_deref(), Some("http://127.0.0.1:4100"));
        drop(child);
    }
}
