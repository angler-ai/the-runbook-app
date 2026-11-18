//! The object-store surface the blob store needs: R2 on Workers
//! (`workers.rs`), an in-memory map in tests. Keys are UTF-8 strings.
use runbook_core::StoreError;
use std::{cell::RefCell, collections::BTreeMap, future::Future, rc::Rc};

/// One page of a prefix listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub keys: Vec<String>,
    /// Pass back to continue; `None` when the listing is complete.
    pub cursor: Option<String>,
}

pub trait ObjectBucket: Clone + 'static {
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>>;
    /// `length` bytes from `offset` (both inside the object), `None` when
    /// the key is absent.
    fn get_range(
        &self,
        key: &str,
        offset: u64,
        length: u64,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>>;
    /// The object's size, `None` when absent.
    fn head(&self, key: &str) -> impl Future<Output = Result<Option<u64>, StoreError>>;
    /// Store `bytes` only if the key does not exist yet; `false` when it did
    /// (nothing written). Atomic on R2 (a conditional put).
    fn put_new(&self, key: &str, bytes: Vec<u8>) -> impl Future<Output = Result<bool, StoreError>>;
    fn delete(&self, key: &str) -> impl Future<Output = Result<(), StoreError>>;
    fn list(
        &self,
        prefix: &str,
        cursor: Option<String>,
    ) -> impl Future<Output = Result<Listing, StoreError>>;
}

/// An in-memory bucket for tests (pages of [`MemoryBucket::PAGE`] keys).
/// It counts the operations made on it (each one an R2 subrequest on
/// Workers; UI-1b's list budget checks it), and can fail every operation
/// from a given count on ([`MemoryBucket::fail_from`]; UI-1c).
#[derive(Clone, Default)]
pub struct MemoryBucket(
    Rc<RefCell<BTreeMap<String, Vec<u8>>>>,
    Rc<std::cell::Cell<u64>>,
    Rc<std::cell::Cell<Option<u64>>>,
);
impl MemoryBucket {
    pub const PAGE: usize = 2;
    /// How many operations (get, range, head, put, delete, list) were made
    /// on this bucket and its clones so far.
    pub fn operations(&self) -> u64 {
        self.1.get()
    }
    /// UI-1c: once [`MemoryBucket::operations`] has reached `operations`,
    /// every further operation fails (not counted), as every subrequest
    /// past the Workers per-invocation limit does. `None` clears it.
    pub fn fail_from(&self, operations: Option<u64>) {
        self.2.set(operations);
    }
    fn count(&self) -> Result<(), StoreError> {
        if self.2.get().is_some_and(|limit| self.1.get() >= limit) {
            return Err(StoreError::Backend("r2.injected_failure"));
        }
        self.1.set(self.1.get() + 1);
        Ok(())
    }
    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }
    pub fn keys(&self) -> Vec<String> {
        self.0.borrow().keys().cloned().collect()
    }
}
impl ObjectBucket for MemoryBucket {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.count()?;
        Ok(self.0.borrow().get(key).cloned())
    }
    async fn get_range(
        &self,
        key: &str,
        offset: u64,
        length: u64,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        self.count()?;
        let bytes = self.0.borrow();
        let Some(bytes) = bytes.get(key) else {
            return Ok(None);
        };
        let start = usize::try_from(offset).map_err(|_| StoreError::Backend("r2.range"))?;
        let end = start
            .checked_add(usize::try_from(length).map_err(|_| StoreError::Backend("r2.range"))?)
            .filter(|end| *end <= bytes.len())
            .ok_or(StoreError::Backend("r2.range"))?;
        Ok(Some(bytes[start..end].to_vec()))
    }
    async fn head(&self, key: &str) -> Result<Option<u64>, StoreError> {
        self.count()?;
        Ok(self.0.borrow().get(key).map(|b| b.len() as u64))
    }
    async fn put_new(&self, key: &str, bytes: Vec<u8>) -> Result<bool, StoreError> {
        self.count()?;
        let mut map = self.0.borrow_mut();
        if map.contains_key(key) {
            return Ok(false);
        }
        map.insert(key.to_owned(), bytes);
        Ok(true)
    }
    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        self.count()?;
        self.0.borrow_mut().remove(key);
        Ok(())
    }
    async fn list(&self, prefix: &str, cursor: Option<String>) -> Result<Listing, StoreError> {
        self.count()?;
        let map = self.0.borrow();
        let mut keys: Vec<String> = map
            .keys()
            .filter(|k| k.starts_with(prefix))
            .filter(|k| cursor.as_ref().is_none_or(|c| k.as_str() > c.as_str()))
            .cloned()
            .collect();
        let more = keys.len() > Self::PAGE;
        keys.truncate(Self::PAGE);
        let cursor = more.then(|| keys.last().cloned()).flatten();
        Ok(Listing { keys, cursor })
    }
}
