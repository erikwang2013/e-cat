// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

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
    /// 配了延迟阈值时再按探针实测值过滤；探针无数据（`None`）视为可用。
    async fn is_available(&self, max_lag: Option<Duration>) -> bool {
        if self.breaker.state() == BreakerState::Open {
            return false;
        }
        match max_lag {
            Some(max) => self.client.replica_lag().await.is_none_or(|lag| lag <= max),
            None => true,
        }
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
    /// 副本实测延迟超过它即跳过；`None` = 不探测（默认，保持原行为）。
    max_replica_lag: Option<Duration>,
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
            max_replica_lag: None,
        }
    }

    /// 副本全不可用时是否降级读主。默认 `true`；`false` 时读请求报
    /// [`RdbmsError::NoAvailableReplica`]。
    pub fn fallback_to_primary(mut self, yes: bool) -> Self {
        self.fallback_to_primary = yes;
        self
    }

    /// 跳过实测延迟超过 `lag` 的副本；延迟由 [`RdbmsClient::replica_lag`]
    /// 探测，探不出结果时不跳过。默认不探测。
    pub fn max_replica_lag(mut self, lag: Duration) -> Self {
        self.max_replica_lag = Some(lag);
        self
    }

    /// 从 `next` 开始轮询，跳过 `Open` 的副本，返回第一个可用的。
    ///
    /// `None` = 一个可用副本都没有（含「没配副本」）；降级还是报错由调用方决定。
    ///
    /// ⚠️ 已知边界：`Open → HalfOpen` 的冷却转换发生在 `Breaker::call` 里，而这里
    /// **跳过**了 `Open` 的端点 ⇒ 没人再去调它，冷却期过后也不会被重新放行，
    /// 副本恢复后仍被永久排除。修法需要 breaker 侧暴露「冷却期是否已过」。
    async fn pick_replica(&self) -> Option<&Endpoint> {
        let n = self.replicas.len();
        if n == 0 {
            return None;
        }
        let start = self.next.fetch_add(1, Ordering::Relaxed);
        for i in 0..n {
            let ep = &self.replicas[(start + i) % n];
            if ep.is_available(self.max_replica_lag).await {
                return Some(ep);
            }
        }
        None
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
        match self.pick_replica().await {
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
        match self.pick_replica().await {
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

/// 读己之写的显式句柄：读写与事务**全部**落主库，不参与副本轮询。
///
/// 只读副本天然有复制延迟，写完立刻读会读到旧值；需要「读己之写」的场景
/// 用 [`RdbmsRouting::primary_only`] 显式声明，而不是把整条链路改成读主。
pub struct PrimaryOnly<'a> {
    router: &'a RdbmsRouting,
}

impl RdbmsRouting {
    /// 返回读己之写句柄。它只借用路由、不改变路由状态，
    /// 副本轮询与各自熔断器的既有行为不受影响。
    pub fn primary_only(&self) -> PrimaryOnly<'_> {
        PrimaryOnly { router: self }
    }
}

#[async_trait]
impl SqlExecutor for PrimaryOnly<'_> {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.router.primary.execute(sql).await
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.router.primary.query(sql).await
    }

    async fn execute_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        self.router.primary.execute_with(sql, params).await
    }

    async fn query_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.router.primary.query_with(sql, params).await
    }

    async fn query_write(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.router.primary.query_write(sql, params).await
    }

    async fn execute_then_query(
        &self,
        first: &str,
        first_params: &[serde_json::Value],
        second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        self.router
            .primary
            .execute_then_query(first, first_params, second)
            .await
    }

    fn dialect(&self) -> Dialect {
        self.router.primary.dialect()
    }
}

#[async_trait]
impl RdbmsClient for PrimaryOnly<'_> {
    /// 与路由一致：事务走主库客户端，不经熔断。
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        self.router.primary.client.transaction().await
    }
}

#[cfg(test)]
mod tests;
