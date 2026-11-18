//! The pure authoring context shared by the server and the browser compiler (B3).
//!
//! The server reads users, teams, the project's latest skills and its variable
//! declarations from storage; the browser reads the same facts through `/v1`.
//! Both then call [`compile_context`], so a source compiles against identical
//! pins. [`validate_source`] is the whole `/v1/validate` response body. Nothing
//! here performs I/O, reads a clock or depends on hash ordering.
use crate::{CompileContext, Diagnostic};
use runbook_model::{TeamId, UserId, ValidateResponse};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The largest source `/v1/validate` and the authoring routes accept.
pub const MAX_SOURCE_BYTES: usize = 1_048_576;

/// One directory user. Only active users are pinnable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryUser {
    pub id: UserId,
    pub name: String,
    #[serde(default = "active")]
    pub active: bool,
}
fn active() -> bool {
    true
}

/// One directory team, pinnable by slug or ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryTeam {
    pub id: TeamId,
    pub slug: String,
}

/// The latest version of one library skill. `hash` is 64 hex digits on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatestSkill {
    pub name: String,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(serialize_with = "hex_out", deserialize_with = "hex_in")]
    pub hash: [u8; 32],
}

/// Directory facts a source compiles against. Every list is optional on the wire.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompileDirectory {
    #[serde(default)]
    pub users: Vec<DirectoryUser>,
    #[serde(default)]
    pub teams: Vec<DirectoryTeam>,
    /// The project's latest skill versions, in catalog order.
    #[serde(default)]
    pub skills: Vec<LatestSkill>,
    /// The project's variable declarations (`project.vars`), as stored.
    #[serde(default)]
    pub variables: Map<String, Value>,
}

/// A skill version as reference resolution sees it.
pub trait SkillEntry {
    fn skill_name(&self) -> &str;
    fn skill_reference(&self) -> Option<&str>;
    fn skill_hash(&self) -> [u8; 32];
}
impl SkillEntry for LatestSkill {
    fn skill_name(&self) -> &str {
        &self.name
    }
    fn skill_reference(&self) -> Option<&str> {
        self.reference.as_deref()
    }
    fn skill_hash(&self) -> [u8; 32] {
        self.hash
    }
}

/// Resolve a source skill reference against the latest versions: `skill://name`
/// by name, anything else by recorded provenance and then by its last segment.
pub fn resolve_skill_reference<S: SkillEntry>(latest: &[S], reference: &str) -> Option<[u8; 32]> {
    let found = if let Some(name) = reference.strip_prefix("skill://") {
        if name.contains(['/', '@']) {
            None
        } else {
            latest.iter().find(|v| v.skill_name() == name)
        }
    } else {
        latest
            .iter()
            .find(|v| v.skill_reference() == Some(reference))
            .or_else(|| {
                let name = reference.rsplit('/').next().unwrap_or(reference);
                latest.iter().find(|v| v.skill_name() == name)
            })
    };
    found.map(SkillEntry::skill_hash)
}

/// Build the compile context for `source`. Fails only when the stored variable
/// declarations are invalid (the server answers that with a 503).
pub fn compile_context(
    directory: &CompileDirectory,
    source: &str,
) -> Result<CompileContext, Vec<Diagnostic>> {
    let mut ctx = CompileContext {
        project_variables: crate::parse_declarations(&Value::Object(directory.variables.clone()))?,
        ..Default::default()
    };
    // Directory order never matters: users and teams are keyed by ID first.
    let users: BTreeMap<&UserId, &DirectoryUser> =
        directory.users.iter().map(|u| (&u.id, u)).collect();
    let mut names = BTreeMap::<&str, Vec<&UserId>>::new();
    for u in users.values().filter(|u| u.active) {
        ctx.users.insert(u.id.to_string(), u.id.clone());
        names.entry(&u.name).or_default().push(&u.id);
    }
    for (name, ids) in names {
        if let [id] = ids[..] {
            ctx.users.insert(name.into(), id.clone());
        }
    }
    let teams: BTreeMap<&TeamId, &DirectoryTeam> =
        directory.teams.iter().map(|t| (&t.id, t)).collect();
    for t in teams.values() {
        ctx.teams.insert(t.slug.clone(), t.id.clone());
        ctx.teams.insert(t.id.to_string(), t.id.clone());
    }
    for skill in &directory.skills {
        ctx.skills
            .insert(format!("skill://{}", skill.name), skill.hash);
    }
    if let Ok(spec) = crate::parse(source)
        && let Some(aliases) = spec.source_json()["spec"]["skills"].as_object()
    {
        for reference in aliases.values().filter_map(Value::as_str) {
            if let Some(hash) = resolve_skill_reference(&directory.skills, reference) {
                ctx.skills.insert(reference.into(), hash);
            }
        }
    }
    Ok(ctx)
}

/// The `/v1/validate` response for `source`: compile and publication-policy
/// diagnostics with 1-based spans, plus the worst-case step budget.
pub fn validate_source(source: &str, context: &CompileContext) -> ValidateResponse {
    match crate::parse(source).and_then(|spec| crate::compile(&spec, context)) {
        Ok(c) => ValidateResponse {
            diagnostics: wire_diagnostics(crate::validate_policy(&c)),
            step_budget: Some(c.worst_case_step_runs()),
        },
        Err(e) => ValidateResponse {
            diagnostics: wire_diagnostics(e),
            step_budget: None,
        },
    }
}

/// Compiler diagnostics in the public wire shape (spans of 0 mean unknown).
pub fn wire_diagnostics(errors: Vec<Diagnostic>) -> Vec<runbook_model::Diagnostic> {
    errors
        .into_iter()
        .map(|e| runbook_model::Diagnostic {
            message: e
                .message
                .unwrap_or_else(|| crate::messages::message_for(&e.code, &e.path)),
            code: e.code,
            path: e.path,
            line: u32::try_from(e.line).ok().filter(|n| *n > 0),
            column: u32::try_from(e.column).ok().filter(|n| *n > 0),
        })
        .collect()
}

fn hex_out<S: Serializer>(hash: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let text: String = hash
        .iter()
        .flat_map(|b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 15)]])
        .map(char::from)
        .collect();
    s.serialize_str(&text)
}
fn hex_in<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
    let text = String::deserialize(d)?;
    let invalid = || serde::de::Error::custom("expected 64 hex digits");
    if text.len() != 64 {
        return Err(invalid());
    }
    let mut out = [0; 32];
    for (byte, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        let digit = |c: u8| char::from(c).to_digit(16).ok_or_else(invalid);
        *byte = u8::try_from(digit(pair[0])? * 16 + digit(pair[1])?).map_err(|_| invalid())?;
    }
    Ok(out)
}
