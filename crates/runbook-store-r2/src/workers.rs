//! [`ObjectBucket`] over an R2 binding (wasm32 only).
use crate::bucket::{Listing, ObjectBucket};
use runbook_core::StoreError;
use worker::{Bucket, Conditional, Range};

#[derive(Clone)]
pub struct R2Bucket(pub Bucket);

fn backend(code: &'static str) -> impl Fn(worker::Error) -> StoreError {
    move |_| StoreError::Backend(code)
}

impl ObjectBucket for R2Bucket {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        match self.0.get(key).execute().await.map_err(backend("r2.get"))? {
            Some(object) => match object.body() {
                Some(body) => body.bytes().await.map(Some).map_err(backend("r2.get")),
                None => Ok(Some(vec![])),
            },
            None => Ok(None),
        }
    }
    async fn get_range(
        &self,
        key: &str,
        offset: u64,
        length: u64,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        // R2 refuses an empty range; an empty read only needs the object.
        if length == 0 {
            return Ok(self.head(key).await?.map(|_| vec![]));
        }
        let object = self
            .0
            .get(key)
            .range(Range::OffsetWithLength { offset, length })
            .execute()
            .await
            .map_err(backend("r2.get"))?;
        match object {
            Some(object) => match object.body() {
                Some(body) => body.bytes().await.map(Some).map_err(backend("r2.get")),
                None => Ok(Some(vec![])),
            },
            None => Ok(None),
        }
    }
    async fn head(&self, key: &str) -> Result<Option<u64>, StoreError> {
        Ok(self
            .0
            .head(key)
            .await
            .map_err(backend("r2.head"))?
            .map(|object| object.size()))
    }
    async fn put_new(&self, key: &str, bytes: Vec<u8>) -> Result<bool, StoreError> {
        // `etagDoesNotMatch: "*"`: write only when no object has this key.
        let written = self
            .0
            .put(key, bytes)
            .only_if(Conditional {
                etag_matches: None,
                etag_does_not_match: Some("*".into()),
                uploaded_before: None,
                uploaded_after: None,
            })
            .execute()
            .await
            .map_err(backend("r2.put"))?;
        Ok(written.is_some())
    }
    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        self.0.delete(key).await.map_err(backend("r2.delete"))
    }
    async fn list(&self, prefix: &str, cursor: Option<String>) -> Result<Listing, StoreError> {
        let mut request = self.0.list().prefix(prefix);
        if let Some(cursor) = cursor {
            request = request.cursor(cursor);
        }
        let page = request.execute().await.map_err(backend("r2.list"))?;
        Ok(Listing {
            keys: page.objects().iter().map(|o| o.key()).collect(),
            cursor: if page.truncated() {
                page.cursor()
            } else {
                None
            },
        })
    }
}
