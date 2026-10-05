// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use crate::rdbms::RdbmsError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 查询超时累计次数。`metrics` feature 开启时由后端注册为
/// `ecat_rdbms_query_timeout_total`。
///
/// 用进程级 `AtomicU64` 而非依赖 `ecat-metrics`：本 crate 保持零外部依赖，
/// 指标的读取方按需接入。
pub static QUERY_TIMEOUTS: AtomicU64 = AtomicU64::new(0);

/// 未提交即 Drop 的事务累计数（`Transaction` 的 Drop guard 递增）。
pub static TRANSACTIONS_LEAKED: AtomicU64 = AtomicU64::new(0);

/// 给数据库调用套一层超时。
///
/// `None` 表示禁用超时，直接透传结果。超时发生时递增 [`QUERY_TIMEOUTS`]
/// 并返回 [`RdbmsError::Timeout`]。
///
/// 这是池耗尽的头号防线：`acquire_timeout` 只约束「等连接」，
/// 拿到连接后卡死的查询会一直占着它。
pub async fn run_with_timeout<F, T>(timeout: Option<Duration>, fut: F) -> Result<T, RdbmsError>
where
    F: std::future::Future<Output = Result<T, RdbmsError>>,
{
    match timeout {
        None => fut.await,
        Some(d) => match tokio::time::timeout(d, fut).await {
            Ok(result) => result,
            Err(_) => {
                QUERY_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
                Err(RdbmsError::Timeout(format!("query exceeded {d:?}")))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdbms::RdbmsError;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn none_timeout_passes_result_through() {
        let r: Result<u64, RdbmsError> = run_with_timeout(None, async { Ok(42) }).await;
        assert_eq!(r.unwrap(), 42);
    }

    #[tokio::test]
    async fn fast_future_completes_within_timeout() {
        let r: Result<u64, RdbmsError> =
            run_with_timeout(Some(Duration::from_secs(5)), async { Ok(1) }).await;
        assert_eq!(r.unwrap(), 1);
    }

    #[tokio::test]
    async fn slow_future_times_out_and_counts() {
        let before = QUERY_TIMEOUTS.load(Ordering::SeqCst);
        let r: Result<(), RdbmsError> = run_with_timeout(Some(Duration::from_millis(10)), async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(())
        })
        .await;
        let err = r.unwrap_err();
        assert!(matches!(err, RdbmsError::Timeout(_)), "got: {err:?}");
        assert_eq!(QUERY_TIMEOUTS.load(Ordering::SeqCst), before + 1);
    }
}
