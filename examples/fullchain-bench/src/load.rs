// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 独立进程的压测客户端：每个 worker 一个 HTTP 连接池 / 一条 gRPC Channel。
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ecat_bench::{DetailedBenchReport, run_bench_detailed};
use ecat_data::{Cache, SqlExecutor};
use serde_json::{Value, json};

use crate::pb::bench_client::BenchClient;
use crate::pb::{Key, PingRequest, WriteRequest};
use crate::state::{self, BenchState, ROWS};

type HttpCall = fn(
    reqwest::Client,
    String,
    Arc<AtomicU64>,
) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
type GrpcCall = fn(
    BenchClient<tonic::transport::Channel>,
    Arc<AtomicU64>,
) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// 服务端自报的处理耗时（HTTP header / gRPC 字段）的最大值，用于区分
/// “客户端观测总延迟”与“服务端实际处理时间”。
static MAX_SERVER_US: AtomicU64 = AtomicU64::new(0);
/// 每轮预热请求也走同一个闭包；只有序号 >= warmup 的请求才是计时阶段。
static MEASURE_FROM: AtomicU64 = AtomicU64::new(0);

fn note_server_us(value: u64, sequence: u64) {
    if sequence >= MEASURE_FROM.load(Ordering::Relaxed) {
        MAX_SERVER_US.fetch_max(value, Ordering::Relaxed);
    }
}

fn take_server_max() -> u64 {
    MAX_SERVER_US.swap(0, Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub struct LoadConfig {
    pub transport: String,
    pub op: String,
    pub concurrency: usize,
    pub total: u64,
    pub warmup: u64,
    pub rounds: u32,
    pub threshold_us: u64,
    pub http_base: String,
    pub grpc_endpoint: String,
}

pub async fn run(cfg: &LoadConfig) -> Result<(), String> {
    let mut failed = false;
    for round in 1..=cfg.rounds {
        let report = match (cfg.transport.as_str(), cfg.op.as_str()) {
            ("http", "read") => http_run(cfg, "http-read", round, http_read).await?,
            ("http", "static") => http_run(cfg, "http-static", round, http_static).await?,
            ("http", "write") => http_run(cfg, "http-write", round, http_write).await?,
            ("grpc", "read") => grpc_run(cfg, "grpc-read", round, grpc_read).await?,
            ("grpc", "static") => grpc_run(cfg, "grpc-static", round, grpc_static).await?,
            ("grpc", "write") => grpc_run(cfg, "grpc-write", round, grpc_write).await?,
            (transport, op) => {
                return Err(format!(
                    "unsupported transport/op: {transport}/{op} (want http|grpc × read|write|static)"
                ));
            }
        };
        if report.errors > 0 {
            failed = true;
        }
        println!("{}", report_json(&report, round, cfg, take_server_max()));
        print_human(&report, round, cfg);
    }
    if failed {
        Err("benchmark completed with errors".into())
    } else {
        Ok(())
    }
}

async fn http_run(
    cfg: &LoadConfig,
    name: &str,
    round: u32,
    call: HttpCall,
) -> Result<DetailedBenchReport, String> {
    // 每个 worker 一个连接池：共享 Client 的 hyper 池锁在 8 并发下会出现
    // 数毫秒的分配/唤醒抖动，与 gRPC 每 worker 一条连接的做法对齐。
    let mut clients = Vec::with_capacity(cfg.concurrency.max(1));
    for _ in 0..cfg.concurrency.max(1) {
        clients.push(
            reqwest::Client::builder()
                .pool_max_idle_per_host(1)
                .tcp_nodelay(true)
                .timeout(Duration::from_secs(5))
                .build()
                .map_err(|error| format!("http client: {error}"))?,
        );
    }
    MEASURE_FROM.store(cfg.warmup, Ordering::Relaxed);
    MAX_SERVER_US.store(0, Ordering::Relaxed);
    let clients = Arc::new(clients);
    let base = cfg.http_base.trim_end_matches('/').to_owned();
    let sequence = Arc::new(AtomicU64::new(0));
    let f = move || {
        let clients = Arc::clone(&clients);
        let base = base.clone();
        let sequence = Arc::clone(&sequence);
        async move {
            let index = sequence.load(Ordering::Relaxed) as usize % clients.len();
            let client = clients[index].clone();
            call(client, base, sequence).await
        }
    };
    Ok(run_bench_detailed(
        &format!("{name}-r{round}"),
        cfg.concurrency,
        cfg.total,
        cfg.warmup,
        Duration::from_micros(cfg.threshold_us),
        f,
    )
    .await)
}

fn http_read(
    client: reqwest::Client,
    base: String,
    sequence: Arc<AtomicU64>,
) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
    Box::pin(async move {
        let seq = sequence.fetch_add(1, Ordering::Relaxed);
        let id = seq % ROWS as u64;
        let response = client
            .get(format!("{base}/read?id={id}"))
            .send()
            .await
            .map_err(|error| format!("http read: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("http read: status {status}"));
        }
        let server_us = response
            .headers()
            .get("x-server-us")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| "http read: missing x-server-us".to_string())?;
        note_server_us(server_us, seq);
        let body = response
            .bytes()
            .await
            .map_err(|error| format!("http read body: {error}"))?;
        let parsed: Value =
            serde_json::from_slice(&body).map_err(|error| format!("http read json: {error}"))?;
        if parsed
            .get("value")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
            || parsed.get("cache_len").and_then(Value::as_u64).unwrap_or(0) == 0
        {
            return Err("http read: empty value or cache".into());
        }
        Ok(())
    })
}

fn http_static(
    client: reqwest::Client,
    base: String,
    sequence: Arc<AtomicU64>,
) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
    Box::pin(async move {
        let _ = sequence.fetch_add(1, Ordering::Relaxed);
        let response = client
            .get(format!("{base}/static"))
            .send()
            .await
            .map_err(|error| format!("http static: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("http static: status {}", response.status()));
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| format!("http static body: {error}"))?;
        if body.as_ref() != b"OK" {
            return Err("http static: unexpected body".into());
        }
        Ok(())
    })
}

fn http_write(
    client: reqwest::Client,
    base: String,
    sequence: Arc<AtomicU64>,
) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
    Box::pin(async move {
        let seq = sequence.fetch_add(1, Ordering::Relaxed);
        let id = seq % ROWS as u64;
        let response = client
            .post(format!("{base}/write"))
            .json(&json!({ "id": id, "value": format!("v-{seq}") }))
            .send()
            .await
            .map_err(|error| format!("http write: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("http write: status {}", response.status()));
        }
        let server_us = response
            .headers()
            .get("x-server-us")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| "http write: missing x-server-us".to_string())?;
        note_server_us(server_us, seq);
        let body = response
            .bytes()
            .await
            .map_err(|error| format!("http write body: {error}"))?;
        let parsed: Value =
            serde_json::from_slice(&body).map_err(|error| format!("http write json: {error}"))?;
        if parsed.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err("http write: not ok".into());
        }
        Ok(())
    })
}

async fn grpc_run(
    cfg: &LoadConfig,
    name: &str,
    round: u32,
    call: GrpcCall,
) -> Result<DetailedBenchReport, String> {
    // 每个 worker 一条 HTTP/2 连接：单连接多路复用在本机高负载下出现过
    // 67ms 的单流停顿，和 HTTP/1.1 的多连接基线不可比。
    let mut clients = Vec::with_capacity(cfg.concurrency.max(1));
    for _ in 0..cfg.concurrency.max(1) {
        let endpoint = tonic::transport::Endpoint::from_shared(cfg.grpc_endpoint.clone())
            .map_err(|error| format!("grpc endpoint: {error}"))?
            .tcp_nodelay(true)
            .connect_timeout(Duration::from_secs(5));
        let channel = endpoint
            .connect()
            .await
            .map_err(|error| format!("grpc connect: {error}"))?;
        clients.push(BenchClient::new(channel));
    }
    MEASURE_FROM.store(cfg.warmup, Ordering::Relaxed);
    MAX_SERVER_US.store(0, Ordering::Relaxed);
    let clients = Arc::new(clients);
    let sequence = Arc::new(AtomicU64::new(0));
    let f = move || {
        let clients = Arc::clone(&clients);
        let sequence = Arc::clone(&sequence);
        async move {
            let index = sequence.load(Ordering::Relaxed) as usize % clients.len();
            let client = clients[index].clone();
            call(client, sequence).await
        }
    };
    Ok(run_bench_detailed(
        &format!("{name}-r{round}"),
        cfg.concurrency,
        cfg.total,
        cfg.warmup,
        Duration::from_micros(cfg.threshold_us),
        f,
    )
    .await)
}

fn grpc_read(
    mut client: BenchClient<tonic::transport::Channel>,
    sequence: Arc<AtomicU64>,
) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
    Box::pin(async move {
        let seq = sequence.fetch_add(1, Ordering::Relaxed);
        let id = seq % ROWS as u64;
        let response =
            tokio::time::timeout(Duration::from_secs(5), client.read(Key { id: id as i64 }))
                .await
                .map_err(|_| "grpc read: deadline exceeded".to_string())?
                .map_err(|error| format!("grpc read: {error}"))?
                .into_inner();
        note_server_us(response.server_us, seq);
        if response.value.is_empty() || response.cache_len == 0 {
            return Err("grpc read: empty value or cache".into());
        }
        Ok(())
    })
}

fn grpc_static(
    mut client: BenchClient<tonic::transport::Channel>,
    sequence: Arc<AtomicU64>,
) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
    Box::pin(async move {
        let _ = sequence.fetch_add(1, Ordering::Relaxed);
        tokio::time::timeout(Duration::from_secs(5), client.ping(PingRequest {}))
            .await
            .map_err(|_| "grpc ping: deadline exceeded".to_string())?
            .map_err(|error| format!("grpc ping: {error}"))?;
        Ok(())
    })
}

fn grpc_write(
    mut client: BenchClient<tonic::transport::Channel>,
    sequence: Arc<AtomicU64>,
) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
    Box::pin(async move {
        let seq = sequence.fetch_add(1, Ordering::Relaxed);
        let id = seq % ROWS as u64;
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            client.write(WriteRequest {
                id: id as i64,
                value: format!("v-{seq}"),
            }),
        )
        .await
        .map_err(|_| "grpc write: deadline exceeded".to_string())?
        .map_err(|error| format!("grpc write: {error}"))?
        .into_inner();
        note_server_us(response.server_us, seq);
        if !response.ok {
            return Err("grpc write: not ok".into());
        }
        Ok(())
    })
}

pub async fn probe(
    component: &str,
    concurrency: usize,
    total: u64,
    warmup: u64,
    rounds: u32,
    threshold_us: u64,
) -> Result<(), String> {
    let state = state::connect_state().await?;
    let cfg = probe_config(component, concurrency, total, warmup, rounds, threshold_us);
    for round in 1..=rounds {
        let report = match component {
            "mysql" => probe_mysql(&state, concurrency, total, warmup, threshold_us, round).await,
            "redis" => probe_redis(&state, concurrency, total, warmup, threshold_us, round).await,
            other => return Err(format!("unsupported probe component: {other}")),
        };
        println!("{}", report_json(&report, round, &cfg, 0));
        print_human(&report, round, &cfg);
    }
    Ok(())
}

async fn probe_mysql(
    state: &BenchState,
    concurrency: usize,
    total: u64,
    warmup: u64,
    threshold_us: u64,
    round: u32,
) -> DetailedBenchReport {
    let state = state.clone();
    let sequence = Arc::new(AtomicU64::new(0));
    let f = move || {
        let state = state.clone();
        let sequence = Arc::clone(&sequence);
        async move {
            let id = (sequence.fetch_add(1, Ordering::Relaxed) % ROWS as u64) as i64;
            state
                .db
                .query_with(
                    &format!("SELECT v FROM {} WHERE id = ?", state::TABLE),
                    &[json!(id)],
                )
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
    };
    run_bench_detailed(
        &format!("probe-mysql-r{round}"),
        concurrency,
        total,
        warmup,
        Duration::from_micros(threshold_us),
        f,
    )
    .await
}

async fn probe_redis(
    state: &BenchState,
    concurrency: usize,
    total: u64,
    warmup: u64,
    threshold_us: u64,
    round: u32,
) -> DetailedBenchReport {
    let state = state.clone();
    let sequence = Arc::new(AtomicU64::new(0));
    let f = move || {
        let state = state.clone();
        let sequence = Arc::clone(&sequence);
        async move {
            let id = (sequence.fetch_add(1, Ordering::Relaxed) % ROWS as u64) as i64;
            state
                .redis
                .get(&state::cache_key(id))
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
    };
    run_bench_detailed(
        &format!("probe-redis-r{round}"),
        concurrency,
        total,
        warmup,
        Duration::from_micros(threshold_us),
        f,
    )
    .await
}

fn probe_config(
    component: &str,
    concurrency: usize,
    total: u64,
    warmup: u64,
    rounds: u32,
    threshold_us: u64,
) -> LoadConfig {
    LoadConfig {
        transport: "probe".into(),
        op: component.into(),
        concurrency,
        total,
        warmup,
        rounds,
        threshold_us,
        http_base: String::new(),
        grpc_endpoint: String::new(),
    }
}

fn report_json(
    report: &DetailedBenchReport,
    round: u32,
    cfg: &LoadConfig,
    server_max_us: u64,
) -> String {
    json!({
        "transport": cfg.transport,
        "op": cfg.op,
        "round": round,
        "requests": report.total_requests,
        "errors": report.errors,
        "duration_secs": report.total_duration.as_secs_f64(),
        "throughput_rps": report.throughput_rps,
        "p50_us": report.p50_latency_us,
        "p99_us": report.p99_latency_us,
        "p999_us": report.p999_latency_us,
        "max_us": report.max_latency_us,
        "over_threshold": report.over_threshold,
        "threshold_us": report.threshold_us,
        "server_max_us": server_max_us,
    })
    .to_string()
}

fn print_human(report: &DetailedBenchReport, round: u32, cfg: &LoadConfig) {
    eprintln!(
        "{} r{}: requests={} errors={} p50={:.0}us p99={:.0}us p99.9={:.0}us max={:.0}us >= {}us={} qps={:.0} ({} {})",
        cfg.transport,
        round,
        report.total_requests,
        report.errors,
        report.p50_latency_us,
        report.p99_latency_us,
        report.p999_latency_us,
        report.max_latency_us,
        cfg.threshold_us,
        report.over_threshold,
        report.throughput_rps,
        cfg.op,
        cfg.transport,
    );
}
