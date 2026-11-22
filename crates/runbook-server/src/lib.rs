//! Native administrative shell. No listener or network bootstrap endpoint.
pub mod archive;
pub mod auth_child;
pub mod auth_proxy;
pub mod backends;
pub mod bootstrap;
pub mod client_ip;
pub mod config;
pub mod daily_archives;
pub mod device;
pub mod drain;
pub mod email;
pub mod executions;
pub mod http;
pub mod images;
pub mod lifecycle;
pub mod log;
mod mcp;
pub mod owner_recovery;
pub mod runner_proxy;
pub mod session;
pub mod signing;
mod slack_http;
pub mod web_assets;
pub mod webhooks;
use std::fmt;
#[derive(Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
/// `bootstrap [--owner-email <e> [--owner-name <n>] [--with-token]]`.
/// Without an email it is the headless bootstrap, which always prints a PAT
/// and a service token; with one (AUTH5) the owner signs in by email and
/// tokens are printed only with `--with-token`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BootstrapArgs {
    pub owner_email: Option<String>,
    pub owner_name: Option<String>,
    pub with_token: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Bootstrap(BootstrapArgs),
    Doctor,
    /// UPD1: apply pending migrations and exit (a Kubernetes Job).
    Migrate,
    /// One-off: copy Postgres-held blobs to the configured object store.
    CopyBlobs,
    Serve,
    /// UPD1: `--version`: the release and the schema this code migrates to.
    Version,
    ResetMfa(String),
    AssignOwner {
        org: runbook_model::OrganizationId,
        user: runbook_model::UserId,
    },
    /// D3: write one project as a portable archive directory.
    Export {
        org: runbook_model::OrganizationId,
        /// Project ID or slug within the organization.
        project: String,
        out: std::path::PathBuf,
    },
    /// D3: import a portable archive directory into an empty project.
    Import {
        input: std::path::PathBuf,
        /// When given, the archive must belong to this organization.
        org: Option<runbook_model::OrganizationId>,
        /// Allow an organization that already exists on this instance.
        into_existing_org: bool,
    },
}
const USAGE: &str = "usage: runbook-server <bootstrap|doctor|migrate|copy-blobs|serve|admin|assign-owner|export|import|--version>";
/// UPD1: what `runbook-server --version` prints: the release (the workspace
/// version, which `/v1/meta` also reports), the Postgres schema this code
/// migrates to, and its rollback floor (the oldest release whose code is
/// safe on that schema; `none` when any release since UPD1 is). The
/// update script reads all three.
pub fn version_line() -> String {
    format!(
        "runbook-server {} (schema postgres {}; rollback floor {})",
        runbook_api::VERSION,
        runbook_store_postgres::SCHEMA_VERSION,
        rollback_floor().unwrap_or("none")
    )
}
/// UPD1 review #2: the rollback floor baked in by build.rs.
pub fn rollback_floor() -> Option<&'static str> {
    Some(env!("RUNBOOK_ROLLBACK_FLOOR")).filter(|f| !f.is_empty())
}
#[cfg(test)]
mod floor {
    include!("rollback_floor.rs");
}
const BOOTSTRAP_USAGE: &str =
    "usage: bootstrap [--owner-email <email> [--owner-name <name>] [--with-token]]";
const EXPORT_USAGE: &str = "usage: export --org <id> --project <id|slug> --out <dir>";
const IMPORT_USAGE: &str = "usage: import --in <dir|day.zip> [--org <id>] [--into-existing-org]";
/// Parse the arguments following `admin` with clap's help and diagnostics.
pub fn parse_admin_command(args: impl IntoIterator<Item = String>) -> Result<Command, clap::Error> {
    use clap::{Arg, Command as Cli, builder::ValueParser};

    let matches = Cli::new("admin")
        .bin_name("runbook-server admin")
        .no_binary_name(true)
        .subcommand_required(true)
        .subcommand(
            Cli::new("mfa").subcommand_required(true).subcommand(
                Cli::new("reset")
                    .about("Reset multi-factor authentication for a user")
                    .arg(
                        Arg::new("email")
                            .value_name("EMAIL")
                            .required(true)
                            .value_parser(ValueParser::new(parse_mfa_email)),
                    ),
            ),
        )
        .try_get_matches_from(args)?;
    let email = matches
        .subcommand_matches("mfa")
        .and_then(|mfa| mfa.subcommand_matches("reset"))
        .and_then(|reset| reset.get_one::<String>("email"))
        .expect("clap requires mfa reset and an email");
    Ok(Command::ResetMfa(email.clone()))
}

fn parse_mfa_email(value: &str) -> Result<String, &'static str> {
    use email_address::{EmailAddress, Options};

    let email =
        runbook_core::directory::email_key(value).map_err(|_| "expected an email address")?;
    EmailAddress::parse_with_options(&email, Options::default().without_display_text())
        .map_err(|_| "expected an email address")?;
    Ok(email)
}

pub fn parse_command(args: impl IntoIterator<Item = String>) -> Result<Command, Error> {
    let mut args = args.into_iter();
    let cmd = match args.next().as_deref() {
        Some("bootstrap") => {
            let mut parsed = BootstrapArgs::default();
            while let Some(flag) = args.next() {
                let mut value = || {
                    args.next()
                        .filter(|v| !v.trim().is_empty())
                        .ok_or(Error(BOOTSTRAP_USAGE))
                };
                match flag.as_str() {
                    "--with-token" if !parsed.with_token => parsed.with_token = true,
                    "--owner-email" if parsed.owner_email.is_none() => {
                        parsed.owner_email = Some(value()?)
                    }
                    "--owner-name" if parsed.owner_name.is_none() => {
                        parsed.owner_name = Some(value()?)
                    }
                    _ => return Err(Error(BOOTSTRAP_USAGE)),
                }
            }
            if parsed.owner_email.is_none() && (parsed.with_token || parsed.owner_name.is_some()) {
                return Err(Error(BOOTSTRAP_USAGE));
            }
            Command::Bootstrap(parsed)
        }
        Some("doctor") => Command::Doctor,
        Some("migrate") => Command::Migrate,
        Some("copy-blobs") => Command::CopyBlobs,
        Some("--version" | "-V" | "version") => Command::Version,
        Some("serve") => Command::Serve,
        Some("admin") => {
            return parse_admin_command(args).map_err(|_| Error("invalid admin command"));
        }
        Some("assign-owner") => {
            let mut org = None;
            let mut user = None;
            while let Some(flag) = args.next() {
                let value = args
                    .next()
                    .ok_or(Error("usage: assign-owner --org <id> --user <id>"))?;
                match flag.as_str() {
                    "--org" if org.is_none() => {
                        org = Some(
                            value
                                .parse()
                                .map_err(|_| Error("owner_recovery.invalid_org_id"))?,
                        )
                    }
                    "--user" if user.is_none() => {
                        user = Some(
                            value
                                .parse()
                                .map_err(|_| Error("owner_recovery.invalid_user_id"))?,
                        )
                    }
                    _ => return Err(Error("usage: assign-owner --org <id> --user <id>")),
                }
            }
            Command::AssignOwner {
                org: org.ok_or(Error("owner_recovery.org_required"))?,
                user: user.ok_or(Error("owner_recovery.user_required"))?,
            }
        }
        Some("export") => {
            let (mut org, mut project, mut out) = (None, None, None);
            while let Some(flag) = args.next() {
                let value = args.next().ok_or(Error(EXPORT_USAGE))?;
                match flag.as_str() {
                    "--org" if org.is_none() => {
                        org = Some(value.parse().map_err(|_| Error("export.invalid_org_id"))?)
                    }
                    "--project" if project.is_none() && !value.is_empty() => project = Some(value),
                    "--out" if out.is_none() && !value.is_empty() => out = Some(value.into()),
                    _ => return Err(Error(EXPORT_USAGE)),
                }
            }
            Command::Export {
                org: org.ok_or(Error("export.org_required"))?,
                project: project.ok_or(Error("export.project_required"))?,
                out: out.ok_or(Error("export.out_required"))?,
            }
        }
        Some("import") => {
            let (mut input, mut org, mut into_existing_org) = (None, None, false);
            while let Some(flag) = args.next() {
                if flag == "--into-existing-org" && !into_existing_org {
                    into_existing_org = true;
                    continue;
                }
                let value = args.next().ok_or(Error(IMPORT_USAGE))?;
                match flag.as_str() {
                    "--in" if input.is_none() && !value.is_empty() => input = Some(value.into()),
                    "--org" if org.is_none() => {
                        org = Some(value.parse().map_err(|_| Error("import.invalid_org_id"))?)
                    }
                    _ => return Err(Error(IMPORT_USAGE)),
                }
            }
            Command::Import {
                input: input.ok_or(Error("import.in_required"))?,
                org,
                into_existing_org,
            }
        }
        _ => return Err(Error(USAGE)),
    };
    if args.next().is_some() {
        return Err(Error(USAGE));
    }
    Ok(cmd)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mfa_reset_normalizes_a_valid_email_address() {
        let args = ["admin", "mfa", "reset", "Member+Ops@Example.Test"].map(str::to_owned);
        assert_eq!(
            parse_command(args),
            Ok(Command::ResetMfa("member+ops@example.test".into()))
        );
    }

    #[test]
    fn version_prints_the_release_and_schema() {
        for flag in ["--version", "-V", "version"] {
            assert_eq!(parse_command([flag.to_owned()]), Ok(Command::Version));
        }
        assert_eq!(
            version_line(),
            format!(
                "runbook-server {} (schema postgres {}; rollback floor {})",
                env!("CARGO_PKG_VERSION"),
                runbook_store_postgres::SCHEMA_VERSION,
                rollback_floor().unwrap_or("none")
            )
        );
    }
    #[test]
    fn the_rollback_floor_is_the_newest_unused_since_or_the_metadata() {
        use super::floor::rollback_floor as floor;
        let a = "-- contract(unused-since 0.2.0): a\nDROP TABLE runbook.a;\n";
        let b = "-- x\n-- contract(unused-since 0.10.1): b\nDROP TABLE runbook.b;\n";
        let none = "CREATE TABLE runbook.c (x INT);\n-- contract(unused-since soon): bad\n";
        assert_eq!(floor([none], ""), None);
        assert_eq!(floor([a, b, none], "").as_deref(), Some("0.10.1"));
        let meta = "[workspace.package]\nversion = \"0.11.0\"\n[workspace.metadata.runbook]\nrollback-floor = \"0.10.5\"\n";
        assert_eq!(floor([a, b], meta).as_deref(), Some("0.10.5"));
        let other = "[package.metadata.runbook]\nrollback-floor = \"9.9.9\"\n";
        assert_eq!(floor([a], other).as_deref(), Some("0.2.0"));
        // This checkout: no contract migration yet.
        assert_eq!(rollback_floor(), None);
    }
    /// UPD1 re-check #5: the floor parsers agree on one fixture (the JS
    /// migration check and update.mjs read the same file).
    #[test]
    fn the_floor_parsers_agree_on_the_shared_fixture() {
        use super::floor::rollback_floor as floor;
        let sql = include_str!("../../../scripts/tests/fixtures/floor-annotations.sql");
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../../scripts/tests/fixtures/floor-annotations.json"
        ))
        .unwrap();
        assert_eq!(floor([sql], "").as_deref(), expected["floor"].as_str());
        let manual = "[workspace.metadata.runbook]\nrollback-floor = \"0.4.0\"\n";
        assert_eq!(floor([sql], manual).as_deref(), Some("0.4.0"));
        let indented = "[workspace.metadata.runbook]\n  rollback-floor = \"9.0.0\"\n";
        assert_eq!(
            floor([sql], indented).as_deref(),
            expected["floor"].as_str()
        );
    }
    #[test]
    fn assign_owner_requires_explicit_valid_local_arguments() {
        let org = format!("org_{:026}", 1);
        let user = format!("usr_{:026}", 2);
        for args in [
            vec!["assign-owner", "--org", &org, "--user", &user],
            vec!["assign-owner", "--user", &user, "--org", &org],
        ] {
            assert_eq!(
                parse_command(args.into_iter().map(str::to_owned)),
                Ok(Command::AssignOwner {
                    org: org.parse().unwrap(),
                    user: user.parse().unwrap(),
                })
            );
        }
        for args in [
            vec!["assign-owner"],
            vec!["assign-owner", "--org", &org],
            vec!["assign-owner", "--org"],
            vec![
                "assign-owner",
                "--org",
                &org,
                "--org",
                &org,
                "--user",
                &user,
            ],
            vec![
                "assign-owner",
                "--org",
                "sensitive-invalid",
                "--user",
                &user,
            ],
            vec!["assign-owner", "--org", &org, "--user", "sensitive-invalid"],
            vec![
                "assign-owner",
                "--org",
                &org,
                "--user",
                &user,
                "--unknown",
                "sensitive-invalid",
            ],
        ] {
            let error = parse_command(args.into_iter().map(str::to_owned)).unwrap_err();
            assert!(!error.0.contains("sensitive-invalid"));
        }
        assert!(
            !runbook_contract::openapi()["paths"]
                .as_object()
                .unwrap()
                .keys()
                .any(|path| path.contains("assign-owner"))
        );
    }
    #[test]
    fn export_and_import_take_explicit_local_arguments() {
        let org = format!("org_{:026}", 1);
        let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            parse_command(args(&[
                "export",
                "--org",
                &org,
                "--project",
                "api",
                "--out",
                "/tmp/a"
            ])),
            Ok(Command::Export {
                org: org.parse().unwrap(),
                project: "api".into(),
                out: "/tmp/a".into(),
            })
        );
        assert_eq!(
            parse_command(args(&["import", "--in", "/tmp/a"])),
            Ok(Command::Import {
                input: "/tmp/a".into(),
                org: None,
                into_existing_org: false,
            })
        );
        assert_eq!(
            parse_command(args(&["import", "--into-existing-org", "--in", "/tmp/a"])),
            Ok(Command::Import {
                input: "/tmp/a".into(),
                org: None,
                into_existing_org: true,
            })
        );
        assert_eq!(
            parse_command(args(&["import", "--org", &org, "--in", "/tmp/a"])),
            Ok(Command::Import {
                input: "/tmp/a".into(),
                org: Some(org.parse().unwrap()),
                into_existing_org: false,
            })
        );
        for (list, code) in [
            (
                vec!["export", "--org", &org, "--project", "api"],
                "export.out_required",
            ),
            (
                vec!["export", "--project", "api", "--out", "x"],
                "export.org_required",
            ),
            (
                vec!["export", "--org", "sensitive-invalid"],
                "export.invalid_org_id",
            ),
            (vec!["export", "--org", &org, "--force", "x"], EXPORT_USAGE),
            (vec!["import"], "import.in_required"),
            (vec!["import", "--in", "x", "--in", "y"], IMPORT_USAGE),
            (
                vec!["import", "--in", "x", "--org", "sensitive-invalid"],
                "import.invalid_org_id",
            ),
            (vec!["import", "--in"], IMPORT_USAGE),
            (
                vec![
                    "import",
                    "--in",
                    "x",
                    "--into-existing-org",
                    "--into-existing-org",
                ],
                IMPORT_USAGE,
            ),
        ] {
            assert_eq!(parse_command(args(&list)), Err(Error(code)));
        }
        assert!(
            !runbook_contract::openapi()["paths"]
                .as_object()
                .unwrap()
                .keys()
                .any(|path| path.contains("export") || path.contains("import"))
        );
    }
    #[test]
    fn shell_accepts_only_local_administrative_commands() {
        assert_eq!(
            parse_command(["bootstrap".into()]),
            Ok(Command::Bootstrap(BootstrapArgs::default()))
        );
        assert_eq!(parse_command(["doctor".into()]), Ok(Command::Doctor));
        assert_eq!(parse_command(["migrate".into()]), Ok(Command::Migrate));
        assert_eq!(parse_command(["copy-blobs".into()]), Ok(Command::CopyBlobs));
        for args in [
            vec![],
            vec!["unknown".into()],
            vec!["bootstrap".into(), "secret".into()],
            vec!["migrate".into(), "secret".into()],
            vec!["copy-blobs".into(), "secret".into()],
        ] {
            let error = parse_command(args).unwrap_err();
            assert!(!error.0.contains("secret"));
        }
    }
    #[test]
    fn bootstrap_by_email_takes_a_name_and_prints_tokens_only_when_asked() {
        let args = |list: &[&str]| parse_command(list.iter().map(|a| (*a).to_owned()));
        assert_eq!(
            args(&["bootstrap", "--owner-email", "ada@example.test"]),
            Ok(Command::Bootstrap(BootstrapArgs {
                owner_email: Some("ada@example.test".into()),
                owner_name: None,
                with_token: false,
            }))
        );
        assert_eq!(
            args(&[
                "bootstrap",
                "--with-token",
                "--owner-name",
                "Ada Lovelace",
                "--owner-email",
                "ada@example.test",
            ]),
            Ok(Command::Bootstrap(BootstrapArgs {
                owner_email: Some("ada@example.test".into()),
                owner_name: Some("Ada Lovelace".into()),
                with_token: true,
            }))
        );
        for bad in [
            &["bootstrap", "--owner-email"][..],
            &["bootstrap", "--owner-email", ""],
            &[
                "bootstrap",
                "--owner-email",
                "a@example.test",
                "--owner-email",
                "b@example.test",
            ],
            &[
                "bootstrap",
                "--owner-email",
                "a@example.test",
                "--owner-name",
                " ",
            ],
            &[
                "bootstrap",
                "--owner-email",
                "a@example.test",
                "--with-token",
                "--with-token",
            ],
            // The headless bootstrap always prints its tokens; a name needs an email.
            &["bootstrap", "--with-token"],
            &["bootstrap", "--owner-name", "Ada"],
            &["bootstrap", "--owner", "a@example.test"],
        ] {
            assert!(args(bad).is_err(), "{bad:?}");
        }
    }
}

mod run_search;

mod event_stream;
