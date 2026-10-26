// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 全链路基准：`serve` 起 HTTP+gRPC 服务，`load`/`probe` 在独立进程测量。
//!
//! 用法：
//!   fullchain-bench serve [middleware=1]
//!   fullchain-bench load transport=http|grpc op=read|write|static \
//!     concurrency=8 total=10000 warmup=2000 rounds=5 threshold_us=10000
//!   fullchain-bench probe component=mysql|redis concurrency=8 total=10000 ...
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ecat_transport::Server as _;
use ecat_transport_grpc::GrpcServer;
use ecat_transport_http::HttpServer;

mod grpc_api;
mod http_api;
mod load;
mod pb;
mod state;

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    if let Err(error) = runtime.block_on(real_main()) {
        eprintln!("fullchain-bench: {error}");
        std::process::exit(1);
    }
}

async fn real_main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let role = args.next().ok_or_else(usage)?;
    let rest: Vec<String> = args.collect();
    let options = parse_options(&rest)?;
    match role.as_str() {
        "serve" => serve(&options).await,
        "load" => {
            let config = load_config(&options)?;
            load::run(&config).await
        }
        "probe" => {
            let component = options
                .get("component")
                .cloned()
                .unwrap_or_else(|| "mysql".into());
            load::probe(
                &component,
                number(&options, "concurrency", 8) as usize,
                number(&options, "total", 10_000),
                number(&options, "warmup", 2_000),
                number(&options, "rounds", 5) as u32,
                number(&options, "threshold_us", 10_000),
            )
            .await
        }
        other => Err(format!("unknown role `{other}`\n{}", usage())),
    }
}

async fn serve(options: &HashMap<String, String>) -> Result<(), String> {
    let middleware = options.get("middleware").is_some_and(|value| value == "1");
    if middleware {
        // 与 App::run 的默认行为对齐：没有 dispatcher 时安装 fmt subscriber，
        // 否则 LoggingLayer 的 info! 事件会被直接丢弃，低估计时。
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .try_init();
    }
    let http_addr =
        std::env::var("ECAT_BENCH_HTTP_ADDR").unwrap_or_else(|_| "127.0.0.1:8000".into());
    let grpc_addr =
        std::env::var("ECAT_BENCH_GRPC_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".into());

    let state = state::connect_state().await?;
    state::seed(&state).await?;

    let http = Arc::new(
        HttpServer::new(http_addr.clone()).router(http_api::router(state.clone(), middleware)),
    );
    let grpc = Arc::new(
        GrpcServer::new(grpc_addr.clone()).routes(tonic::service::Routes::new(
            grpc_api::BenchService::new(state).server(),
        )),
    );

    let http_task = {
        let server = Arc::clone(&http);
        tokio::spawn(async move {
            if let Err(error) = server.start().await {
                eprintln!("http server stopped: {error}");
            }
        })
    };
    let grpc_task = {
        let server = Arc::clone(&grpc);
        tokio::spawn(async move {
            if let Err(error) = server.start().await {
                eprintln!("grpc server stopped: {error}");
            }
        })
    };

    let mut http_ready = false;
    let mut grpc_ready = false;
    for _ in 0..200 {
        if http_task.is_finished() {
            return Err("http server exited before becoming ready".into());
        }
        if grpc_task.is_finished() {
            return Err("grpc server exited before becoming ready".into());
        }
        http_ready |= tokio::net::TcpStream::connect(&http_addr).await.is_ok();
        grpc_ready |= tokio::net::TcpStream::connect(&grpc_addr).await.is_ok();
        if http_ready && grpc_ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !http_ready || !grpc_ready {
        return Err(format!(
            "server did not become ready (http={http_ready}, grpc={grpc_ready})"
        ));
    }
    println!(
        "READY http={http_addr} grpc={grpc_addr} middleware={}",
        if middleware { "http-only" } else { "none" }
    );
    use std::io::Write as _;
    let _ = std::io::stdout().flush();

    tokio::signal::ctrl_c()
        .await
        .map_err(|error| format!("signal: {error}"))?;
    http.stop().await.map_err(|error| error.to_string())?;
    grpc.stop().await.map_err(|error| error.to_string())?;
    let _ = tokio::time::timeout(Duration::from_secs(5), http_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), grpc_task).await;
    Ok(())
}

fn load_config(options: &HashMap<String, String>) -> Result<load::LoadConfig, String> {
    let transport = options
        .get("transport")
        .cloned()
        .unwrap_or_else(|| "http".into());
    let op = options.get("op").cloned().unwrap_or_else(|| "read".into());
    Ok(load::LoadConfig {
        transport,
        op,
        concurrency: number(options, "concurrency", 8) as usize,
        total: number(options, "total", 10_000),
        warmup: number(options, "warmup", 2_000),
        rounds: number(options, "rounds", 5) as u32,
        threshold_us: number(options, "threshold_us", 10_000),
        http_base: std::env::var("ECAT_BENCH_HTTP")
            .unwrap_or_else(|_| "http://127.0.0.1:8000".into()),
        grpc_endpoint: std::env::var("ECAT_BENCH_GRPC")
            .unwrap_or_else(|_| "http://127.0.0.1:50051".into()),
    })
}

fn parse_options(args: &[String]) -> Result<HashMap<String, String>, String> {
    let mut options = HashMap::new();
    for arg in args {
        let (key, value) = arg
            .split_once('=')
            .ok_or_else(|| format!("expected key=value, got `{arg}`"))?;
        options.insert(key.to_owned(), value.to_owned());
    }
    Ok(options)
}

fn number(options: &HashMap<String, String>, key: &str, default: u64) -> u64 {
    options
        .get(key)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn usage() -> String {
    "usage: fullchain-bench serve | load transport=http|grpc op=read|write|static [options] | probe component=mysql|redis [options]".into()
}
