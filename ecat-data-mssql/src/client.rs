// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! [`MssqlClient`]：连接池 + 参数绑定 + [`SqlExecutor`] / [`RdbmsClient`] 实现。
//!
//! 单独成文件（而不是像 `ecat-data-sqlx` 那样放进 `lib.rs`）：本 crate 的执行器
//! 比那边多一个事务 wrapper（tiberius 没有事务对象，得自己造），放一起会顶到
//! 500 行上限。公开路径不变，`lib.rs` 照样 `pub use`。

use crate::bind::Bind;
use crate::cell::cell_to_json;
use crate::config::MssqlConfig;
use crate::pool::MssqlManager;
use async_trait::async_trait;
use deadpool::managed::{Object, Pool, PoolError};
use ecat_data::{
    Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction, TransactionInner,
    run_with_timeout,
};
use serde_json::Value;
use std::time::Duration;
use tiberius::ToSql;

/// 取用时探活/清脏的上限。
///
/// 比建连短得多：探活本该是一次往返的事，超了就是这条连接有问题，该丢弃重建。
///
/// **缺了它会怎样**：面对半开连接（服务端已消失但 TCP 未 RST），探活的 read 会
/// 阻塞到 OS 重传超时（几十秒到分钟级），而 `recycle` 是在 `Pool::get()` 路径上
/// **同步等**的 —— 一次死连接就能把取连接卡到 `acquire_timeout`。
const RECYCLE_TIMEOUT: Duration = Duration::from_secs(5);

/// SQL Server 后端。
///
/// 用 `tiberius-ng`（TDS 驱动）+ `deadpool`（连接池）实现 [`ecat_data::SqlExecutor`]。
/// 驱动细节全部封装在本 crate 内，`tiberius::Client` 类型不外泄
/// （包名 `tiberius-ng`，lib 名 `tiberius`）。
pub struct MssqlClient {
    pool: Pool<MssqlManager>,
    query_timeout: Option<Duration>,
    min_connections: u32,
}

impl MssqlClient {
    /// 从连接串建客户端。两种形态都收：`mssql://user:pass@host:1433/db` 与 ADO 串
    /// （见 [`MssqlConfig::from_str`]）。
    pub async fn connect(url: &str) -> Result<Self, RdbmsError> {
        // 折进 `from_config` 而不是另走一条路：URL 形态与配置形态必须共用同一套
        // 默认值（会话初始化、池参数、超时），否则两条构造路径会静默分叉。
        Self::from_config(MssqlConfig::from_str(url)?).await
    }

    pub async fn from_config(cfg: MssqlConfig) -> Result<Self, RdbmsError> {
        let params = cfg.pool();
        let manager = MssqlManager::new(
            cfg.build_config()?,
            cfg.effective_session_init(),
            params.acquire_timeout,
        );

        // 从 manager 读数而不是直接用 `params.acquire_timeout`：管理着这个值的
        // 只有 [`MssqlManager`] 一处，两处各留一份迟早会分叉（Task 3 把 getter
        // 留出来就是这个用途）。
        let acquire_timeout = manager.acquire_timeout();

        let pool = Pool::builder(manager)
            .max_size(params.max_connections as usize)
            // 三个超时缺一不可，理由见 MssqlConfig 的 create_timeout_secs 与
            // RECYCLE_TIMEOUT。
            .wait_timeout(Some(acquire_timeout))
            .create_timeout(Some(params.create_timeout))
            .recycle_timeout(Some(RECYCLE_TIMEOUT))
            // 设了超时就**必须**给运行时，否则 `build()` 直接失败
            // （`deadpool-0.13.1/src/managed/builder.rs:90-98`）。
            .runtime(deadpool::Runtime::Tokio1)
            .build()
            .map_err(|e| RdbmsError::Config(format!("连接池构建失败: {e}")))?;

        Ok(Self {
            pool,
            query_timeout: params.query_timeout,
            min_connections: params.min_connections,
        })
    }

    /// 用已有池构造客户端。
    ///
    /// 拿的是**裸池**，没有 `MssqlConfig` 可读，因此查询超时与 [`Self::warm_up`]
    /// 在这里都是关的（`query_timeout: None`、`min_connections: 0`）—— 与
    /// `SqlxClient::from_pool` 同理，是 API 形状决定的，不是遗漏。需要两者请用
    /// [`Self::connect`] 或 [`Self::from_config`]。
    ///
    /// 池自身的三个超时不受影响：它们在池被构建时就定下了。
    pub fn from_pool(pool: Pool<MssqlManager>) -> Self {
        Self {
            pool,
            query_timeout: None,
            min_connections: 0,
        }
    }

    /// 预热：取 `min_connections` 条连接再放回，服务启动后立刻处于就绪态。
    /// 在服务启动时调用一次。
    ///
    /// deadpool **没有保底连接**的概念（池里没有后台任务维持下限），所以只能自己
    /// 取一遍 —— 取完放回，池里就真躺着这么多条建好的连接。这与 sqlx 侧不同：
    /// 那边 `min_connections` 由 sqlx 的后台任务异步维护，`warm_up` 只是把
    /// 「异步建满」变成「同步等待建满」。
    pub async fn warm_up(&self) -> Result<(), RdbmsError> {
        let mut guards = Vec::with_capacity(self.min_connections as usize);
        while (guards.len() as u32) < self.min_connections {
            guards.push(self.pool.get().await.map_err(pool_err)?);
        }
        // guards 在此处 drop，连接归还池中
        Ok(())
    }

    /// 池的运行状态（空闲/在用/等待数），供批次 4 的 metrics。
    pub fn pool_status(&self) -> deadpool::Status {
        self.pool.status()
    }
}

/// 取连接失败 → [`RdbmsError::Connection`]：这是「池子给不出连接」，不是 SQL 的问题。
///
/// `Backend` 里装的本来就是本项目的错误（建连失败 / 探活失败），原样透出，
/// 别把它降级成一句字符串。
fn pool_err(e: PoolError<RdbmsError>) -> RdbmsError {
    match e {
        PoolError::Backend(e) => e,
        other => RdbmsError::Connection(other.to_string()),
    }
}

/// tiberius 的执行错误 → [`RdbmsError::Database`]（与 `ecat-data-sqlx` 的
/// `db_err` 同一归类：语句/协议层面的事）。
fn db_err(e: tiberius::error::Error) -> RdbmsError {
    RdbmsError::Database(e.to_string())
}

/// 绑定值的引用切片：`query`/`execute` 收的是 `&[&dyn ToSql]`。
fn to_sql_refs(binds: &[Bind]) -> Vec<&dyn ToSql> {
    binds.iter().map(|b| b as &dyn ToSql).collect()
}

/// tiberius 结果集 → [`Row`]：列名取结果元数据，值走 [`cell_to_json`]。
///
/// 用 `Row::cells()` 一次拿到「列 + 值」的配对，不用 `columns()[i]` 与
/// `get_column_data(i)` 两处索引对齐（对齐错了是静默串列）。
fn rows_to_result(rows: Vec<tiberius::Row>) -> Result<Vec<Row>, RdbmsError> {
    rows.iter()
        .map(|row| {
            let mut columns = Vec::with_capacity(row.columns().len());
            let mut values = Vec::with_capacity(row.columns().len());
            for (col, data) in row.cells() {
                columns.push(col.name().to_string());
                values.push(cell_to_json(data, col.name())?);
            }
            Ok(Row::new(columns, values))
        })
        .collect()
}

#[async_trait]
impl SqlExecutor for MssqlClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        run_with_timeout(self.query_timeout, async {
            let mut conn = self.pool.get().await.map_err(pool_err)?;
            let result = conn.execute(sql, &[]).await.map_err(db_err)?;
            // `total()`：一批多条语句时把各条的行数加总（与 sqlx 的
            // `rows_affected()` 一样只给一个数）。
            Ok(result.total())
        })
        .await
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        run_with_timeout(self.query_timeout, async {
            let mut conn = self.pool.get().await.map_err(pool_err)?;
            let stream = conn.query(sql, &[]).await.map_err(db_err)?;
            // 惰性流必须读到流结束：否则语句没跑完、错误也不会浮现。
            let rows = stream.into_first_result().await.map_err(db_err)?;
            rows_to_result(rows)
        })
        .await
    }

    async fn execute_with(&self, sql: &str, params: &[Value]) -> Result<u64, RdbmsError> {
        run_with_timeout(self.query_timeout, async {
            // 先把 Value 物化成绑定值，再借出引用切片 —— 借用要活过
            // `execute` 的整条调用（见 `bind.rs` 的理由）。
            let binds: Vec<Bind> = params.iter().map(Bind::from_json).collect();
            let refs = to_sql_refs(&binds);

            let mut conn = self.pool.get().await.map_err(pool_err)?;
            let result = conn.execute(sql, &refs).await.map_err(db_err)?;
            Ok(result.total())
        })
        .await
    }

    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        run_with_timeout(self.query_timeout, async {
            let binds: Vec<Bind> = params.iter().map(Bind::from_json).collect();
            let refs = to_sql_refs(&binds);

            let mut conn = self.pool.get().await.map_err(pool_err)?;
            let stream = conn.query(sql, &refs).await.map_err(db_err)?;
            let rows = stream.into_first_result().await.map_err(db_err)?;
            rows_to_result(rows)
        })
        .await
    }

    // `query_write` 不覆写：用默认委托给 `query_with` 的实现。读写分离路由由
    // 上层做，本客户端只有一个池。
    fn dialect(&self) -> Dialect {
        Dialect::Mssql
    }
}

/// 事务 wrapper。
///
/// tiberius 的事务是**连接上的状态**（`begin_transaction` / `commit_transaction`
/// / `rollback_transaction` 都收 `&mut self`），没有 `sqlx::Transaction` 那样的对象，
/// 所以本结构持有整条池连接 —— 连接被持有到事务结束，期间别的取用者拿不到它。
///
/// **drop 语义**：本结构被 drop（即 [`Transaction`] 既没提交也没回滚）时，连接会
/// 带着未关闭的事务直接回到池里。清脏由 `MssqlManager::recycle` 负责 —— 依据就是
/// 连接上的 `in_transaction` 标记，本结构在 drop 时**不需要**做任何事（Drop 里也
/// 没法 await 回滚）。见 [`Self::finish`]。
///
/// **超时语义**：四个执行方法与客户端一样套 [`run_with_timeout`] —— 事务里挂死的
/// 查询一样会永久占住连接，事务不是例外。但超时把查询从半路切断后，**事务状态
/// 不再确定**（服务端可能已执行、可能还在执行、也可能已中止），本结构不会自作
/// 主张回滚，只返回 [`RdbmsError::Timeout`] 交由调用方判断；**调用方应当回滚或
/// 直接丢弃本事务，不要在这个事务上继续执行**。
struct MssqlTransaction {
    conn: Object<MssqlManager>,
    query_timeout: Option<Duration>,
}

impl MssqlTransaction {
    /// 事务结束（提交或回滚成功）后清掉脏标记，归还时 `recycle` 就不会再回滚一次。
    ///
    /// 失败**不清标记**：连接的状态已经不确定，留着让 `recycle` 去处理
    /// （大概率是回滚失败 → 连接被池子丢弃重建）。
    fn finish(&mut self) {
        self.conn.in_transaction = false;
    }
}

#[async_trait]
impl TransactionInner for MssqlTransaction {
    // 四个执行方法与客户端同一套 `run_with_timeout`（超时语义见结构体文档）。
    async fn execute(&mut self, sql: &str) -> Result<u64, RdbmsError> {
        run_with_timeout(self.query_timeout, async {
            let result = self.conn.execute(sql, &[]).await.map_err(db_err)?;
            Ok(result.total())
        })
        .await
    }

    async fn query(&mut self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        run_with_timeout(self.query_timeout, async {
            let stream = self.conn.query(sql, &[]).await.map_err(db_err)?;
            rows_to_result(stream.into_first_result().await.map_err(db_err)?)
        })
        .await
    }

    async fn execute_with(&mut self, sql: &str, params: &[Value]) -> Result<u64, RdbmsError> {
        run_with_timeout(self.query_timeout, async {
            let binds: Vec<Bind> = params.iter().map(Bind::from_json).collect();
            let refs = to_sql_refs(&binds);
            let result = self.conn.execute(sql, &refs).await.map_err(db_err)?;
            Ok(result.total())
        })
        .await
    }

    async fn query_with(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        run_with_timeout(self.query_timeout, async {
            let binds: Vec<Bind> = params.iter().map(Bind::from_json).collect();
            let refs = to_sql_refs(&binds);
            let stream = self.conn.query(sql, &refs).await.map_err(db_err)?;
            rows_to_result(stream.into_first_result().await.map_err(db_err)?)
        })
        .await
    }

    fn dialect(&self) -> Dialect {
        Dialect::Mssql
    }

    async fn commit(&mut self) -> Result<(), RdbmsError> {
        self.conn.commit_transaction().await.map_err(db_err)?;
        self.finish();
        Ok(())
    }

    async fn rollback(&mut self) -> Result<(), RdbmsError> {
        self.conn.rollback_transaction().await.map_err(db_err)?;
        self.finish();
        Ok(())
    }
}

#[async_trait]
impl RdbmsClient for MssqlClient {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        let mut conn = self.pool.get().await.map_err(pool_err)?;
        conn.begin_transaction().await.map_err(db_err)?;
        // 标记打在 begin 成功之后、连接交出去之前，两句之间没有 await ——
        // 不存在「服务端事务开了但标记没打上」的缝隙。之后的一切交给
        // `MssqlManager::recycle`（见 pool.rs 的清脏说明）。
        conn.in_transaction = true;
        Ok(Transaction::with_inner(Box::new(MssqlTransaction {
            conn,
            query_timeout: self.query_timeout,
        })))
    }
}
