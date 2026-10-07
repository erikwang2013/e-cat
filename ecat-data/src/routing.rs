// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig, BreakerState};

use crate::breaker::map_breaker_error;
use crate::dialect::Dialect;
use crate::rdbms::{RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};

/// 一个端点：客户端 + **它自己的**熔断器。
///
/// 熔断器逐端点各一个 —— 不是包在路由外层：那样单个从库故障会误熔断整条链路
/// （spec:225）。经由端点的每次调用都过它自己的熔断器（`transaction()` 除外，
/// 见 [`RdbmsRouting`] 的 `RdbmsClient` 实现）。
struct Endpoint {
    client: Arc<dyn RdbmsClient>,
    breaker: Breaker,
}

impl Endpoint {
    fn new(client: Arc<dyn RdbmsClient>, cfg: &BreakerConfig) -> Self {
        Self {
            client,
            breaker: Breaker::new(cfg.clone()),
        }
    }

    /// 熔断打开 ⇒ 不参与轮询。这是「跳过已熔断端点」的唯一依据（spec:235-236）。
    fn is_available(&self) -> bool {
        self.breaker.state() != BreakerState::Open
    }

    fn dialect(&self) -> Dialect {
        self.client.dialect()
    }

    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.breaker
            .call(|| self.client.execute(sql))
            .await
            .map_err(map_breaker_error)
    }

    async fn execute_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        self.breaker
            .call(|| self.client.execute_with(sql, params))
            .await
            .map_err(map_breaker_error)
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.breaker
            .call(|| self.client.query(sql))
            .await
            .map_err(map_breaker_error)
    }

    async fn query_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.breaker
            .call(|| self.client.query_with(sql, params))
            .await
            .map_err(map_breaker_error)
    }

    async fn query_write(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.breaker
            .call(|| self.client.query_write(sql, params))
            .await
            .map_err(map_breaker_error)
    }

    async fn execute_then_query(
        &self,
        first: &str,
        first_params: &[serde_json::Value],
        second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        self.breaker
            .call(|| self.client.execute_then_query(first, first_params, second))
            .await
            .map_err(map_breaker_error)
    }
}

/// 读写分离路由：写落主库、读落副本轮询，**跳过熔断打开（`Open`）的副本**。
///
/// 只给每个端点包一层熔断是不够的：从库挂掉后轮询仍会把 1/N 的读转过去、
/// 靠熔断快速失败 —— 那不是故障隔离，是**稳定的 1/N 失败率**（spec:235-236）。
pub struct RdbmsRouting {
    primary: Endpoint,
    replicas: Vec<Endpoint>,
    /// 轮询起点；`fetch_add` 即可，无需锁。
    next: AtomicUsize,
    /// 副本全不可用时是否降级读主（默认 `true`）。
    fallback_to_primary: bool,
}

impl RdbmsRouting {
    /// 每个端点一个熔断器，配置取 [`BreakerConfig::default`]。
    pub fn new(primary: Arc<dyn RdbmsClient>, replicas: Vec<Arc<dyn RdbmsClient>>) -> Self {
        Self::with_breaker_config(primary, replicas, BreakerConfig::default())
    }

    /// 显式指定各端点的熔断配置（冷却期、失败率阈值、窗口）。
    pub fn with_breaker_config(
        primary: Arc<dyn RdbmsClient>,
        replicas: Vec<Arc<dyn RdbmsClient>>,
        cfg: BreakerConfig,
    ) -> Self {
        Self {
            primary: Endpoint::new(primary, &cfg),
            replicas: replicas
                .into_iter()
                .map(|client| Endpoint::new(client, &cfg))
                .collect(),
            next: AtomicUsize::new(0),
            fallback_to_primary: true,
        }
    }

    /// 副本全不可用时是否降级读主。默认 `true`；`false` 时读请求报
    /// [`RdbmsError::NoAvailableReplica`]。
    pub fn fallback_to_primary(mut self, yes: bool) -> Self {
        self.fallback_to_primary = yes;
        self
    }

    /// 从 `next` 开始轮询，跳过 `Open` 的副本，返回第一个可用的。
    ///
    /// `None` = 一个可用副本都没有（含「没配副本」）；降级还是报错由调用方决定。
    ///
    /// ⚠️ 已知边界：`Open → HalfOpen` 的冷却转换发生在 `Breaker::call` 里，而这里
    /// **跳过**了 `Open` 的端点 ⇒ 没人再去调它，冷却期过后也不会被重新放行，
    /// 副本恢复后仍被永久排除。修法需要 breaker 侧暴露「冷却期是否已过」。
    fn pick_replica(&self) -> Option<&Endpoint> {
        let n = self.replicas.len();
        if n == 0 {
            return None;
        }
        let start = self.next.fetch_add(1, Ordering::Relaxed);
        (0..n)
            .map(|i| &self.replicas[(start + i) % n])
            .find(|ep| ep.is_available())
    }
}

#[async_trait]
impl SqlExecutor for RdbmsRouting {
    /// 写 → 主库。
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.primary.execute(sql).await
    }

    /// 读 → 副本轮询；全不可用时按 [`RdbmsRouting::fallback_to_primary`] 处置。
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        match self.pick_replica() {
            Some(replica) => replica.query(sql).await,
            None if self.fallback_to_primary => self.primary.query(sql).await,
            None => Err(RdbmsError::NoAvailableReplica),
        }
    }

    /// 写 → 主库。
    async fn execute_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        self.primary.execute_with(sql, params).await
    }

    /// 读 → 副本轮询；全不可用时按 [`RdbmsRouting::fallback_to_primary`] 处置。
    async fn query_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        match self.pick_replica() {
            Some(replica) => replica.query_with(sql, params).await,
            None if self.fallback_to_primary => self.primary.query_with(sql, params).await,
            None => Err(RdbmsError::NoAvailableReplica),
        }
    }

    /// 写路径的返回行查询（`INSERT ... RETURNING`）→ 主库：
    /// 落副本会读到**陈旧数据**。
    async fn query_write(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.primary.query_write(sql, params).await
    }

    /// 两步式插入（MySQL 的 `LAST_INSERT_ID()`）也是写路径 → 主库。
    async fn execute_then_query(
        &self,
        first: &str,
        first_params: &[serde_json::Value],
        second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        self.primary
            .execute_then_query(first, first_params, second)
            .await
    }

    /// 取主库的方言：它是路由对外承诺的写方言。
    fn dialect(&self) -> Dialect {
        self.primary.dialect()
    }
}

#[async_trait]
impl RdbmsClient for RdbmsRouting {
    /// 事务天然读主（规避副本延迟），且**不经熔断**：事务失败由 SQL 层报错，
    /// 熔断器在这里只会把「主库慢」升级成「主库不可用」（spec §2.5 组合顺序）。
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        self.primary.client.transaction().await
    }
}

#[cfg(test)]
mod tests;
