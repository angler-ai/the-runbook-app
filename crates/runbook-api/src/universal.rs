//! SEARCH2: one search across every project the caller can read.
//! Server-side fan-out over the caller's projects (one HTTP call, each
//! project authorized on its own), merged and capped per kind. Steps
//! are the open assignments (the steps a person can act on now).
use crate::{Request, backends::Backends, problem::Response};
use runbook_core::{
    StoreError,
    authoring::{AuthoringFactory, AuthoringStore},
    catalog::{CatalogStore, TenantScope},
    credentials::{Scope, authorize},
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
    memory_entries::{MemoryQuery, MemoryStore},
    search::RunFilters,
};
use runbook_model::{
    RunSearchEntry, UniversalMemoryHit, UniversalRunHit, UniversalRunbookHit, UniversalSearchView,
    UniversalSkillHit, UniversalStepHit,
};
use serde_json::json;

const PER_KIND: usize = 8;
const MAX_PROJECTS: usize = 50;

type Failure = (u16, &'static str);

pub fn recognizes(path: &str) -> bool {
    crate::directory::match_path("/v1/orgs/{org}/search", path).is_some()
}

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match dispatch(backends, &r).await {
        Ok(body) => Response::json(200, body, &r.request_id),
        Err((status, code)) => Response::error(status, code, &r.request_id),
    }
}

const UNAVAILABLE: Failure = (503, "storage.unavailable");

fn unavailable(_: StoreError) -> Failure {
    UNAVAILABLE
}

async fn dispatch<B: Backends>(backends: &B, r: &Request) -> Result<serde_json::Value, Failure> {
    if r.method != "GET" {
        return Err((405, "request.method_not_allowed"));
    }
    let text = r
        .query
        .get("q")
        .cloned()
        .unwrap_or_default()
        .trim()
        .to_owned();
    let mut view = UniversalSearchView::default();
    let record = r
        .credential_record(&backends.credentials())
        .await
        .map_err(|e| e.status())?;
    // The path names the caller's own organization; anything else is
    // unknown to them.
    let org = crate::directory::match_path("/v1/orgs/{org}/search", &r.path)
        .and_then(|params| params.get("org").cloned())
        .unwrap_or_default();
    if org != record.org_id.as_str() {
        return Err((404, "resource.not_found"));
    }
    if text.is_empty() {
        return Ok(json!(view));
    }
    let directory = backends.directory();
    let now = directory.current_time_ms().await.map_err(|_| UNAVAILABLE)?;
    // A revoked or expired credential is refused, not answered empty.
    if record.validate().is_err()
        || record.revoked_at.is_some()
        || record.expires_at.is_some_and(|t| t <= now)
    {
        return Err((401, "auth.invalid_credential"));
    }
    let snapshot = directory
        .snapshot(&record.org_id)
        .await
        .map_err(|_| UNAVAILABLE)?;
    let factory = backends.authoring();
    let needle = text.to_lowercase();

    let mut scanned = 0;
    for project in snapshot.projects.values() {
        if scanned >= MAX_PROJECTS {
            break;
        }
        if authorize(&record, &snapshot, now, &project.id, Scope::RunbooksRead).is_err() {
            continue;
        }
        scanned += 1;
        let scope = TenantScope {
            org_id: record.org_id.clone(),
            project_id: project.id.clone(),
        };
        // Each kind needs the scope its own routes need: runs and steps
        // executions:read, skills skills:read (runbooks and memory
        // runbooks:read, checked above).
        let can = |scope| authorize(&record, &snapshot, now, &project.id, scope).is_ok();
        let (runs_ok, skills_ok) = (can(Scope::ExecutionsRead), can(Scope::SkillsRead));
        let store = factory.scoped(scope.clone());
        let catalog = store.catalog();

        // Runbooks: slug or draft title.
        if view.runbooks.len() < PER_KIND {
            for record_rb in store.catalog_runbooks().await.map_err(unavailable)? {
                if record_rb.archived {
                    continue;
                }
                let title = match store.catalog().get_draft(&record_rb.runbook_id).await {
                    Ok(Some(draft)) => runbook_spec::parse(&draft.source)
                        .map(|spec| spec.metadata.title)
                        .unwrap_or_else(|_| record_rb.slug.clone()),
                    _ => record_rb.slug.clone(),
                };
                if record_rb.slug.to_lowercase().contains(&needle)
                    || title.to_lowercase().contains(&needle)
                {
                    view.runbooks.push(UniversalRunbookHit {
                        project_id: project.id.clone(),
                        project_slug: project.slug.clone(),
                        runbook_id: record_rb.runbook_id,
                        slug: record_rb.slug,
                        title,
                    });
                }
            }
        }

        // Runs: the D22 text filter, merged newest-first at the end.
        let filters = RunFilters {
            text: Some(text.clone()),
            ..Default::default()
        };
        let page = if runs_ok {
            catalog
                .search_runs(filters, None, PER_KIND)
                .await
                .map_err(unavailable)?
                .runs
        } else {
            vec![]
        };
        for run in page {
            view.runs.push(UniversalRunHit {
                project_id: project.id.clone(),
                project_slug: project.slug.clone(),
                run: RunSearchEntry {
                    id: run.execution_id,
                    runbook_id: run.runbook_id,
                    status: run.status,
                    title: run.title,
                    tags: run.tags,
                    fields: run.fields,
                    created_by: run.created_by,
                    created_at_ms: run.created_at_ms,
                    completed_at_ms: run.completed_at_ms,
                },
            });
        }

        // Steps: open assignments whose title or path matches.
        if runs_ok && view.steps.len() < PER_KIND {
            for assignment in catalog.list_assignments().await.map_err(unavailable)? {
                if assignment.title.to_lowercase().contains(&needle)
                    || assignment.path.to_lowercase().contains(&needle)
                {
                    view.steps.push(UniversalStepHit {
                        project_id: project.id.clone(),
                        project_slug: project.slug.clone(),
                        execution_id: assignment.execution_id,
                        step_run_id: assignment.step_run_id,
                        path: assignment.path,
                        title: assignment.title,
                    });
                }
            }
        }

        // Memory: the project memory store's own full-text search.
        if view.memory.len() < PER_KIND {
            let execution = backends.execution(scope.clone());
            let query = MemoryQuery {
                text: Some(text.clone()),
                ..Default::default()
            };
            if let Ok(page) = execution
                .search_memory(&scope, &query, None, PER_KIND)
                .await
            {
                for record in page.items {
                    view.memory.push(UniversalMemoryHit {
                        project_id: project.id.clone(),
                        project_slug: project.slug.clone(),
                        memory_id: record.id,
                        title: record.title,
                    });
                }
            }
        }

        // Skills: latest version per name, by name.
        if skills_ok && view.skills.len() < PER_KIND {
            let mut seen = std::collections::BTreeSet::new();
            for skill in store.skill_versions().await.map_err(unavailable)? {
                if skill.name.to_lowercase().contains(&needle) && seen.insert(skill.name.clone()) {
                    view.skills.push(UniversalSkillHit {
                        project_id: project.id.clone(),
                        project_slug: project.slug.clone(),
                        name: skill.name,
                        hash: skill.hash.iter().map(|b| format!("{b:02x}")).collect(),
                    });
                }
            }
        }
    }

    view.runs.sort_by(|a, b| {
        b.run
            .created_at_ms
            .cmp(&a.run.created_at_ms)
            .then_with(|| b.run.id.cmp(&a.run.id))
    });
    view.runbooks.truncate(PER_KIND);
    view.runs.truncate(PER_KIND);
    view.steps.truncate(PER_KIND);
    view.memory.truncate(PER_KIND);
    view.skills.truncate(PER_KIND);
    Ok(json!(view))
}
