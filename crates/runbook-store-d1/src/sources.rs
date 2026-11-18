//! CF11: large runbook source out of D1. With a [`SourceBlobs`] store, a
//! draft's or version's source is written to the project's
//! content-addressed blobs (R2 on Workers, beside the snapshots and skill
//! files, keyed by its SHA-256), and its D1 row keeps only the digest and
//! size, with the text column empty. Reads stitch it back, checking the
//! digest. Rows written inline (before 1010, or with no blob store) read as
//! before: a row is external exactly when its digest column is set.
use runbook_core::StoreError;
use std::future::Future;
use std::pin::Pin;

pub type SourceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + 'a>>;

/// The project's content-addressed blobs, as the catalog uses them.
pub trait SourceBlobs {
    /// Store `bytes` under `digest` (SHA-256 of the bytes); idempotent.
    fn put(&self, digest: [u8; 32], bytes: Vec<u8>) -> SourceFuture<'_, ()>;
    fn get(&self, digest: [u8; 32]) -> SourceFuture<'_, Option<Vec<u8>>>;
}

/// A source as one D1 row stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSource {
    /// The text when inline; empty when external.
    pub inline: String,
    pub digest: Option<[u8; 32]>,
    pub size: Option<i64>,
}
impl StoredSource {
    pub fn inline(text: &str) -> Self {
        Self {
            inline: text.to_owned(),
            digest: None,
            size: None,
        }
    }
    pub fn digest_value(&self) -> Option<Vec<u8>> {
        self.digest.map(|d| d.to_vec())
    }
}

pub fn digest(text: &str) -> [u8; 32] {
    runbook_core::portable::sha256(text.as_bytes())
}

/// Write `text` to `sources` (when there is one) and return what the row
/// keeps.
pub async fn store_source(
    sources: Option<&dyn SourceBlobs>,
    text: &str,
) -> Result<StoredSource, StoreError> {
    let Some(sources) = sources else {
        return Ok(StoredSource::inline(text));
    };
    let digest = digest(text);
    sources.put(digest, text.as_bytes().to_vec()).await?;
    Ok(StoredSource {
        inline: String::new(),
        digest: Some(digest),
        size: Some(i64::try_from(text.len()).map_err(|_| StoreError::Backend("d1.source_size"))?),
    })
}

/// A row's source: inline, or fetched by its digest and checked.
pub async fn stitch_source(
    sources: Option<&dyn SourceBlobs>,
    inline: String,
    digest: Option<Vec<u8>>,
) -> Result<String, StoreError> {
    let Some(digest) = digest else {
        return Ok(inline);
    };
    let digest: [u8; 32] = digest
        .try_into()
        .map_err(|_| StoreError::Backend("d1.source_digest"))?;
    let sources = sources.ok_or(StoreError::Backend("d1.source_unavailable"))?;
    let bytes = sources
        .get(digest)
        .await?
        .ok_or(StoreError::Backend("d1.source_missing"))?;
    if runbook_core::portable::sha256(&bytes) != digest {
        return Err(StoreError::Backend("d1.source_corrupt"));
    }
    String::from_utf8(bytes).map_err(|_| StoreError::Backend("d1.source_utf8"))
}

/// An in-memory [`SourceBlobs`] (tests).
#[derive(Clone, Default)]
pub struct MemorySources(
    pub std::rc::Rc<std::cell::RefCell<std::collections::BTreeMap<[u8; 32], Vec<u8>>>>,
);
impl SourceBlobs for MemorySources {
    fn put(&self, digest: [u8; 32], bytes: Vec<u8>) -> SourceFuture<'_, ()> {
        self.0.borrow_mut().insert(digest, bytes);
        Box::pin(async { Ok(()) })
    }
    fn get(&self, digest: [u8; 32]) -> SourceFuture<'_, Option<Vec<u8>>> {
        let found = self.0.borrow().get(&digest).cloned();
        Box::pin(async move { Ok(found) })
    }
}
