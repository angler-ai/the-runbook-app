//! CF11: the D1 shard seam. Every D1 access on this edition resolves its
//! database here, and there are exactly two ways to ask:
//!
//! - [`D1Shards::for_org`]: the database holding one organization's data
//!   (catalog, runs' rows, uploads, memory, archives, webhook
//!   subscriptions and events, quotas, notices). Everything that has an
//!   organization in hand asks this way.
//! - [`D1Shards::control`]: the one database for what is not org-scoped, or
//!   is looked up before the organization is known: the directory (orgs,
//!   users and identities, projects, teams, members, verified emails,
//!   project variables, directory requests, invitations, device codes, auth
//!   links), personal and service credentials and execution-token routes
//!   (both found by a secret's hash), the instance bootstrap marker, and the
//!   auth Worker's Better Auth tables.
//!
//! Passes that sweep every organization (the Cron passes) walk
//! [`D1Shards::data_shards`] and keep only the rows each shard owns
//! ([`D1Shards::owns`]), so an organization's rows left on a shard it moved
//! off are never acted on twice. Migrations run on [`D1Shards::every`]: each
//! database has the full schema, unused tables empty.
//!
//! The mapping is configuration, `RUNBOOK_D1_SHARDS` (JSON, every field
//! optional, binding names of the Worker's D1 bindings):
//!
//! ```json
//! {"control": "DB", "default": "DB", "orgs": {"org_…": "DB_2"}}
//! ```
//!
//! Unset, everything is `DB`: one database, as before CF11. A setting that
//! does not parse refuses to serve (`d1.shards_invalid`) rather than guess
//! where an organization's rows are.
use runbook_model::OrganizationId;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// The Worker variable holding the mapping.
pub const VAR: &str = "RUNBOOK_D1_SHARDS";
/// The binding everything maps to unless configured otherwise.
pub const DEFAULT_BINDING: &str = "DB";

/// Which D1 binding holds what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShardMap {
    control: String,
    default: String,
    orgs: BTreeMap<String, String>,
}

impl Default for ShardMap {
    fn default() -> Self {
        Self {
            control: DEFAULT_BINDING.to_owned(),
            default: DEFAULT_BINDING.to_owned(),
            orgs: BTreeMap::new(),
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    control: Option<String>,
    default: Option<String>,
    #[serde(default)]
    orgs: BTreeMap<String, String>,
}

/// A Worker binding name: what `wrangler.jsonc` accepts for a D1 binding.
fn binding(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl ShardMap {
    /// The mapping from `RUNBOOK_D1_SHARDS`'s value (`None`: unset).
    pub fn parse(value: Option<&str>) -> Result<Self, &'static str> {
        let Some(text) = value.map(str::trim).filter(|t| !t.is_empty()) else {
            return Ok(Self::default());
        };
        let raw: Raw = serde_json::from_str(text).map_err(|_| "d1.shards_invalid")?;
        let map = Self {
            control: raw.control.unwrap_or_else(|| DEFAULT_BINDING.to_owned()),
            default: raw.default.unwrap_or_else(|| DEFAULT_BINDING.to_owned()),
            orgs: raw.orgs,
        };
        for org in map.orgs.keys() {
            if org.parse::<OrganizationId>().is_err() {
                return Err("d1.shards_invalid");
            }
        }
        if map.bindings().iter().all(|b| binding(b)) {
            Ok(map)
        } else {
            Err("d1.shards_invalid")
        }
    }
    /// The binding holding `org`'s data.
    pub fn binding_for(&self, org: &OrganizationId) -> &str {
        self.orgs.get(org.as_str()).unwrap_or(&self.default)
    }
    pub fn control(&self) -> &str {
        &self.control
    }
    /// Every binding the mapping names (each opened once).
    pub fn bindings(&self) -> BTreeSet<&str> {
        let mut out: BTreeSet<&str> = self.data_bindings();
        out.insert(&self.control);
        out
    }
    /// The bindings that can hold organization data.
    pub fn data_bindings(&self) -> BTreeSet<&str> {
        let mut out: BTreeSet<&str> = self.orgs.values().map(String::as_str).collect();
        out.insert(&self.default);
        out
    }
}

/// One database per binding the mapping names, resolved by
/// [`ShardMap`]. Clones share the handles.
pub struct D1Shards<D> {
    map: Rc<ShardMap>,
    handles: Rc<BTreeMap<String, D>>,
}
impl<D> Clone for D1Shards<D> {
    fn clone(&self) -> Self {
        Self {
            map: self.map.clone(),
            handles: self.handles.clone(),
        }
    }
}

impl<D: Clone> D1Shards<D> {
    /// Everything on `db` (the default mapping; tests and single-database
    /// hosts).
    pub fn single(db: D) -> Self {
        Self {
            map: Rc::new(ShardMap::default()),
            handles: Rc::new(BTreeMap::from([(DEFAULT_BINDING.to_owned(), db)])),
        }
    }
    /// Open every binding `map` names with `open`.
    pub fn open<E>(map: ShardMap, mut open: impl FnMut(&str) -> Result<D, E>) -> Result<Self, E> {
        let mut handles = BTreeMap::new();
        for name in map.bindings() {
            handles.insert(name.to_owned(), open(name)?);
        }
        Ok(Self {
            map: Rc::new(map),
            handles: Rc::new(handles),
        })
    }
    fn handle(&self, name: &str) -> D {
        // `open` opened every name the map can return.
        self.handles
            .get(name)
            .or_else(|| self.handles.values().next())
            .cloned()
            .expect("a shard map names at least one binding")
    }
    /// The database holding `org`'s data.
    pub fn for_org(&self, org: &OrganizationId) -> D {
        self.handle(self.map.binding_for(org))
    }
    /// The control database: the directory, credentials and token routes,
    /// bootstrap (see the module documentation).
    pub fn control(&self) -> D {
        self.handle(self.map.control())
    }
    /// Whether `org`'s data and the control rows share one database, so a
    /// write spanning both can be one batch.
    pub fn colocated(&self, org: &OrganizationId) -> bool {
        self.map.binding_for(org) == self.map.control()
    }
    /// Each database that can hold organization data, with its binding.
    pub fn data_shards(&self) -> Vec<(String, D)> {
        self.map
            .data_bindings()
            .into_iter()
            .map(|name| (name.to_owned(), self.handle(name)))
            .collect()
    }
    /// Every database (migrations).
    pub fn every(&self) -> Vec<D> {
        self.handles.values().cloned().collect()
    }
    /// Whether the shard bound as `binding` holds `org`'s live data: a
    /// sweep over that shard acts only on such rows.
    pub fn owns(&self, binding: &str, org: &OrganizationId) -> bool {
        self.map.binding_for(org) == binding
    }
    pub fn map(&self) -> &ShardMap {
        &self.map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "org_01AAAAAAAAAAAAAAAAAAAAAAAA";
    const B: &str = "org_01BBBBBBBBBBBBBBBBBBBBBBBB";

    fn org(id: &str) -> OrganizationId {
        id.parse().unwrap()
    }

    #[test]
    fn unset_maps_everything_to_db() {
        for value in [None, Some(""), Some("  ")] {
            let map = ShardMap::parse(value).unwrap();
            assert_eq!(map, ShardMap::default());
            assert_eq!(map.binding_for(&org(A)), "DB");
            assert_eq!(map.control(), "DB");
            assert_eq!(map.bindings(), BTreeSet::from(["DB"]));
        }
    }

    #[test]
    fn an_org_maps_to_its_shard_and_the_rest_to_the_default() {
        let map = ShardMap::parse(Some(&format!(r#"{{"orgs": {{"{B}": "DB_2"}}}}"#))).unwrap();
        assert_eq!(map.binding_for(&org(A)), "DB");
        assert_eq!(map.binding_for(&org(B)), "DB_2");
        assert_eq!(map.control(), "DB");
        assert_eq!(map.bindings(), BTreeSet::from(["DB", "DB_2"]));
        assert_eq!(map.data_bindings(), BTreeSet::from(["DB", "DB_2"]));

        let split = ShardMap::parse(Some(r#"{"control": "CONTROL", "default": "DB"}"#)).unwrap();
        assert_eq!(split.data_bindings(), BTreeSet::from(["DB"]));
        assert_eq!(split.bindings(), BTreeSet::from(["CONTROL", "DB"]));
    }

    #[test]
    fn a_bad_mapping_is_refused_not_guessed() {
        for bad in [
            "not json",
            r#"{"shards": {}}"#,
            r#"{"control": ""}"#,
            r#"{"default": "db-2"}"#,
            r#"{"orgs": {"not-an-org": "DB_2"}}"#,
            &format!(r#"{{"orgs": {{"{A}": "2DB"}}}}"#),
        ] {
            assert_eq!(
                ShardMap::parse(Some(bad)),
                Err("d1.shards_invalid"),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_resolver_hands_out_the_mapped_handle() {
        let map = ShardMap::parse(Some(&format!(r#"{{"orgs": {{"{B}": "DB_2"}}}}"#))).unwrap();
        let mut opened = vec![];
        let shards = D1Shards::open(map, |name| {
            opened.push(name.to_owned());
            Ok::<_, ()>(name.to_owned())
        })
        .unwrap();
        assert_eq!(opened, ["DB", "DB_2"]);
        assert_eq!(shards.for_org(&org(A)), "DB");
        assert_eq!(shards.for_org(&org(B)), "DB_2");
        assert_eq!(shards.control(), "DB");
        assert!(shards.colocated(&org(A)));
        assert!(!shards.colocated(&org(B)));
        assert!(shards.owns("DB_2", &org(B)) && !shards.owns("DB", &org(B)));
        assert!(shards.owns("DB", &org(A)));
        let names: Vec<_> = shards.data_shards().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["DB", "DB_2"]);

        let single = D1Shards::single("only".to_owned());
        assert_eq!(single.for_org(&org(B)), "only");
        assert_eq!(single.control(), "only");
        assert_eq!(single.every(), ["only"]);
    }
}
