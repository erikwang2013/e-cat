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

#[test]
fn renew_script_compares_token_and_pexpires() {
    for needle in [r#"redis.call("get""#, r#"redis.call("pexpire""#, "ARGV[2]"] {
        assert!(RENEW_SCRIPT.contains(needle), "missing {needle}");
    }
}

#[test]
fn fence_script_increments_before_set_and_uses_nx_px() {
    let incr = FENCE_SCRIPT
        .find(r#"redis.call("incr""#)
        .expect("fence script must INCR the fence key");
    let set = FENCE_SCRIPT
        .find(r#"redis.call("set""#)
        .expect("fence script must SET the lock key");
    assert!(incr < set, "fence must increment before SET");
    for needle in ["KEYS[2]", "ARGV[1]", "ARGV[2]", "\"NX\"", "\"PX\""] {
        assert!(FENCE_SCRIPT.contains(needle), "missing {needle}");
    }
}

#[test]
fn fence_key_is_separate_from_lock_key() {
    assert_eq!(fence_key("job:1"), "ecat:fence:job:1");
    assert_ne!(fence_key("job:1"), "job:1");
}

#[test]
fn ttl_millis_clamps_overflow() {
    assert_eq!(ttl_millis(Duration::from_millis(1500)), 1500);
    assert_eq!(ttl_millis(Duration::MAX), u64::MAX);
}

#[tokio::test]
async fn redis_lock_from_config_reads_query_timeout() {
    let url = spawn_silent_redis().await;
    let lock = RedisLock::connect(&url).await.unwrap();
    assert_eq!(lock.query_timeout, Some(Duration::from_secs(30)));

    let cfg: RedisConfig =
        serde_json::from_str(&format!(r#"{{"url": "{url}", "query_timeout_secs": 5}}"#)).unwrap();
    let lock = RedisLock::from_config(cfg).await.unwrap();
    assert_eq!(lock.query_timeout, Some(Duration::from_secs(5)));

    let cfg: RedisConfig =
        serde_json::from_str(&format!(r#"{{"url": "{url}", "query_timeout_secs": 0}}"#)).unwrap();
    let lock = RedisLock::from_config(cfg).await.unwrap();
    assert_eq!(lock.query_timeout, None, "0 = 禁用超时");
}

/// 外层 5 秒兜底：漏包 `query_timeout` 的后果是永久挂住，兜底把它变成红灯。
async fn assert_lock_times_out<F>(label: &str, fut: F)
where
    F: std::future::Future<Output = Result<(), LockError>>,
{
    let err = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("{label}: 内层超时没开火"))
        .expect_err("装死的后端不该报成功");
    assert!(err.to_string().contains("timed out"), "{label}: {err}");
}

/// 三个锁命令都必须包上 `query_timeout`：假服务端对数据命令与 EVALSHA 都装死。
#[tokio::test]
async fn lock_commands_time_out_when_the_backend_stalls() {
    let url = spawn_silent_redis().await;
    let mut lock = RedisLock::connect(&url).await.unwrap();
    lock.query_timeout = Some(Duration::from_millis(50));

    assert_lock_times_out("acquire", async {
        lock.acquire("k", Duration::from_secs(30)).await.map(|_| ())
    })
    .await;
    assert_lock_times_out("release", lock.release("k", "tok")).await;
    assert_lock_times_out("renew", lock.renew("k", "tok", Duration::from_secs(30))).await;
    assert_lock_times_out("acquire_fenced", async {
        lock.acquire_fenced("k", Duration::from_secs(30))
            .await
            .map(|_| ())
    })
    .await;
    assert_lock_times_out("validate_fence", async {
        lock.validate_fence("k", 1).await.map(|_| ())
    })
    .await;
}

#[tokio::test]
async fn renew_extends_ttl() {
    let Ok(url) = std::env::var("ECAT_TEST_REDIS_URL") else {
        eprintln!("skip renew_extends_ttl: ECAT_TEST_REDIS_URL not set");
        return;
    };
    let lock = RedisLock::connect(&url).await.expect("connect lock");
    let cache = RedisCache::connect(&url).await.expect("connect cache");
    let key = format!("ecat:test:renew:{}", Uuid::new_v4());

    let token = lock
        .acquire(&key, Duration::from_millis(300))
        .await
        .expect("acquire")
        .expect("fresh key must be acquirable");
    tokio::time::sleep(Duration::from_millis(100)).await;
    lock.renew(&key, &token, Duration::from_secs(30))
        .await
        .expect("holder must be able to renew");

    let ttl = cache
        .ttl(&key)
        .await
        .expect("ttl")
        .expect("renewed key must still exist");
    assert!(
        ttl > Duration::from_secs(20),
        "renew must reset the TTL, got {ttl:?}"
    );

    let err = lock
        .renew(&key, "forged-token", Duration::from_secs(30))
        .await
        .expect_err("non-holder must not renew");
    assert!(err.to_string().contains("token mismatch"), "got: {err}");

    lock.release(&key, &token).await.expect("cleanup release");
}

/// Fencing 用例都打真 Redis：Lua 的原子语义无法用假服务端验证。
async fn fence_test_env() -> Option<(RedisLock, RedisCache)> {
    let Ok(url) = std::env::var("ECAT_TEST_REDIS_URL") else {
        eprintln!("skip fence test: ECAT_TEST_REDIS_URL not set");
        return None;
    };
    let lock = RedisLock::connect(&url).await.expect("connect lock");
    let cache = RedisCache::connect(&url).await.expect("connect cache");
    Some((lock, cache))
}

fn fence_test_key() -> String {
    format!("ecat:test:fence:{}", Uuid::new_v4())
}

#[tokio::test]
async fn fence_increases_across_acquire_release_reacquire() {
    let Some((lock, cache)) = fence_test_env().await else {
        return;
    };
    let key = fence_test_key();
    let first = lock
        .acquire_fenced(&key, Duration::from_secs(30))
        .await
        .expect("acquire")
        .expect("fresh key must be acquirable");
    lock.release(&key, &first.token).await.expect("release");
    let second = lock
        .acquire_fenced(&key, Duration::from_secs(30))
        .await
        .expect("reacquire")
        .expect("released key must be acquirable");
    assert!(
        second.fence > first.fence,
        "fence must increase: {} -> {}",
        first.fence,
        second.fence
    );
    lock.release(&key, &second.token).await.expect("cleanup");
    cache.delete(&fence_key(&key)).await.expect("cleanup fence");
}

#[tokio::test]
async fn only_one_concurrent_acquire_wins() {
    let Some((lock, cache)) = fence_test_env().await else {
        return;
    };
    let key = fence_test_key();
    let (a, b) = tokio::join!(
        lock.acquire_fenced(&key, Duration::from_secs(30)),
        lock.acquire_fenced(&key, Duration::from_secs(30))
    );
    let guards: Vec<FenceGuard> = [a.expect("a"), b.expect("b")]
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(guards.len(), 1, "exactly one concurrent acquirer must win");
    let guard = guards.into_iter().next().unwrap();
    lock.release(&key, &guard.token).await.expect("release");
    let next = lock
        .acquire_fenced(&key, Duration::from_secs(30))
        .await
        .expect("reacquire")
        .expect("released key must be acquirable");
    // 输掉的那次获取也 INCR：两次竞争 + 一次重取 = +2
    assert_eq!(
        next.fence,
        guard.fence + 2,
        "failed acquire must still bump the fence"
    );
    lock.release(&key, &next.token).await.expect("cleanup");
    cache.delete(&fence_key(&key)).await.expect("cleanup fence");
}

#[tokio::test]
async fn stale_fence_is_rejected() {
    let Some((lock, cache)) = fence_test_env().await else {
        return;
    };
    let key = fence_test_key();
    let first = lock
        .acquire_fenced(&key, Duration::from_secs(30))
        .await
        .expect("acquire")
        .expect("fresh key must be acquirable");
    lock.release(&key, &first.token).await.expect("release");
    let second = lock
        .acquire_fenced(&key, Duration::from_secs(30))
        .await
        .expect("reacquire")
        .expect("released key must be acquirable");
    assert!(
        !lock
            .validate_fence(&key, first.fence)
            .await
            .expect("validate stale"),
        "stale fence must be rejected after reacquire"
    );
    assert!(
        lock.validate_fence(&key, second.fence)
            .await
            .expect("validate current"),
        "current fence must validate"
    );
    assert!(
        !lock
            .validate_fence(&key, second.fence + 1)
            .await
            .expect("validate future"),
        "unknown fence must not validate"
    );
    lock.release(&key, &second.token).await.expect("cleanup");
    cache.delete(&fence_key(&key)).await.expect("cleanup fence");
}

#[tokio::test]
async fn renew_does_not_change_fence() {
    let Some((lock, cache)) = fence_test_env().await else {
        return;
    };
    let key = fence_test_key();
    let guard = lock
        .acquire_fenced(&key, Duration::from_secs(30))
        .await
        .expect("acquire")
        .expect("fresh key must be acquirable");
    lock.renew(&key, &guard.token, Duration::from_secs(60))
        .await
        .expect("renew");
    assert!(
        lock.validate_fence(&key, guard.fence)
            .await
            .expect("validate"),
        "renew must not change the fence"
    );
    lock.release(&key, &guard.token).await.expect("cleanup");
    cache.delete(&fence_key(&key)).await.expect("cleanup fence");
}

#[tokio::test]
async fn release_does_not_decrease_fence() {
    let Some((lock, cache)) = fence_test_env().await else {
        return;
    };
    let key = fence_test_key();
    let first = lock
        .acquire_fenced(&key, Duration::from_secs(30))
        .await
        .expect("acquire")
        .expect("fresh key must be acquirable");
    lock.release(&key, &first.token).await.expect("release");
    assert!(
        lock.validate_fence(&key, first.fence)
            .await
            .expect("validate after release"),
        "release must not delete or decrease the fence"
    );
    let second = lock
        .acquire_fenced(&key, Duration::from_secs(30))
        .await
        .expect("reacquire")
        .expect("released key must be acquirable");
    assert!(second.fence > first.fence, "fence must keep increasing");
    assert!(
        !lock
            .validate_fence(&key, first.fence)
            .await
            .expect("validate stale"),
        "old fence must be rejected after reacquire"
    );
    lock.release(&key, &second.token).await.expect("cleanup");
    cache.delete(&fence_key(&key)).await.expect("cleanup fence");
}

/// 假 Redis 服务端：**应答握手、对数据命令装死**。
///
/// 数据命令不回应 ⇒ 调用方在超时前一直挂着。这正是「一个卡死的 Redis GET」。
/// 握手命令必须应答，否则 `get_multiplexed_async_connection` 就卡在建连上了，
/// 测到的是建连超时而不是命令超时。
/// `pub(crate)`：`metrics` 的 `from_config` 自动注册用例也要用它
/// （Redis 的 `from_config` 真建连，不能指向无人监听的地址）。
pub(crate) async fn spawn_silent_redis() -> String {
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
                        // EVALSHA 也装死：锁的 release/renew 走脚本，超时测试要能挂住。
                        let is_data = matches!(
                            cmd.as_str(),
                            "GET"
                                | "MGET"
                                | "SET"
                                | "PSETEX"
                                | "DEL"
                                | "INCR"
                                | "INCRBY"
                                | "TTL"
                                | "EVALSHA"
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
