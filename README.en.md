# e-cat

[简体中文](README.md) | English

**e-cat** is a Rust microservices framework inspired by [go-kratos/kratos](https://github.com/go-kratos/kratos) v3.

It provides an API-first development experience, pluggable component architecture, unified HTTP/gRPC middleware abstraction, and a complete CLI toolchain. Developers familiar with Kratos can get started immediately, while also leveraging Rust's type safety, zero-cost abstractions, and exceptional performance.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                         ecat-cli                             │
│              (new │ proto │ run │ build)                     │
├──────────────────────────────────────────────────────────────┤
│                     ecat (App Lifecycle)                     │
│      AppBuilder → App { name, servers, hooks, ... }         │
├────────────────────┬────────────────────┬────────────────────┤
│     transport      │    middleware      │     registry       │
│     ─────────      │    ──────────      │     ────────       │
│     HTTP (axum)    │    RecoveryLayer   │     etcd           │
│     gRPC (tonic)   │    TracingLayer    │     consul         │
│     encoding       │    LoggingLayer    │     dns            │
│                    │    TimeoutLayer    │     memory         │
├────────────────────┼────────────────────┼────────────────────┤
│     config         │     errors         │     metadata       │
│     ──────         │     ──────         │     ────────       │
│     file / env     │     ErrorCode      │     key-value      │
│     remote source  │     Error          │     HTTP/gRPC      │
├────────────────────┴────────────────────┴────────────────────┤
│                         data layer                            │
│     ────────────────────────────────────────────────          │
│     rdbms:   SQLite / PostgreSQL / MySQL / TiDB              │
│     cache:   Redis / Memcached                               │
│     olap:    ClickHouse                                      │
│     search:  OpenSearch / Elasticsearch                      │
│     graph:   Neo4j / NebulaGraph / ArangoDB                  │
│     tsdb:    InfluxDB / Apache IoTDB / QuestDB               │
├──────────────────────────────────────────────────────────────┤
│                       ecat-protos                             │
│     (shared .proto definitions: errors, metadata, ...)       │
└──────────────────────────────────────────────────────────────┘
```

## Features

- **API-first**: Protobuf-defined APIs, error codes, and metadata
- **Dual protocol**: HTTP (axum) and gRPC (tonic) sharing one middleware chain
- **Pluggable**: Registry, Config, Logging, Encoding via trait abstractions
- **Middleware**: Built-in Recovery, Tracing, Logging, Timeout layers
- **Lifecycle**: Builder pattern, concurrent servers, graceful shutdown
- **Type-safe**: Protobuf-based error codes with compile-time HTTP mapping
- **Observable**: tracing + OpenTelemetry + Prometheus out of the box

## Tech Stack

| Component | Choice |
|-----------|--------|
| Runtime | **tokio** |
| HTTP | **axum** |
| gRPC | **tonic** |
| Protobuf | **prost + tonic-build** |
| Middleware | **tower::Service / Layer** |
| Tracing | **tracing + opentelemetry-rust** |
| Metrics | **prometheus** |
| Serialization | **serde + prost** |
| RDBMS | **sqlx** |
| CLI | **clap** |

## Supported Databases

| Category | Database | Crate | Rust Driver |
|----------|----------|-------|-------------|
| RDBMS | SQLite | `ecat-data-sqlx` | [sqlx](https://crates.io/crates/sqlx) |
| RDBMS | PostgreSQL | `ecat-data-sqlx` | [sqlx](https://crates.io/crates/sqlx) |
| RDBMS | MySQL | `ecat-data-sqlx` | [sqlx](https://crates.io/crates/sqlx) |
| RDBMS | TiDB | `ecat-data-sqlx` | [sqlx](https://crates.io/crates/sqlx) |
| Cache | Redis | `ecat-data-redis` | [redis-rs](https://crates.io/crates/redis) |
| Cache | Memcached | `ecat-data-memcached` | [memcache](https://crates.io/crates/memcache) |
| OLAP | ClickHouse | `ecat-data-clickhouse` | [clickhouse-rs](https://crates.io/crates/clickhouse-rs) |
| Search | OpenSearch | `ecat-data-opensearch` | [opensearch](https://crates.io/crates/opensearch) |
| Search | Elasticsearch | `ecat-data-elasticsearch` | [elasticsearch](https://crates.io/crates/elasticsearch) |
| Graph | Neo4j | `ecat-data-neo4j` | [neo4rs](https://crates.io/crates/neo4rs) |
| Graph | NebulaGraph | `ecat-data-nebulagraph` | nebula-client |
| Graph | ArangoDB | `ecat-data-arangodb` | [arangors](https://crates.io/crates/arangors) |
| TSDB | InfluxDB | `ecat-data-influxdb` | [influxdb2](https://crates.io/crates/influxdb2) |
| TSDB | Apache IoTDB | `ecat-data-iotdb` | iotdb-client-rs |
| TSDB | QuestDB | `ecat-data-questdb` | questdb-rs (ILP) |

> All backends share unified trait abstractions (`RdbmsClient` / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient`). Import the corresponding contrib crate as needed.

## Quick Start

```rust
use ecat::App;
use ecat_transport_http::HttpServer;
use ecat_transport_grpc::GrpcServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = App::builder()
        .name("my-service")
        .version("v1.0.0")
        .server(HttpServer::new(":8000"))
        .server(GrpcServer::new(":9000"))
        .build()?;

    app.run().await?; // blocks until SIGTERM
    Ok(())
}
```

## Implementation Progress

| Phase | Status | Content |
|-------|--------|---------|
| Phase 1 | ✅ Done | Project skeleton, protos, errors, metadata, encoding, logging |
| Phase 2 | ✅ Done | Transport layer (HTTP + gRPC) |
| Phase 3 | ✅ Done | Middleware (Recovery/Tracing/Logging/Timeout) |
| Phase 4 | ✅ Done | App lifecycle management |
| Phase 5 | ✅ Done | Registry, Config, Metrics |
| Phase 5.5 | ✅ Done | Data access layer (traits + sqlx backend) |
| Phase 6 | ✅ Done | CLI toolchain (new/proto/run/build) |
| Phase 7 | ✅ Done | README, examples (helloworld), design docs |

## Documentation

- [Design Spec](docs/superpowers/specs/2026-07-29-ecat-framework-design.md)
- [Implementation Plan](docs/superpowers/plans/2026-07-29-ecat-framework.md)

## License

MIT
