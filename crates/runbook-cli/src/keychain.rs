//! CLI3: the OS keychain for `runbook login` tokens (PRD §10: "credentials
//! in the OS keychain, with a 0600 file fallback").
//!
//! The macOS Keychain and the Windows Credential Manager are used through
//! the `keyring` crate. Linux builds have no keychain backend (the Secret
//! Service needs D-Bus and a desktop session, which a static CLI on a
//! headless host cannot count on), so they use the 0600 file.
//!
//! `RUNBOOK_CREDENTIAL_STORE` chooses: `keychain`, `file`, or `auto` (the
//! default: the keychain where this build has one). Debug builds treat
//! `auto` as `file`, so test and development binaries never write to the
//! developer's real keychain; there, `RUNBOOK_TEST_KEYCHAIN_DIR` stands in
//! for the OS keychain with a directory of 0600 files, and
//! `RUNBOOK_TEST_KEYCHAIN_FAIL` makes its writes fail.

/// The keychain service name every login is stored under.
pub const SERVICE: &str = "therunbook-cli";

pub trait Keychain {
    fn set(&self, account: &str, secret: &str) -> Result<(), String>;
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    /// False when there was no entry.
    fn delete(&self, account: &str) -> Result<bool, String>;
}

/// The keychain account of a login: the host, or `<host>#<context>` for a
/// context's own login.
pub fn account(host: &str, context: Option<&str>) -> String {
    match context {
        Some(c) => format!("{host}#{c}"),
        None => host.to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Keychain,
    File,
}

fn mode() -> Mode {
    match std::env::var("RUNBOOK_CREDENTIAL_STORE").as_deref() {
        Ok("keychain") => Mode::Keychain,
        Ok("file") => Mode::File,
        Ok("auto") | Ok("") | Err(_) => Mode::Auto,
        Ok(other) => {
            eprintln!(
                "warning: RUNBOOK_CREDENTIAL_STORE={other:?} is not keychain, file or auto; using auto"
            );
            Mode::Auto
        }
    }
}

/// The keychain new logins go to, or `None` for the 0600 file.
pub fn for_writing() -> Option<Box<dyn Keychain>> {
    match mode() {
        Mode::File => None,
        Mode::Keychain => available(),
        Mode::Auto if cfg!(debug_assertions) => test_keychain(),
        Mode::Auto => available(),
    }
}

/// The keychain that holds logins already recorded as being in it, whatever
/// `RUNBOOK_CREDENTIAL_STORE` says about new ones.
pub fn for_reading() -> Option<Box<dyn Keychain>> {
    available()
}

/// The test directory (debug builds), else this platform's OS keychain.
fn available() -> Option<Box<dyn Keychain>> {
    test_keychain().or_else(os)
}

fn test_keychain() -> Option<Box<dyn Keychain>> {
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("RUNBOOK_TEST_KEYCHAIN_DIR").filter(|d| !d.is_empty()) {
        return Some(Box::new(test::DirKeychain(dir.into())));
    }
    None
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn os() -> Option<Box<dyn Keychain>> {
    Some(Box::new(native::OsKeychain))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn os() -> Option<Box<dyn Keychain>> {
    None
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use super::{Keychain, SERVICE};
    use keyring::{Entry, Error};

    pub struct OsKeychain;

    fn entry(account: &str) -> Result<Entry, String> {
        Entry::new(SERVICE, account).map_err(|e| e.to_string())
    }

    impl Keychain for OsKeychain {
        fn set(&self, account: &str, secret: &str) -> Result<(), String> {
            entry(account)?
                .set_password(secret)
                .map_err(|e| e.to_string())
        }
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            match entry(account)?.get_password() {
                Ok(secret) => Ok(Some(secret)),
                Err(Error::NoEntry) => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        }
        fn delete(&self, account: &str) -> Result<bool, String> {
            match entry(account)?.delete_credential() {
                Ok(()) => Ok(true),
                Err(Error::NoEntry) => Ok(false),
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

#[cfg(debug_assertions)]
mod test {
    use super::Keychain;
    use std::path::PathBuf;

    /// A directory standing in for the OS keychain in tests: one 0600 file
    /// per account, named by the account's hex.
    pub struct DirKeychain(pub PathBuf);

    impl DirKeychain {
        fn path(&self, account: &str) -> PathBuf {
            let name: String = account.bytes().map(|b| format!("{b:02x}")).collect();
            self.0.join(name)
        }
    }

    impl Keychain for DirKeychain {
        fn set(&self, account: &str, secret: &str) -> Result<(), String> {
            if std::env::var_os("RUNBOOK_TEST_KEYCHAIN_FAIL").is_some() {
                return Err("the test keychain refused the write".into());
            }
            std::fs::create_dir_all(&self.0).map_err(|e| e.to_string())?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            use std::io::Write;
            options
                .open(self.path(account))
                .and_then(|mut f| f.write_all(secret.as_bytes()))
                .map_err(|e| e.to_string())
        }
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            match std::fs::read_to_string(self.path(account)) {
                Ok(s) => Ok(Some(s)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        }
        fn delete(&self, account: &str) -> Result<bool, String> {
            match std::fs::remove_file(self.path(account)) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_separate_hosts_and_contexts() {
        assert_eq!(account("https://h.example", None), "https://h.example");
        assert_eq!(
            account("https://h.example", Some("bot")),
            "https://h.example#bot"
        );
        assert_ne!(
            account("https://h.example", Some("a")),
            account("https://h.example", Some("b"))
        );
    }
}
