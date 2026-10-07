// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! 慢查询告警（feature = "tracing"）。
//!
//! 本模块**始终编译**，但只有 [`timed`] 是常驻的：feature 关闭时它是
//! 「直接 await」的直通函数（阈值判定、截断、打日志全部被 cfg 掉），
//! 调用点因此不需要任何 `#[cfg]`，也不会多读一次时钟。
//!
//! 阈值来自配置的 `slow_query_ms`（见 [`crate::SqlxConfig::slow_query`]）。
//!
//! **未接入**：事务里的语句走 `transaction.rs` 的四个方法（它们自己套
//! `run_with_timeout`），本模块只覆盖客户端的四个执行方法。

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
    use crate::SqlxConfig;
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

    /// 端到端：真客户端 + 真慢查询 → 捕获到 warn，SQL 已截断且带总长度。
    ///
    /// 这条同时证明调用点真的接上了 [`timed`] —— 把 `query` 里的包装删掉，
    /// 它会红。
    #[tokio::test]
    async fn slow_query_warns_through_the_client() {
        let cfg: SqlxConfig =
            serde_json::from_str(r#"{"url": "sqlite::memory:", "slow_query_ms": 1}"#).unwrap();
        let client = crate::SqlxClient::from_config(cfg).await.unwrap();

        let (capture, _guard) = capture_logs();
        client.query(&slow_sql()).await.unwrap();
        let log = capture.text();

        assert!(log.contains("慢查询"), "没打出慢查询 warn，实际:\n{log}");
        assert!(log.contains("sql_chars="), "warn 要带 SQL 总长度:\n{log}");
        assert!(
            log.contains(&"n".repeat(100)),
            "warn 要带截断后的 SQL 原文:\n{log}"
        );
        assert!(
            !log.contains(&"n".repeat(300)),
            "SQL 必须截断（300 个 n 是原文里的填充）:\n{log}"
        );
    }

    /// 不慢的查询不该打（否则日志会被淹没，这个 feature 等于没开）。
    #[tokio::test]
    async fn fast_query_stays_quiet() {
        let cfg: SqlxConfig =
            serde_json::from_str(r#"{"url": "sqlite::memory:", "slow_query_ms": 600000}"#).unwrap();
        let client = crate::SqlxClient::from_config(cfg).await.unwrap();

        let (capture, _guard) = capture_logs();
        client.query("SELECT 1").await.unwrap();
        assert_eq!(capture.text(), "", "阈值远高于实际耗时，不该打");
    }

    /// 足够慢的语句：递归 CTE 数 20 万行 —— 无库可连也要跑够毫秒级。
    /// 前缀塞满 `n` 是为了让「截断」有可断言的东西。
    fn slow_sql() -> String {
        format!(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 200000) \
             SELECT /* {} */ count(*) FROM c",
            "n".repeat(300)
        )
    }
}
