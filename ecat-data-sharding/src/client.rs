// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 键绑定的单分片客户端：六个 `SqlExecutor` 方法与 `transaction` 原样委托，
//! 不做 SQL 解析，也不跨分片。
use std::sync::Arc;

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};
use serde_json::Value;

/// 绑定到某个分片的客户端；由 [`crate::ShardRouter::for_key`] 产生。
pub struct KeyedClient {
    client: Arc<dyn RdbmsClient>,
    table: String,
}

impl KeyedClient {
    pub(crate) fn new(client: Arc<dyn RdbmsClient>, table: String) -> Self {
        Self { client, table }
    }

    /// 本次工作单元所属的表名。
    pub fn table(&self) -> &str {
        &self.table
    }

    /// 底层客户端；双写用它判断两侧是否同一物理目标。
    pub(crate) fn client(&self) -> &Arc<dyn RdbmsClient> {
        &self.client
    }
}

#[async_trait]
impl SqlExecutor for KeyedClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.client.execute(sql).await
    }
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.client.query(sql).await
    }
    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<u64, RdbmsError> {
        self.client.execute_with(sql, params).await
    }
    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        self.client.query_with(sql, params).await
    }
    async fn query_write(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        self.client.query_write(sql, params).await
    }
    async fn execute_then_query(
        &self,
        first: &str,
        first_params: &[Value],
        second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        self.client
            .execute_then_query(first, first_params, second)
            .await
    }
    fn dialect(&self) -> Dialect {
        self.client.dialect()
    }
}

#[async_trait]
impl RdbmsClient for KeyedClient {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        self.client.transaction().await
    }
}
