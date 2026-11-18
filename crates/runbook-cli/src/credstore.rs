//! Local credential and agent-loop state (PRD §10): a 0600-file store under
//! the config directory (`$XDG_CONFIG_HOME/runbook/credentials`, else
//! `~/.config/runbook/credentials`), directories 0700. Login tokens may be in
//! the OS keychain instead (CLI3, `logins.rs`); execution pairs always stay
//! here. Secrets never appear in `Debug` output.
//!
//! Layout:
//! - `executions/<exe>.json`: one execution access/refresh pair, bound to a host
//! - `executions/<exe>.lock`: advisory (flock) refresh lock; never deleted
//! - `executions/<exe>.finished`: this store's last write on the execution
//!   ended its work (a report, release, request-human or decision; removed
//!   by a new claim or a new pair). A revoked pair after that is the run
//!   ending, so `step next` says `exit` instead of failing.
//! - `active.json`: the execution whose pair `step` commands use by default
//! - `steps/<sr>.json`: the owning execution and current claim of a step run
//! - `logins.json`: host logins stored in the file by `runbook login`, in the
//!   pre-CLI3 shape (`{"hosts": {host: token}}`) and nothing else, so a CLI
//!   older than CLI3 that reads and rewrites it loses nothing.
//! - `logins.v2.json` (CLI3): the records an older CLI does not know: host
//!   logins whose token is in the OS keychain (`keychain_hosts`) and the
//!   logins of named contexts (`contexts`), each bound to the host it was
//!   stored for (`logins.rs`).
//! - `logins.lock` (CLI3): advisory (flock) lock held across each login
//!   update's read and write; never deleted.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCredential {
    pub host: String,
    pub execution_id: String,
    pub family_id: String,
    pub generation: u64,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_ms: i64,
    pub refresh_expires_at_ms: i64,
}

impl fmt::Debug for ExecutionCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutionCredential")
            .field("host", &self.host)
            .field("execution_id", &self.execution_id)
            .field("family_id", &self.family_id)
            .field("generation", &self.generation)
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("refresh_expires_at_ms", &self.refresh_expires_at_ms)
            .finish()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepState {
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    /// The `{id, generation}` claim reference returned by `step claim`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Active {
    pub host: String,
    pub execution_id: String,
}

/// `runbook login` records, as read from both files. Never printed.
#[derive(Clone, Default)]
struct Logins {
    /// Host logins whose token is in the file (`logins.json`).
    hosts: BTreeMap<String, String>,
    /// Host logins whose token is in the OS keychain (CLI3).
    keychain_hosts: BTreeSet<String>,
    /// Logins of named contexts (CLI3), so one host can have several.
    contexts: BTreeMap<String, ContextLogin>,
}

/// `logins.json`. Earlier CLI3 builds also wrote the CLI3 records here;
/// they are still read (and move to `logins.v2.json` on the next write).
#[derive(Clone, Default, Serialize, Deserialize)]
struct HostLoginsFile {
    #[serde(default)]
    hosts: BTreeMap<String, String>,
    #[serde(default, skip_serializing)]
    keychain_hosts: BTreeSet<String>,
    #[serde(default, skip_serializing)]
    contexts: BTreeMap<String, ContextLogin>,
}

/// `logins.v2.json`: the CLI3 records an older CLI does not know.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct RecordsFile {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    keychain_hosts: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    contexts: BTreeMap<String, ContextLogin>,
}

/// One context's login: the host it was stored for, and its token unless
/// the token is in the OS keychain.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ContextLogin {
    host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    keychain: bool,
}

/// Where a stored login's token is.
#[derive(Clone, PartialEq, Eq)]
pub enum StoredLogin {
    /// In the 0600 login files.
    File(String),
    /// In the OS keychain; only the record is in `logins.v2.json`.
    Keychain,
}

impl fmt::Debug for StoredLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoredLogin::File(_) => f.write_str("File([REDACTED])"),
            StoredLogin::Keychain => f.write_str("Keychain"),
        }
    }
}

/// A context's login record whatever host it was stored for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRecord {
    pub host: String,
    pub login: Option<StoredLogin>,
}

/// Every stored login, for migration: `(host, context, token)` of the ones
/// still in the file.
pub type FileLogin = (String, Option<String>, String);

/// Hosts whose keychain marker a login update dropped because a token for
/// the host is in `logins.json` (an older CLI logged in over a keychain
/// login, and the file token wins): their keychain entries are stale and
/// the caller deletes them.
pub type StaleKeychainHosts = Vec<String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedVersion {
    pub minimum_cli_version: Option<String>,
    pub checked_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct StateDir {
    root: PathBuf,
}

/// A held OS advisory lock (`flock`) on the execution's lock file. The lock
/// is released when the guard's file handle closes; the file itself is never
/// deleted, so a waiter can never remove or steal another holder's lock.
#[derive(Debug)]
pub struct LockGuard {
    _file: fs::File,
}

/// IDs become file names; accept only the ULID-shaped public ID alphabet.
fn safe_name(id: &str) -> io::Result<&str> {
    if !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Ok(id)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid identifier",
        ))
    }
}

fn private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if !path.exists() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

fn open_private(path: &Path, create_new: bool) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Write via a private temporary file and rename, so readers never see a
/// partial pair and the file is never readable by others. The temporary
/// name is unique per write (F70): two writers, or a leftover from a
/// crashed one, can never share, block or clobber it.
fn write_private(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| io::Error::other("no randomness"))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid state path"))?;
    let tmp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ));
    let mut file = open_private(&tmp, true)?;
    let written = file
        .write_all(&bytes)
        .and_then(|_| file.sync_all())
        .and_then(|_| {
            drop(file);
            fs::rename(&tmp, path)
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "corrupt state file")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

impl StateDir {
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }
    /// `<config dir>/credentials`, from the same resolution as `config.toml`.
    pub fn from_env(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<Self> {
        let config = crate::config::config_path(xdg_config_home, home)?;
        Some(Self::at(config.parent()?.join("credentials")))
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn dir(&self, sub: &str) -> io::Result<PathBuf> {
        private_dir(&self.root)?;
        let dir = self.root.join(sub);
        private_dir(&dir)?;
        Ok(dir)
    }
    fn execution_path(&self, exe: &str) -> PathBuf {
        self.root.join("executions").join(format!("{exe}.json"))
    }

    pub fn save_execution(&self, cred: &ExecutionCredential) -> io::Result<()> {
        let dir = self.dir("executions")?;
        write_private(
            &dir.join(format!("{}.json", safe_name(&cred.execution_id)?)),
            cred,
        )
    }
    pub fn load_execution(&self, exe: &str) -> io::Result<Option<ExecutionCredential>> {
        read_json(&self.execution_path(safe_name(exe)?))
    }
    pub fn remove_execution(&self, exe: &str) -> io::Result<()> {
        remove(&self.execution_path(safe_name(exe)?))?;
        match self.active()? {
            Some(active) if active.execution_id == exe => remove(&self.root.join("active.json")),
            _ => Ok(()),
        }
    }

    /// Refuse a file argument that is in this store, or is a symbolic or
    /// hard link to a file in it (F66): an agent allowed only `runbook …`
    /// commands must not read stored secrets through `--log-file`,
    /// `spec validate` or `push`. A path that does not exist passes (the
    /// read fails on its own).
    pub fn refuse_inside(&self, path: &Path) -> Result<(), String> {
        let refused = || {
            format!(
                "'{}' is in the CLI's credential store; it is never read or uploaded",
                path.display()
            )
        };
        let Ok(target) = fs::canonicalize(path) else {
            return Ok(());
        };
        if let Ok(root) = fs::canonicalize(&self.root)
            && target.starts_with(&root)
        {
            return Err(refused());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let Ok(meta) = fs::metadata(&target) else {
                return Ok(());
            };
            if meta.nlink() > 1 {
                let same = |p: &Path| {
                    fs::symlink_metadata(p)
                        .is_ok_and(|m| m.dev() == meta.dev() && m.ino() == meta.ino())
                };
                let mut dirs = vec![self.root.clone()];
                while let Some(dir) = dirs.pop() {
                    for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
                        let p = entry.path();
                        match entry.file_type() {
                            Ok(t) if t.is_dir() => dirs.push(p),
                            Ok(t) if t.is_file() && same(&p) => return Err(refused()),
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn finished_path(&self, exe: &str) -> io::Result<PathBuf> {
        Ok(self
            .root
            .join("executions")
            .join(format!("{}.finished", safe_name(exe)?)))
    }
    /// Record (or clear) that this store's last write on `exe` at `host`
    /// ended its work there.
    pub fn set_finished(&self, host: &str, exe: &str, finished: bool) -> io::Result<()> {
        if !finished {
            return remove(&self.finished_path(exe)?);
        }
        self.dir("executions")?;
        write_private(
            &self.finished_path(exe)?,
            &Active {
                host: host.into(),
                execution_id: exe.into(),
            },
        )
    }
    pub fn finished(&self, host: &str, exe: &str) -> bool {
        self.finished_path(exe)
            .and_then(|p| read_json::<Active>(&p))
            .ok()
            .flatten()
            .is_some_and(|f| f.host == host && f.execution_id == exe)
    }

    pub fn set_active(&self, host: &str, exe: &str) -> io::Result<()> {
        private_dir(&self.root)?;
        write_private(
            &self.root.join("active.json"),
            &Active {
                host: host.into(),
                execution_id: safe_name(exe)?.into(),
            },
        )
    }
    pub fn active(&self) -> io::Result<Option<Active>> {
        read_json(&self.root.join("active.json"))
    }

    pub fn save_step(&self, sr: &str, state: &StepState) -> io::Result<()> {
        let dir = self.dir("steps")?;
        write_private(&dir.join(format!("{}.json", safe_name(sr)?)), state)
    }
    pub fn load_step(&self, sr: &str) -> io::Result<Option<StepState>> {
        read_json(
            &self
                .root
                .join("steps")
                .join(format!("{}.json", safe_name(sr)?)),
        )
    }

    fn login_files(&self) -> io::Result<(HostLoginsFile, RecordsFile)> {
        Ok((
            read_json(&self.root.join("logins.json"))?.unwrap_or_default(),
            read_json(&self.root.join("logins.v2.json"))?.unwrap_or_default(),
        ))
    }
    fn logins(&self) -> io::Result<Logins> {
        let (v1, v2) = self.login_files()?;
        Ok(merge_logins(v1, v2))
    }
    /// Change the records under the login lock, so concurrent logins of
    /// this CLI never lose each other's records, and write only the files
    /// whose content changed (0600, each atomic): a change to the CLI3
    /// records alone leaves `logins.json` as it is (re-check N4). The file
    /// that gains an entry is written first, so a crash between the two
    /// writes leaves a login readable rather than lost; records an earlier
    /// CLI3 build kept in `logins.json` reach `logins.v2.json` before
    /// `logins.json` is rewritten without them (re-check N6).
    ///
    /// A host with both a token in `logins.json` and a keychain marker had
    /// an older CLI log in over a keychain login. The file token wins, so
    /// the marker is dropped here and its host returned: the caller deletes
    /// the stale keychain entry (re-check N5).
    fn update_logins<T>(
        &self,
        f: impl FnOnce(&mut Logins) -> T,
    ) -> io::Result<(T, StaleKeychainHosts)> {
        private_dir(&self.root)?;
        let _lock = self.logins_lock()?;
        let (old_v1, old_v2) = self.login_files()?;
        let legacy = !old_v1.keychain_hosts.is_empty() || !old_v1.contexts.is_empty();
        let mut logins = merge_logins(old_v1.clone(), old_v2.clone());
        let mut stale: Vec<String> = logins
            .keychain_hosts
            .iter()
            .filter(|h| logins.hosts.contains_key(*h))
            .cloned()
            .collect();
        for host in &stale {
            logins.keychain_hosts.remove(host);
        }
        let out = f(&mut logins);
        // A marker set again by `f` (a new keychain login) is not stale.
        stale.retain(|h| !logins.keychain_hosts.contains(h));
        let v1 = HostLoginsFile {
            hosts: logins.hosts,
            ..Default::default()
        };
        let v2 = RecordsFile {
            keychain_hosts: logins.keychain_hosts,
            contexts: logins.contexts,
        };
        let v1_changed = legacy || v1.hosts != old_v1.hosts;
        let v2_changed = v2 != old_v2;
        let v1_loses = legacy || old_v1.hosts.keys().any(|h| !v1.hosts.contains_key(h));
        let write_v1 = || write_private(&self.root.join("logins.json"), &v1);
        let write_v2 = || write_private(&self.root.join("logins.v2.json"), &v2);
        match (v1_changed, v2_changed) {
            (true, true) if v1_loses => {
                write_v2()?;
                crash_point()?;
                write_v1()?;
            }
            (true, true) => {
                write_v1()?;
                crash_point()?;
                write_v2()?;
            }
            (true, false) => write_v1()?,
            (false, true) => write_v2()?,
            (false, false) => {}
        }
        Ok((out, stale))
    }
    /// The login files' lock, or `None` where the filesystem has no file
    /// locks (some network mounts): login updates then run unlocked, as
    /// they did before CLI3, rather than failing.
    fn logins_lock(&self) -> io::Result<Option<LockGuard>> {
        match flock(
            &self.root.join("logins.lock"),
            Duration::from_secs(10),
            "timed out waiting for the login files' lock",
        ) {
            Ok(guard) => Ok(Some(guard)),
            Err(e) if locks_unsupported(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }
    /// Store the personal access token for `host` (0600, atomic rename).
    pub fn save_login(&self, host: &str, token: &str) -> io::Result<StaleKeychainHosts> {
        Ok(self
            .update_logins(|l| {
                l.keychain_hosts.remove(host);
                l.hosts.insert(host.into(), token.into());
            })?
            .1)
    }
    /// The host's login: its token from the file, or that it is in the
    /// keychain.
    pub fn host_login(&self, host: &str) -> io::Result<Option<StoredLogin>> {
        let logins = self.logins()?;
        Ok(match logins.hosts.get(host) {
            Some(token) => Some(StoredLogin::File(token.clone())),
            None if logins.keychain_hosts.contains(host) => Some(StoredLogin::Keychain),
            None => None,
        })
    }
    pub fn load_login(&self, host: &str) -> io::Result<Option<String>> {
        Ok(self.logins()?.hosts.get(host).cloned())
    }
    /// Record that the host's login is in the keychain (dropping any file copy).
    pub fn mark_host_keychain(&self, host: &str) -> io::Result<StaleKeychainHosts> {
        Ok(self
            .update_logins(|l| {
                l.hosts.remove(host);
                l.keychain_hosts.insert(host.into());
            })?
            .1)
    }
    /// Forget the token for `host` (in the file, the keychain marker, or
    /// both); false when none was stored.
    pub fn remove_login(&self, host: &str) -> io::Result<(bool, StaleKeychainHosts)> {
        // Nothing stored: no write, so a read-only config dir still says
        // "not logged in".
        let current = self.logins()?;
        if !current.hosts.contains_key(host) && !current.keychain_hosts.contains(host) {
            return Ok((false, Vec::new()));
        }
        self.update_logins(|l| {
            let in_file = l.hosts.remove(host).is_some();
            let marked = l.keychain_hosts.remove(host);
            in_file || marked
        })
    }

    /// Store `context`'s token for `host` in the file.
    pub fn save_context_login(
        &self,
        context: &str,
        host: &str,
        token: &str,
    ) -> io::Result<StaleKeychainHosts> {
        Ok(self
            .update_logins(|l| {
                l.contexts.insert(
                    context.into(),
                    ContextLogin {
                        host: host.into(),
                        token: Some(token.into()),
                        keychain: false,
                    },
                );
            })?
            .1)
    }
    /// Record that `context`'s login for `host` is in the keychain.
    pub fn mark_context_keychain(
        &self,
        context: &str,
        host: &str,
    ) -> io::Result<StaleKeychainHosts> {
        Ok(self
            .update_logins(|l| {
                l.contexts.insert(
                    context.into(),
                    ContextLogin {
                        host: host.into(),
                        token: None,
                        keychain: true,
                    },
                );
            })?
            .1)
    }
    /// `context`'s login, only when it was stored for `host`: a login is
    /// never sent to another host than the one it was stored for.
    pub fn context_login(&self, context: &str, host: &str) -> io::Result<Option<StoredLogin>> {
        Ok(self
            .logins()?
            .contexts
            .get(context)
            .filter(|c| c.host == host)
            .and_then(|c| match (&c.token, c.keychain) {
                (Some(token), _) => Some(StoredLogin::File(token.clone())),
                (None, true) => Some(StoredLogin::Keychain),
                (None, false) => None,
            }))
    }
    /// `context`'s login record, whatever host it was stored for.
    pub fn context_record(&self, context: &str) -> io::Result<Option<ContextRecord>> {
        Ok(self.logins()?.contexts.get(context).map(|c| ContextRecord {
            host: c.host.clone(),
            login: match (&c.token, c.keychain) {
                (Some(token), _) => Some(StoredLogin::File(token.clone())),
                (None, true) => Some(StoredLogin::Keychain),
                (None, false) => None,
            },
        }))
    }
    /// Forget `context`'s login; false when it had none.
    pub fn remove_context_login(&self, context: &str) -> io::Result<(bool, StaleKeychainHosts)> {
        self.update_logins(|l| l.contexts.remove(context).is_some())
    }
    /// Every login whose token is still in the file.
    pub fn file_logins(&self) -> io::Result<Vec<FileLogin>> {
        let logins = self.logins()?;
        let mut out: Vec<FileLogin> = logins
            .hosts
            .iter()
            .map(|(h, t)| (h.clone(), None, t.clone()))
            .collect();
        for (name, c) in &logins.contexts {
            if let Some(token) = &c.token {
                out.push((c.host.clone(), Some(name.clone()), token.clone()));
            }
        }
        Ok(out)
    }

    pub fn version_cache_path(&self) -> PathBuf {
        self.root.join("version_cache.json")
    }

    pub fn load_version_cache(&self) -> io::Result<BTreeMap<String, CachedVersion>> {
        Ok(read_json(&self.version_cache_path())?.unwrap_or_default())
    }

    pub fn save_version_cache(&self, cache: &BTreeMap<String, CachedVersion>) -> io::Result<()> {
        private_dir(&self.root)?;
        write_private(&self.version_cache_path(), cache)
    }

    /// Serialize refreshes of one execution's pair across processes with an
    /// advisory lock. The OS releases it if the holder dies, so no staleness
    /// heuristic is needed; waiting longer than `wait` is an error.
    pub fn lock(&self, exe: &str, wait: Duration) -> io::Result<LockGuard> {
        let path = self
            .dir("executions")?
            .join(format!("{}.lock", safe_name(exe)?));
        flock(
            &path,
            wait,
            "timed out waiting for the credential refresh lock",
        )
    }
}

/// The records of both login files together.
fn merge_logins(v1: HostLoginsFile, v2: RecordsFile) -> Logins {
    let mut contexts = v1.contexts;
    contexts.extend(v2.contexts);
    let mut keychain_hosts = v1.keychain_hosts;
    keychain_hosts.extend(v2.keychain_hosts);
    Logins {
        hosts: v1.hosts,
        keychain_hosts,
        contexts,
    }
}

/// Hold an advisory lock on `path` (created 0600, never deleted), waiting
/// at most `wait`.
/// Whether a lock attempt failed because the filesystem has no file locks.
fn locks_unsupported(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    if e.raw_os_error() == Some(libc::ENOLCK) {
        return true;
    }
    false
}

fn flock(path: &Path, wait: Duration, timed_out: &'static str) -> io::Result<LockGuard> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(LockGuard { _file: file }),
            Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, timed_out));
            }
            Err(fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

// Tests stop a login update between its two file writes, as a crash
// would.
#[cfg(test)]
thread_local! {
    static CRASH_BETWEEN_WRITES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn crash_point() -> io::Result<()> {
    #[cfg(test)]
    if CRASH_BETWEEN_WRITES.with(|c| c.get()) {
        return Err(io::Error::other("crashed between the two writes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, StateDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = StateDir::at(dir.path().join("credentials"));
        (dir, store)
    }

    /// Re-check 3: logging out with nothing stored writes nothing, so a
    /// read-only config dir still answers "not logged in".
    #[test]
    fn removing_a_login_that_is_not_stored_writes_nothing() {
        let (_dir, store) = store();
        assert_eq!(
            store.remove_login("https://h.example").unwrap(),
            (false, vec![])
        );
        assert!(!store.root.exists(), "the credentials dir was created");
    }

    #[test]
    fn missing_file_locks_are_not_an_error() {
        assert!(locks_unsupported(&io::Error::from(
            io::ErrorKind::Unsupported
        )));
        #[cfg(unix)]
        assert!(locks_unsupported(&io::Error::from_raw_os_error(
            libc::ENOLCK
        )));
        assert!(!locks_unsupported(&io::Error::from(
            io::ErrorKind::TimedOut
        )));
    }

    /// Re-check N4: a change to the CLI3 records alone leaves
    /// `logins.json` as it is, so a login an older CLI wrote there
    /// meanwhile is not overwritten from an earlier read.
    #[test]
    fn a_context_login_leaves_logins_json_alone() {
        let (_dir, store) = store();
        fs::create_dir_all(store.root()).unwrap();
        let v1 = store.root().join("logins.json");
        let written = br#"{"hosts":{"https://a.example":"trb_pat_a"}}"#;
        fs::write(&v1, written).unwrap();
        store
            .save_context_login("bot", "https://a.example", "trb_pat_b")
            .unwrap();
        assert_eq!(fs::read(&v1).unwrap(), written);
        store
            .mark_context_keychain("bot", "https://a.example")
            .unwrap();
        assert_eq!(fs::read(&v1).unwrap(), written);
        assert_eq!(
            store.context_login("bot", "https://a.example").unwrap(),
            Some(StoredLogin::Keychain)
        );
    }

    /// Re-check N4: concurrent logins (separate processes in practice)
    /// each keep their record.
    #[test]
    fn concurrent_logins_are_all_kept() {
        let (_dir, store) = store();
        let threads: Vec<_> = (0..12)
            .map(|n| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for round in 0..5 {
                        let host = format!("https://h{n}.example");
                        if n % 2 == 0 {
                            store.save_login(&host, &format!("t{round}")).unwrap();
                        } else {
                            store
                                .save_context_login(&format!("c{n}"), &host, &format!("t{round}"))
                                .unwrap();
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        for n in 0..12 {
            let host = format!("https://h{n}.example");
            let found = if n % 2 == 0 {
                store.host_login(&host).unwrap()
            } else {
                store.context_login(&format!("c{n}"), &host).unwrap()
            };
            assert_eq!(found, Some(StoredLogin::File("t4".into())), "{host}");
        }
    }

    /// Re-check N6: records an earlier CLI3 build kept in `logins.json`
    /// reach `logins.v2.json` before `logins.json` is rewritten without
    /// them, so a crash between the two writes loses nothing.
    #[test]
    fn moving_earlier_records_out_of_logins_json_survives_a_crash() {
        let (_dir, store) = store();
        fs::create_dir_all(store.root()).unwrap();
        fs::write(
            store.root().join("logins.json"),
            serde_json::to_vec(&serde_json::json!({
                "hosts": {"https://a.example": "trb_pat_a"},
                "keychain_hosts": ["https://k.example"],
                "contexts": {"bot": {"host": "https://a.example", "token": "trb_pat_bot"}},
            }))
            .unwrap(),
        )
        .unwrap();
        CRASH_BETWEEN_WRITES.with(|c| c.set(true));
        let crashed = store.save_login("https://b.example", "trb_pat_b");
        CRASH_BETWEEN_WRITES.with(|c| c.set(false));
        assert!(crashed.is_err());
        assert_eq!(
            store.context_login("bot", "https://a.example").unwrap(),
            Some(StoredLogin::File("trb_pat_bot".into()))
        );
        assert_eq!(
            store.host_login("https://k.example").unwrap(),
            Some(StoredLogin::Keychain)
        );
        assert_eq!(
            store.host_login("https://a.example").unwrap(),
            Some(StoredLogin::File("trb_pat_a".into()))
        );
    }

    /// Re-check N5: a host with a file token (an older CLI's login) and a
    /// keychain marker: the file token wins, and the next write drops the
    /// stale marker (`logins.rs` deletes its keychain entry).
    #[test]
    fn a_write_drops_a_keychain_marker_shadowed_by_a_file_token() {
        let (_dir, store) = store();
        store.mark_host_keychain("https://a.example").unwrap();
        // An older CLI's login: it rewrites logins.json with `hosts` only.
        fs::write(
            store.root().join("logins.json"),
            br#"{"hosts":{"https://a.example":"trb_pat_old"}}"#,
        )
        .unwrap();
        assert_eq!(
            store.host_login("https://a.example").unwrap(),
            Some(StoredLogin::File("trb_pat_old".into()))
        );
        let stale = store
            .save_context_login("bot", "https://b.example", "trb_pat_b")
            .unwrap();
        assert_eq!(stale, vec!["https://a.example".to_string()]);
        // An older CLI's logout now leaves no login behind.
        fs::write(store.root().join("logins.json"), br#"{"hosts":{}}"#).unwrap();
        assert_eq!(store.host_login("https://a.example").unwrap(), None);
    }

    /// Review (low): a login's token never shows in `Debug` output.
    #[test]
    fn stored_logins_do_not_print_their_token() {
        let shown = format!("{:?}", StoredLogin::File("trb_pat_secret".into()));
        assert!(!shown.contains("trb_pat_secret"), "{shown}");
    }
}
