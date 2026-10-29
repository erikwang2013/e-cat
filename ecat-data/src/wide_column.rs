// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_errors::Error;

/// Wide-column store client abstraction (e.g. Apache HBase).
///
/// Cells are keyed by `family:qualifier`; values are opaque JSON values whose
/// mapping to bytes is backend-specific.
#[async_trait]
pub trait WideColumnClient: Send + Sync {
    /// Write `cells` to `row` in `table`, creating or merging the row.
    async fn put(
        &self,
        table: &str,
        row: &str,
        cells: &std::collections::BTreeMap<String, serde_json::Value>,
    ) -> Result<(), Error>;

    /// Read every cell of `row`; `None` when the row does not exist.
    async fn get(
        &self,
        table: &str,
        row: &str,
    ) -> Result<Option<std::collections::BTreeMap<String, serde_json::Value>>, Error>;

    /// Scan rows starting at `prefix` (a row-key prefix), returning at most
    /// `limit` rows as `(row key, cells)` pairs.
    async fn scan(
        &self,
        table: &str,
        prefix: &str,
        limit: usize,
    ) -> Result<
        Vec<(
            String,
            std::collections::BTreeMap<String, serde_json::Value>,
        )>,
        Error,
    >;

    /// Delete `row` entirely.
    async fn delete(&self, table: &str, row: &str) -> Result<(), Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Noop;

    #[async_trait]
    impl WideColumnClient for Noop {
        async fn put(
            &self,
            _table: &str,
            _row: &str,
            _cells: &BTreeMap<String, serde_json::Value>,
        ) -> Result<(), Error> {
            Ok(())
        }

        async fn get(
            &self,
            _table: &str,
            _row: &str,
        ) -> Result<Option<BTreeMap<String, serde_json::Value>>, Error> {
            Ok(None)
        }

        async fn scan(
            &self,
            _table: &str,
            _prefix: &str,
            _limit: usize,
        ) -> Result<Vec<(String, BTreeMap<String, serde_json::Value>)>, Error> {
            Ok(Vec::new())
        }

        async fn delete(&self, _table: &str, _row: &str) -> Result<(), Error> {
            Ok(())
        }
    }

    /// The trait must stay object-safe: backends are stored as
    /// `Arc<dyn WideColumnClient>`.
    #[tokio::test]
    async fn trait_is_object_safe() {
        let client: Box<dyn WideColumnClient> = Box::new(Noop);
        let mut cells = BTreeMap::new();
        cells.insert("cf:a".to_string(), serde_json::json!(1));
        client.put("t", "r", &cells).await.unwrap();
        assert!(client.get("t", "r").await.unwrap().is_none());
        assert!(client.scan("t", "", 10).await.unwrap().is_empty());
        client.delete("t", "r").await.unwrap();
    }
}
