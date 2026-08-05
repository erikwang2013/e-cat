// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! S3 / MinIO object storage client (rust-s3, path-style addressing).
//!
//! All operations — including `list()` — run against the rust-s3 HTTP client,
//! which applies a 60-second default request timeout
//! (`Bucket::DEFAULT_REQUEST_TIMEOUT`); a hung server therefore cannot block
//! a call forever. The timeout can be tuned per bucket via
//! `Bucket::with_request_timeout` / `set_request_timeout`.
//!
//! `Credentials::new` runs in the synchronous `from_config` and performs no
//! network I/O (it only reads env vars/credential files), so no
//! `spawn_blocking` wrapper is required.

use async_trait::async_trait;
use ecat_data::{StorageClient, StorageError};
use s3::creds::Credentials;
use s3::{Bucket, Region};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct S3Config {
    pub endpoint: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    #[serde(default)]
    pub tls: Option<bool>,
}

pub struct S3Client {
    region: Region,
    credentials: Credentials,
}

impl S3Client {
    pub fn from_config(cfg: S3Config) -> Result<Self, StorageError> {
        let scheme = if cfg.tls.unwrap_or(false) {
            "https"
        } else {
            "http"
        };
        let endpoint = format!("{scheme}://{}", cfg.endpoint);
        let region = Region::Custom {
            region: cfg.region,
            endpoint: endpoint.clone(),
        };
        let credentials = Credentials::new(
            Some(&cfg.access_key),
            Some(&cfg.secret_key),
            None,
            None,
            None,
        )
        .map_err(|e| StorageError::Other(format!("s3 credentials: {e}")))?;
        Ok(Self {
            region,
            credentials,
        })
    }

    fn bucket(&self, name: &str) -> Result<Box<Bucket>, StorageError> {
        Bucket::new(name, self.region.clone(), self.credentials.clone())
            .map(|b| b.with_path_style())
            .map_err(|e| StorageError::Other(format!("s3 bucket: {e}")))
    }
}

#[async_trait]
impl StorageClient for S3Client {
    async fn put(&self, bucket: &str, key: &str, data: &[u8]) -> Result<(), StorageError> {
        self.bucket(bucket)?
            .put_object(key, data)
            .await
            .map_err(|e| StorageError::Other(format!("s3 put: {e}")))?;
        Ok(())
    }

    async fn get(&self, bucket: &str, key: &str) -> Result<Vec<u8>, StorageError> {
        let resp = self
            .bucket(bucket)?
            .get_object(key)
            .await
            .map_err(|e| StorageError::Other(format!("s3 get: {e}")))?;
        Ok(resp.bytes().to_vec())
    }

    async fn delete(&self, bucket: &str, key: &str) -> Result<(), StorageError> {
        self.bucket(bucket)?
            .delete_object(key)
            .await
            .map_err(|e| StorageError::Other(format!("s3 delete: {e}")))?;
        Ok(())
    }

    /// List object keys under `prefix`.
    ///
    /// Bounded by the rust-s3 client's 60-second default request timeout (see
    /// crate docs), so a stuck server returns an error instead of hanging.
    async fn list(&self, bucket: &str, prefix: &str) -> Result<Vec<String>, StorageError> {
        let pages = self
            .bucket(bucket)?
            .list(prefix.to_string(), None)
            .await
            .map_err(|e| StorageError::Other(format!("s3 list: {e}")))?;
        Ok(pages
            .iter()
            .flat_map(|p| p.contents.iter().map(|o| o.key.clone()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_deserializes() {
        let cfg: S3Config = serde_json::from_value(serde_json::json!({
            "endpoint": "localhost:9000",
            "region": "us-east-1",
            "access_key": "minioadmin",
            "secret_key": "minioadmin",
        }))
        .unwrap();
        assert_eq!(cfg.region, "us-east-1");
    }

    #[test]
    fn client_constructs() {
        let client = S3Client::from_config(S3Config {
            endpoint: "localhost:9000".into(),
            region: "us-east-1".into(),
            access_key: "minioadmin".into(),
            secret_key: "minioadmin".into(),
            tls: None,
        })
        .unwrap();
        assert!(matches!(client.region, Region::Custom { .. }));
    }
}
