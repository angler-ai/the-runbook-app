//! A daily archive as a D3 portable project (PRD §14: "archives can be
//! imported elsewhere"). The ZIP's `project/` folder is a portable archive
//! without runs; this puts each run folder back into its run unit and hands
//! the result to the existing importer ([`portable::decode`] checks the
//! context, [`portable::validate`] the whole, and the adapters' import
//! functions write it). Archives are untrusted: every listed file must match
//! its manifest line, and the ZIP may hold nothing else.
use super::{
    build::{
        DayManifest, EVENTS, EXECUTION, LOGS_DIR, MANIFEST, PROJECT_DIR, REPORT, RunFolder,
        manifest_files,
    },
    read::parse_manifest,
    zip::Zip,
};
use crate::portable::{self, Archive, ClaimLog, PortableError, ProjectData, RunUnit};
use runbook_engine::EventEnvelope;
use runbook_model::StepRunId;
use std::collections::BTreeMap;

fn bad(code: &'static str) -> PortableError {
    PortableError::archive(code)
}

/// Decode one daily archive into the portable project it represents.
pub fn to_project_data(bytes: &[u8]) -> Result<(DayManifest, ProjectData), PortableError> {
    let zip = Zip::parse(bytes).map_err(|_| bad("archive.corrupt"))?;
    let read = |name: &str| -> Result<Vec<u8>, PortableError> {
        zip.read(name)
            .map_err(|_| bad("archive.corrupt"))?
            .ok_or(PortableError::at("archive.missing_file", name))
    };
    let manifest = parse_manifest(&read(MANIFEST)?).map_err(|_| bad("archive.invalid_manifest"))?;
    if manifest.project != PROJECT_DIR || manifest.run_count as usize != manifest.runs.len() {
        return Err(bad("archive.invalid_manifest"));
    }
    let mut seen = 1usize; // manifest.json
    let mut project = Archive::default();
    for entry in &zip.entries {
        if let Some(name) = entry.name.strip_prefix(PROJECT_DIR) {
            project.files.insert(name.to_owned(), read(&entry.name)?);
            seen += 1;
        }
    }
    let (_, mut data) = portable::decode(&project)?;
    if data.scope.org_id != manifest.org_id
        || data.scope.project_id != manifest.project_id
        || !data.rows.runs.is_empty()
    {
        return Err(bad("archive.scope_mismatch"));
    }
    for run in &manifest.runs {
        if run.folder != super::build::folder_of(&run.execution_id) {
            return Err(bad("archive.invalid_manifest"));
        }
        let mut files = BTreeMap::new();
        for file in &run.files {
            files.insert(
                file.path.clone(),
                read(&format!("{}{}", run.folder, file.path))?,
            );
        }
        seen += files.len();
        if manifest_files(&files) != run.files {
            return Err(bad("archive.digest_mismatch"));
        }
        let folder = RunFolder {
            manifest: run.clone(),
            files,
        };
        let unit = unit_of(&folder)?;
        if unit.slice.run.projection.execution_id != run.execution_id
            || unit.slice.run.projection.runbook_id != manifest.runbook_id
        {
            return Err(bad("archive.foreign_id"));
        }
        data.rows.runs.push(unit);
    }
    if seen != zip.entries.len() {
        return Err(bad("archive.unlisted_file"));
    }
    portable::normalize(&mut data);
    portable::validate(&data)?;
    Ok((manifest, data))
}

/// Rebuild the D3 run unit from a folder: `execution.json` plus its events
/// and claim logs.
pub fn unit_of(folder: &RunFolder) -> Result<RunUnit, PortableError> {
    let file = |name: &str| {
        folder
            .files
            .get(name)
            .ok_or(PortableError::at("archive.missing_file", name))
    };
    let mut unit: RunUnit =
        serde_json::from_slice(file(EXECUTION)?).map_err(|_| bad("archive.invalid_run"))?;
    if !unit.slice.run.events.is_empty() || !unit.claim_logs.is_empty() {
        return Err(bad("archive.invalid_run"));
    }
    unit.slice.run.events = lines::<EventEnvelope>(file(EVENTS)?)?;
    file(REPORT)?;
    for (path, bytes) in &folder.files {
        let Some(name) = path.strip_prefix(LOGS_DIR) else {
            if ![EVENTS, EXECUTION, REPORT].contains(&path.as_str()) {
                return Err(PortableError::at("archive.unlisted_file", path));
            }
            continue;
        };
        let step_run_id: StepRunId = name
            .strip_suffix(".ndjson")
            .and_then(|id| id.parse().ok())
            .ok_or(PortableError::at("archive.unlisted_file", path))?;
        unit.claim_logs.push(ClaimLog {
            step_run_id,
            lines: lines::<String>(bytes)?,
        });
    }
    portable::normalize_run(&mut unit);
    Ok(unit)
}

fn lines<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<Vec<T>, PortableError> {
    if bytes.is_empty() {
        return Ok(vec![]);
    }
    let text = bytes
        .strip_suffix(b"\n")
        .ok_or(bad("archive.invalid_run"))?;
    text.split(|b| *b == b'\n')
        .map(|line| serde_json::from_slice(line).map_err(|_| bad("archive.invalid_run")))
        .collect()
}
