//! Who owns what a blob key names. Blobs carry no ownership of their own:
//! an execution's and an upload's tenant is the run's owner, an archive's
//! is its runbook's. On Cloudflare those facts live in D1 (CF3 implements
//! this trait over its directory and catalog); tests use [`MemoryOwners`].
use runbook_core::{StoreError, catalog::TenantScope};
use runbook_model::{ExecutionId, RunbookId};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    future::Future,
    rc::Rc,
};

pub trait BlobOwners: Clone + 'static {
    fn execution_owner(
        &self,
        id: &ExecutionId,
    ) -> impl Future<Output = Result<Option<TenantScope>, StoreError>>;
    fn runbook_owner(
        &self,
        id: &RunbookId,
    ) -> impl Future<Output = Result<Option<TenantScope>, StoreError>>;
    /// Retention deleted `id` in this tenant: its uploads stay readable and
    /// deletable until its daily file expires, but are never written again.
    fn retained(
        &self,
        scope: &TenantScope,
        id: &ExecutionId,
    ) -> impl Future<Output = Result<bool, StoreError>>;
}

#[derive(Default)]
struct Owners {
    executions: BTreeMap<ExecutionId, TenantScope>,
    runbooks: BTreeMap<RunbookId, TenantScope>,
    retained: BTreeSet<(TenantScope, ExecutionId)>,
}

/// In-memory ownership facts for tests.
#[derive(Clone, Default)]
pub struct MemoryOwners(Rc<RefCell<Owners>>);
impl MemoryOwners {
    pub fn own_execution(&self, id: ExecutionId, scope: TenantScope) {
        self.0.borrow_mut().executions.insert(id, scope);
    }
    pub fn own_runbook(&self, id: RunbookId, scope: TenantScope) {
        self.0.borrow_mut().runbooks.insert(id, scope);
    }
    /// Retention deleted the run: it has no owner, its uploads are retained.
    pub fn retain(&self, id: &ExecutionId) {
        let mut owners = self.0.borrow_mut();
        if let Some(scope) = owners.executions.remove(id) {
            owners.retained.insert((scope, id.clone()));
        }
    }
}
impl BlobOwners for MemoryOwners {
    async fn execution_owner(&self, id: &ExecutionId) -> Result<Option<TenantScope>, StoreError> {
        Ok(self.0.borrow().executions.get(id).cloned())
    }
    async fn runbook_owner(&self, id: &RunbookId) -> Result<Option<TenantScope>, StoreError> {
        Ok(self.0.borrow().runbooks.get(id).cloned())
    }
    async fn retained(&self, scope: &TenantScope, id: &ExecutionId) -> Result<bool, StoreError> {
        Ok(self
            .0
            .borrow()
            .retained
            .contains(&(scope.clone(), id.clone())))
    }
}
