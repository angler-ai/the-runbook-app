//! `runbook skill` commands (CLI1): `list`, `push`, `pull`, and `install`.
use crate::agent::Env;
use crate::api::{ApiClient, Request};
use crate::cliutil::{Exit, OK, api_failed_json, print_json, usage};
use clap::{Args, Subcommand, ValueEnum};
use runbook_core::skills::{
    DEFAULT_MAX_BUNDLE_BYTES, SkillError, SkillFile, build_bundle, validate_path,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

// Agent skill directories from vendor documentation (checked 2026-11-13):
// - Claude Code: https://code.claude.com/docs/en/skills
// - OpenAI Codex: https://developers.openai.com/codex/skills
// - Cursor: https://cursor.com/docs/context/skills
//
// | agent  | --scope user (default)    | --scope project (current directory's repo root) |
// | claude | ~/.claude/skills/<name>/  | .claude/skills/<name>/                          |
// | codex  | ~/.agents/skills/<name>/  | .agents/skills/<name>/                          |
// | cursor | ~/.cursor/skills/<name>/  | .cursor/skills/<name>/                          |

pub const THERUNBOOK_SKILL_MD: &str = include_str!("../../../skills/therunbook/SKILL.md");
pub const THERUNBOOK_CLI_MD: &str = include_str!("../../../skills/therunbook/references/cli.md");
pub const THERUNBOOK_AUTHORING_MD: &str =
    include_str!("../../../skills/therunbook/references/authoring.md");

pub type SkillFileData = (String, Vec<u8>);
pub type SkillWithFiles = (Value, Vec<SkillFileData>);
pub type InstallItem = (String, Value, Vec<SkillFileData>);

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Cursor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeKind {
    User,
    Project,
}

#[derive(Subcommand, Debug)]
pub enum SkillCommands {
    #[command(about = "List project skills and versions with runbook usages")]
    List(SkillListArgs),
    #[command(about = "Upload a skill folder (SKILL.md plus supporting files)")]
    Push(SkillPushArgs),
    #[command(about = "Download a skill folder from the project")]
    Pull(SkillPullArgs),
    #[command(about = "Install skills into an agent's skill directory")]
    Install(SkillInstallArgs),
}

impl SkillCommands {
    pub fn is_json(&self) -> bool {
        match self {
            SkillCommands::List(a) => a.json,
            SkillCommands::Push(a) => a.json,
            SkillCommands::Pull(a) => a.json,
            SkillCommands::Install(a) => a.json,
        }
    }

    pub fn talks_to_server(&self) -> bool {
        match self {
            SkillCommands::List(_) | SkillCommands::Push(_) | SkillCommands::Pull(_) => true,
            SkillCommands::Install(a) => !a.therunbook,
        }
    }
}

#[derive(Args, Debug)]
pub struct SkillListArgs {
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (default: \"default\")"
    )]
    pub project: Option<String>,
    #[arg(
        long,
        help = "Page limit (1-100)",
        value_parser = clap::value_parser!(u32).range(1..=100)
    )]
    pub limit: Option<u32>,
    #[arg(long, help = "Pagination cursor")]
    pub cursor: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct SkillPushArgs {
    #[arg(help = "Path to the skill directory containing SKILL.md")]
    pub dir: PathBuf,
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (default: \"default\")"
    )]
    pub project: Option<String>,
    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct SkillPullArgs {
    #[arg(help = "Skill name, optionally with @<version|hash> (e.g. 'foo' or 'foo@<hash>')")]
    pub name: String,
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (default: \"default\")"
    )]
    pub project: Option<String>,
    #[arg(long, help = "Output directory (default: ./<name>)")]
    pub out: Option<PathBuf>,
    #[arg(long, help = "Allow overwriting non-empty target directory")]
    pub force: bool,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct SkillInstallArgs {
    #[arg(
        long,
        value_enum,
        help = "Target agent: claude, codex, or cursor (default: every one of them set up in your home directory)"
    )]
    pub agent: Option<AgentKind>,
    #[arg(
        long,
        value_enum,
        default_value = "user",
        help = "Installation scope: user or project"
    )]
    pub scope: ScopeKind,
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (default: \"default\")"
    )]
    pub project: Option<String>,
    #[arg(
        long,
        help = "Specific skill name to install (default: all project skills)"
    )]
    pub skill: Option<String>,
    #[arg(
        long,
        help = "Install the bundled therunbook agent skill itself (embedded in binary)"
    )]
    pub therunbook: bool,
    #[arg(long, help = "Allow overwriting non-empty target directory")]
    pub force: bool,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

pub fn handle(env: &Env, cmd: SkillCommands) -> i32 {
    match cmd {
        SkillCommands::List(a) => list(env, a).unwrap_or_else(|e| e),
        SkillCommands::Push(a) => push(env, a).unwrap_or_else(|e| e),
        SkillCommands::Pull(a) => pull(env, a).unwrap_or_else(|e| e),
        SkillCommands::Install(a) => install(env, a).unwrap_or_else(|e| e),
    }
}

pub fn agent_skills_base_dir(agent: AgentKind, scope: ScopeKind) -> Result<PathBuf, String> {
    match scope {
        ScopeKind::User => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| "HOME environment variable not set".to_string())?;
            let rel = match agent {
                AgentKind::Claude => ".claude/skills",
                AgentKind::Codex => ".agents/skills",
                AgentKind::Cursor => ".cursor/skills",
            };
            Ok(home.join(rel))
        }
        ScopeKind::Project => {
            let repo_root = find_repo_root()?;
            let rel = match agent {
                AgentKind::Claude => ".claude/skills",
                AgentKind::Codex => ".agents/skills",
                AgentKind::Cursor => ".cursor/skills",
            };
            Ok(repo_root.join(rel))
        }
    }
}

fn find_repo_root() -> Result<PathBuf, String> {
    let current =
        std::env::current_dir().map_err(|e| format!("cannot determine current directory: {e}"))?;
    let mut cur: &Path = &current;
    loop {
        if cur.join(".git").exists() || cur.join(".runbook").exists() {
            return Ok(cur.to_path_buf());
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }
    Ok(current)
}

pub fn agent_hint(agent: AgentKind, dir_existed_before: bool) -> Option<&'static str> {
    match agent {
        AgentKind::Claude => {
            if !dir_existed_before {
                Some("hint: if the skills directory didn't exist before, run /reload-skills")
            } else {
                None
            }
        }
        AgentKind::Codex => {
            Some("hint: restart Codex if the skill doesn't appear (/skills lists them)")
        }
        AgentKind::Cursor => Some("hint: restart Cursor"),
    }
}

fn resolve_project(project_arg: Option<&str>) -> Result<String, i32> {
    let p = project_arg.unwrap_or("default");
    crate::selector::validate_project_ref(p).map_err(usage)?;
    Ok(p.to_string())
}

fn client(env: &Env) -> Result<ApiClient, i32> {
    crate::cliutil::client(env)
}

fn list(env: &Env, a: SkillListArgs) -> Exit<i32> {
    let project = resolve_project(a.project.as_deref())?;
    let mut query = Vec::new();
    if let Some(l) = a.limit {
        query.push(("limit".into(), l.to_string()));
    }
    if let Some(c) = &a.cursor {
        query.push(("cursor".into(), c.clone()));
    }
    let path = format!("/v1/projects/{project}/skills");
    let reply = client(env)?
        .send(&Request {
            method: "GET",
            path: &path,
            query: &query,
            ..Default::default()
        })
        .map_err(|e| api_failed_json(e, a.json))?;

    if a.json {
        print_json(&reply.body);
        return Ok(OK);
    }

    if let Some(items) = reply.body["items"].as_array() {
        if items.is_empty() {
            println!("No skills found in project '{project}'.");
            return Ok(OK);
        }
        for item in items {
            let name = item["name"].as_str().unwrap_or("");
            let hash = item["hash"].as_str().unwrap_or("");
            let latest = item["latest"].as_bool().unwrap_or(false);
            let desc = item["description"].as_str().unwrap_or("");
            let latest_str = if latest { " (latest)" } else { "" };
            println!("{name}  {hash}{latest_str}");
            if !desc.is_empty() {
                println!("  {desc}");
            }
            if let Some(usages) = item["usages"].as_array()
                && !usages.is_empty()
            {
                for u in usages {
                    let rb = u["runbook_id"].as_str().unwrap_or("");
                    let steps = u["step_ids"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    println!("  used by runbook {rb} (steps: {steps})");
                }
            }
        }
    }
    if let Some(next) = reply.body["next_cursor"].as_str().filter(|s| !s.is_empty()) {
        println!("next cursor: {next}");
    }
    Ok(OK)
}

pub fn validate_skill_dir(dir: &Path) -> Result<(String, String, Vec<SkillFile>), i32> {
    if !dir.is_dir() {
        return Err(usage(format!("'{}' is not a directory", dir.display())));
    }
    let folder_name = dir
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
        .or_else(|| dir.file_name().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_default();

    let skill_md_path = dir.join("SKILL.md");
    if !skill_md_path.is_file() {
        return Err(usage(format!("SKILL.md is missing in '{}'", dir.display())));
    }
    let skill_md_content = std::fs::read(&skill_md_path)
        .map_err(|e| usage(format!("failed to read '{}': {e}", skill_md_path.display())))?;

    let (name, description) = parse_frontmatter(&skill_md_content)?;

    if name != folder_name {
        return Err(usage(format!(
            "skill name '{name}' does not match folder name '{folder_name}'"
        )));
    }

    let mut files = Vec::new();
    collect_files_recursive(dir, dir, &mut files)?;

    Ok((name, description, files))
}

fn collect_files_recursive(
    root: &Path,
    current: &Path,
    out: &mut Vec<SkillFile>,
) -> Result<(), i32> {
    let entries = std::fs::read_dir(current).map_err(|e| {
        usage(format!(
            "cannot read directory '{}': {e}",
            current.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| usage(format!("directory read error: {e}")))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| usage(format!("cannot read '{}': {e}", path.display())))?;
        if file_type.is_symlink() {
            return Err(usage(format!(
                "refusing to upload symlink '{}'; copy the file into the skill folder",
                path.display()
            )));
        }
        if file_type.is_dir() {
            collect_files_recursive(root, &path, out)?;
        } else if file_type.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| usage(format!("path strip error: {e}")))?;
            let rel_str = rel
                .to_str()
                .ok_or_else(|| usage(format!("invalid UTF-8 path '{}'", rel.display())))?;
            validate_path(rel_str)
                .map_err(|e| usage(format!("invalid file path '{rel_str}': {}", e.code())))?;
            let content = std::fs::read(&path)
                .map_err(|e| usage(format!("cannot read file '{}': {e}", path.display())))?;
            out.push(SkillFile {
                path: rel_str.to_string(),
                content,
            });
        }
    }
    Ok(())
}

fn parse_frontmatter(content: &[u8]) -> Result<(String, String), i32> {
    let text =
        std::str::from_utf8(content).map_err(|_| usage("SKILL.md content is not valid UTF-8"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or_else(|| usage("SKILL.md is missing opening frontmatter delimiter '---'"))?;
    let end = rest
        .split_inclusive('\n')
        .scan(0usize, |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, line.trim_end_matches(['\n', '\r'])))
        })
        .find(|(_, line)| *line == "---")
        .map(|(start, _)| start)
        .ok_or_else(|| usage("SKILL.md is missing closing frontmatter delimiter '---'"))?;

    let docs = yaml_rust2::YamlLoader::load_from_str(&rest[..end])
        .map_err(|e| usage(format!("SKILL.md frontmatter is invalid YAML: {e}")))?;
    let map = docs
        .first()
        .and_then(|doc| doc.as_hash())
        .ok_or_else(|| usage("SKILL.md frontmatter must be a YAML mapping"))?;

    let field = |key: &str| {
        map.get(&yaml_rust2::Yaml::String(key.into()))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };

    let name = field("name").ok_or_else(|| usage("SKILL.md frontmatter lacks 'name'"))?;
    let valid_name = !name.is_empty()
        && name.chars().count() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if !valid_name {
        return Err(usage(format!(
            "skill name '{name}' is invalid: must be lowercase letters, digits, and hyphens"
        )));
    }

    let description =
        field("description").ok_or_else(|| usage("SKILL.md frontmatter lacks 'description'"))?;
    if description.trim().is_empty() || description.chars().count() > 1024 {
        return Err(usage(
            "SKILL.md description must be non-empty and at most 1024 characters",
        ));
    }

    Ok((name, description))
}

fn query_max_bundle_bytes(env: &Env) -> u64 {
    if let Ok(c) = client(env)
        && let Ok(reply) = c.send(&Request {
            method: "GET",
            path: "/v1/meta",
            ..Default::default()
        })
        && let Some(bytes) = reply.body["limits"]["skill_bundle_bytes"].as_u64()
    {
        return bytes;
    }
    DEFAULT_MAX_BUNDLE_BYTES
}

fn push(env: &Env, a: SkillPushArgs) -> Exit<i32> {
    let project = resolve_project(a.project.as_deref())?;
    let (name, _desc, files) = validate_skill_dir(&a.dir)?;

    let max_bytes = query_max_bundle_bytes(env);
    let bundle = build_bundle(&files, max_bytes).map_err(|e| match e {
        SkillError::TooLarge => usage(format!(
            "skill bundle size ({} bytes) exceeds server limit ({} bytes)",
            files.iter().map(|f| f.content.len()).sum::<usize>(),
            max_bytes
        )),
        _ => usage(format!("invalid skill bundle: {}", e.code())),
    })?;

    use base64::Engine;
    let upload_files = files
        .into_iter()
        .map(|f| {
            json!({
                "path": f.path,
                "content": base64::engine::general_purpose::STANDARD.encode(f.content),
            })
        })
        .collect::<Vec<_>>();

    let payload = json!({
        "files": upload_files,
        "reference": format!("./skills/{name}"),
    });

    let ik = crate::cliutil::idempotency_key(&a.idempotency_key)?;
    let path = format!("/v1/projects/{project}/skills");
    let reply = client(env)?
        .send(&Request {
            method: "POST",
            path: &path,
            body: Some(&payload),
            idempotency_key: Some(&ik),
            ..Default::default()
        })
        .map_err(|e| api_failed_json(e, a.json))?;

    if a.json {
        print_json(&reply.body);
        return Ok(OK);
    }

    let hash_hex = reply.body["hash"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| runbook_core::authoring::hex(&bundle.hash));
    let latest = if reply.body["latest"].as_bool() == Some(true) {
        " (latest)"
    } else {
        ""
    };
    println!("uploaded {name} {hash_hex}{latest}");
    Ok(OK)
}

pub fn parse_skill_selector(selector: &str) -> (&str, Option<&str>) {
    if let Some((name, version)) = selector.split_once('@') {
        (name, Some(version))
    } else {
        (selector, None)
    }
}

pub fn safe_write_skill_files(
    target_dir: &Path,
    files: &[(String, Vec<u8>)],
    force: bool,
) -> Result<(), i32> {
    if target_dir.exists() {
        if !target_dir.is_dir() {
            return Err(usage(format!(
                "target '{}' exists and is not a directory",
                target_dir.display()
            )));
        }
        let mut entries = std::fs::read_dir(target_dir)
            .map_err(|e| usage(format!("cannot read '{}': {e}", target_dir.display())))?;
        if entries.next().is_some() && !force {
            return Err(usage(format!(
                "target directory '{}' is not empty; use --force to overwrite",
                target_dir.display()
            )));
        }
    }

    for (rel_path, _) in files {
        validate_path(rel_path)
            .map_err(|e| usage(format!("unsafe file path '{rel_path}': {}", e.code())))?;
    }

    std::fs::create_dir_all(target_dir).map_err(|e| {
        usage(format!(
            "cannot create directory '{}': {e}",
            target_dir.display()
        ))
    })?;
    let canonical_target = target_dir.canonicalize().map_err(|e| {
        usage(format!(
            "cannot canonicalize '{}': {e}",
            target_dir.display()
        ))
    })?;

    for (rel_path, content) in files {
        let dest = target_dir.join(rel_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                usage(format!(
                    "cannot create directory '{}': {e}",
                    parent.display()
                ))
            })?;
            let canonical_parent = parent.canonicalize().map_err(|e| {
                usage(format!(
                    "cannot canonicalize directory '{}': {e}",
                    parent.display()
                ))
            })?;
            if !canonical_parent.starts_with(&canonical_target) {
                return Err(usage(format!(
                    "file path '{rel_path}' escapes target directory"
                )));
            }
        }
        if let Ok(meta) = std::fs::symlink_metadata(&dest)
            && meta.file_type().is_symlink()
        {
            return Err(usage(format!(
                "refusing to write to symlink '{}'",
                dest.display()
            )));
        }
        std::fs::write(&dest, content)
            .map_err(|e| usage(format!("failed to write '{}': {e}", dest.display())))?;
    }

    Ok(())
}

/// Every skill version in the project, following `next_cursor` to the end.
fn list_all_skills(client: &ApiClient, project: &str, json: bool) -> Result<Vec<Value>, i32> {
    let path = format!("/v1/projects/{project}/skills");
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut query = vec![("limit".to_string(), "100".to_string())];
        if let Some(c) = &cursor {
            query.push(("cursor".to_string(), c.clone()));
        }
        let reply = client
            .send(&Request {
                method: "GET",
                path: &path,
                query: &query,
                ..Default::default()
            })
            .map_err(|e| api_failed_json(e, json))?;
        let page = reply.body["items"]
            .as_array()
            .ok_or_else(|| crate::cliutil::fail("malformed server response"))?;
        items.extend(page.iter().cloned());
        match reply.body["next_cursor"].as_str().filter(|c| !c.is_empty()) {
            Some(c) => cursor = Some(c.to_string()),
            None => return Ok(items),
        }
    }
}

/// Picks one version of `name`: the latest without a version, otherwise the
/// version whose hash equals or uniquely starts with `version_spec`.
pub fn select_skill_version<'a>(
    items: &'a [Value],
    project: &str,
    name: &str,
    version_spec: Option<&str>,
) -> Result<&'a Value, i32> {
    let matching = items
        .iter()
        .filter(|i| i["name"].as_str() == Some(name))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Err(crate::cliutil::fail(format!(
            "skill '{name}' not found in project '{project}'"
        )));
    }
    match version_spec {
        None | Some("latest") => matching
            .iter()
            .find(|i| i["latest"].as_bool() == Some(true))
            .copied()
            .ok_or_else(|| {
                crate::cliutil::fail(format!(
                    "skill '{name}' has no latest version in project '{project}'"
                ))
            }),
        Some("") => Err(usage("empty version after '@'")),
        Some(spec) => {
            let spec = spec.to_ascii_lowercase();
            if let Some(exact) = matching.iter().find(|i| i["hash"].as_str() == Some(&spec)) {
                return Ok(exact);
            }
            let prefixed = matching
                .iter()
                .filter(|i| i["hash"].as_str().is_some_and(|h| h.starts_with(&spec)))
                .collect::<Vec<_>>();
            match prefixed.as_slice() {
                [one] => Ok(one),
                [] => Err(crate::cliutil::fail(format!(
                    "version '{spec}' of skill '{name}' not found in project '{project}'"
                ))),
                _ => Err(usage(format!(
                    "version '{spec}' of skill '{name}' is ambiguous; give more of the hash"
                ))),
            }
        }
    }
}

/// Downloads one version's files in a single request (`?files=true`).
fn fetch_skill_files(
    client: &ApiClient,
    project: &str,
    hash: &str,
    json: bool,
) -> Result<Vec<SkillFileData>, i32> {
    let path = format!("/v1/projects/{project}/skills/{hash}");
    let reply = client
        .send(&Request {
            method: "GET",
            path: &path,
            query: &[("files".into(), "true".into())],
            ..Default::default()
        })
        .map_err(|e| api_failed_json(e, json))?;
    use base64::Engine;
    let malformed = || crate::cliutil::fail("malformed server response");
    let mut files = Vec::new();
    for f in reply.body["files"].as_array().ok_or_else(malformed)? {
        let path = f["path"].as_str().ok_or_else(malformed)?.to_string();
        let content = f["content"].as_str().ok_or_else(|| {
            crate::cliutil::fail(format!(
                "the server sent no content for '{path}'; it may predate `runbook skill pull`"
            ))
        })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|_| malformed())?;
        files.push((path, bytes));
    }
    Ok(files)
}

fn fetch_skill_version_and_files(
    client: &ApiClient,
    project: &str,
    name: &str,
    version_spec: Option<&str>,
    json: bool,
) -> Result<SkillWithFiles, i32> {
    let items = list_all_skills(client, project, json)?;
    let selected = select_skill_version(&items, project, name, version_spec)?;
    let hash = selected["hash"]
        .as_str()
        .ok_or_else(|| crate::cliutil::fail("malformed server response"))?;
    let files = fetch_skill_files(client, project, hash, json)?;
    Ok((selected.clone(), files))
}

fn pull(env: &Env, a: SkillPullArgs) -> Exit<i32> {
    let project = resolve_project(a.project.as_deref())?;
    let (name, version_spec) = parse_skill_selector(&a.name);
    let out_dir = a.out.unwrap_or_else(|| PathBuf::from(name));

    let client = client(env)?;
    let (version_view, files) =
        fetch_skill_version_and_files(&client, &project, name, version_spec, a.json)?;

    safe_write_skill_files(&out_dir, &files, a.force)?;

    if a.json {
        let hash = version_view["hash"].as_str().unwrap_or("");
        let file_paths = files.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>();
        print_json(&json!({
            "name": name,
            "hash": hash,
            "out": out_dir.display().to_string(),
            "files": file_paths,
        }));
        return Ok(OK);
    }

    let hash = version_view["hash"].as_str().unwrap_or("");
    println!(
        "pulled {name} ({hash}) to {} ({} files)",
        out_dir.display(),
        files.len()
    );
    Ok(OK)
}

/// The bundled skill's files, as `runbook skill install --therunbook` writes them.
fn therunbook_files() -> Vec<SkillFileData> {
    vec![
        (
            "SKILL.md".to_string(),
            THERUNBOOK_SKILL_MD.as_bytes().to_vec(),
        ),
        (
            "references/cli.md".to_string(),
            THERUNBOOK_CLI_MD.as_bytes().to_vec(),
        ),
        (
            "references/authoring.md".to_string(),
            THERUNBOOK_AUTHORING_MD.as_bytes().to_vec(),
        ),
    ]
}

/// The agents whose configuration folder exists in `home`: what
/// `runbook skill install` targets when no `--agent` is given. Codex reads
/// skills from ~/.agents/skills and keeps its settings in ~/.codex.
pub fn detected_agents(home: &Path) -> Vec<AgentKind> {
    let mut agents = Vec::new();
    if home.join(".claude").is_dir() {
        agents.push(AgentKind::Claude);
    }
    if home.join(".codex").is_dir() || home.join(".agents").is_dir() {
        agents.push(AgentKind::Codex);
    }
    if home.join(".cursor").is_dir() {
        agents.push(AgentKind::Cursor);
    }
    agents
}

/// True when `dir` holds nothing but files of the bundled skill: an earlier
/// `--therunbook` install, which a new one may replace without `--force`.
pub fn is_bundled_install(dir: &Path) -> bool {
    let known: Vec<String> = therunbook_files().into_iter().map(|(p, _)| p).collect();
    fn walk(dir: &Path, base: &Path, known: &[String]) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries {
            // An entry that can't be read is not known to be ours: keep it.
            let Ok(entry) = entry else {
                return false;
            };
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                return false;
            };
            if meta.is_dir() {
                if !walk(&path, base, known) {
                    return false;
                }
            } else {
                let rel = path.strip_prefix(base).unwrap_or(&path);
                let rel = rel.to_string_lossy().replace('\\', "/");
                if !meta.is_file() || !known.contains(&rel) {
                    return false;
                }
            }
        }
        true
    }
    dir.is_dir() && walk(dir, dir, &known)
}

fn install(env: &Env, a: SkillInstallArgs) -> Exit<i32> {
    let agents = match a.agent {
        Some(agent) => vec![agent],
        None => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| usage("HOME environment variable not set"))?;
            let found = detected_agents(&home);
            if found.is_empty() {
                return Err(usage(
                    "no agent found (looked for ~/.claude, ~/.codex or ~/.agents, and ~/.cursor); \
                     name one with --agent claude, --agent codex or --agent cursor",
                ));
            }
            found
        }
    };

    let mut records = Vec::new();
    if a.therunbook {
        for agent in &agents {
            let base_dir = agent_skills_base_dir(*agent, a.scope).map_err(usage)?;
            let existed_before = base_dir.exists();
            let target_dir = base_dir.join("therunbook");
            // An earlier install of the bundled skill is replaced: that is how
            // it is updated after upgrading the CLI.
            let force = a.force || is_bundled_install(&target_dir);
            safe_write_skill_files(&target_dir, &therunbook_files(), force)?;
            records.push((
                *agent,
                "therunbook".to_string(),
                String::new(),
                target_dir,
                existed_before,
            ));
        }
    } else {
        let project = resolve_project(a.project.as_deref())?;
        let client = client(env)?;
        let skills_to_install: Vec<InstallItem> = if let Some(skill_name) = &a.skill {
            let (name, version_spec) = parse_skill_selector(skill_name);
            let (view, files) =
                fetch_skill_version_and_files(&client, &project, name, version_spec, a.json)?;
            vec![(name.to_string(), view, files)]
        } else {
            let items = list_all_skills(&client, &project, a.json)?;
            let mut out = Vec::new();
            for item in items.iter().filter(|i| i["latest"].as_bool() == Some(true)) {
                let malformed = || crate::cliutil::fail("malformed server response");
                let name = item["name"].as_str().ok_or_else(malformed)?;
                let hash = item["hash"].as_str().ok_or_else(malformed)?;
                let files = fetch_skill_files(&client, &project, hash, a.json)?;
                out.push((name.to_string(), item.clone(), files));
            }
            out
        };
        for agent in &agents {
            let base_dir = agent_skills_base_dir(*agent, a.scope).map_err(usage)?;
            let existed_before = base_dir.exists();
            for (name, view, files) in &skills_to_install {
                let target_dir = base_dir.join(name);
                safe_write_skill_files(&target_dir, files, a.force)?;
                let hash = view["hash"].as_str().unwrap_or("").to_string();
                records.push((*agent, name.clone(), hash, target_dir, existed_before));
            }
        }
    }

    if a.json {
        let installed = records
            .iter()
            .map(|(agent, name, hash, path, _)| {
                let mut v = json!({
                    "agent": agent,
                    "skill": name,
                    "installed_to": path.display().to_string(),
                });
                if !hash.is_empty() {
                    v["hash"] = json!(hash);
                }
                v
            })
            .collect::<Vec<_>>();
        print_json(&json!({ "scope": a.scope, "installed": installed }));
    } else {
        let mut hinted = Vec::new();
        for (agent, name, hash, path, existed_before) in &records {
            if hash.is_empty() {
                println!("installed {name} to {}", path.display());
            } else {
                println!("installed {name} ({hash}) to {}", path.display());
            }
            if !hinted.contains(agent) {
                hinted.push(*agent);
                if let Some(hint) = agent_hint(*agent, *existed_before) {
                    println!("{hint}");
                }
            }
        }
    }
    Ok(OK)
}
