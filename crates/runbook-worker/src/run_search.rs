use crate::cloudflare::{CfBackends, CfHost};
use runbook_api::{executions::ExecutionApiError as Error, search::SearchQuery};
use runbook_core::{
    StoreError,
    authoring::{AuthoringFactory, AuthoringStore},
    blob::{BlobKey, BlobStore},
    catalog::{CatalogStore, TenantScope},
    search::{cursor_from, indexed_properties},
};
use runbook_model::{RunSearchEntry, RunSearchPage, RunbookId};

use serde_json::{Value, json};
/// B3.3 run search on this edition (ported from runbook-server): the
/// runbook's schema decides which fields are searchable (D22).
pub async fn search<H: CfHost>(
    backends: &CfBackends<H>,
    scope: &TenantScope,
    mut query: SearchQuery,
    origin: &str,
) -> Result<Value, Error> {
    use runbook_api::backends::Backends;
    let factory = backends.authoring();
    let authoring = factory.scoped(scope.clone());
    let store = authoring.catalog();
    let mut schema_checked = false;
    if let Some(name) = query.runbook {
        let id = if let Ok(id) = name.parse::<RunbookId>() {
            id
        } else {
            authoring
                .catalog_runbooks()
                .await
                .map_err(error)?
                .into_iter()
                .find(|r| r.slug == name)
                .map(|r| r.runbook_id)
                .ok_or_else(|| Error::new(404, "resource.not_found"))?
        };
        query.filters.runbook_id = Some(id.clone());
        let version = store.resolve_version(&id, None).await.map_err(error)?;
        let version = store
            .version_record(&id, &version.runbook_version_id)
            .await
            .map_err(error)?
            .ok_or_else(|| Error::new(404, "runbook.version_not_found"))?;
        let bytes = store
            .get(&BlobKey::Shared {
                hash: version.snapshot_digest,
            })
            .await
            .map_err(error)?;
        let compiled = runbook_spec::restore(&bytes)
            .map_err(|_| Error::new(503, "execution.snapshot_invalid"))?;
        let indexed = indexed_properties(&compiled.ir().fields);
        if query
            .filters
            .fields
            .iter()
            .any(|f| !indexed.iter().any(|(name, _)| name.as_str() == f.field()))
        {
            return Err(Error::new(400, "query.field_not_indexed"));
        }
        schema_checked = true;
    }
    if let Some(cursor) = &query.cursor {
        cursor_from(scope, &query.filters, cursor).map_err(error)?;
    }
    let page = match store
        .search_runs(query.filters, query.cursor, query.limit)
        .await
    {
        Ok(page) => page,
        // A declared indexed field missing from every stored summary cannot
        // match an AND filter. Honor the schema even before its first value exists.
        Err(StoreError::InvalidBatch("query.field_not_indexed")) if schema_checked => {
            return Ok(json!(RunSearchPage {
                items: vec![],
                next_cursor: None
            }));
        }
        Err(e) => return Err(error(e)),
    };
    Ok(json!(RunSearchPage {
        items: page
            .runs
            .into_iter()
            .map(|r| {
                let url = runbook_model::execution_api_url(origin, &r.execution_id);
                RunSearchEntry {
                    web_url: runbook_model::execution_web_url(&url, &r.execution_id),
                    url,
                    id: r.execution_id,
                    runbook_id: r.runbook_id,
                    status: r.status,
                    title: r.title,
                    tags: r.tags,
                    fields: r.fields,
                    created_by: r.created_by,
                    created_at_ms: r.created_at_ms,
                    completed_at_ms: r.completed_at_ms,
                }
            })
            .collect(),
        next_cursor: page.next_cursor
    }))
}
fn error(e: StoreError) -> Error {
    match e {
        StoreError::InvalidBatch(code) => Error::new(400, code),
        StoreError::NotFound => Error::new(404, "resource.not_found"),
        _ => Error::new(503, "storage.unavailable"),
    }
}
