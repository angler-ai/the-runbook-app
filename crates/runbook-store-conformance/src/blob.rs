use crate::*;
use runbook_core::blob::*;
pub async fn blob_roundtrip_preserves_chunk_boundaries<F: BlobFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.blobs(execution::projection()).await?;
    let foreign = f.foreign_blobs(&s, scope(2)).await?;
    for n in [0, BLOB_CHUNK_BYTES, BLOB_CHUNK_BYTES + 1] {
        let key = BlobKey::Execution {
            execution_id: execution::projection().execution_id,
            name: format!("evidence/{n}"),
        };
        let bytes: Vec<_> = (0..n).map(|i| (i % 251) as u8).collect();
        s.put(key.clone(), &bytes).await?;
        assert_eq!(s.get(&key).await?, bytes);
        assert_eq!(foreign.get(&key).await, Err(StoreError::NotFound));
        assert!(foreign.put(key.clone(), b"overwrite").await.is_err());
        assert!(foreign.delete(&key).await.is_err());
        assert_eq!(s.get(&key).await?, bytes);
        s.delete(&key).await?;
        assert_eq!(s.get(&key).await, Err(StoreError::NotFound));
    }
    assert!(
        s.list_prefix(&BlobPrefix::Execution(execution::projection().execution_id))
            .await?
            .is_empty()
    );
    Ok(())
}
pub async fn shared_blobs_are_separate_and_immutable<F: BlobFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.blobs(execution::projection()).await?;
    let q = f.foreign_blobs(&s, scope(2)).await?;
    let key = BlobKey::Shared { hash: [1; 32] };
    s.put(key.clone(), b"snapshot").await?;
    s.put(key.clone(), b"snapshot").await?;
    assert!(s.put(key.clone(), b"changed").await.is_err());
    assert_eq!(s.get(&key).await?, b"snapshot");
    assert_eq!(s.list_prefix(&BlobPrefix::Shared).await?, vec![key.clone()]);
    assert_eq!(q.get(&key).await, Err(StoreError::NotFound));
    assert!(q.list_prefix(&BlobPrefix::Shared).await?.is_empty());
    assert!(
        s.list_prefix(&BlobPrefix::Execution(execution::projection().execution_id))
            .await?
            .is_empty()
    );
    Ok(())
}
