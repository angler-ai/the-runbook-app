//! Export and import for the memory adapters. The execution store's bound
//! scope names the project. Import stages every adapter first and installs
//! all three only when each staged cleanly: no partial import.
use super::{ImportOptions, PortableError, ProjectData, normalize, referenced_teams, validate};
use crate::{
    directory::memory::MemoryDirectory, store::memory::MemoryExecutionStore,
    webhooks::memory::MemoryWebhooks,
};

pub fn export(
    store: &MemoryExecutionStore,
    directory: &MemoryDirectory,
    webhooks: &MemoryWebhooks,
) -> Result<ProjectData, PortableError> {
    let scope = store.portable_scope().clone();
    let rows = store.portable_rows()?;
    let (directory, variables) =
        directory.portable_export(&scope.org_id, &scope.project_id, &referenced_teams(&rows))?;
    let mut data = ProjectData {
        rows,
        webhooks: webhooks.portable_rows(&scope),
        scope,
        directory,
        variables,
    };
    normalize(&mut data);
    Ok(data)
}

/// Import into the store's bound project, which must be the archive's and
/// hold no data. The archive is validated first (it is untrusted).
pub fn import(
    store: &MemoryExecutionStore,
    directory: &MemoryDirectory,
    webhooks: &MemoryWebhooks,
    data: &ProjectData,
    options: ImportOptions,
) -> Result<(), PortableError> {
    if store.portable_scope() != &data.scope {
        return Err(PortableError::Conflict("import.scope_mismatch"));
    }
    validate(data)?;
    let staged_directory =
        directory.portable_stage(&data.directory, data.variables.as_ref(), options)?;
    let staged_store = store.portable_stage(&data.rows)?;
    let staged_webhooks = webhooks.portable_stage(&data.scope, &data.webhooks)?;
    directory.portable_install(staged_directory);
    store.portable_install(staged_store);
    webhooks.portable_install(staged_webhooks);
    Ok(())
}
