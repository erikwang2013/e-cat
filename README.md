# e-cat

[English](README.en.md) | 简体中文

**e-cat** 是对标 [go-kratos/kratos](https://github.com/go-kratos/kratos) v3 的 Rust 微服务框架。

提供 API-first 开发体验、可插拔的组件架构、统一的 HTTP/gRPC 中间件抽象，以及完备的 CLI 工具链。让熟悉 Kratos 的开发者可以无缝上手，同时充分利用 Rust 的类型安全、零成本抽象和极致性能。

## 设计架构

```
┌──────────────────────────────────────────────────────────────┐
│                         ecat-cli                             │
│              (new │ proto │ run │ build)                     │
├──────────────────────────────────────────────────────────────┤
│                     ecat (应用生命周期)                         │
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
│                         data 层                               │
│     ────────────────────────────────────────────────          │
│     rdbms:   SQLite / PostgreSQL / MySQL / TiDB              │
│     cache:   Redis / Memcached                               │
│     olap:    ClickHouse                                      │
│     search:  OpenSearch / Elasticsearch                      │
│     graph:   Neo4j / NebulaGraph / ArangoDB                  │
│     tsdb:    InfluxDB / Apache IoTDB / QuestDB               │
├──────────────────────────────────────────────────────────────┤
│                       ecat-protos                             │
│     (共享 .proto 定义: errors, metadata, ...)                 │
└──────────────────────────────────────────────────────────────┘
```

### 请求处理流程

```
客户端请求
  │
  ├─ HTTP :8000 ────→ axum::Router ──┐
  │                                   │
  └─ gRPC :9000 ────→ tonic::Server ─┤
                                      │
                              ┌───────┴───────┐
                              │   Middleware   │
                              │   ──────────   │
                              │ 1. Recovery    │  捕获 panic
                              │ 2. Tracing     │  注入 trace_id
                              │ 3. Logging     │  请求日志
                              │ 4. Auth        │  认证鉴权
                              │ 5. Metrics     │  指标采集
                              └───────┬───────┘
                                      │
                              ┌───────┴───────┐
                              │    Handler     │  用户业务逻辑
                              │ (tower::Service)│
                              └───────┬───────┘
                                      │
                              ┌───────┴───────┐
                              │   Response     │  编码序列化
                              │ JSON/Protobuf  │
                              └───────────────┘
```

## 功能

- **API-first**：Protobuf 定义 API、错误码、元数据；prost + tonic-build 代码生成
- **双协议支持**：HTTP（axum）和 gRPC（tonic）共用同一套 tower::Layer 中间件
- **可插拔架构**：Registry、Config、Logging、Encoding 全部通过 trait 抽象，默认提供生产可用实现
- **中间件体系**：内置 Recovery、Tracing、Logging、Timeout；通过 tower::ServiceBuilder 组合
- **应用生命周期**：Builder 模式构建 App，多 Server 并发启动，SIGTERM/SIGINT 优雅停机
- **类型安全**：基于 protobuf 的错误码体系，编译期 HTTP 状态码映射
- **可观测性**：tracing + opentelemetry + Prometheus 开箱即用
- **多数据源**：RDBMS（SQLite/PG/MySQL/TiDB）、缓存、OLAP、搜索引擎、图数据库、时序数据库

### Kratos 概念映射

| Kratos (Go) | e-cat (Rust) | 说明 |
|-------------|-------------|------|
| `kratos.New()` | `App::builder()` | Builder 模式 |
| `http.Handler` | `tower::Service` | Rust 生态标准 trait |
| `http.Server` | `axum::Router` | 社区主流 HTTP 框架 |
| `grpc.Server` | `tonic::transport::Server` | 最成熟的 gRPC 实现 |
| `proto generate` | `prost + tonic-build` | 社区标准 protobuf |
| `registry.Discovery` | `Registry` trait | 可插拔注册发现 |
| `config.Source` | `ConfigSource` trait | 多源配置加载 |

## 技术栈

| 组件 | 选型 |
|------|------|
| 异步运行时 | **tokio** |
| HTTP | **axum** |
| gRPC | **tonic** |
| Protobuf | **prost + tonic-build** |
| 中间件 | **tower::Service / Layer** |
| 日志/追踪 | **tracing + opentelemetry-rust** |
| 指标 | **prometheus** |
| 序列化 | **serde + prost** |
| RDBMS | **sqlx** |
| CLI | **clap** |

## 项目结构

```
e-cat/
├── ecat/                       # 核心：App 生命周期
├── ecat-transport/             # 传输抽象（Server trait）
├── ecat-transport-http/        # axum 实现
├── ecat-transport-grpc/        # tonic 实现
├── ecat-middleware/            # tower::Layer 中间件
├── ecat-protos/                # Protobuf 定义
├── ecat-errors/                # 错误码体系
├── ecat-metadata/              # 元数据传递
├── ecat-encoding/              # 序列化抽象
├── ecat-logging/               # tracing 集成
├── ecat-registry/              # 服务注册发现
├── ecat-config/                # 配置管理
├── ecat-metrics/               # Prometheus 集成
├── ecat-data/                  # 数据访问 trait
├── ecat-cli/                   # CLI 工具
├── docs/                       # 设计文档与实现计划
└── examples/                   # 示例项目
```

## 快速开始

### 前提条件

- Rust 1.80+（stable 工具链）
- [protoc](https://github.com/protocolbuffers/protobuf)（Protocol Buffers 编译器）

### 安装 CLI

```bash
cargo install ecat-cli
```

### 创建服务

```bash
# 脚手架生成项目
ecat new helloworld
cd helloworld

# 添加 proto 定义
ecat proto add api/helloworld/helloworld.proto

# 生成客户端和服务端代码
ecat proto client api/helloworld/helloworld.proto
ecat proto server api/helloworld/helloworld.proto -t internal/service

# 开发模式运行
ecat run
```

访问 `http://localhost:8000/helloworld/ecat`。

### 代码示例

```rust
use ecat::App;
use ecat_transport_http::HttpServer;
use ecat_transport_grpc::GrpcServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let http_srv = HttpServer::new(":8000");
    let grpc_srv = GrpcServer::new(":9000");

    let app = App::builder()
        .name("my-service")
        .version("v1.0.0")
        .server(http_srv)
        .server(grpc_srv)
        .on_start(|| async {
            tracing::info!("service started");
            Ok(())
        })
        .on_stop(|| async {
            tracing::info!("service stopped");
            Ok(())
        })
        .build()?;

    app.run().await?; // 阻塞直到 SIGTERM/SIGINT
    Ok(())
}
```

### 中间件

```rust
use tower::ServiceBuilder;
use ecat_middleware::{RecoveryLayer, TracingLayer, LoggingLayer, TimeoutLayer};
use std::time::Duration;

let layer = ServiceBuilder::new()
    .layer(RecoveryLayer)
    .layer(TracingLayer)
    .layer(LoggingLayer)
    .layer(TimeoutLayer::new(Duration::from_secs(30)));
```

### 错误处理

```rust
use ecat_errors::{Error, ErrorCode};

fn get_user(id: u64) -> Result<User, Error> {
    if id == 0 {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "bad_request",
            "user id must be positive",
        ));
    }
    // ...
}
```

## 实现阶段

| 阶段 | 状态 | 内容 |
|------|------|------|
| Phase 1 | ✅ 完成 | 项目骨架、protos、errors、metadata、encoding、logging |
| Phase 2 | ✅ 完成 | Transport 层（HTTP + gRPC） |
| Phase 3 | ✅ 完成 | Middleware 体系（Recovery/Tracing/Logging/Timeout） |
| Phase 4 | ✅ 完成 | App 生命周期管理 |
| Phase 5 | ⏳ 待实现 | Registry、Config、Metrics |
| Phase 5.5 | ⏳ 待实现 | Data 数据访问层（15 种存储后端） |
| Phase 6 | ⏳ 待实现 | CLI 工具链 |
| Phase 7 | ⏳ 待实现 | 文档、示例、生态 |

## 设计目标

| # | 目标 | 说明 |
|---|------|------|
| 1 | **Kratos 对齐** | 保持 Kratos 的 API-first、可插拔、统一抽象理念 |
| 2 | **Rust 惯用** | 复用 tower::Service、trait 泛型、零成本抽象；不做「Go in Rust」 |
| 3 | **类型安全** | 编译期捕获错误，Protobuf 定义全强类型化 |
| 4 | **可插拔** | Registry、Config、Logging、Encoding 全部通过 trait 抽象 |
| 5 | **工具链完备** | CLI 支持项目脚手架、proto 代码生成、开发运行 |
| 6 | **性能优先** | 零成本抽象 + 异步运行时 |
| 7 | **可观测** | tracing + OpenTelemetry + Prometheus 开箱即用 |

## 技术说明

### 为什么选择 tower::Service

[`tower::Service`](https://docs.rs/tower/latest/tower/trait.Service.html) 是 Rust 异步生态的 `http.Handler` 等价物。axum 和 tonic 都构建在 tower 之上，因此 e-cat 不需要自定义中间件 trait——直接提供 tower::Layer 实现即可达到与 Kratos 中间件相同的效果，且零适配器开销。

### 为什么用 Cargo Workspace

与 Kratos 的模块化设计一致。每个 `ecat-*` crate 独立版本、独立编译，用户按需引入。核心 crate 保持最小依赖，contrib crate 提供可选集成。

### 为什么用 prost（而非 protobuf-rs）

prost 是 Rust 社区最广泛使用的 protobuf 实现，编译期生成类型安全代码，与 tonic 深度集成。

## 设计文档

- [设计规范](docs/superpowers/specs/2026-07-29-ecat-framework-design.md)
- [实现计划](docs/superpowers/plans/2026-07-29-ecat-framework.md)

## 许可证

MIT
