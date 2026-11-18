//! CLI3: which stored login a command uses, and where its token lives.
//!
//! A context names a host and an identity, so one host can have several
//! logins. When the resolved host is the host of the named context (the
//! `--context` flag, else the current context), that context's own login is
//! used; otherwise, or when the context has none, the host's login (the
//! pre-CLI3 shape) is. A context's login is bound to the host it was stored
//! for and is never sent anywhere else.
//!
//! Tokens go to the OS keychain when one is available (`keychain.rs`), else
//! to the 0600 login files; a keychain that refuses the write falls back to
//! the file with a warning. The files always keep the record (host,
//! context, and whether the token is in the keychain), never a keychain
//! token: host logins in the file stay in `logins.json` in the pre-CLI3
//! shape, and the CLI3 records (keychain markers, context logins) are in
//! `logins.v2.json`, so an older CLI rewriting `logins.json` loses none of
//! them (`credstore.rs`). Logins stored in the file earlier keep working,
//! and `runbook login --migrate-to-keychain` moves them.
//!
//! `logout` forgets the login a command would use: the context's own, else
//! the host's login the context falls back to.
use crate::agent::Env;
use crate::credstore::{StaleKeychainHosts, StateDir, StoredLogin};
use crate::keychain::{self, Keychain};

/// The login a command uses: the host, and the context whose identity
/// applies (when its host is the resolved host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginKey {
    pub host: String,
    pub context: Option<String>,
}

impl LoginKey {
    pub fn of(env: &Env) -> Self {
        Self {
            host: env.host.clone(),
            context: env.context.clone(),
        }
    }
    fn account(&self) -> String {
        keychain::account(&self.host, self.context.as_deref())
    }
}

/// Where `save` put a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    Keychain,
    File,
}

impl Storage {
    pub fn as_str(self) -> &'static str {
        match self {
            Storage::Keychain => "keychain",
            Storage::File => "file",
        }
    }
}

fn read_err(e: std::io::Error) -> String {
    format!("cannot read stored credentials: {e}")
}

fn write_err(e: std::io::Error) -> String {
    format!("cannot store credentials: {e}")
}

/// `key`'s own record: the context's when `key` names one, else the host's.
fn own(store: &StateDir, key: &LoginKey) -> Result<Option<StoredLogin>, String> {
    match &key.context {
        Some(context) => store.context_login(context, &key.host).map_err(read_err),
        None => store.host_login(&key.host).map_err(read_err),
    }
}

/// The stored token for `key`: the context's own login, else the host's.
pub fn load(store: &StateDir, key: &LoginKey) -> Result<Option<String>, String> {
    let (found, account) = match own(store, key)? {
        Some(found) => (found, key.account()),
        None if key.context.is_some() => {
            let host = LoginKey {
                host: key.host.clone(),
                context: None,
            };
            match own(store, &host)? {
                Some(found) => (found, host.account()),
                None => return Ok(None),
            }
        }
        None => return Ok(None),
    };
    match found {
        StoredLogin::File(token) => Ok(Some(token)),
        StoredLogin::Keychain => {
            let unreadable = |why: String| {
                format!(
                    "the login for {} is in the OS keychain but cannot be read ({why}); run `runbook login` again",
                    key.host
                )
            };
            let chain = keychain::for_reading()
                .ok_or_else(|| unreadable("this build has no keychain".into()))?;
            match chain.get(&account) {
                Ok(Some(token)) => Ok(Some(token)),
                Ok(None) => Err(unreadable("the entry is missing".into())),
                Err(e) => Err(unreadable(e)),
            }
        }
    }
}

/// Whether a command through `key` finds a stored login.
pub fn is_logged_in(store: &StateDir, key: &LoginKey) -> bool {
    let host = LoginKey {
        host: key.host.clone(),
        context: None,
    };
    own(store, key).ok().flatten().is_some() || own(store, &host).ok().flatten().is_some()
}

fn save_file(store: &StateDir, key: &LoginKey, token: &str) -> Result<(), String> {
    let stale = match &key.context {
        Some(context) => store.save_context_login(context, &key.host, token),
        None => store.save_login(&key.host, token),
    }
    .map_err(write_err)?;
    delete_stale(stale);
    Ok(())
}

fn mark_keychain(store: &StateDir, key: &LoginKey) -> Result<(), String> {
    let stale = match &key.context {
        Some(context) => store.mark_context_keychain(context, &key.host),
        None => store.mark_host_keychain(&key.host),
    }
    .map_err(write_err)?;
    delete_stale(stale);
    Ok(())
}

/// Delete the keychain entries of host logins an older CLI's file login
/// shadowed; the login update has already dropped their markers
/// (re-check N5).
fn delete_stale(hosts: StaleKeychainHosts) {
    for host in hosts {
        delete_entry(&keychain::account(&host, None));
    }
}

/// Store `token` for `key` (a context's own login when `key` names one):
/// in the keychain when one is available, else in the file.
pub fn save(store: &StateDir, key: &LoginKey, token: &str) -> Result<Storage, String> {
    // A context record left for another host is replaced; its keychain
    // entry (under that host's account) would otherwise be orphaned.
    if let Some(context) = &key.context
        && let Some(record) = store.context_record(context).map_err(read_err)?
        && record.host != key.host
        && record.login == Some(StoredLogin::Keychain)
    {
        delete_entry(&keychain::account(&record.host, Some(context)));
    }
    if let Some(chain) = keychain::for_writing() {
        match chain.set(&key.account(), token) {
            Ok(()) => {
                mark_keychain(store, key)?;
                return Ok(Storage::Keychain);
            }
            Err(e) => eprintln!(
                "warning: the OS keychain refused the login ({e}); storing it in the 0600 credentials file instead"
            ),
        }
    }
    let previous = own(store, key)?;
    save_file(store, key, token)?;
    // A token stored earlier in the keychain is now stale.
    if previous == Some(StoredLogin::Keychain) {
        delete_entry(&key.account());
    }
    Ok(Storage::File)
}

/// Best-effort removal of a keychain entry; a failure only warns.
fn delete_entry(account: &str) {
    match keychain::for_reading() {
        Some(chain) => {
            if let Err(e) = chain.delete(account) {
                eprintln!("warning: could not delete the login from the OS keychain: {e}");
            }
        }
        None => eprintln!(
            "warning: this build cannot reach the OS keychain; delete the '{}' entry for {account} by hand",
            keychain::SERVICE
        ),
    }
}

/// Forget the host's own login (and its keychain entry); false when none.
/// A file login an older CLI made over a keychain login goes with the
/// keychain login it shadowed.
fn remove_host(store: &StateDir, host: &str) -> Result<bool, String> {
    let previous = store.host_login(host).map_err(read_err)?;
    let (removed, stale) = store.remove_login(host).map_err(update_err)?;
    if previous == Some(StoredLogin::Keychain) {
        delete_entry(&keychain::account(host, None));
    }
    delete_stale(stale);
    Ok(removed)
}

fn update_err(e: std::io::Error) -> String {
    format!("cannot update credentials: {e}")
}

/// Forget a context's login whatever host it was stored for (`context
/// remove`, `context add` moving it to another host, or a record left for
/// another host), with the keychain entry of that host.
pub fn remove_context(store: &StateDir, context: &str) -> Result<bool, String> {
    let Some(record) = store.context_record(context).map_err(read_err)? else {
        return Ok(false);
    };
    let (_, stale) = store.remove_context_login(context).map_err(update_err)?;
    delete_stale(stale);
    if record.login == Some(StoredLogin::Keychain) {
        delete_entry(&keychain::account(&record.host, Some(context)));
    }
    Ok(true)
}

/// Which login `logout` forgot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removed {
    /// The context's own login.
    Context,
    /// The host's login (which a context without its own login uses).
    Host,
}

/// Forget the login a command through `key` would use: the context's own
/// when it has one for this host, else the host's login it falls back to.
/// A context record left for another host is dropped too.
pub fn logout(store: &StateDir, key: &LoginKey) -> Result<Option<Removed>, String> {
    if let Some(context) = &key.context {
        let own = store
            .context_login(context, &key.host)
            .map_err(read_err)?
            .is_some();
        remove_context(store, context)?;
        if own {
            return Ok(Some(Removed::Context));
        }
    }
    Ok(remove_host(store, &key.host)?.then_some(Removed::Host))
}

/// Move every login still in the file into the keychain; returns how many
/// moved. Each moves on its own (keychain entry first, then the record), so
/// a failure part-way leaves every login usable.
pub fn migrate_to_keychain(store: &StateDir) -> Result<usize, String> {
    let chain: Box<dyn Keychain> = keychain::for_writing().ok_or_else(|| {
        "no OS keychain is available to this build (or RUNBOOK_CREDENTIAL_STORE=file); logins stay in the 0600 file".to_string()
    })?;
    let mut moved = 0;
    for (host, context, token) in store.file_logins().map_err(read_err)? {
        let key = LoginKey { host, context };
        chain
            .set(&key.account(), &token)
            .map_err(|e| format!("the OS keychain refused the login for {}: {e}", key.host))?;
        mark_keychain(store, &key)?;
        moved += 1;
    }
    Ok(moved)
}
