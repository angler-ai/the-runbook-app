use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_core::listing::{DraftListing, ListedRunbook, ListingQuery};
use sha2::{Digest, Sha256};
impl<D: DirectoryOperations, C: CredentialStore, F: AuthoringFactory> AuthoringService<D, C, F> {
    // The request's parts as the dispatcher resolved them.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn read(
        &self,
        r: &Request,
        route: &PublicRoute,
        p: &BTreeMap<String, String>,
        rb: Option<&RunbookId>,
        store: &F::Store,
        principal: &str,
        with_runs: bool,
    ) -> Result<StoredResponse, Error> {
        let catalog = store.catalog();
        let items = match route.path {
            "/v1/projects/{p}/runbooks" => {
                // UI-1b: the store answers the page from rows (the search
                // and the cut included; no source is read), then the rest
                // (latest versions, runs) for the page's rows in one read
                // each.
                let search = RunbookSearch::from_query(&r.query);
                let cursor = Cursor::of(r, principal, search.binding(&r.query))?;
                // UI-1b review round 1: the position is a runbook ID; a
                // forged one is refused here, not passed to the store (a
                // NUL would fail the Postgres query).
                if cursor
                    .after
                    .as_deref()
                    .is_some_and(|after| after.parse::<RunbookId>().is_err())
                {
                    return Err(error(r, 400, "request.invalid_cursor"));
                }
                let page = store
                    .runbook_listing_page(&ListingQuery {
                        q: search.q,
                        tags: search.tags,
                        after: cursor.after.clone(),
                        limit: cursor.limit + 1,
                    })
                    .await
                    .map_err(|e| store_error(r, e))?;
                let mut rows = page.rows;
                let more = rows.len() > cursor.limit;
                rows.truncate(cursor.limit);
                // A store that stopped early (D1's bounded backfill) says
                // how far it looked: the next page continues after that
                // runbook, so a short page never ends the list early.
                let next = if more {
                    rows.last()
                        .map(|row| cursor.next(row.record.runbook_id.as_str()))
                } else {
                    page.examined_to.map(|last| cursor.next(&last))
                };
                let items = catalog_views(r, store, rows, with_runs).await?;
                return Ok(response(200, json!({"items": items, "next_cursor": next})));
            }
            "/v1/runbooks/{rb}" | "/v1/projects/{p}/runbooks/{slug}" => {
                let record = if let Some(slug) = p.get("slug") {
                    // UI-1c: every store refuses to create a runbook whose
                    // slug fails this check, so such a slug names none
                    // (and a NUL, which Postgres refuses in text, never
                    // reaches a store).
                    if parse_runbook_ref(slug).is_err() || slug.contains('@') {
                        return Err(error(r, 404, "resource.not_found"));
                    }
                    store.runbook_by_slug(slug).await
                } else {
                    catalog
                        .read_runbook(rb.ok_or_else(|| error(r, 404, "resource.not_found"))?)
                        .await
                }
                .map_err(|e| store_error(r, e))?
                .ok_or_else(|| error(r, 404, "resource.not_found"))?;
                let draft = catalog
                    .get_draft(&record.runbook_id)
                    .await
                    .map_err(|e| store_error(r, e))?
                    .map(|draft| draft.source);
                let row = DraftListing::of(&record.slug, draft.as_deref()).listed(record);
                let view = catalog_views(r, store, vec![row], with_runs)
                    .await?
                    .pop()
                    .ok_or_else(|| error(r, 404, "resource.not_found"))?;
                return Ok(response(200, json!(view)));
            }
            "/v1/runbooks/{rb}/draft" => {
                let draft = catalog
                    .get_draft(rb.ok_or_else(|| error(r, 404, "resource.not_found"))?)
                    .await
                    .map_err(|e| store_error(r, e))?
                    .ok_or_else(|| error(r, 404, "resource.not_found"))?;
                return Ok(StoredResponse {
                    status: 200,
                    etag: Some(draft_etag(&draft)),
                    body: json!(draft),
                });
            }
            "/v1/runbooks/{rb}/versions" => {
                let rb = rb.ok_or_else(|| error(r, 404, "resource.not_found"))?;
                match r.query.get("preview").map(String::as_str) {
                    None | Some("false") => {
                        if r.query.contains_key("branch") {
                            return Err(error(r, 400, "request.invalid_query"));
                        }
                        catalog
                            .list_versions(rb)
                            .await
                            .map_err(|e| store_error(r, e))?
                            .into_iter()
                            .map(|v| {
                                (
                                    format!("{:010}", v.runbook_version),
                                    json!(VersionView {
                                        runbook_id: v.runbook_id,
                                        runbook_version_id: v.runbook_version_id,
                                        runbook_version: Some(v.runbook_version),
                                        branch: None,
                                        commit_sha: None
                                    }),
                                )
                            })
                            .collect()
                    }
                    Some("true") => catalog
                        .list_previews(rb)
                        .await
                        .map_err(|e| store_error(r, e))?
                        .into_iter()
                        .filter(|v| {
                            r.query
                                .get("branch")
                                .is_none_or(|b| v.branch.as_ref() == Some(b))
                        })
                        .map(|v| (v.runbook_version_id.to_string(), json!(v)))
                        .collect(),
                    _ => return Err(error(r, 400, "request.invalid_query")),
                }
            }
            "/v1/runbooks/{rb}/versions/{version}" => {
                let rb = rb.ok_or_else(|| error(r, 404, "resource.not_found"))?;
                let v = store
                    .version_data(rb, selector(r, &p["version"])?)
                    .await
                    .map_err(|e| store_error(r, e))?;
                return Ok(response(
                    200,
                    json!(VersionSourceView {
                        version: v.view(rb),
                        source: v.source,
                        semantic_hash: hex(&v.semantic_hash),
                        snapshot_digest: hex(&v.snapshot_digest)
                    }),
                ));
            }
            "/v1/runbooks/{rb}/versions/{version}/form" => {
                let rb = rb.ok_or_else(|| error(r, 404, "resource.not_found"))?;
                let v = store
                    .version_data(rb, selector(r, &p["version"])?)
                    .await
                    .map_err(|e| store_error(r, e))?;
                let bytes = catalog
                    .get(&BlobKey::Shared {
                        hash: v.snapshot_digest,
                    })
                    .await
                    .map_err(|e| store_error(r, e))?;
                let compiled = runbook_spec::restore(&bytes)
                    .map_err(|_| error(r, 503, "storage.unavailable"))?;
                if compiled.hash() != &v.semantic_hash {
                    return Err(error(r, 503, "storage.unavailable"));
                }
                return Ok(response(
                    200,
                    json!(VersionFormView {
                        inputs: runbook_spec::schema_json(&compiled.ir().inputs),
                        fields: runbook_spec::schema_json(&compiled.ir().fields),
                        steps: outline(&compiled.ir().steps),
                        finally: outline(&compiled.ir().finally),
                    }),
                ));
            }
            "/v1/runbooks/{rb}/versions/{a}/diff/{b}" => {
                let rb = rb.ok_or_else(|| error(r, 404, "resource.not_found"))?;
                let a = store
                    .version_data(rb, selector(r, &p["a"])?)
                    .await
                    .map_err(|e| store_error(r, e))?;
                let b = store
                    .version_data(rb, selector(r, &p["b"])?)
                    .await
                    .map_err(|e| store_error(r, e))?;
                return Ok(response(
                    200,
                    json!(version_diff(&a.source, &b.source, &p["a"], &p["b"])),
                ));
            }
            "/v1/projects/{p}/skills" => {
                let latest = latest_skills(r, catalog).await?;
                let mut items = vec![];
                for v in store
                    .skill_versions()
                    .await
                    .map_err(|e| store_error(r, e))?
                {
                    let key = format!("{}:{}", v.name, hex(&v.hash));
                    let is_latest = latest.contains(&(v.name.clone(), v.hash));
                    items.push((
                        key,
                        json!(skill_with_usage(r, catalog, v, is_latest).await?),
                    ));
                }
                items
            }
            "/v1/projects/{p}/skills/{hash}" => {
                let hash = parse_hash(r, &p["hash"])?;
                let v = catalog
                    .skill_version(&hash)
                    .await
                    .map_err(|e| store_error(r, e))?
                    .ok_or_else(|| error(r, 404, "resource.not_found"))?;
                let is_latest = latest_skills(r, catalog)
                    .await?
                    .contains(&(v.name.clone(), v.hash));
                let mut view = skill_with_usage(r, catalog, v, is_latest).await?;
                // CLI1: `?files=true` adds each file's bytes (base64), so a
                // client pulls a whole skill folder in one request. Bounded
                // by the skill bundle limit.
                let with_files = match r.query.get("files").map(String::as_str) {
                    None | Some("false") => false,
                    Some("true") => true,
                    Some(_) => return Err(error(r, 400, "request.invalid_query")),
                };
                if with_files {
                    use base64::Engine;
                    for f in &mut view.files {
                        let bytes = runbook_core::skill_library::read_pinned_file(
                            catalog, catalog, &hash, &f.path,
                        )
                        .await
                        .map_err(|e| store_error(r, e))?;
                        f.content = Some(base64::engine::general_purpose::STANDARD.encode(bytes));
                    }
                }
                return Ok(response(200, json!(view)));
            }
            _ => return Err(error(r, 404, "resource.not_found")),
        };
        Ok(response(200, page(r, principal, items)?))
    }
}
/// UI-1: the list's search. `q` matches title, slug, description and tags
/// as a case-insensitive substring (surrounding whitespace ignored); `tag`
/// is a comma-separated list the runbook must carry all of (exact). Empty
/// means absent.
#[derive(Debug, Default)]
struct RunbookSearch {
    q: Option<String>,
    tags: Vec<String>,
}
impl RunbookSearch {
    fn from_query(query: &BTreeMap<String, String>) -> Self {
        let mut tags: Vec<String> = query
            .get("tag")
            .map(|v| {
                split_tags(v)
                    .iter()
                    .map(|t| t.trim())
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        tags.sort();
        tags.dedup();
        Self {
            q: query
                .get("q")
                .map(|v| v.trim().to_lowercase())
                .filter(|v| !v.is_empty()),
            tags,
        }
    }
    /// The parameters a cursor is bound to: every one but `cursor` and
    /// `limit`, with `q` and `tag` as searched (so an empty one binds as
    /// an absent one).
    fn binding(&self, query: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut filters = query.clone();
        filters.remove("cursor");
        filters.remove("limit");
        filters.remove("q");
        filters.remove("tag");
        if let Some(q) = &self.q {
            filters.insert("q".into(), q.clone());
        }
        if !self.tags.is_empty() {
            let escaped: Vec<String> = self
                .tags
                .iter()
                .map(|t| t.replace('\\', "\\\\").replace(',', "\\,"))
                .collect();
            filters.insert("tag".into(), escaped.join(","));
        }
        filters
    }
}
/// UI-1 review: `tag` is a comma-separated list; `\,` is a comma inside a
/// tag and `\\` a backslash (any escaped character stands for itself; a
/// trailing lone backslash is kept).
fn split_tags(value: &str) -> Vec<String> {
    let mut tags = vec![String::new()];
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let last = tags.last_mut().expect("one tag at least");
                last.push(chars.next().unwrap_or('\\'));
            }
            ',' => tags.push(String::new()),
            c => tags.last_mut().expect("one tag at least").push(c),
        }
    }
    tags
}
/// Catalog rows carry no title or version summary: the title, description
/// and tags come from the current draft ([`DraftSummary`]), the latest
/// published number and `draft_ahead` from the latest published version,
/// and the run fields from the run index (UI-1). No stored update timestamp
/// exists, so the view has none.
///
/// UI-1 review: one read for the page's latest versions and, only when the
/// credential also holds `executions:read` (`with_runs`), one for its runs;
/// without it `last_run` and `active_runs` are omitted.
async fn catalog_views<S: AuthoringStore>(
    r: &Request,
    store: &S,
    rows: Vec<ListedRunbook>,
    with_runs: bool,
) -> Result<Vec<RunbookCatalogView>, Error> {
    let ids: Vec<RunbookId> = rows
        .iter()
        .map(|row| row.record.runbook_id.clone())
        .collect();
    let latest = store
        .latest_published_digests(&ids)
        .await
        .map_err(|e| store_error(r, e))?;
    let mut runs = if with_runs {
        Some(
            store
                .catalog()
                .runbook_run_stats(&ids, ACTIVE_RUNS_CAP)
                .await
                .map_err(|e| store_error(r, e))?,
        )
    } else {
        None
    };
    Ok(rows
        .into_iter()
        .map(|row| {
            let record = row.record;
            let published = latest.get(&record.runbook_id);
            // UI-1b: by digest (SHA-256 of the exact source), no source read.
            let draft_ahead = match (published, &row.draft_digest) {
                (Some((_, published)), Some(draft)) => published != draft,
                _ => false,
            };
            let stats = runs
                .as_mut()
                .map(|runs| runs.remove(&record.runbook_id).unwrap_or_default());
            RunbookCatalogView {
                origin: serde_json::to_value(record.origin)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                runbook_id: record.runbook_id,
                slug: record.slug,
                archived: record.archived,
                title: Some(row.title),
                latest_version: published.map(|(number, _)| number.get()),
                description: row.description,
                description_truncated: row.description_truncated,
                tags: row.tags,
                draft_ahead,
                last_run: stats.as_ref().and_then(|s| s.last_run.as_ref()).map(|run| {
                    RunbookLastRun {
                        id: run.execution_id.clone(),
                        status: run.status,
                        created_at_ms: run.created_at_ms,
                        completed_at_ms: run.completed_at_ms,
                    }
                }),
                active_runs: stats.map(|s| u32::try_from(s.active_runs).unwrap_or(u32::MAX)),
            }
        })
        .collect())
}
/// UI-1: `active_runs` counts at most this many.
const ACTIVE_RUNS_CAP: usize = 100;
/// Each name's latest version as `(name, hash)`: the same set the server's
/// compile context pins (`CatalogStore::latest_skills`).
async fn latest_skills(
    r: &Request,
    catalog: &impl CatalogStore,
) -> Result<std::collections::HashSet<(String, [u8; 32])>, Error> {
    Ok(catalog
        .latest_skills()
        .await
        .map_err(|e| store_error(r, e))?
        .into_iter()
        .map(|v| (v.name, v.hash))
        .collect())
}
async fn skill_with_usage(
    r: &Request,
    catalog: &(impl CatalogStore + BlobStore),
    version: SkillVersion,
    latest: bool,
) -> Result<SkillVersionView, Error> {
    let hash = version.hash;
    let mut v = skill_view(version, latest);
    v.usages = runbook_core::skill_library::skill_usage(catalog, catalog, &hash)
        .await
        .map_err(|e| store_error(r, e))?
        .into_iter()
        .map(|u| SkillUsageView {
            runbook_id: u.runbook_id,
            version_id: u.version_id,
            step_ids: u.step_ids,
        })
        .collect();
    Ok(v)
}
fn parse_hash(r: &Request, s: &str) -> Result<[u8; 32], Error> {
    if s.len() != 64 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(error(r, 404, "resource.not_found"));
    }
    let mut hash = [0; 32];
    for (i, b) in hash.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|_| error(r, 404, "resource.not_found"))?;
    }
    Ok(hash)
}
fn selector(r: &Request, s: &str) -> Result<Option<VersionSelector>, Error> {
    if s == "latest" {
        return Ok(None);
    }
    let reference = parse_runbook_ref(&format!("version@{s}"))
        .map_err(|_| error(r, 404, "resource.not_found"))?;
    Ok(reference.1)
}
fn page(r: &Request, principal: &str, items: Vec<(String, Value)>) -> Result<Value, Error> {
    let mut filters = r.query.clone();
    filters.remove("cursor");
    filters.remove("limit");
    let (items, next) = select_page(r, principal, filters, items)?;
    Ok(json!({"items":items.into_iter().map(|(_,v)|v).collect::<Vec<_>>(),"next_cursor":next}))
}
/// A page's keyed rows and the next page's cursor.
type Page<T> = (Vec<(String, T)>, Option<String>);
/// A list request's page size, cursor binding and position. The binding
/// covers the principal, the path and `filters` (the query without
/// `cursor` and `limit`).
struct Cursor {
    limit: usize,
    binding: String,
    after: Option<String>,
}
impl Cursor {
    fn of(r: &Request, principal: &str, filters: BTreeMap<String, String>) -> Result<Self, Error> {
        let limit = r
            .query
            .get("limit")
            .map(|v| v.parse::<usize>())
            .transpose()
            .map_err(|_| error(r, 400, "request.invalid_query"))?
            .unwrap_or(100);
        if !(1..=100).contains(&limit) {
            return Err(error(r, 400, "request.invalid_query"));
        }
        let binding = hex(&Sha256::digest(
            serde_json::to_vec(&(principal, &r.path, filters))
                .map_err(|_| error(r, 400, "request.invalid_cursor"))?,
        ));
        let after = if let Some(cursor) = r.query.get("cursor") {
            let v: Value = serde_json::from_slice(
                &URL_SAFE_NO_PAD
                    .decode(cursor)
                    .map_err(|_| error(r, 400, "request.invalid_cursor"))?,
            )
            .map_err(|_| error(r, 400, "request.invalid_cursor"))?;
            if v["binding"] != binding {
                return Err(error(r, 400, "request.invalid_cursor"));
            }
            Some(
                v["after"]
                    .as_str()
                    .ok_or_else(|| error(r, 400, "request.invalid_cursor"))?
                    .to_owned(),
            )
        } else {
            None
        };
        Ok(Self {
            limit,
            binding,
            after,
        })
    }
    /// The cursor of the page after the row keyed `last`.
    fn next(&self, last: &str) -> String {
        URL_SAFE_NO_PAD.encode(json!({"binding": self.binding, "after": last}).to_string())
    }
}
/// One page of keyed rows, newest key first, after the request's cursor;
/// the cursor binds the principal, the path and `filters` (the query
/// without `cursor` and `limit`). Returns the rows and the next cursor.
fn select_page<T>(
    r: &Request,
    principal: &str,
    filters: BTreeMap<String, String>,
    mut items: Vec<(String, T)>,
) -> Result<Page<T>, Error> {
    let cursor = Cursor::of(r, principal, filters)?;
    items.sort_by(|a, b| b.0.cmp(&a.0));
    items.retain(|v| cursor.after.as_ref().is_none_or(|a| &v.0 < a));
    let more = items.len() > cursor.limit;
    items.truncate(cursor.limit);
    let next = if more {
        items.last().map(|v| cursor.next(&v.0))
    } else {
        None
    };
    Ok((items, next))
}
/// CLI3: the step outline of a compiled version (ID, title as written,
/// kind, nested steps in source order).
pub(super) fn outline<'a>(
    nodes: impl IntoIterator<Item = &'a runbook_spec::ir::Node>,
) -> Vec<StepOutline> {
    use runbook_spec::ir::NodeKind;
    nodes
        .into_iter()
        .map(|node| {
            let (kind, children): (&str, Vec<&runbook_spec::ir::Node>) = match &node.kind {
                NodeKind::Task(_) => ("task", vec![]),
                NodeKind::Group(g) => (
                    "group",
                    g.children
                        .iter()
                        .chain(&g.catch)
                        .chain(&g.finally)
                        .collect(),
                ),
                NodeKind::Parallel { children, .. } => ("parallel", children.iter().collect()),
                NodeKind::Foreach(f) => ("foreach", f.children.iter().collect()),
                NodeKind::Loop(l) => ("loop", l.children.iter().collect()),
                NodeKind::Switch(s) => (
                    "switch",
                    s.cases
                        .iter()
                        .flat_map(|c| &c.children)
                        .chain(&s.default)
                        .collect(),
                ),
                NodeKind::Wait(_) => ("wait", vec![]),
                NodeKind::Approval(_) => ("approval", vec![]),
                NodeKind::Set(_) => ("set", vec![]),
                NodeKind::End(_) => ("end", vec![]),
                NodeKind::Call(_) => ("call", vec![]),
            };
            StepOutline {
                id: node.id.clone(),
                title: node.title.clone(),
                kind: kind.into(),
                steps: outline(children),
            }
        })
        .collect()
}
fn version_diff(a: &str, b: &str, from: &str, to: &str) -> VersionDiffView {
    // Bound diff work on adversarial, entirely different drafts. A single replacement
    // hunk remains a valid exact unified diff when the quadratic budget is exceeded.
    let lines_a = a.lines().count();
    let lines_b = b.lines().count();
    let unified_diff = if lines_a.saturating_mul(lines_b) <= 1_000_000 {
        similar::TextDiff::from_lines(a, b)
            .unified_diff()
            .header(from, to)
            .to_string()
    } else if a == b {
        String::new()
    } else {
        let mut out = format!(
            "--- {from}\n+++ {to}\n@@ -{},{} +{},{} @@\n",
            usize::from(lines_a > 0),
            lines_a,
            usize::from(lines_b > 0),
            lines_b
        );
        for (prefix, text) in [('-', a), ('+', b)] {
            for line in text.split_inclusive('\n') {
                out.push(prefix);
                out.push_str(line);
                if !line.ends_with('\n') {
                    out.push_str("\n\\ No newline at end of file\n");
                }
            }
        }
        out
    };
    let old = steps(a);
    let new = steps(b);
    VersionDiffView {
        unified_diff,
        added_steps: new
            .keys()
            .filter(|k| !old.contains_key(*k))
            .cloned()
            .collect(),
        removed_steps: old
            .keys()
            .filter(|k| !new.contains_key(*k))
            .cloned()
            .collect(),
        changed_steps: old
            .iter()
            .filter(|(k, v)| new.get(*k).is_some_and(|n| n != *v))
            .map(|(k, _)| k.clone())
            .collect(),
    }
}
fn steps(source: &str) -> BTreeMap<String, Value> {
    fn walk(v: &Value, out: &mut BTreeMap<String, Value>) {
        match v {
            Value::Object(o) => {
                if let Some(id) = o.get("id").and_then(Value::as_str) {
                    out.insert(id.into(), v.clone());
                }
                for (key, v) in o {
                    if [
                        "steps", "finally", "catch", "parallel", "foreach", "loop", "switch",
                        "cases", "default", "do",
                    ]
                    .contains(&key.as_str())
                    {
                        walk(v, out);
                    }
                }
            }
            Value::Array(a) => {
                for v in a {
                    walk(v, out)
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    if let Ok(spec) = runbook_spec::parse(source) {
        walk(&spec.source_json()["spec"]["steps"], &mut out);
        walk(&spec.source_json()["spec"]["finally"], &mut out);
    }
    out
}

#[cfg(test)]
mod outline_tests {
    use super::outline;
    use serde_json::json;

    #[test]
    fn the_outline_nests_steps_in_source_order_and_carries_titles_only() {
        let source = "apiVersion: therunbook.app/v1
kind: Runbook
metadata: {id: demo, title: Demo}
spec:
  inputs:
    regions: {type: list, items: {type: string}, default: [a]}
  steps:
    - id: smoke
      title: Smoke
      parallel:
        - {id: health, title: Health, instructions: Secret detail.}
        - {id: login, title: Log in, instructions: Log in.}
    - id: regions
      title: Per region
      foreach:
        items: ${{ inputs.regions }}
        as: region
        max_items: 5
        steps:
          - {id: rate, title: 'Rate in ${{ region }}', instructions: Check.}
    - id: decide
      title: Keep it?
      approval:
        prompt: Keep the deploy?
        options: [keep, roll]
        approvers: {teams: [oncall]}
  finally:
    - {id: notify, title: Notify, instructions: Post.}
";
        let spec = runbook_spec::parse(source).unwrap();
        let ir = runbook_spec::normalize(&spec).unwrap();
        let steps = json!(outline(&ir.steps));
        assert_eq!(
            steps,
            json!([
                {"id": "smoke", "title": "Smoke", "kind": "parallel", "steps": [
                    {"id": "health", "title": "Health", "kind": "task", "steps": []},
                    {"id": "login", "title": "Log in", "kind": "task", "steps": []},
                ]},
                {"id": "regions", "title": "Per region", "kind": "foreach", "steps": [
                    {"id": "rate", "title": "Rate in ${{ region }}", "kind": "task", "steps": []},
                ]},
                {"id": "decide", "title": "Keep it?", "kind": "approval", "steps": []},
            ])
        );
        assert!(!steps.to_string().contains("Secret detail"));
        assert_eq!(
            json!(outline(&ir.finally)),
            json!([{"id": "notify", "title": "Notify", "kind": "task", "steps": []}])
        );
    }
}

#[cfg(test)]
mod catalog_search_tests {
    use super::RunbookSearch;
    use runbook_core::listing::{DraftListing, ListingQuery, listed_description};
    use std::collections::BTreeMap;

    fn draft_summary_of(slug: &str, source: Option<String>) -> DraftListing {
        DraftListing::of(slug, source.as_deref())
    }
    impl RunbookSearch {
        fn matches(&self, slug: &str, listing: &DraftListing) -> bool {
            ListingQuery {
                q: self.q.clone(),
                tags: self.tags.clone(),
                ..Default::default()
            }
            .matches(slug, listing)
        }
    }

    fn query(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn the_draft_gives_title_description_and_tags() {
        let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: deploy, title: Deploy API, description: Ships it, tags: [prod, api]}\nspec:\n  steps: [{id: work, title: Work}]\n";
        let summary = draft_summary_of("deploy", Some(source.to_owned()));
        assert_eq!(summary.title, "Deploy API");
        assert_eq!(summary.description.as_deref(), Some("Ships it"));
        assert_eq!(summary.tags, ["prod", "api"]);
        assert_eq!(
            summary.digest,
            Some(runbook_core::portable::sha256(source.as_bytes()))
        );
        // No description, empty title, odd tags: slug title, string tags only.
        let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: x, title: '', description: '', tags: [a, 3, b]}\nspec: {steps: []}\n";
        let summary = draft_summary_of("x", Some(source.to_owned()));
        assert_eq!(summary.title, "x");
        assert_eq!(summary.description, None);
        assert_eq!(summary.tags, ["a", "b"]);
        // LANG7: no title at all: the title derived from the ID.
        let source = "metadata: {id: check-health}\nspec: {steps: []}\n";
        assert_eq!(
            draft_summary_of("check-health", Some(source.to_owned())).title,
            "Check health"
        );
        // A draft that does not parse, or none: the slug and nothing else.
        for source in [Some("[".to_owned()), None] {
            let summary = draft_summary_of("broken", source.clone());
            assert_eq!(summary.title, "broken");
            assert_eq!(summary.description, None);
            assert!(summary.tags.is_empty());
            assert_eq!(summary.digest.is_some(), source.is_some());
        }
    }

    /// UI-1 review: tags are trimmed, empty ones dropped and duplicates
    /// removed (first occurrence kept), so rows never repeat a tag and
    /// `tag=` matches what the row shows.
    #[test]
    fn listed_tags_are_trimmed_and_deduplicated() {
        let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: x, title: X, tags: [' prod ', prod, '', '  ', api, 'a,b', api]}\nspec: {steps: []}\n";
        let summary = draft_summary_of("x", Some(source.to_owned()));
        assert_eq!(summary.tags, ["prod", "api", "a,b"]);
        let matches =
            |tag: &str| RunbookSearch::from_query(&query(&[("tag", tag)])).matches("x", &summary);
        assert!(matches("prod"));
        assert!(matches(" prod ,api"));
        // A comma inside a tag is written `\,`; a backslash `\\`.
        assert!(matches("a\\,b"));
        assert!(matches("a\\,b,prod"));
        assert!(!matches("a,b"), "two tags, a and b");
        let search = RunbookSearch::from_query(&query(&[("tag", "a\\,b,c\\\\d")]));
        assert_eq!(search.tags, ["a,b", "c\\d"]);
        // The cursor binding re-escapes, so it is stable.
        let bound = |tag: &str| {
            let q = query(&[("tag", tag)]);
            RunbookSearch::from_query(&q).binding(&q)
        };
        assert_eq!(bound("prod,a\\,b"), bound("a\\,b , prod"));
        assert_ne!(bound("a\\,b"), bound("a,b"));
    }

    /// UI-1 review: the list carries at most 300 characters of a
    /// description and says when it cut one; search still reads all of it.
    #[test]
    fn long_descriptions_are_cut_and_marked() {
        let long = "é".repeat(301);
        let source = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: x, title: X, description: '{long} needle'}}\nspec: {{steps: []}}\n"
        );
        let summary = draft_summary_of("x", Some(source));
        let (shown, cut) = listed_description(summary.description.clone());
        assert!(cut);
        assert_eq!(shown.as_deref().map(|d| d.chars().count()), Some(300));
        assert!(shown.unwrap().chars().all(|c| c == 'é'));
        assert!(RunbookSearch::from_query(&query(&[("q", "needle")])).matches("x", &summary));
        let exact = "a".repeat(300);
        assert_eq!(
            listed_description(Some(exact.clone())),
            (Some(exact.clone()), false)
        );
        assert_eq!(listed_description(None), (None, false));
    }

    #[test]
    fn q_and_tag_search_and_normalize_the_cursor_binding() {
        let row = DraftListing {
            title: "Deploy API".into(),
            description: Some("Ships it to Production".into()),
            tags: vec!["prod".into(), "api".into()],
            digest: None,
        };
        let matches = |pairs: &[(&str, &str)]| {
            RunbookSearch::from_query(&query(pairs)).matches("deploy-api", &row)
        };
        for q in [
            "deploy",
            "DEPLOY api",
            "y-a",
            "production",
            "ships",
            "pro",
            "API",
            "  api  ",
        ] {
            assert!(matches(&[("q", q)]), "q={q}");
        }
        for q in ["rollback", "deploy  api", "staging"] {
            assert!(!matches(&[("q", q)]), "q={q}");
        }
        for tag in ["prod", "api,prod", " prod , api ", "prod,,", ""] {
            assert!(matches(&[("tag", tag)]), "tag={tag}");
        }
        for tag in ["PROD", "pro", "prod,staging"] {
            assert!(!matches(&[("tag", tag)]), "tag={tag}");
        }
        assert!(matches(&[("q", "ships"), ("tag", "api")]));
        assert!(!matches(&[("q", "ships"), ("tag", "staging")]));
        // Empty means absent, so it binds a cursor as an absent one does;
        // other parameters (cursor and limit aside) stay bound as before.
        let bound = |pairs: &[(&str, &str)]| {
            RunbookSearch::from_query(&query(pairs)).binding(&query(pairs))
        };
        assert_eq!(bound(&[("q", ""), ("tag", " , ")]), bound(&[]));
        assert_eq!(bound(&[("q", " Prod ")]), bound(&[("q", "prod")]));
        assert_eq!(bound(&[("tag", "b, a")]), bound(&[("tag", "b,a")]));
        assert_ne!(bound(&[("q", "prod")]), bound(&[("q", "api")]));
        assert_ne!(bound(&[("q", "prod")]), bound(&[("tag", "prod")]));
        assert_eq!(bound(&[("other", "1")]), query(&[("other", "1")]));
    }
}
