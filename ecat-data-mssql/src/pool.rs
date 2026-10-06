// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! deadpool 连接管理器：建连、会话初始化、取用时的探活。

use deadpool::managed::{Manager, Metrics, RecycleError, RecycleResult};
use ecat_data::RdbmsError;
use std::time::Duration;
use tiberius::error::Error as TiberiusError;
// 包名是 `tiberius-ng`，lib 名是 `tiberius`（`Cargo.toml` 的 `[lib] name`）。
use tiberius::{Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

/// 池里的连接类型。所在模块不公开（`lib.rs` 只导出 [`MssqlManager`]），
/// 驱动类型不进本 crate 的公开 API。
pub type MssqlConnection = Client<Compat<TcpStream>>;

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

/// 真探活：发一条 `SELECT 1` 并把响应读干净。
///
/// [`Client::simple_query`] 返回的是**惰性**的 `QueryStream`（内部只推进到结果
/// 元数据），不消费它语句就没跑完、服务端的错误也不会浮现 —— 探活形同虚设。
/// `into_row` 会走 `into_first_result` → `into_results` 把整条响应读到流结束，
/// 顺带把连接留在干净状态（无残留 token 污染下一个查询）。
async fn probe(client: &mut MssqlConnection) -> Result<(), TiberiusError> {
    client.simple_query("SELECT 1").await?.into_row().await?;
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

        Ok(client)
    }

    async fn recycle(
        &self,
        client: &mut Self::Type,
        metrics: &Metrics,
    ) -> RecycleResult<Self::Error> {
        if !self.probe_due(metrics) {
            // 刚取用过/刚建好：连接不可能是死的，省一个往返。
            return Ok(());
        }

        if let Err(e) = probe(client).await {
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
