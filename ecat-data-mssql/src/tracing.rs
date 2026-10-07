// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! 慢查询告警（feature = "tracing"）。
//!
//! 与 `ecat-data-sqlx` 的同名模块同构：本模块**始终编译**，但只有 [`timed`]
//! 是常驻的 —— feature 关闭时它是「直接 await」的直通函数（判定、截断、打日志
//! 全部被 cfg 掉），调用点因此不需要任何 `#[cfg]`，也不会多读一次时钟。
//!
//! 阈值来自配置的 `slow_query_ms`（见 [`crate::MssqlConfig::slow_query`]）。
//!
//! **未接入**：事务里的语句走 `MssqlTransaction`（`TransactionInner`），
//! 本模块只覆盖客户端的四个执行方法。

use std::time::Duration;

/// 包住一次查询：feature 关闭时直接 await；开启时计时并按时打 warn。
///
/// `threshold` 为 `None` = 不打（未配置，或显式 `slow_query_ms: 0`）。
pub(crate) async fn timed<F, T>(threshold: Option<Duration>, sql: &str, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    #[cfg(not(feature = "tracing"))]
    {
        let _ = (threshold, sql);
        future.await
    }
    #[cfg(feature = "tracing")]
    {
        let started = std::time::Instant::now();
        let out = future.await;
        warn_if_slow(threshold, started.elapsed(), sql);
        out
    }
}

/// 慢查询 warn 里保留的 SQL 字符数上限。
///
/// 参数化 SQL 可以很长，整条进日志会淹没有用信息；前缀已经足够定位是哪条语句
/// （语句类型 + 前几张表都在开头），配合总长度也够判断「是不是动态拼出来的巨物」。
#[cfg(feature = "tracing")]
pub(crate) const SQL_HEAD_CHARS: usize = 200;

/// 超过阈值（且阈值已配置）才 warn。耗时与**截断后**的 SQL 一起打，
/// SQL 字段另附总字符数。
#[cfg(feature = "tracing")]
fn warn_if_slow(threshold: Option<Duration>, elapsed: Duration, sql: &str) {
    let Some(threshold) = threshold else {
        return;
    };
    if elapsed < threshold {
        return;
    }
    tracing::warn!(
        elapsed_ms = elapsed.as_millis() as u64,
        sql_chars = sql.chars().count(),
        sql = head_of(sql).as_ref(),
        "慢查询：{elapsed:?} 超过阈值 {threshold:?}"
    );
}

/// 截断到前 [`SQL_HEAD_CHARS`] 个**字符**。
///
/// 按字符而不是字节切：多字节 UTF-8 从中间切开既会产生乱码，`&str` 也不允许。
#[cfg(feature = "tracing")]
fn head_of(sql: &str) -> std::borrow::Cow<'_, str> {
    // 每个字符至少 1 字节 ⇒ 字节数不超限就一定不用截断（快路径不数 char）。
    if sql.len() <= SQL_HEAD_CHARS || sql.chars().count() <= SQL_HEAD_CHARS {
        return std::borrow::Cow::Borrowed(sql);
    }
    std::borrow::Cow::Owned(sql.chars().take(SQL_HEAD_CHARS).collect())
}

#[cfg(all(test, feature = "tracing"))]
mod tests {
    use super::*;
    use ecat_data::SqlExecutor;
    use std::io;
    use std::sync::{Arc, Mutex};

    /// 把 tracing 的输出收进内存，供断言用。
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
        }
    }

    impl io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// 装一个把日志写进 `capture` 的 subscriber（线程局部，跑完自动摘掉）。
    fn capture_logs() -> (Capture, tracing::subscriber::DefaultGuard) {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_ansi(false)
            .finish();
        (capture, tracing::subscriber::set_default(subscriber))
    }

    #[test]
    fn head_of_keeps_short_sql_verbatim() {
        let sql = "SELECT 1";
        assert!(matches!(head_of(sql), std::borrow::Cow::Borrowed(_)));
        assert_eq!(head_of(sql), sql);
    }

    /// 多字节字符按**字符**截断：字节数远超上限但字符数没超时，一个字符都不该动。
    #[test]
    fn head_of_counts_chars_not_bytes() {
        let at_limit = "中".repeat(SQL_HEAD_CHARS);
        assert!(at_limit.len() > SQL_HEAD_CHARS, "前提：字节数超限");
        assert!(
            matches!(head_of(&at_limit), std::borrow::Cow::Borrowed(_)),
            "字符数刚好等于上限，不该截断"
        );
        assert_eq!(head_of(&at_limit), at_limit);

        let longer = at_limit.clone() + "再多一个字";
        let head = head_of(&longer);
        assert_eq!(head.chars().count(), SQL_HEAD_CHARS, "截断按字符数算");
        assert_eq!(head, at_limit, "截出来的必须是完整字符，不是切开的半个");
    }

    #[test]
    fn head_of_truncates_long_sql() {
        let sql = "SELECT ".to_string() + &"x".repeat(1000);
        let head = head_of(&sql);
        assert_eq!(head.chars().count(), SQL_HEAD_CHARS);
        assert!(sql.starts_with(head.as_ref()));
    }

    /// 阈值边界：等于阈值算超（`>=`），未配置则永不报。
    #[tokio::test]
    async fn warn_respects_threshold() {
        let (capture, _guard) = capture_logs();
        let sql = "SELECT 1";
        let threshold = Duration::from_millis(50);

        warn_if_slow(None, Duration::from_secs(10), sql);
        assert_eq!(capture.text(), "", "未配置阈值不该打");

        warn_if_slow(Some(threshold), Duration::from_millis(49), sql);
        assert_eq!(capture.text(), "", "没到阈值不该打");

        warn_if_slow(Some(threshold), threshold, sql);
        assert!(capture.text().contains("慢查询"), "等于阈值应当打");
    }

    /// [`timed`] 的直通语义：不超阈值不出声，超了才出声。
    #[tokio::test]
    async fn timed_warns_only_when_over_threshold() {
        let (capture, _guard) = capture_logs();

        let out: Result<u64, ecat_data::RdbmsError> =
            timed(Some(Duration::from_secs(30)), "SELECT 1", async { Ok(7) }).await;
        assert_eq!(out.unwrap(), 7, "直通不改写返回值");
        assert_eq!(capture.text(), "");

        let text = long_sql();
        let out: Result<u64, ecat_data::RdbmsError> =
            timed(Some(Duration::from_millis(1)), &text, async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok(7)
            })
            .await;
        assert_eq!(out.unwrap(), 7);
        let log = capture.text();
        assert!(log.contains("慢查询"), "超阈值该打，实际:\n{log}");
        assert!(log.contains("sql_chars="), "warn 要带 SQL 总长度:\n{log}");
        assert!(
            !log.contains(&"x".repeat(300)),
            "SQL 必须截断（300 个 x 是原文里的填充）:\n{log}"
        );
    }

    /// 端到端：真客户端跑一次**必然慢到超阈值**的调用 —— 连一个只 accept 不回话的
    /// 本地端口，TDS 握手一直等，直到查询超时（1 秒）把它切断。这条同时证明
    /// 调用点真的接上了 [`timed`]：把 `MssqlClient::query` 里的包装删掉，它会红。
    ///
    /// 切断用**查询超时**而不是建连超时：后者会经 `pool_err` 走
    /// `PoolError::Timeout`，给同一进程里的 `ecat_rdbms_pool_timeouts_total`
    /// 计数 —— `metrics` 的用例要钉那个计数器的精确值，并行跑时会被顶掉。
    #[tokio::test]
    async fn slow_call_warns_through_the_client() {
        // 收下连接后挂住不放：TCP 握手由内核完成，之后的 TDS prelogin 等不到回应。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let stall = tokio::spawn(async move {
            let _sock = listener.accept().await.unwrap().0;
            std::future::pending::<()>().await;
        });

        let cfg: crate::MssqlConfig = serde_json::from_str(&format!(
            r#"{{"url": "mssql://sa:pw@127.0.0.1:{port}/app",
                "query_timeout_secs": 1, "slow_query_ms": 1}}"#
        ))
        .unwrap();
        let client = crate::MssqlClient::from_config(cfg).await.unwrap();

        let (capture, _guard) = capture_logs();
        let err = client.query("SELECT 1").await.unwrap_err();
        stall.abort();

        assert!(
            matches!(err, ecat_data::RdbmsError::Timeout(_)),
            "握手不会完成，查询超时该开火，got: {err:?}"
        );
        let log = capture.text();
        assert!(log.contains("慢查询"), "没打出慢查询 warn，实际:\n{log}");
        assert!(log.contains("sql_chars="), "warn 要带 SQL 总长度:\n{log}");
    }

    /// 长 SQL（前缀塞满 `x`，好让「截断」有可断言的东西）。
    fn long_sql() -> String {
        format!("SELECT /* {} */ * FROM t", "x".repeat(300))
    }
}
