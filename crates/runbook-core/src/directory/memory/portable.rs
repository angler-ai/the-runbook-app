//! D3 portable archive slice for the memory directory (staged like the
//! memory execution store; installed only when every adapter staged).
use super::*;
use crate::{
    directory_operations::VariablesRecord,
    portable::{DirectorySlice, ImportOptions, PortableError, directory_commit, directory_slice},
};

pub(crate) struct StagedDirectory(Database);

impl MemoryDirectory {
    pub(crate) fn portable_export(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
        teams: &BTreeSet<String>,
    ) -> Result<(DirectorySlice, Option<VariablesRecord>), PortableError> {
        let db = self.database.borrow();
        let snapshot = db
            .orgs
            .get(org)
            .ok_or(PortableError::Store(crate::StoreError::NotFound))?;
        Ok((
            directory_slice(snapshot, project, teams)?,
            db.variables.get(project).cloned(),
        ))
    }
    pub(crate) fn portable_stage(
        &self,
        slice: &DirectorySlice,
        variables: Option<&VariablesRecord>,
        options: ImportOptions,
    ) -> Result<StagedDirectory, PortableError> {
        let mut db = self.database.borrow().clone();
        let project = &slice.project.id;
        if db
            .projects
            .get(project)
            .is_some_and(|org| org != &slice.organization.id)
        {
            return Err(PortableError::Conflict("import.project_conflict"));
        }
        if db.variables.contains_key(project) {
            return Err(PortableError::TargetNotEmpty);
        }
        let existing = db.orgs.get(&slice.organization.id).cloned();
        let known: BTreeSet<UserId> = slice
            .users
            .iter()
            .filter(|u| db.users.contains_key(&u.id))
            .map(|u| u.id.clone())
            .collect();
        if let Some(commit) = directory_commit(slice, existing.as_ref(), &known, options)?
            && db.apply(commit)? == CommitResult::Conflict
        {
            return Err(PortableError::Conflict("import.directory_conflict"));
        }
        if let Some(variables) = variables {
            db.variables.insert(project.clone(), variables.clone());
        }
        Ok(StagedDirectory(db))
    }
    pub(crate) fn portable_install(&self, staged: StagedDirectory) {
        *self.database.borrow_mut() = staged.0;
    }
}
