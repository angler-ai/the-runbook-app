//! Skill bundles (PRD §7, D19): path rules, the manifest and the folder hash.
//! Pure and portable; storage and resolution live in the adapters (B3.2b).
//!
//! The hash is SHA-256 over the manifest text. Entries are sorted by path bytes
//! and each is `path NUL size NUL sha256-hex LF`, where `size` is the decimal
//! byte count and `sha256-hex` is the lowercase hex digest of the file bytes.
//! Every client (server, CLI, Workers) must produce exactly these bytes.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Longest allowed relative path, in bytes.
pub const MAX_PATH_BYTES: usize = 255;
/// Cloud bundle limit; self-hosted operators may configure their own (D19).
pub const DEFAULT_MAX_BUNDLE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_NAME_CHARS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 1024;

/// One uploaded file. Clients resolve links before upload; there is no link type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillFile {
    pub path: String,
    pub content: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Every file in the bundle, sorted by path bytes. Reads are limited to these paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillManifest {
    pub entries: Vec<ManifestEntry>,
}

/// The validated bundle: its identity, manifest and SKILL.md front matter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillBundle {
    pub hash: [u8; 32],
    pub manifest: SkillManifest,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillError {
    /// The path breaks a D19 rule; the message names the rule, not the content.
    InvalidPath(&'static str),
    DuplicatePath,
    MissingSkillFile,
    InvalidFrontMatter(&'static str),
    TooLarge,
}
impl SkillError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidPath(_) => "skill.invalid_path",
            Self::DuplicatePath => "skill.duplicate_path",
            Self::MissingSkillFile => "skill.missing_skill_md",
            Self::InvalidFrontMatter(_) => "skill.invalid_front_matter",
            Self::TooLarge => "skill.too_large",
        }
    }
}

/// D19: relative, normalized UTF-8, at most 255 bytes, no `..`, no absolute paths.
pub fn validate_path(path: &str) -> Result<(), SkillError> {
    let invalid = SkillError::InvalidPath;
    if path.is_empty() {
        return Err(invalid("empty"));
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(invalid("too_long"));
    }
    if path.starts_with('/') {
        return Err(invalid("absolute"));
    }
    if path.contains('\\') {
        return Err(invalid("backslash"));
    }
    if path.chars().any(char::is_control) {
        return Err(invalid("control_character"));
    }
    for part in path.split('/') {
        match part {
            "" => return Err(invalid("empty_component")),
            "." | ".." => return Err(invalid("dot_component")),
            _ => {}
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

impl SkillManifest {
    /// The exact bytes the folder hash covers.
    pub fn framing(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in &self.entries {
            out.extend_from_slice(entry.path.as_bytes());
            out.push(0);
            out.extend_from_slice(entry.size.to_string().as_bytes());
            out.push(0);
            out.extend_from_slice(entry.sha256.as_bytes());
            out.push(b'\n');
        }
        out
    }
    pub fn hash(&self) -> [u8; 32] {
        Sha256::digest(self.framing()).into()
    }
    /// True only for a path listed in the manifest; never a filesystem lookup.
    pub fn contains(&self, path: &str) -> bool {
        self.entries
            .binary_search_by(|entry| entry.path.as_bytes().cmp(path.as_bytes()))
            .is_ok()
    }
}

/// Validate every path, require SKILL.md with a name and description, enforce the
/// size limit, and compute the folder hash. Upload order never changes the result.
pub fn build_bundle(files: &[SkillFile], max_bytes: u64) -> Result<SkillBundle, SkillError> {
    let mut sorted: BTreeMap<&[u8], &SkillFile> = BTreeMap::new();
    let mut total: u64 = 0;
    for file in files {
        validate_path(&file.path)?;
        total = total
            .checked_add(file.content.len() as u64)
            .ok_or(SkillError::TooLarge)?;
        if total > max_bytes {
            return Err(SkillError::TooLarge);
        }
        if sorted.insert(file.path.as_bytes(), file).is_some() {
            return Err(SkillError::DuplicatePath);
        }
    }
    let skill_md = sorted
        .get("SKILL.md".as_bytes())
        .ok_or(SkillError::MissingSkillFile)?;
    let (name, description) = front_matter(&skill_md.content)?;
    let manifest = SkillManifest {
        entries: sorted
            .values()
            .map(|file| ManifestEntry {
                path: file.path.clone(),
                size: file.content.len() as u64,
                sha256: hex(&Sha256::digest(&file.content)),
            })
            .collect(),
    };
    Ok(SkillBundle {
        hash: manifest.hash(),
        manifest,
        name,
        description,
    })
}

/// SKILL.md starts with `---`, then YAML, then a closing `---` line. The Agent
/// Skills standard requires `name` (lowercase letters, digits, hyphens; at most
/// 64) and a non-empty `description` (at most 1024 characters).
fn front_matter(content: &[u8]) -> Result<(String, String), SkillError> {
    let invalid = SkillError::InvalidFrontMatter;
    let text = std::str::from_utf8(content).map_err(|_| invalid("not_utf8"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or(invalid("missing_opening_delimiter"))?;
    // Byte offsets come from the raw lines, terminators included, so CRLF
    // files slice at the right place (F10).
    let end = rest
        .split_inclusive('\n')
        .scan(0usize, |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, line.trim_end_matches(['\n', '\r'])))
        })
        .find(|(_, line)| *line == "---")
        .map(|(start, _)| start)
        .ok_or(invalid("missing_closing_delimiter"))?;
    let docs =
        yaml_rust2::YamlLoader::load_from_str(&rest[..end]).map_err(|_| invalid("invalid_yaml"))?;
    let map = docs
        .first()
        .and_then(|doc| doc.as_hash())
        .ok_or(invalid("not_a_mapping"))?;
    let field = |key: &str| {
        map.get(&yaml_rust2::Yaml::String(key.into()))
            .and_then(|value| value.as_str())
            .map(str::to_owned)
    };
    let name = field("name").ok_or(invalid("missing_name"))?;
    let valid_name = !name.is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if !valid_name {
        return Err(invalid("invalid_name"));
    }
    let description = field("description").ok_or(invalid("missing_description"))?;
    if description.trim().is_empty() || description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(invalid("invalid_description"));
    }
    Ok((name, description))
}
