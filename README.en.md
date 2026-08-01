<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Ecat

[简体中文](README.md) | English

**Ecat** is a Rust microservices framework (v2.1.7 · 47 crates) inspired by [go-kratos/kratos](https://github.com/go-kratos/kratos) v3.

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

> All backends share unified trait abstractions and provide `XxxConfig` structs (`#[derive(Deserialize)]`) for loading connection info from JSON/YAML config files.

### Database Configuration

Each backend provides a config struct and `from_config()` method:

```rust
use ecat_data_redis::{RedisCache, RedisConfig};
use ecat_data_sqlx::{SqlxClient, SqlxConfig};

// Load from config file
let redis_cfg: RedisConfig = serde_json::from_str(r#"{"url":"redis://localhost"}"#)?;
let cache = RedisCache::from_config(redis_cfg).await?;

let sql_cfg: SqlxConfig = serde_json::from_str(r#"{"url":"postgres://localhost/db"}"#)?;
let db = SqlxClient::from_config(sql_cfg).await?;
let rows = db.query("SELECT * FROM users").await?;
```

| Backend | Config Struct | Fields |
|---------|--------------|--------|
| Redis | `RedisConfig` | `url`, `password`? |
| RDBMS | `SqlxConfig` | `url`, `username`?, `password`? |
| ClickHouse | `ClickhouseConfig` | `base_url`, `database`, `username`?, `password`? |
| QuestDB | `QuestdbConfig` | `base_url`, `username`?, `password`? |
| Elasticsearch | `ElasticsearchConfig` | `base_url`, `username`?, `password`? |
| OpenSearch | `OpenSearchConfig` | `base_url`, `username`?, `password`? |
| InfluxDB | `InfluxConfig` | `base_url`, `org`, `bucket`, `token` |
| Neo4j | `Neo4jConfig` | `base_url`, `username`, `password` |
| NebulaGraph | `NebulaGraphConfig` | `base_url`, `space`, `username`?, `password`? |
| ArangoDB | `ArangoConfig` | `base_url`, `db`, `username`, `password` |
| IoTDB | `IotdbConfig` | `base_url`, `username`, `password` |
| Memcached | `MemcachedConfig` | `username`?, `password`?, `tls`? *(reserved)* |

> All backends support optional `tls` field for client certificate auth (CA cert, mTLS, skip verification). See [TLS Certificate Tutorial](docs/tls-certificate-tutorial.md).

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
| Phase 8 | ✅ Done | Attack detection (security-rust, ecat-security) |
| Phase 9 | ✅ Done | Ecosystem I (health / client / circuit-breaker / auth / consul) |
| Phase 10 | ✅ Done | Ecosystem II (redis / mq / events / config-remote) |
| Phase 11 | ✅ Done | Ecosystem III (testing / deploy / bench / openapi) |
| Phase 12 | ✅ Done | Comms & security (gRPC client / OAuth2 / mTLS / tracing) |
| Phase 13 | ✅ Done | Data backends (etcd / Kafka / OpenSearch / InfluxDB / ES / ClickHouse / Memcached / Neo4j / NebulaGraph / ArangoDB / IoTDB / QuestDB) |
| Phase 14 | ✅ Done | Ops & UX (WebSocket / API versioning / GraphQL / Helm / CI/CD) |

## Documentation

- [Design Spec](docs/superpowers/specs/2026-07-29-ecat-framework-design.md)
- [Implementation Plan](docs/superpowers/plans/2026-07-29-ecat-framework.md)
- [Ecosystem Plan v1](docs/ecosystem-plan.md)
- [Ecosystem Plan v2](docs/ecosystem-plan-v2.md)
- [Ecosystem Plan v3](docs/ecosystem-plan-v3.md) (final)
- [Audit Report r5](docs/audit-report-2026-08-01-r5.md) (2026-08-01)
- [Database Config Tutorial](docs/database-config-tutorial.md)
- [TLS Certificate Tutorial](docs/tls-certificate-tutorial.md)
- [Config Example](config/databases.example.yaml)

## License

MIT
