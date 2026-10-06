// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! deadpool 连接管理器：建连、会话初始化、取用时的探活与清脏。

use deadpool::managed::{Manager, Metrics, RecycleError, RecycleResult};
use ecat_data::RdbmsError;
use std::ops::{Deref, DerefMut};
use std::time::Duration;
use tiberius::error::Error as TiberiusError;
// 包名是 `tiberius-ng`，lib 名是 `tiberius`（`Cargo.toml` 的 `[lib] name`）。
use tiberius::{Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

/// 池里的连接。所在模块不公开（`lib.rs` 只导出 [`MssqlManager`]），
/// 驱动类型不进本 crate 的公开 API。
///
/// 包一层而不是 `type X = Client<...>`，只为一件事：tiberius 的事务**没有对象**，
/// `begin_transaction` 是**连接上的状态**，所以「这条连接上有没有开着的事务」只能
/// 记在连接自己身上。它是事务 wrapper 被 drop 时留下的唯一线索（见
/// [`Manager::recycle`]）。
///
/// `Deref`/`DerefMut` 到 `Client`：取用方照旧写 `conn.query(...)`。
pub struct MssqlConnection {
    client: Client<Compat<TcpStream>>,
    /// 本 crate 的 `transaction()` 在这条连接上开了事务且尚未结束。
    ///
    /// `pub(crate)`：只有 `client.rs` 的事务 wrapper 会写它。
    pub(crate) in_transaction: bool,
}

// 手写而非 derive：`tiberius::Client` 没有 `Debug`，而池的取用方（含 `Manager`
// 的测试）在 `expect_err` 一类的地方要它。
impl std::fmt::Debug for MssqlConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MssqlConnection")
            .field("in_transaction", &self.in_transaction)
            .finish_non_exhaustive()
    }
}

impl Deref for MssqlConnection {
    type Target = Client<Compat<TcpStream>>;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

impl DerefMut for MssqlConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.client
    }
}

/// 空闲多久之内跳过 `SELECT 1` 探活。
///
/// 每次取用都探活会多一个网络往返，而 SQL Server 的往返不便宜；空闲久了
/// （连接可能已被服务端或中间设备掐掉）才值得真探一次。
const DEFAULT_RECYCLE_IDLE_THRESHOLD: Duration = Duration::from_secs(5);

/// deadpool 的连接管理器。
///
/// 生命周期：`create` 建连并跑会话初始化；每次从池里取连接时 deadpool 调
/// `recycle`，由它决定要不要探活（见 [`Self::recycle`]）。
pub struct MssqlManager {
    config: Config,
    session_init: Vec<String>,
    recycle_idle_threshold: Duration,
    /// 取连接的等待上限。池子不由本结构构造，使用方从这里读数落到
    /// deadpool 的 `PoolBuilder::wait_timeout` 上。
    acquire_timeout: Duration,
}

impl MssqlManager {
    /// `session_init` 一般来自 [`crate::MssqlConfig::effective_session_init`]。
    pub fn new(config: Config, session_init: Vec<String>, acquire_timeout: Duration) -> Self {
        Self {
            config,
            session_init,
            recycle_idle_threshold: DEFAULT_RECYCLE_IDLE_THRESHOLD,
            acquire_timeout,
        }
    }

    /// 改探活阈值（调参用；测试也走这里）。
    pub fn with_recycle_idle_threshold(mut self, d: Duration) -> Self {
        self.recycle_idle_threshold = d;
        self
    }

    /// 取连接的等待上限，交给 [`deadpool::managed::PoolBuilder::wait_timeout`]。
    pub fn acquire_timeout(&self) -> Duration {
        self.acquire_timeout
    }

    /// 这次取用要不要真探活：空闲达到阈值才探。
    ///
    /// deadpool 只在**取用**路径上调 `recycle`，并在通过时把 `metrics.recycled`
    /// 置为当下（`managed/pool.rs` 的 `try_recycle`）—— 所以它到现在的时长就是
    /// 这条连接上次被放行后经过的时间（含在用的那段时间，偏保守）。`None`
    /// 表示刚建好还没进过池子，按 0 算：刚握完手再探一次纯属浪费。
    fn probe_due(&self, metrics: &Metrics) -> bool {
        let idle = metrics.recycled.map(|t| t.elapsed()).unwrap_or_default();
        idle >= self.recycle_idle_threshold
    }
}

/// 回滚未结束的事务。`@@TRANCOUNT` 是会话的嵌套事务计数，> 0 说明有没关掉的事务；
/// 计数为 0 时整条语句是个空操作。**不是** `ROLLBACK` 裸发 —— 没有活动事务时
/// 裸 `ROLLBACK` 会报错。
const ROLLBACK_IF_OPEN: &str = "IF @@TRANCOUNT > 0 ROLLBACK";

/// 探活语句：一次往返同时做两件事。
///
/// - 前半段是兜底清脏（理由见 [`MssqlManager::recycle`]）：调用方绕开
///   `transaction()` 直接在连接上 `BEGIN TRAN` 时，连接上的 `in_transaction`
///   标记是假的，只有服务端知道真相。塞进探活里不额外花一次往返。
/// - 后半段是半开连接的探活本体。
///
/// 两半都跑在同一个批里：`IF` 不产生结果集，`SELECT 1` 产生一个 —— 读流的代码
/// 无需区分（见 [`run_batch`]）。
const PROBE_SQL: &str = "IF @@TRANCOUNT > 0 ROLLBACK; SELECT 1";

/// 发一批 SQL 并把响应读干净。
///
/// [`Client::simple_query`] 返回的是**惰性**的 `QueryStream`（内部只推进到结果
/// 元数据），不消费它语句就没跑完、服务端的错误也不会浮现 —— 探活形同虚设。
/// `into_results` 会把整条响应读到流结束，顺带把连接留在干净状态
/// （无残留 token 污染下一个查询）。
///
/// 用 `simple_query` 而不是 `execute`：后者走 `sp_executesql` RPC，而这批语句
/// 没有参数。
async fn run_batch(client: &mut MssqlConnection, sql: &str) -> Result<(), TiberiusError> {
    client.simple_query(sql).await?.into_results().await?;
    Ok(())
}

/// 会话初始化失败的统一措辞。用 `Database`：语句是服务端拒绝的，
/// 与 `ecat-data-sqlx` 的 `after_connect` 错误归为同一类。
fn init_error(stmt: &str, e: TiberiusError) -> RdbmsError {
    RdbmsError::Database(format!("会话初始化语句 {stmt:?} 失败: {e}"))
}

impl Manager for MssqlManager {
    type Type = MssqlConnection;
    type Error = RdbmsError;

    async fn create(&self) -> Result<Self::Type, Self::Error> {
        let addr = self.config.get_addr();

        let tcp = TcpStream::connect(&addr)
            .await
            .map_err(|e| RdbmsError::Connection(format!("连接 {addr} 失败: {e}")))?;
        // 与 tiberius 文档示例同款：TDS 是请求/响应小包协议，让 Nagle 攒包
        // 只会平白加延迟。
        tcp.set_nodelay(true)
            .map_err(|e| RdbmsError::Connection(format!("设置 {addr} 的 TCP_NODELAY 失败: {e}")))?;

        let mut client = Client::connect(self.config.clone(), tcp.compat_write())
            .await
            .map_err(|e| RdbmsError::Connection(format!("与 {addr} 握手/登录失败: {e}")))?;

        // 会话初始化：任一条失败即整条连接作废（spec §2.7）。少设一条
        // （例如 ARITHABORT 没开）这条连接就带着错的会话语义，池子不能把它发出去。
        //
        // 用 `simple_query` 发 SQL 批，而不是 `execute`（走 sp_executesql RPC）：
        // SET 选项在 RPC/存储过程作用域结束时会被还原，批则作用于会话本身。
        for stmt in &self.session_init {
            client
                .simple_query(stmt.as_str())
                .await
                .map_err(|e| init_error(stmt, e))?
                // 惰性流同样要读干净，否则语句没跑完、DONE 之后的错误看不到。
                .into_results()
                .await
                .map_err(|e| init_error(stmt, e))?;
        }

        Ok(MssqlConnection {
            client,
            in_transaction: false,
        })
    }

    /// 取用时的把关：先清脏（有遗留事务就回滚），再按空闲时长决定要不要探活。
    ///
    /// **清脏为什么必须有**：sqlx 那边靠 `sqlx::Transaction` 在 drop 时自动回滚，
    /// tiberius **没有 Transaction 对象** —— 事务未提交就 drop 掉 wrapper 时，
    /// 连接会带着 `@@TRANCOUNT > 0` 回到池里。脏连接的危害不是「数据没提交」
    /// （那部分别的会话本来就看不到），而是它**攥着锁**、并且会把下一个取用者的
    /// 语句一起吸进这个事务里（那些语句随后可能被谁提交掉，也可能一直挂着）。
    ///
    /// 清脏不看空闲时长（`probe_due`）：标记只在本 crate 的 `transaction()` 里置位，
    /// 一次往返买「池里不会有已知的脏连接」是值得的；而**高频场景下每次取用都探活**
    /// 才是要避免的那个往返（spec §3 的智能探活）。
    async fn recycle(
        &self,
        client: &mut Self::Type,
        metrics: &Metrics,
    ) -> RecycleResult<Self::Error> {
        if client.in_transaction {
            if let Err(e) = run_batch(client, ROLLBACK_IF_OPEN).await {
                // 回滚都失败说明连接已经不可用，交给池子丢掉重建。
                tracing::warn!("回滚遗留事务失败，连接交由池子重建: {e}");
                return Err(RecycleError::Backend(RdbmsError::Connection(format!(
                    "回滚遗留事务失败: {e}"
                ))));
            }
            client.in_transaction = false;
        }

        if !self.probe_due(metrics) {
            // 刚取用过/刚建好：连接不可能是死的，省一个往返。
            return Ok(());
        }

        if let Err(e) = run_batch(client, PROBE_SQL).await {
            // deadpool 只会静默丢掉这条连接重开一条，失败原因必须在这里留痕。
            tracing::warn!("探活（SELECT 1）失败，连接交由池子重建: {e}");
            return Err(RecycleError::Backend(RdbmsError::Connection(format!(
                "探活（SELECT 1）失败: {e}"
            ))));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn manager() -> MssqlManager {
        let config =
            Config::from_ado_string("Server=db.local,1433;Database=app;User Id=sa;Password=pw")
                .unwrap();
        MssqlManager::new(
            config,
            vec!["SET ARITHABORT ON".to_string()],
            Duration::from_secs(30),
        )
    }

    /// `recycled` = 上次取用时刻；`None` 表示刚建好还没进过池子。
    fn metrics(idle: Option<Duration>) -> Metrics {
        Metrics {
            recycled: idle.map(|d| Instant::now() - d),
            ..Metrics::default()
        }
    }

    #[test]
    fn new_keeps_arguments_and_default_threshold() {
        let m = manager();
        assert_eq!(m.session_init, vec!["SET ARITHABORT ON".to_string()]);
        assert_eq!(m.acquire_timeout(), Duration::from_secs(30));
        assert_eq!(m.recycle_idle_threshold, Duration::from_secs(5));
        assert_eq!(m.config.get_addr(), "db.local:1433");
    }

    #[test]
    fn with_recycle_idle_threshold_overrides_default() {
        let m = manager().with_recycle_idle_threshold(Duration::from_millis(50));
        assert_eq!(m.recycle_idle_threshold, Duration::from_millis(50));
    }

    /// 阈值分支：空闲不够久不探活，够久才探。新建的连接（`recycled: None`）
    /// 按空闲 0 算 —— 刚握完手，再探一次纯属浪费往返。
    #[test]
    fn probe_due_follows_idle_threshold() {
        let m = manager();
        assert!(!m.probe_due(&metrics(None)), "新建连接不该探活");
        assert!(!m.probe_due(&metrics(Some(Duration::from_secs(1)))));
        assert!(m.probe_due(&metrics(Some(Duration::from_secs(6)))));
    }

    /// 阈值可调到「每次取用都探活」（0）也能调大。
    #[test]
    fn probe_due_respects_custom_threshold() {
        let always = manager().with_recycle_idle_threshold(Duration::ZERO);
        assert!(always.probe_due(&metrics(None)));

        let rare = manager().with_recycle_idle_threshold(Duration::from_secs(600));
        assert!(!rare.probe_due(&metrics(Some(Duration::from_secs(60)))));
        assert!(rare.probe_due(&metrics(Some(Duration::from_secs(601)))));
    }

    /// 连不上必须映射成 `RdbmsError::Connection`，不能 panic、不能是别的变体。
    /// 先占一个端口再释放，得到一个必然没人监听的地址（不依赖外部环境）。
    #[tokio::test]
    async fn create_maps_connect_failure_to_connection_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let config = Config::from_ado_string(&format!(
            "Server=127.0.0.1,{};User Id=sa;Password=pw",
            addr.port()
        ))
        .unwrap();
        let m = MssqlManager::new(config, vec![], Duration::from_secs(30));

        // 超时只是保险：端口没人监听时 connect 立刻被拒。
        let err = tokio::time::timeout(Duration::from_secs(10), m.create())
            .await
            .expect("connect 不该挂住")
            .expect_err("端口没人监听，create 必须失败");
        match err {
            RdbmsError::Connection(msg) => {
                assert!(
                    msg.contains(&addr.to_string()),
                    "错误信息要带地址，got: {msg}"
                )
            }
            other => panic!("应为 RdbmsError::Connection，got: {other:?}"),
        }
    }
}
