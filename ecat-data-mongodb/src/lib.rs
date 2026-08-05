// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{DocumentClient, DocumentError};
use ecat_tls::TlsClientConfig;
use futures_util::TryStreamExt;
use mongodb::bson;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct MongoConfig {
    pub url: String,
    pub database: String,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
}

pub struct MongoClient {
    client: mongodb::Client,
    database: String,
}

impl MongoClient {
    pub async fn from_config(cfg: MongoConfig) -> Result<Self, DocumentError> {
        let client = mongodb::Client::with_uri_str(&cfg.url)
            .await
            .map_err(|e| DocumentError::Other(format!("mongodb connect: {e}")))?;
        Ok(Self {
            client,
            database: cfg.database,
        })
    }
}

#[async_trait]
impl DocumentClient for MongoClient {
    async fn insert(&self, collection: &str, doc: &Value) -> Result<String, DocumentError> {
        let doc = bson::to_document(doc)
            .map_err(|e| DocumentError::Other(format!("mongodb bson: {e}")))?;
        let result = self
            .client
            .database(&self.database)
            .collection::<bson::Document>(collection)
            .insert_one(doc)
            .await
            .map_err(|e| DocumentError::Other(format!("mongodb insert: {e}")))?;
        Ok(result.inserted_id.to_string())
    }

    async fn find(&self, collection: &str, filter: &Value) -> Result<Vec<Value>, DocumentError> {
        let filter = bson::to_document(filter)
            .map_err(|e| DocumentError::Other(format!("mongodb bson: {e}")))?;
        let cursor = self
            .client
            .database(&self.database)
            .collection::<bson::Document>(collection)
            .find(filter)
            .await
            .map_err(|e| DocumentError::Other(format!("mongodb find: {e}")))?;
        let docs: Vec<bson::Document> = cursor
            .try_collect()
            .await
            .map_err(|e| DocumentError::Other(format!("mongodb find: {e}")))?;
        docs.iter()
            .map(|d| {
                serde_json::to_value(d)
                    .map_err(|e| DocumentError::Other(format!("mongodb json: {e}")))
            })
            .collect()
    }

    async fn update(
        &self,
        collection: &str,
        filter: &Value,
        update: &Value,
    ) -> Result<u64, DocumentError> {
        let filter = bson::to_document(filter)
            .map_err(|e| DocumentError::Other(format!("mongodb bson: {e}")))?;
        let update = bson::to_document(update)
            .map_err(|e| DocumentError::Other(format!("mongodb bson: {e}")))?;
        let result = self
            .client
            .database(&self.database)
            .collection::<bson::Document>(collection)
            .update_many(filter, update)
            .await
            .map_err(|e| DocumentError::Other(format!("mongodb update: {e}")))?;
        Ok(result.modified_count)
    }

    async fn delete(&self, collection: &str, filter: &Value) -> Result<u64, DocumentError> {
        let filter = bson::to_document(filter)
            .map_err(|e| DocumentError::Other(format!("mongodb bson: {e}")))?;
        let result = self
            .client
            .database(&self.database)
            .collection::<bson::Document>(collection)
            .delete_many(filter)
            .await
            .map_err(|e| DocumentError::Other(format!("mongodb delete: {e}")))?;
        Ok(result.deleted_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_deserializes() {
        let cfg: MongoConfig = serde_json::from_value(serde_json::json!({
            "url": "mongodb://localhost:27017",
            "database": "app",
        }))
        .unwrap();
        assert_eq!(cfg.database, "app");
    }

    #[tokio::test]
    async fn from_config_rejects_bad_uri() {
        let result = MongoClient::from_config(MongoConfig {
            url: "not-a-valid-uri".into(),
            database: "app".into(),
            tls: None,
        })
        .await;
        assert!(result.is_err());
    }
}
