# e-cat

[简体中文](README.md) | English

**e-cat** is a Rust microservices framework inspired by [go-kratos/kratos](https://github.com/go-kratos/kratos) v3.

It provides an API-first development experience, pluggable component architecture, unified HTTP/gRPC middleware abstraction, and a complete CLI toolchain. Developers familiar with Kratos can get started immediately, while also leveraging Rust's type safety, zero-cost abstractions, and exceptional performance.

## Architecture

```
┌──────────────────────────────────────────────────┐
│                  ecat-cli                        │  ← CLI Toolchain
│        (new | proto | run | build)               │
├──────────────────────────────────────────────────┤
│              ecat (App Lifecycle)                 │  ← Orchestration
│    AppBuilder → App { http_srv, grpc_srv, ... }  │
├──────────────┬──────────────┬────────────────────┤
│  transport   │  middleware  │     registry       │  ← Core Components
│  ─────────   │  ─────────   │     ────────       │
│  HTTP/gRPC   │  recovery    │     etcd/consul    │
│  encoding    │  tracing     │     dns/memory     │
│              │  auth/...    │                    │
├──────────────┼──────────────┼────────────────────┤
│   config     │   errors     │     metadata       │  ← Infrastructure
├──────────────┴──────────────┴────────────────────┤
│                    data                          │  ← Data Access
│  ─────────────────────────────────────           │
│  rdbms:    SQLite / PostgreSQL / MySQL / TiDB    │
│  cache:    Redis / Memcached                     │
│  olap:     ClickHouse                            │
│  search:   OpenSearch / Elasticsearch             │
│  graph:    Neo4j / NebulaGraph / ArangoDB        │
│  tsdb:     InfluxDB / IoTDB / QuestDB            │
├──────────────────────────────────────────────────┤
│              ecat-protos                         │  ← IDL Definitions
│    (shared protobuf: errors, metadata, ...)      │
└──────────────────────────────────────────────────┘
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
| Phase 5 | ⏳ Pending | Registry, Config, Metrics |
| Phase 5.5 | ⏳ Pending | Data access layer (15 storage backends) |
| Phase 6 | ⏳ Pending | CLI toolchain |
| Phase 7 | ⏳ Pending | Documentation, examples, ecosystem |

## Documentation

- [Design Spec](docs/superpowers/specs/2026-07-29-ecat-framework-design.md)
- [Implementation Plan](docs/superpowers/plans/2026-07-29-ecat-framework.md)

## License

MIT
