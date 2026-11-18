//! STO1: Configuration and builder for object storage backends (GCS, S3).
use object_store::ObjectStore;
use std::{fmt, sync::Arc, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobStoreKind {
    Postgres,
    Gcs,
    S3,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectConfigError(pub &'static str);

impl fmt::Display for ObjectConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ObjectConfigError {}

#[derive(Clone)]
pub struct BlobStoreConfig {
    pub kind: BlobStoreKind,
    pub bucket: Option<String>,
    pub prefix: String,
    pub s3_endpoint: Option<String>,
    pub s3_region: Option<String>,
    pub s3_force_path_style: bool,
    pub sweep_interval: Duration,
    pub sweep_age: Duration,
}

impl Default for BlobStoreConfig {
    fn default() -> Self {
        Self {
            kind: BlobStoreKind::Postgres,
            bucket: None,
            prefix: "runbook/".to_owned(),
            s3_endpoint: None,
            s3_region: None,
            s3_force_path_style: false,
            sweep_interval: Duration::from_secs(3600),
            sweep_age: Duration::from_secs(3600),
        }
    }
}

impl BlobStoreConfig {
    pub fn from_env() -> Result<Self, ObjectConfigError> {
        let store_var = std::env::var("RUNBOOK_BLOB_STORE").ok();
        let kind = match store_var
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            None | Some("postgres") => BlobStoreKind::Postgres,
            Some("gcs") => BlobStoreKind::Gcs,
            Some("s3") => BlobStoreKind::S3,
            Some(_) => return Err(ObjectConfigError("config.invalid_blob_store")),
        };

        let bucket = std::env::var("RUNBOOK_BLOB_BUCKET")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());

        if (kind == BlobStoreKind::Gcs || kind == BlobStoreKind::S3) && bucket.is_none() {
            return Err(ObjectConfigError("config.blob_bucket_required"));
        }

        let prefix = std::env::var("RUNBOOK_BLOB_PREFIX")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "runbook/".to_owned());

        let s3_endpoint = std::env::var("RUNBOOK_S3_ENDPOINT")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());

        let s3_region = std::env::var("RUNBOOK_S3_REGION")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());

        let s3_force_path_style = match std::env::var("RUNBOOK_S3_FORCE_PATH_STYLE")
            .ok()
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some("true" | "1") => true,
            Some("false" | "0") | None => false,
            Some(_) => return Err(ObjectConfigError("config.invalid_s3_force_path_style")),
        };

        let sweep_interval = match std::env::var("RUNBOOK_BLOB_SWEEP_INTERVAL_SECS")
            .ok()
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(val) => {
                let secs: u64 = val
                    .parse()
                    .ok()
                    .filter(|&s| (1..=86400 * 30).contains(&s))
                    .ok_or(ObjectConfigError("config.invalid_blob_sweep_interval"))?;
                Duration::from_secs(secs)
            }
            None => Duration::from_secs(3600),
        };

        let sweep_age = match std::env::var("RUNBOOK_BLOB_SWEEP_AGE_SECS")
            .ok()
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(val) => {
                let secs: u64 = val
                    .parse()
                    .ok()
                    .filter(|&s| s <= 86400 * 365)
                    .ok_or(ObjectConfigError("config.invalid_blob_sweep_age"))?;
                Duration::from_secs(secs)
            }
            None => Duration::from_secs(3600),
        };

        Ok(Self {
            kind,
            bucket,
            prefix,
            s3_endpoint,
            s3_region,
            s3_force_path_style,
            sweep_interval,
            sweep_age,
        })
    }

    /// Build an `Arc<dyn ObjectStore>` from this configuration.
    /// Returns `Ok(None)` when `kind == BlobStoreKind::Postgres`.
    pub fn build_store(&self) -> Result<Option<Arc<dyn ObjectStore>>, ObjectConfigError> {
        match self.kind {
            BlobStoreKind::Postgres => Ok(None),
            BlobStoreKind::Gcs => {
                let bucket = self
                    .bucket
                    .as_deref()
                    .ok_or(ObjectConfigError("config.blob_bucket_required"))?;
                let builder =
                    object_store::gcp::GoogleCloudStorageBuilder::new().with_bucket_name(bucket);
                let store = builder
                    .build()
                    .map_err(|_| ObjectConfigError("config.gcs_build_failed"))?;
                Ok(Some(Arc::new(store)))
            }
            BlobStoreKind::S3 => {
                let bucket = self
                    .bucket
                    .as_deref()
                    .ok_or(ObjectConfigError("config.blob_bucket_required"))?;
                let mut builder =
                    object_store::aws::AmazonS3Builder::new().with_bucket_name(bucket);
                if let Some(region) = &self.s3_region {
                    builder = builder.with_region(region);
                }
                if let Some(endpoint) = &self.s3_endpoint {
                    builder = builder.with_endpoint(endpoint);
                }
                if self.s3_force_path_style {
                    builder = builder.with_virtual_hosted_style_request(false);
                }
                let store = builder
                    .build()
                    .map_err(|_| ObjectConfigError("config.s3_build_failed"))?;
                Ok(Some(Arc::new(store)))
            }
        }
    }

    /// Build an `ObjectStorage` from this configuration.
    /// Returns `Ok(None)` when `kind == BlobStoreKind::Postgres`.
    pub fn build_storage(
        &self,
    ) -> Result<Option<Arc<crate::storage::ObjectStorage>>, ObjectConfigError> {
        let store = self.build_store()?;
        Ok(store.map(|s| Arc::new(crate::storage::ObjectStorage::new(s, self.prefix.clone()))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_to_postgres() {
        let config = BlobStoreConfig::default();
        assert_eq!(config.kind, BlobStoreKind::Postgres);
        assert_eq!(config.prefix, "runbook/");
        assert_eq!(config.sweep_interval, Duration::from_secs(3600));
        assert_eq!(config.sweep_age, Duration::from_secs(3600));
        assert!(config.build_store().unwrap().is_none());
    }

    #[test]
    fn config_validation_errors() {
        // GCS without bucket
        let gcs = BlobStoreConfig {
            kind: BlobStoreKind::Gcs,
            bucket: None,
            ..Default::default()
        };
        assert_eq!(
            gcs.build_store().unwrap_err(),
            ObjectConfigError("config.blob_bucket_required")
        );

        // S3 without bucket
        let s3 = BlobStoreConfig {
            kind: BlobStoreKind::S3,
            bucket: None,
            ..Default::default()
        };
        assert_eq!(
            s3.build_store().unwrap_err(),
            ObjectConfigError("config.blob_bucket_required")
        );
    }
}
