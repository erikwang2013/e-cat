// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 双写保护：hash 重分片/切流期间，调用方按同一分片键同时写旧、新拓扑。
//!
//! - 回填和切流前用 [`DualWrite::before_cutover`]：旧拓扑为主、新拓扑为镜像；
//! - 切流后用 [`DualWrite::after_cutover`]：新拓扑为主、旧拓扑为镜像，
//!   这样 `rollback` 切回旧拓扑时数据仍是最新的。
//!
//! 主拓扑先写，成功后才写镜像：主写失败时两侧都未半写；镜像失败返回
//! `dual-write mirror failed` 错误，不静默吞掉。物理同一性只认底层客户端
//! `Arc` 指针相同；URL 和分片名都不是证据。两侧同名但客户端不同时默认报错，
//! 调用方确认后可用 [`DualWrite::assume_distinct`]（两边都写，要求 SQL 幂等/
//! 可重放）或 [`DualWrite::assume_same`]（只写主拓扑）显式选择。
//! 两次写不在同一事务内：镜像失败时主写已提交，不要用 [`DualWriteClient`] 重试
//! （会重复主写）；应通过 [`DualWriteClient::mirror_client`] 重放同一语句，或
//! 在切流前用 `verify()` 对账。主写与镜像写之间的崩溃窗口仍然存在。
//! `transaction()` 只在主拓扑开启 —— 跨库事务不在本 helper 的范围内，需要
//! 事务内双写的调用方必须自行拆分。
use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};

use super::*;
use crate::ShardRouter;
use crate::client::KeyedClient;

/// 同名分片两侧客户端不同时的处理策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SameNamePolicy {
    /// 默认：无法证明同一物理目标，拒绝双写。
    Error,
    /// 调用方断言两侧是不同物理分片：两边都写。
    WriteBoth,
    /// 调用方断言两侧是同一物理分片：只写主拓扑。
    PrimaryOnly,
}

/// 旧/新拓扑的双写入口；只为分片键绑定的单次工作单元负责。
pub struct DualWrite<'a> {
    primary: &'a ShardRouter,
    mirror: &'a ShardRouter,
    same_name: SameNamePolicy,
}

impl<'a> DualWrite<'a> {
    /// 切流前：旧拓扑为主，新拓扑为镜像。
    pub fn before_cutover(old: &'a ShardRouter, new: &'a ShardRouter) -> Self {
        Self {
            primary: old,
            mirror: new,
            same_name: SameNamePolicy::Error,
        }
    }

    /// 切流后：新拓扑为主，旧拓扑为镜像（供回滚时仍有完整数据）。
    pub fn after_cutover(new: &'a ShardRouter, old: &'a ShardRouter) -> Self {
        Self {
            primary: new,
            mirror: old,
            same_name: SameNamePolicy::Error,
        }
    }

    /// 断言同名但客户端不同的分片是不同物理分片：两侧都写。
    /// SQL 必须幂等或可安全重放，否则同一逻辑写会执行两次。
    pub fn assume_distinct(mut self) -> Self {
        self.same_name = SameNamePolicy::WriteBoth;
        self
    }

    /// 断言同名但客户端不同的分片是同一物理分片：只写主拓扑。
    pub fn assume_same(mut self) -> Self {
        self.same_name = SameNamePolicy::PrimaryOnly;
        self
    }

    /// 按 `table` 的 `key` 同时绑定两侧目标分片。
    pub fn for_key(&self, table: &str, key: &Value) -> Result<DualWriteClient, ShardError> {
        let (primary_shard, primary) = self.primary.for_key_with_shard(table, key)?;
        let (mirror_shard, mirror) = self.mirror.for_key_with_shard(table, key)?;
        let mirror = if std::sync::Arc::ptr_eq(primary.client(), mirror.client()) {
            // 同一个客户端实例就是同一物理目标；名字不同也一样。
            None
        } else if primary_shard != mirror_shard {
            // 正常的 hash 重分片：旧、新拓扑落不同分片，必须双写。
            Some(mirror)
        } else {
            match self.same_name {
                SameNamePolicy::Error => {
                    return Err(ShardError::Config(format!(
                        "dual-write: shard `{primary_shard}` has different clients in the old and \
                         new topologies; physical identity is unproven. Call \
                         DualWrite::assume_same() if they are the same physical shard, or \
                         DualWrite::assume_distinct() (requires idempotent/replay-safe SQL) to \
                         write both."
                    )));
                }
                SameNamePolicy::WriteBoth => Some(mirror),
                SameNamePolicy::PrimaryOnly => None,
            }
        };
        Ok(DualWriteClient { primary, mirror })
    }
}

/// 绑定到「主 + 可选镜像」分片的执行器：读走主拓扑，写先主后镜像。
pub struct DualWriteClient {
    primary: KeyedClient,
    mirror: Option<KeyedClient>,
}

impl DualWriteClient {
    /// 镜像分片；`None` 表示已证明同一物理目标或调用方 `assume_same()`。
    ///
    /// 写返回 `dual-write mirror failed` 时主写已提交：不要重试 [`DualWriteClient`]
    /// （会再次执行主写）；在这里取回镜像客户端并重放同一条语句，或切流前
    /// 用 `verify()` 对账。
    pub fn mirror_client(&self) -> Option<&KeyedClient> {
        self.mirror.as_ref()
    }

    fn mirror_failure(&self, error: RdbmsError) -> RdbmsError {
        RdbmsError::Database(format!(
            "dual-write mirror failed for table `{}` after the primary write committed; do not \
             retry through DualWriteClient, replay the same statement on mirror_client(): {error}",
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
