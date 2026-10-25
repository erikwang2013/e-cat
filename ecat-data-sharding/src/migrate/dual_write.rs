// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 双写保护：hash 重分片/切流期间，调用方按同一分片键同时写旧、新拓扑。
//!
//! - 回填和切流前用 [`DualWrite::before_cutover`]：旧拓扑为主、新拓扑为镜像；
//! - 切流后用 [`DualWrite::after_cutover`]：新拓扑为主、旧拓扑为镜像，
//!   这样 `rollback` 切回旧拓扑时数据仍是最新的。
//!
//! 主拓扑先写，成功后才写镜像：主写失败时两侧都未半写；镜像失败返回
//! `dual-write mirror failed` 错误，不静默吞掉。两次写不在同一事务内，调用方
//! 需重试失败的镜像写。同一分片名在两侧目录里视为同一物理分片，只写一次。
//! `transaction()` 只在主拓扑开启 —— 跨库事务不在本 helper 的范围内，需要
//! 事务内双写的调用方必须自行拆分。
use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};

use super::*;
use crate::ShardRouter;
use crate::client::KeyedClient;

/// 旧/新拓扑的双写入口；只为分片键绑定的单次工作单元负责。
pub struct DualWrite<'a> {
    primary: &'a ShardRouter,
    mirror: &'a ShardRouter,
}

impl<'a> DualWrite<'a> {
    /// 切流前：旧拓扑为主，新拓扑为镜像。
    pub fn before_cutover(old: &'a ShardRouter, new: &'a ShardRouter) -> Self {
        Self {
            primary: old,
            mirror: new,
        }
    }

    /// 切流后：新拓扑为主，旧拓扑为镜像（供回滚时仍有完整数据）。
    pub fn after_cutover(new: &'a ShardRouter, old: &'a ShardRouter) -> Self {
        Self {
            primary: new,
            mirror: old,
        }
    }

    /// 按 `table` 的 `key` 同时绑定两侧目标分片。
    pub fn for_key(&self, table: &str, key: &Value) -> Result<DualWriteClient, ShardError> {
        let (primary_shard, primary) = self.primary.for_key_with_shard(table, key)?;
        let (mirror_shard, mirror) = self.mirror.for_key_with_shard(table, key)?;
        // 未迁移的分片两侧同名：同一物理分片写一次，避免非幂等语句被重复执行。
        let mirror = (primary_shard != mirror_shard).then_some(mirror);
        Ok(DualWriteClient { primary, mirror })
    }
}

/// 绑定到「主 + 可选镜像」分片的执行器：读走主拓扑，写先主后镜像。
pub struct DualWriteClient {
    primary: KeyedClient,
    mirror: Option<KeyedClient>,
}

impl DualWriteClient {
    fn mirror_failure(&self, error: RdbmsError) -> RdbmsError {
        RdbmsError::Database(format!(
            "dual-write mirror failed for table `{}` after the primary write committed: {error}",
            self.primary.table()
        ))
    }
}

#[async_trait]
impl SqlExecutor for DualWriteClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        let affected = self.primary.execute(sql).await?;
        if let Some(mirror) = &self.mirror {
            mirror
                .execute(sql)
                .await
                .map_err(|e| self.mirror_failure(e))?;
        }
        Ok(affected)
    }

    /// 读只走主拓扑；镜像结果不参与返回值。
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.primary.query(sql).await
    }

    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<u64, RdbmsError> {
        let affected = self.primary.execute_with(sql, params).await?;
        if let Some(mirror) = &self.mirror {
            mirror
                .execute_with(sql, params)
                .await
                .map_err(|e| self.mirror_failure(e))?;
        }
        Ok(affected)
    }

    /// 读只走主拓扑；镜像结果不参与返回值。
    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        self.primary.query_with(sql, params).await
    }

    async fn query_write(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        let rows = self.primary.query_write(sql, params).await?;
        if let Some(mirror) = &self.mirror {
            mirror
                .query_write(sql, params)
                .await
                .map_err(|e| self.mirror_failure(e))?;
        }
        Ok(rows)
    }

    /// 主拓扑保留「写 + 读」的原子性；镜像只重放写语句 `first`。
    async fn execute_then_query(
        &self,
        first: &str,
        first_params: &[Value],
        second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        let rows = self
            .primary
            .execute_then_query(first, first_params, second)
            .await?;
        if let Some(mirror) = &self.mirror {
            mirror
                .execute_with(first, first_params)
                .await
                .map_err(|e| self.mirror_failure(e))?;
        }
        Ok(rows)
    }

    fn dialect(&self) -> Dialect {
        self.primary.dialect()
    }
}

#[async_trait]
impl RdbmsClient for DualWriteClient {
    /// 只在主拓扑开启事务；镜像写不在同一事务内。
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        self.primary.transaction().await
    }
}
