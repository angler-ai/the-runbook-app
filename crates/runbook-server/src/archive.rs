//! D3 local administrative commands: `export` writes one project as a
//! portable archive directory, `import` loads one into this database. They
//! are not HTTP routes. Messages name IDs, counts and archive file names,
//! never secrets or connection details.
use runbook_core::portable::{self, Archive, BLOB_DIR, MANIFEST, PortableError};
use runbook_model::OrganizationId;
use runbook_store_postgres::{
    DatabaseConfig, PostgresStore, export_project, import_project, resolve_project,
};
use std::{fmt, fs, path::Path};

/// A failed command; the text is safe to print.
#[derive(Debug, PartialEq, Eq)]
pub struct CommandError(pub String);
impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CommandError {}
impl From<PortableError> for CommandError {
    fn from(error: PortableError) -> Self {
        Self(error.to_string())
    }
}
fn fail(code: &str) -> CommandError {
    CommandError(code.into())
}

/// Write the archive to `out`, which must be absent or an empty directory.
/// Files go to a temporary sibling folder that is renamed into place only
/// when complete, so a failure leaves no partial archive.
pub fn write_dir(archive: &Archive, out: &Path) -> Result<(), CommandError> {
    let unwritable = || fail("export.out_unwritable");
    match fs::read_dir(out) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(fail("export.out_not_empty"));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(unwritable()),
    }
    let name = out
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(unwritable)?;
    let parent = match out.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent).map_err(|_| unwritable())?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let temp = parent.join(format!(".{name}.partial-{}-{nanos}", std::process::id()));
    private_dir(&temp).map_err(|_| unwritable())?;
    let written = (|| {
        private_dir(&temp.join(BLOB_DIR.trim_end_matches('/')))?;
        for (path, bytes) in &archive.files {
            private_file(&temp.join(path), bytes)?;
        }
        // Replaces an empty `out` directory atomically, or creates it.
        fs::rename(&temp, out)
    })();
    if written.is_err() {
        let _ = fs::remove_dir_all(&temp);
        return Err(unwritable());
    }
    Ok(())
}

/// F53: archives hold a project's runs, so only the owner may read them:
/// directories 0700 and files 0600 from creation (no umask window).
fn private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}
fn private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(bytes)
}

/// Per-file and whole-archive byte caps for `import --in`.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub file_bytes: u64,
    pub total_bytes: u64,
}
pub const LIMITS: Limits = Limits {
    file_bytes: 1 << 30,
    total_bytes: 4 << 30,
};

/// Read an archive directory with the default [`LIMITS`].
pub fn read_dir(input: &Path) -> Result<Archive, CommandError> {
    read_dir_with(input, LIMITS)
}

/// Read one regular file without following links: the entry must not be a
/// link, and the opened handle must be that same file (so a swap between
/// the check and the open is refused). Reads stop past the caps.
fn read_file(
    path: &Path,
    name: &str,
    limits: Limits,
    total: &mut u64,
) -> Result<Vec<u8>, CommandError> {
    use std::io::Read;
    let unreadable = || fail("import.archive_unreadable");
    let unexpected = || CommandError(format!("archive.unexpected_file ({name})"));
    let too_large = || CommandError(format!("archive.too_large ({name})"));
    let entry = fs::symlink_metadata(path).map_err(|_| unreadable())?;
    if !entry.file_type().is_file() {
        return Err(unexpected());
    }
    let file = fs::File::open(path).map_err(|_| unreadable())?;
    let opened = file.metadata().map_err(|_| unreadable())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (entry.dev(), entry.ino()) != (opened.dev(), opened.ino()) {
            return Err(unexpected());
        }
    }
    if !opened.is_file() {
        return Err(unexpected());
    }
    let mut bytes = Vec::new();
    file.take(limits.file_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    let size = bytes.len() as u64;
    if size > limits.file_bytes {
        return Err(too_large());
    }
    *total += size;
    if *total > limits.total_bytes {
        return Err(too_large());
    }
    Ok(bytes)
}

/// Read an archive directory: regular files at the top level and in
/// `blobs/` only. Anything else (links, other folders) is rejected, as is
/// a file or archive over `limits`.
pub fn read_dir_with(input: &Path, limits: Limits) -> Result<Archive, CommandError> {
    let mut archive = Archive::default();
    let mut total = 0;
    let unreadable = || fail("import.archive_unreadable");
    for entry in fs::read_dir(input).map_err(|_| unreadable())? {
        let entry = entry.map_err(|_| unreadable())?;
        let name = entry.file_name().into_string().map_err(|_| unreadable())?;
        let kind = entry.file_type().map_err(|_| unreadable())?;
        if kind.is_file() {
            let bytes = read_file(&entry.path(), &name, limits, &mut total)?;
            archive.files.insert(name, bytes);
        } else if kind.is_dir() && format!("{name}/") == BLOB_DIR {
            for blob in fs::read_dir(entry.path()).map_err(|_| unreadable())? {
                let blob = blob.map_err(|_| unreadable())?;
                let blob_name = blob.file_name().into_string().map_err(|_| unreadable())?;
                let path = format!("{BLOB_DIR}{blob_name}");
                let bytes = read_file(&blob.path(), &path, limits, &mut total)?;
                archive.files.insert(path, bytes);
            }
        } else {
            return Err(CommandError(format!("archive.unexpected_file ({name})")));
        }
    }
    if !archive.files.contains_key(MANIFEST) {
        return Err(CommandError(format!("archive.missing_file ({MANIFEST})")));
    }
    Ok(archive)
}

async fn migrated(config: &DatabaseConfig) -> Result<(), CommandError> {
    let mut store = PostgresStore::connect(config)
        .await
        .map_err(|e| fail(e.0))?;
    store.migrate().await.map_err(|e| fail(e.0))?;
    Ok(())
}

/// `runbook-server export --org <id> --project <id|slug> --out <dir>`.
pub async fn export(
    config: &DatabaseConfig,
    org: &OrganizationId,
    project: &str,
    out: &Path,
    created_at_ms: i64,
) -> Result<String, CommandError> {
    migrated(config).await?;
    let project_id = resolve_project(config, org, project)
        .await?
        .ok_or(fail("export.project_not_found"))?;
    let scope = runbook_core::catalog::TenantScope {
        org_id: org.clone(),
        project_id,
    };
    let data = export_project(config, &scope).await?;
    let archive = portable::encode(&data, created_at_ms)?;
    write_dir(&archive, out)?;
    Ok(summary("Exported", &data, &archive))
}

/// `runbook-server import --in <dir|day.zip> [--org <id>]`. The archive is
/// read and verified before the database is opened. ARC3: a regular file is
/// a daily archive ZIP (D53); its `project/` context and run folders become
/// the same portable project and go through the same importer.
pub async fn import(
    config: &DatabaseConfig,
    input: &Path,
    org: Option<&OrganizationId>,
    options: portable::ImportOptions,
) -> Result<String, CommandError> {
    let is_file = fs::symlink_metadata(input).is_ok_and(|m| m.file_type().is_file());
    let (archive, data) = if is_file {
        let name = input
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("archive.zip")
            .to_owned();
        let mut total = 0;
        let bytes = read_file(input, &name, LIMITS, &mut total)?;
        let (_, data) = runbook_core::archives::import::to_project_data(&bytes)?;
        let mut archive = Archive::default();
        archive.files.insert(name, bytes);
        (archive, data)
    } else {
        let archive = read_dir(input)?;
        let (_, data) = portable::decode(&archive)?;
        (archive, data)
    };
    if org.is_some_and(|org| org != &data.scope.org_id) {
        return Err(fail("import.org_mismatch"));
    }
    migrated(config).await?;
    import_project(config, &data, options).await?;
    Ok(summary("Imported", &data, &archive))
}

fn summary(verb: &str, data: &portable::ProjectData, archive: &Archive) -> String {
    format!(
        "{verb} project {} of organization {}: {} runbooks, {} runs, {} blobs, {} files.",
        data.scope.project_id,
        data.scope.org_id,
        data.rows.runbooks.len(),
        data.rows.runs.len(),
        data.rows.blobs.len(),
        archive.files.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "runbook-d3-{}-{name}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }
    fn sample() -> Archive {
        let mut archive = Archive::default();
        archive.files.insert(MANIFEST.into(), b"{}\n".to_vec());
        archive
            .files
            .insert("users.ndjson".into(), b"{}\n".to_vec());
        archive
            .files
            .insert(format!("{BLOB_DIR}{}", "a".repeat(64)), vec![0, 1, 2]);
        archive
    }

    #[test]
    fn directory_round_trip_and_refusals() {
        let dir = scratch("roundtrip");
        write_dir(&sample(), &dir).unwrap();
        assert_eq!(read_dir(&dir).unwrap(), sample());
        // Never into a non-empty directory.
        assert_eq!(
            write_dir(&sample(), &dir),
            Err(fail("export.out_not_empty"))
        );
        // Unknown folders and stray nested files are rejected by name.
        fs::create_dir(dir.join("extra")).unwrap();
        assert_eq!(
            read_dir(&dir),
            Err(CommandError("archive.unexpected_file (extra)".into()))
        );
        fs::remove_dir(dir.join("extra")).unwrap();
        fs::remove_file(dir.join(MANIFEST)).unwrap();
        assert_eq!(
            read_dir(&dir),
            Err(CommandError(format!("archive.missing_file ({MANIFEST})")))
        );
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read_dir(&dir), Err(fail("import.archive_unreadable")));
    }

    /// F53: an archive holds a project's runs; only the owner may read it.
    #[cfg(unix)]
    #[test]
    fn exported_archives_are_private_to_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("private");
        write_dir(&sample(), &dir).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(BLOB_DIR.trim_end_matches('/'))), 0o700);
        for path in sample().files.keys() {
            assert_eq!(mode(&dir.join(path)), 0o600, "{path}");
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn links_are_never_followed() {
        let dir = scratch("links");
        write_dir(&sample(), &dir).unwrap();
        let outside = scratch("outside-target");
        fs::write(&outside, b"secret outside the archive").unwrap();
        fs::remove_file(dir.join("users.ndjson")).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("users.ndjson")).unwrap();
        assert_eq!(
            read_dir(&dir),
            Err(CommandError(
                "archive.unexpected_file (users.ndjson)".into()
            ))
        );
        // A file swapped for a link after the listing is still refused when
        // it is opened.
        let mut total = 0;
        assert_eq!(
            read_file(
                &dir.join("users.ndjson"),
                "users.ndjson",
                LIMITS,
                &mut total
            ),
            Err(CommandError(
                "archive.unexpected_file (users.ndjson)".into()
            ))
        );
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_file(&outside).unwrap();
    }

    #[test]
    fn oversized_files_and_archives_are_refused() {
        let dir = scratch("sizes");
        write_dir(&sample(), &dir).unwrap();
        let tight = Limits {
            file_bytes: 2,
            total_bytes: 1 << 20,
        };
        assert!(matches!(
            read_dir_with(&dir, tight),
            Err(CommandError(message)) if message.starts_with("archive.too_large (")
        ));
        let small_total = Limits {
            file_bytes: 1 << 20,
            total_bytes: 5,
        };
        assert!(matches!(
            read_dir_with(&dir, small_total),
            Err(CommandError(message)) if message.starts_with("archive.too_large")
        ));
        assert_eq!(read_dir_with(&dir, LIMITS).unwrap(), sample());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_export_leaves_no_partial_archive() {
        let parent = scratch("partial");
        fs::create_dir_all(&parent).unwrap();
        let out = parent.join("archive");
        let mut broken = sample();
        // A file whose folder does not exist: the write fails midway.
        broken.files.insert("missing/inner.ndjson".into(), vec![1]);
        assert_eq!(write_dir(&broken, &out), Err(fail("export.out_unwritable")));
        assert!(!out.exists());
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
        // A good export lands complete, with no temporary folder left.
        write_dir(&sample(), &out).unwrap();
        assert_eq!(read_dir(&out).unwrap(), sample());
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
        fs::remove_dir_all(&parent).unwrap();
    }
}
