// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use crate::rdbms::RdbmsError;
use ecat_errors::{Error, ErrorCode};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 未提交即 Drop 的事务累计数（`Transaction` 的 Drop guard 递增）。
pub static TRANSACTIONS_LEAKED: AtomicU64 = AtomicU64::new(0);

/// 出站调用的后端类别，用于把超时计数分维度 ——
/// 否则只有一个总数，看不出是哪个后端在出事。
///
/// 判别值即 [`TIMEOUTS`] 的下标，**顺序不可改**。
///
/// # 选哪个维度：跟 trait 家族走，不跟产品品类走
///
/// 一个产品可以同时属于两个家族：ClickHouse 既实现 `SqlExecutor` / `RdbmsClient`
/// （错误类型 [`RdbmsError`]）又实现 `TsdbClient`（错误类型 [`Error`]）。
/// 判据是**调用点包着哪个 trait**：
///
/// | 调用点包的 trait | `kind` |
/// |---|---|
/// | `SqlExecutor` / `RdbmsClient` | [`Rdbms`](BackendKind::Rdbms) |
/// | `Cache` | [`Cache`](BackendKind::Cache) |
/// | `SearchClient` | [`Search`](BackendKind::Search) |
/// | `GraphClient` | [`Graph`](BackendKind::Graph) |
/// | `DocumentClient` | [`Document`](BackendKind::Document) |
/// | `StorageClient` | [`Storage`](BackendKind::Storage) |
/// | `TsdbClient` | [`Tsdb`](BackendKind::Tsdb) |
///
/// 只有 `Rdbms` 维度被 `ecat-data-sqlx` / `ecat-data-mssql` 的 `metrics` 读成
/// `ecat_rdbms_query_timeout_total`：走 `SqlExecutor` 的调用点若错填 `Tsdb`，
/// 该指标会**静默少数**。`E: TimeoutError` 与 `kind` 是互相独立的两个轴
/// （`Tsdb` + [`RdbmsError`] 照样编译），所以**没有编译期保护** —— 只能靠这条
/// 判据加评审。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Rdbms = 0,
    Cache = 1,
    Search = 2,
    Graph = 3,
    Document = 4,
    Storage = 5,
    Tsdb = 6,
}

impl BackendKind {
    /// `Error::reason` 用的后端短标识，与 [`TIMEOUTS`] 的维度一一对应。
    ///
    /// 粒度取家族（7 维）而非产品名：计数只到家族一级，reason 也到这一级，
    /// 否则同一个槽会散出 `"redis"` / `"cache"` 两种 reason。
    pub fn slug(&self) -> &'static str {
        match self {
            BackendKind::Rdbms => "rdbms",
            BackendKind::Cache => "cache",
            BackendKind::Search => "search",
            BackendKind::Graph => "graph",
            BackendKind::Document => "document",
            BackendKind::Storage => "storage",
            BackendKind::Tsdb => "tsdb",
        }
    }
}

/// 各类后端的累计超时次数，下标即 [`BackendKind`] 的判别值。
///
/// 用进程级静态量而非依赖 `ecat-metrics`：本 crate 保持零外部依赖，
/// 指标的读取方按需接入（各后端的 `metrics` feature）。
///
/// 本量取代了 5.0.0 的 `QUERY_TIMEOUTS`（只覆盖 RDBMS）。
///
/// 取槽用 [`timeout_counter`]，别自己写 `TIMEOUTS[kind as usize]`。
pub static TIMEOUTS: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];

/// 取某类后端的超时计数器。
///
/// 内部是**无 `_` 分支**的 `match`，不用 `TIMEOUTS[kind as usize]`：
/// 加第 8 个变体时这里直接编译失败，而不是静默编译通过、运行期数错槽。
pub fn timeout_counter(kind: BackendKind) -> &'static AtomicU64 {
    match kind {
        BackendKind::Rdbms => &TIMEOUTS[0],
        BackendKind::Cache => &TIMEOUTS[1],
        BackendKind::Search => &TIMEOUTS[2],
        BackendKind::Graph => &TIMEOUTS[3],
        BackendKind::Document => &TIMEOUTS[4],
        BackendKind::Storage => &TIMEOUTS[5],
        BackendKind::Tsdb => &TIMEOUTS[6],
    }
}

/// 可被超时包装的错误类型。
///
/// 六个非 RDBMS trait（`Cache` / `SearchClient` / `GraphClient` /
/// `DocumentClient` / `StorageClient` / `TsdbClient`）共用 `ecat_errors::Error`，
/// RDBMS 路径用 [`RdbmsError`] —— 两个实现就够，不需要六套。
pub trait TimeoutError: Sized {
    /// `kind` 只被 `Error` 那个实现用来填 `reason`（[`BackendKind::slug`]）；
    /// [`RdbmsError::Timeout`] 没有 reason 字段，忽略它。
    fn from_timeout(kind: BackendKind, d: Duration) -> Self;
}

impl TimeoutError for RdbmsError {
    fn from_timeout(_kind: BackendKind, d: Duration) -> Self {
        RdbmsError::Timeout(format!("query exceeded {d:?}"))
    }
}

impl TimeoutError for Error {
    fn from_timeout(kind: BackendKind, d: Duration) -> Self {
        // `reason` 按全仓约定放**组件标识**；「超时」已由 `DeadlineExceeded`
        // 表达，再写 `"timeout"` 会让按 reason 过滤/告警的人漏掉全部超时。
        Error::new(
            ErrorCode::DeadlineExceeded,
            kind.slug(),
            format!("call exceeded {d:?}"),
        )
    }
}

/// 给一次出站调用套一层超时。
///
/// `None` 表示禁用超时，直接透传结果。超时发生时递增 [`timeout_counter`] 的对应
/// 维度，并返回 `E::from_timeout`。
///
/// **`None` 才是禁用；`Some(Duration::ZERO)` 是「立刻超时」**，不是禁用 ——
/// 本仓别处「`0` = 禁用」的惯例（如 `ecat-data-sqlx/src/config.rs` 的
/// `query_timeout_secs`）在这里不适用，别照着填。tokio 先 poll 内层：已就绪的
/// future 仍会成功，只有挂着的才当场判超时。
///
/// 这是池耗尽的头号防线：`acquire_timeout` 只约束「等连接」，
/// 拿到连接后卡死的查询会一直占着它。
pub async fn run_with_timeout<F, T, E>(
    kind: BackendKind,
    timeout: Option<Duration>,
    fut: F,
) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
    E: TimeoutError,
{
    match timeout {
        None => fut.await,
        Some(d) => match tokio::time::timeout(d, fut).await {
            Ok(result) => result,
            Err(_) => {
                timeout_counter(kind).fetch_add(1, Ordering::Relaxed);
                Err(E::from_timeout(kind, d))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdbms::RdbmsError;
    use std::sync::atomic::Ordering;

    /// 该维度的当前值。测试用 `SeqCst`：别的测试也在并发地加同一个槽。
    fn count(kind: BackendKind) -> u64 {
        timeout_counter(kind).load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn none_timeout_passes_result_through() {
        let r: Result<u64, RdbmsError> =
            run_with_timeout(BackendKind::Rdbms, None, async { Ok(42) }).await;
        assert_eq!(r.unwrap(), 42);
    }

    #[tokio::test]
    async fn fast_future_completes_within_timeout() {
        let r: Result<u64, RdbmsError> =
            run_with_timeout(BackendKind::Rdbms, Some(Duration::from_secs(5)), async {
                Ok(1)
            })
            .await;
        assert_eq!(r.unwrap(), 1);
    }

    /// **前提：同二进制里只有本用例写 `Rdbms` 槽。** 精确断言（`== before + 1`）
    /// 依赖它 —— 别处若有用例写 `Rdbms`，它的 `+1` 会插进 `before` 与断言之间
    /// （libtest 并行跑用例）。新增写 `Rdbms` 的用例时：改成方向断言（`> before`），
    /// 或与本案串行。
    #[tokio::test]
    async fn slow_future_times_out_and_counts() {
        let before = count(BackendKind::Rdbms);
        let r: Result<(), RdbmsError> =
            run_with_timeout(BackendKind::Rdbms, Some(Duration::from_millis(10)), async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(())
            })
            .await;
        let err = r.unwrap_err();
        assert!(matches!(err, RdbmsError::Timeout(_)), "got: {err:?}");
        assert_eq!(count(BackendKind::Rdbms), before + 1);
    }

    /// `ecat_errors::Error` 路径：必须是 `DeadlineExceeded` 而不是笼统的 `Internal`，
    /// 否则调用方没法把「超时」与「后端报错」分开处理。`reason` 还要是**组件标识**
    /// （`BackendKind::slug`）—— 全仓约定 reason 放组件名，写 `"timeout"` 会让按
    /// reason 过滤/告警的人漏掉全部超时。
    #[tokio::test]
    async fn error_path_maps_to_deadline_exceeded() {
        let r: Result<(), Error> =
            run_with_timeout(BackendKind::Cache, Some(Duration::from_millis(10)), async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(())
            })
            .await;
        let err = r.unwrap_err();
        assert_eq!(err.code, ErrorCode::DeadlineExceeded, "got: {err:?}");
        assert_eq!(err.reason, "cache", "reason 应为组件标识，got: {err:?}");
    }

    /// 分维度计数**互不串**（spec §8 判据 4）。
    /// 两个维度都真开火，而不是只断言「某维度 += 1」—— 后者在没有分维度时也会过。
    ///
    /// 观测槽取 `Storage` 而非 `Rdbms`：`slow_future_times_out_and_counts`
    /// 同样在等 10ms 后推进 `Rdbms` 槽，两条用例被 libtest 并发调度时窗口重叠，
    /// 拿 `Rdbms` 当观测槽是竞态（实测 12/15 次误红）。`Storage` 无其它写者，
    /// 断言才确定；若实现把超时计到别的槽，这一条仍会红。
    ///
    /// **约束：证人槽必须是同二进制内没有任何其它用例会写的槽**，否则同一个竞态
    /// 复发（写它的用例会把 `+1` 插进 `witness_before` 与断言之间）。加用例前
    /// `grep -rn 'BackendKind::Storage'` 确认无人写它；要写就先换证人槽。
    #[tokio::test]
    async fn timeout_counters_are_per_backend_kind() {
        let witness_before = count(BackendKind::Storage);
        let tsdb_before = count(BackendKind::Tsdb);
        let slow = || async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok::<(), Error>(())
        };
        let _: Result<(), Error> =
            run_with_timeout(BackendKind::Tsdb, Some(Duration::from_millis(10)), slow()).await;
        assert_eq!(count(BackendKind::Tsdb), tsdb_before + 1);
        assert_eq!(
            count(BackendKind::Storage),
            witness_before,
            "Tsdb 的超时不得落到别的槽"
        );
    }
}
