// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 测试独立成文件：lib.rs 有 500 行硬上限（项目约定，见批次 4 的同类拆分）。
use super::*;
use ecat_circuit_breaker::BreakerState;
use ecat_data::timeout_counter;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn connect_fails_bad_url() {
    let result = RedisCache::connect("redis://nonexistent:9999").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn lock_connect_fails_bad_url() {
    let result = RedisLock::connect("redis://nonexistent:9999").await;
    assert!(result.is_err());
}

#[test]
fn ttl_to_duration_maps_redis_semantics() {
    assert_eq!(ttl_to_duration(-2), None, "missing key");
    assert_eq!(ttl_to_duration(-1), None, "no expiry");
    assert_eq!(ttl_to_duration(0), Some(Duration::ZERO));
    assert_eq!(ttl_to_duration(120), Some(Duration::from_secs(120)));
}

fn arg_bytes(a: redis::Arg<&[u8]>) -> Vec<u8> {
    match a {
        redis::Arg::Simple(bytes) => bytes.to_vec(),
        redis::Arg::Cursor => b"*".to_vec(),
    }
}

#[test]
fn incrby_cmd_targets_key_and_delta() {
    let mut cmd = redis::cmd("INCRBY");
    cmd.arg("rl:key").arg(3i64);
    let args: Vec<Vec<u8>> = cmd.args_iter().map(arg_bytes).collect();
    assert_eq!(
        args,
        vec![b"INCRBY".to_vec(), b"rl:key".to_vec(), b"3".to_vec()]
    );
}

#[test]
fn mget_cmd_targets_all_keys() {
    let mut cmd = redis::cmd("MGET");
    cmd.arg("k1").arg("k2").arg("k3");
    let args: Vec<Vec<u8>> = cmd.args_iter().map(arg_bytes).collect();
    assert_eq!(
        args,
        vec![
            b"MGET".to_vec(),
            b"k1".to_vec(),
            b"k2".to_vec(),
            b"k3".to_vec()
        ]
    );
}

#[test]
fn config_deserializes_with_password() {
    let cfg: RedisConfig =
        serde_json::from_str(r#"{"url": "redis://localhost:6379", "password": "secret"}"#).unwrap();
    assert_eq!(cfg.url, "redis://localhost:6379");
    assert_eq!(cfg.password.as_deref(), Some("secret"));
    assert!(cfg.tls.is_none());
}

#[test]
fn config_missing_url_is_error() {
    let result: Result<RedisConfig, _> = serde_json::from_str(r#"{"password": "x"}"#);
    assert!(result.is_err());
}

fn tls_enabled() -> TlsClientConfig {
    TlsClientConfig {
        ca_cert: None,
        client_cert: None,
        client_key: None,
        skip_verify: Some(true),
    }
}

fn tls_disabled() -> TlsClientConfig {
    TlsClientConfig {
        ca_cert: None,
        client_cert: None,
        client_key: None,
        skip_verify: None,
    }
}

#[test]
fn build_url_swaps_to_rediss_when_tls_enabled() {
    let cfg = RedisConfig {
        url: "redis://localhost:6379".into(),
        password: None,
        tls: Some(tls_enabled()),
        query_timeout_secs: None,
        breaker: None,
    };
    assert_eq!(build_url(&cfg), "rediss://localhost:6379");
}

#[test]
fn build_url_keeps_redis_when_tls_disabled() {
    let cfg = RedisConfig {
        url: "redis://localhost:6379".into(),
        password: None,
        tls: Some(tls_disabled()),
        query_timeout_secs: None,
        breaker: None,
    };
    assert_eq!(build_url(&cfg), "redis://localhost:6379");
}

#[test]
fn build_url_keeps_non_redis_scheme_unchanged() {
    // TLS 只换 redis:// 前缀；非标准 scheme 原样保留
    let cfg = RedisConfig {
        url: "unix:///tmp/redis.sock".into(),
        password: None,
        tls: Some(tls_enabled()),
        query_timeout_secs: None,
        breaker: None,
    };
    assert_eq!(build_url(&cfg), "unix:///tmp/redis.sock");
}

#[tokio::test]
async fn from_config_with_password_path_fails_on_unreachable() {
    // 走 connect_with_password 分支：密码经 ConnectionInfo 传递而非嵌入 URL
    let cfg = RedisConfig {
        url: "redis://127.0.0.1:59999".into(),
        password: Some("pw".into()),
        tls: None,
        query_timeout_secs: None,
        breaker: None,
    };
    // RedisCache 无 Debug，用 match 拿错误文本
    match RedisCache::from_config(cfg).await {
        Err(e) => assert!(!e.to_string().contains("pw"), "password leaked: {e}"),
        Ok(_) => panic!("unreachable redis should fail"),
    }
}

#[tokio::test]
async fn lock_from_config_with_password_path_fails_on_unreachable() {
    let cfg = RedisConfig {
        url: "redis://127.0.0.1:59999".into(),
        password: Some("pw".into()),
        tls: None,
        query_timeout_secs: None,
        breaker: None,
    };
    // RedisLock 无 Debug，用 match 拿错误文本
    match RedisLock::from_config(cfg).await {
        Err(e) => assert!(!e.to_string().contains("pw"), "password leaked: {e}"),
        Ok(_) => panic!("unreachable redis should fail"),
    }
}

/// 假 Redis 服务端：**应答握手、对数据命令装死**。
///
/// 数据命令不回应 ⇒ 调用方在超时前一直挂着。这正是「一个卡死的 Redis GET」。
/// 握手命令必须应答，否则 `get_multiplexed_async_connection` 就卡在建连上了，
/// 测到的是建连超时而不是命令超时。
async fn spawn_silent_redis() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let Ok(n) = tokio::io::AsyncReadExt::read(&mut sock, &mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    // RESP 数组：`*<n>\r\n` 之后是 n 个 `$<len>\r\n<bytes>\r\n`
                    for cmd in drain_commands(&mut buf) {
                        let is_data = matches!(
                            cmd.as_str(),
                            "GET" | "MGET" | "SET" | "PSETEX" | "DEL" | "INCR" | "INCRBY" | "TTL"
                        );
                        if !is_data {
                            let _ =
                                tokio::io::AsyncWriteExt::write_all(&mut sock, b"+OK\r\n").await;
                            let _ = tokio::io::AsyncWriteExt::flush(&mut sock).await;
                        }
                    }
                }
            });
        }
    });
    format!("redis://{addr}")
}

/// 从 `buf` 头部切出完整的 RESP 命令（命令名大写），不完整的留在 `buf` 里。
/// 返回本次切出的命令名列表。
fn drain_commands(buf: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let Some(star) = buf.iter().position(|b| *b == b'*') else {
            return out;
        };
        let Some(nl) = buf[star..].iter().position(|b| *b == b'\n') else {
            return out;
        };
        let argc: usize = match std::str::from_utf8(&buf[star + 1..star + nl])
            .ok()
            .and_then(|s| s.trim_end_matches('\r').parse().ok())
        {
            Some(n) => n,
            None => return out,
        };
        let mut pos = star + nl + 1;
        let mut first: Option<String> = None;
        let mut complete = true;
        for i in 0..argc {
            let Some(nl) = buf[pos..].iter().position(|b| *b == b'\n') else {
                complete = false;
                break;
            };
            let len: usize = match std::str::from_utf8(&buf[pos + 1..pos + nl])
                .ok()
                .and_then(|s| s.trim_end_matches('\r').parse().ok())
            {
                Some(n) => n,
                None => {
                    complete = false;
                    break;
                }
            };
            let start = pos + nl + 1;
            if buf.len() < start + len + 2 {
                complete = false;
                break;
            }
            if i == 0 {
                first = Some(String::from_utf8_lossy(&buf[start..start + len]).to_uppercase());
            }
            pos = start + len + 2;
        }
        if !complete {
            return out;
        }
        if let Some(name) = first {
            out.push(name);
        }
        buf.drain(..pos);
    }
}

/// 建连必须能完成 —— 这一条是后面三条的前提。
/// **若它失败，不要改断言**：把实际错误原样回报给 lead。
#[tokio::test]
async fn connect_succeeds_against_fake_server() {
    let url = spawn_silent_redis().await;
    let cache = RedisCache::connect(&url).await.unwrap();
    assert_eq!(cache.breaker().state(), BreakerState::Closed);
}

/// 超时真的开火（spec §8 判据 2）：假服务端对 GET 永不回应。
#[tokio::test]
async fn get_times_out_with_deadline_exceeded() {
    let url = spawn_silent_redis().await;
    let mut cache = RedisCache::connect(&url).await.unwrap();
    cache.query_timeout = Some(Duration::from_millis(50));

    let before = timeout_counter(BackendKind::Cache).load(Ordering::SeqCst);
    let err = cache.get("k").await.expect_err("GET 不该返回");
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "got: {err}");
    assert!(
        timeout_counter(BackendKind::Cache).load(Ordering::SeqCst) > before,
        "超时必须计入 Cache 维度"
    );
}

/// 熔断真的打开（spec §8 判据 3）：连续超时后**快速失败**，
/// 且不再等满超时 —— 这是「快」的可观测判据，不是只断言 is_err。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let url = spawn_silent_redis().await;
    let mut cache = RedisCache::connect(&url).await.unwrap();
    cache.query_timeout = Some(Duration::from_millis(20));

    for _ in 0..5 {
        let _ = cache.get("k").await;
    }
    assert_eq!(cache.breaker().state(), BreakerState::Open);
    assert_eq!(cache.breaker().opened_total(), 1);

    let start = std::time::Instant::now();
    let err = cache.get("k").await.expect_err("熔断已打开");
    assert!(
        start.elapsed() < Duration::from_millis(20),
        "熔断打开后必须立即返回，实际耗时 {:?}",
        start.elapsed()
    );
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
}

/// 超时与熔断**不是**同一件事：超时由 `run_with_timeout` 报 `DeadlineExceeded`，
/// 熔断打开由映射函数报 `Unavailable`。两者混成一个码，调用方就没法区分
/// 「这次慢」和「后端已经放弃了」。
#[tokio::test]
async fn timeout_and_breaker_open_have_distinct_codes() {
    let url = spawn_silent_redis().await;
    let mut cache = RedisCache::connect(&url).await.unwrap();
    cache.query_timeout = Some(Duration::from_millis(20));
    let err = cache.get("k").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "got: {err}");
    for _ in 0..5 {
        let _ = cache.get("k").await;
    }
    let err = cache.get("k").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
}

/// `query_timeout_secs: 0` 是**禁用**而非「0 秒立刻超时」（全仓约定）。
/// 这一条查的是**配置到字段的接线**：`0` 必须落成 `None`。
/// （不发 GET —— 假服务端对数据命令永不回应，禁用超时下那会永久挂住。）
#[tokio::test]
async fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));

    let url = spawn_silent_redis().await;
    let cfg: RedisConfig =
        serde_json::from_str(&format!(r#"{{"url": "{url}", "query_timeout_secs": 0}}"#)).unwrap();
    let cache = RedisCache::from_config(cfg).await.unwrap();
    assert_eq!(cache.query_timeout, None);
}

/// 一次调用必须在**外层 5 秒内**返回 `DeadlineExceeded`，且推进 Cache 槽。
///
/// 外层 5 秒是**把挂死变成红灯**：漏包 `guarded` 的后果不是报错而是永远不返回
/// （假服务端对数据命令装死，配置的超时是唯一能结束它的东西），
/// 没有这层的话整个测试二进制会卡住而不是 FAILED。
async fn assert_times_out<F>(label: &str, fut: F)
where
    F: std::future::Future<Output = Result<(), Error>>,
{
    let before = timeout_counter(BackendKind::Cache).load(Ordering::SeqCst);
    let err = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("{label}: 内层超时没开火（漏包 guarded？）"))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "{label}: {err}");
    assert!(
        timeout_counter(BackendKind::Cache).load(Ordering::SeqCst) > before,
        "{label}: 超时必须计入 Cache 维度"
    );
}

/// 其余五个方法的 `guarded` 包装也要有端到端覆盖 —— 只测 `get` 的话，
/// 谁漏包一个（比如 `set` 直接直连），现有测试不会红。
/// 假服务端对数据命令一律装死，所以五个方法各自都必须等到自己的超时。
///
/// Cache 槽的断言取**方向**（`> before`）而非 `+1`：`TIMEOUTS` 是进程级静态量，
/// 别的用例在并发推进同一槽（同 `get_times_out_with_deadline_exceeded`）。
///
/// 五条调用正好打满熔断窗口的下限（5 条失败 ⇒ 第 5 条之后才 Open），
/// 所以五条都还能落到后端、都拿到 `DeadlineExceeded`；再多一条就会是 `Unavailable`。
#[tokio::test]
async fn every_cache_method_times_out_when_the_backend_stalls() {
    let url = spawn_silent_redis().await;
    let mut cache = RedisCache::connect(&url).await.unwrap();
    cache.query_timeout = Some(Duration::from_millis(20));

    assert_times_out("set", async {
        cache
            .set("k", b"v", Duration::from_secs(60))
            .await
            .map(|_| ())
    })
    .await;
    assert_times_out("delete", async { cache.delete("k").await }).await;
    assert_times_out("increment", async {
        cache.increment("k", 1).await.map(|_| ())
    })
    .await;
    assert_times_out("ttl", async { cache.ttl("k").await.map(|_| ()) }).await;
    assert_times_out("multi_get", async {
        cache.multi_get(&["k1", "k2"]).await.map(|_| ())
    })
    .await;
}
