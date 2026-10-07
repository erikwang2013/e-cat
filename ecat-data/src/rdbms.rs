// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;

use crate::dialect::Dialect;

#[derive(Debug, Clone)]
pub struct Row {
    columns: Vec<String>,
    values: Vec<serde_json::Value>,
}

impl Row {
    /// Create a new Row with the given columns and values.
    pub fn new(columns: Vec<String>, values: Vec<serde_json::Value>) -> Self {
        debug_assert_eq!(
            columns.len(),
            values.len(),
            "columns and values must have the same length"
        );
        Self { columns, values }
    }

    pub fn get(&self, col: &str) -> Option<&serde_json::Value> {
        self.columns
            .iter()
            .position(|c| c == col)
            .and_then(|i| self.values.get(i))
    }
}

/// 事务内部实现。后端（sqlx / tiberius）实现它，`Transaction` 转发调用。
#[async_trait]
pub trait TransactionInner: Send {
    async fn execute(&mut self, sql: &str) -> Result<u64, RdbmsError>;
    async fn query(&mut self, sql: &str) -> Result<Vec<Row>, RdbmsError>;
    async fn execute_with(
        &mut self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError>;
    async fn query_with(
        &mut self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError>;
    fn dialect(&self) -> Dialect;
    async fn commit(&mut self) -> Result<(), RdbmsError>;
    async fn rollback(&mut self) -> Result<(), RdbmsError>;
}

/// 无 backing 连接的事务（[`Transaction::new`]）执行 SQL 时的错误文案。
/// 这类事务只能作为空占位，执行任何语句都是编程错误 —— 必须报错而非
/// 静默返回 0 行影响，否则写操作会无声丢失。
const NO_BACKING: &str = "transaction has no backing connection (created via Transaction::new)";

pub struct Transaction {
    committed: bool,
    rolled_back: bool,
    /// 在 `with_inner` 时从 inner 拷贝，避免 `dialect(&self)` 这个同步方法
    /// 需要等待异步锁。
    dialect: Dialect,
    inner: tokio::sync::Mutex<Option<Box<dyn TransactionInner>>>,
}

impl Transaction {
    /// 创建一个无 backing 连接的空事务。
    ///
    /// 只能作为占位符用于「不执行任何语句」的场景；在其中执行 SQL 会返回错误。
    /// 需要真正执行语句时用 [`Transaction::with_inner`] 或后端的 `transaction()`。
    pub fn new() -> Self {
        Self {
            committed: false,
            rolled_back: false,
            dialect: Dialect::Standard,
            inner: tokio::sync::Mutex::new(None),
        }
    }

    pub fn with_inner(inner: Box<dyn TransactionInner>) -> Self {
        let dialect = inner.dialect();
        Self {
            committed: false,
            rolled_back: false,
            dialect,
            inner: tokio::sync::Mutex::new(Some(inner)),
        }
    }

    pub async fn commit(mut self) -> Result<(), RdbmsError> {
        if let Some(inner) = self.inner.get_mut().as_mut() {
            inner.commit().await?;
        }
        self.committed = true;
        Ok(())
    }

    pub async fn rollback(mut self) -> Result<(), RdbmsError> {
        if let Some(inner) = self.inner.get_mut().as_mut() {
            inner.rollback().await?;
        }
        self.committed = false;
        self.rolled_back = true;
        Ok(())
    }
}

impl Default for Transaction {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SqlExecutor for Transaction {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.execute(sql).await,
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.query(sql).await,
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    async fn execute_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.execute_with(sql, params).await,
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    async fn query_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.query_with(sql, params).await,
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    /// 在**调用方已有的事务**里跑两条语句：不另开事务，也不提交 ——
    /// 原子边界归调用方，`insert(&tx, …)` 的语义就是「用我这个事务发这两条」。
    ///
    /// 全程持锁：若在两条语句之间放开，并发的 `tx.execute(…)`（同一条连接上的
    /// 另一个 INSERT）会把 `LAST_INSERT_ID()` 的值换成它插的那条 ——
    /// 正是本方法要挡的静默错值。
    async fn execute_then_query(
        &self,
        first: &str,
        first_params: &[serde_json::Value],
        second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => {
                inner.execute_with(first, first_params).await?;
                inner.query(second).await
            }
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    fn dialect(&self) -> Dialect {
        self.dialect
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        // 这里只记日志与计数：Drop 里无法执行异步回滚，实际回滚依赖
        // 底层 sqlx / tiberius 事务在未提交时 Drop 自动回滚。
        if !self.committed && !self.rolled_back {
            crate::timeout::TRANSACTIONS_LEAKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!("transaction dropped without commit — rolling back");
        }
    }
}

#[async_trait]
pub trait SqlExecutor: Send + Sync {
    /// 执行一条 SQL 语句，返回受影响行数。
    /// 用户提供的值请走 [`SqlExecutor::execute_with`]。
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError>;
    /// 查询多行。用户提供的值请走 [`SqlExecutor::query_with`]。
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError>;
    /// 参数化执行，防注入。无法绑定参数的后端返回错误。
    async fn execute_with(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        Err(RdbmsError::Database(
            "parameterized execute not supported by this backend".into(),
        ))
    }
    /// 参数化查询，防注入。无法绑定参数的后端返回错误。
    async fn query_with(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        Err(RdbmsError::Database(
            "parameterized query not supported by this backend".into(),
        ))
    }
    /// 写路径且需要返回结果（`INSERT ... RETURNING` / `OUTPUT INSERTED`）。
    /// 默认委托给 [`SqlExecutor::query_with`]；只有读写分离路由需要覆写，
    /// 否则写语句会被路由到从库。
    async fn query_write(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.query_with(sql, params).await
    }
    /// 在**同一条连接**上先执行 `first`、再执行 `second`（查询），两者原子。
    ///
    /// 存在的理由：MySQL 的 `LAST_INSERT_ID()` 是**连接作用域**的 ——
    /// 池下两次独立取用可能落到不同连接、取回别的会话刚插入的值（静默错值），
    /// 所以 `insert` 的两步式主键回填必须落在同一条连接上。
    ///
    /// 默认实现返回「不支持」。三种覆写：
    /// - [`Transaction`] —— **直接在自己身上跑两条**（它本来就在一条连接上的事务里）
    /// - 需要两步式的客户端（如 `ecat-data-sqlx` 的 `SqlxClient`）—— **开一个事务，跑完提交**
    /// - 一步式后端（MSSQL 的 `OUTPUT INSERTED`、PG/SQLite 的 `RETURNING`）—— 用不到，不覆写
    async fn execute_then_query(
        &self,
        _first: &str,
        _first_params: &[serde_json::Value],
        _second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        Err(RdbmsError::Database(
            "this backend cannot run two statements atomically on one connection".into(),
        ))
    }
    /// 本执行器背后的数据库方言。
    fn dialect(&self) -> Dialect;
}

#[async_trait]
pub trait RdbmsClient: SqlExecutor {
    async fn transaction(&self) -> Result<Transaction, RdbmsError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RdbmsError {
    #[error("database error: {0}")]
    Database(String),
    #[error("connection error: {0}")]
    Connection(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("timeout: {0}")]
    Timeout(String),
    /// 读写分离路由：副本全部不可用，且未开启降级读主
    /// （[`crate::RdbmsRouting`]）。
    #[error("no available replica")]
    NoAvailableReplica,
}

// 测试独立成文件：本文件紧贴 500 行上限（项目硬规则），内联测试会顶过。
#[cfg(test)]
mod tests;
