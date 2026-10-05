# e-cat 生态规划 v3 — 最终评估

> **更新（2026-08-07, v2.3.3）**: 剩余缺口 #1「mTLS 接入 transport」已完成——`HttpServer::tls` / `GrpcServer::tls` 基于 tokio-rustls / tonic rustls 真实生效（支持 CA 校验与强制客户端证书）；缺口 #2（Redis 限流）、#3（GitLab CI）此前已随 v2.3.0 完成。规划所列缺口至此全部落地。

**版本:** 2.4.2  
**日期:** 2026-08-01  
**crate 总数:** 55 · 全部规划已完成

---

## 当前覆盖

| 领域 | 已实现 | 覆盖率 |
|------|--------|--------|
| 传输层 | HTTP (axum), gRPC (tonic), WebSocket | 100% |
| 编码 | JSON, Protobuf | 100% |
| 中间件 | Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, Auth×3 | 100% |
| 配置 | env, file (JSON/YAML), Consul KV, 加密 (XOR) | 100% |
| 注册中心 | memory, Consul, etcd | 100% |
| 安全 | 攻击检测, JWT, API Key, OAuth2, TLS 客户端证书, mTLS | 95% |
| 通信 | TLS 客户端证书 — 全部数据后端支持 | 95% |
| 服务通信 | HTTP Client, gRPC Client, Resolver, LoadBalancer | 95% |
| 数据 | RDBMS (sqlx), Redis, OpenSearch, Elasticsearch, ClickHouse, Memcached, Neo4j, NebulaGraph, ArangoDB, InfluxDB, IoTDB, QuestDB — 全部支持 Config 文件配置 | 95% |
| 消息 | MessageQueue trait, InMemory, Kafka, EventBus | 100% |
| 可观测 | tracing, Prometheus, Health, 分布式追踪 | 100% |
| DevOps | CLI, Dockerfile, K8s, Helm, GitHub Actions, Bench, Testing | 95% |
| API 工具 | OpenAPI, Versioning, GraphQL | 100% |

---

## 剩余缺口

### 值得做的 (3项)

| # | 缺口 | 价值 | 工作量 |
|---|------|------|--------|
| 1 | **mTLS 接入 transport** | TlsConfig 已有，未接入 HttpServer/GrpcServer | 小 |
| 2 | **Redis 限流后端** | RateLimitLayer 仅内存，多实例需共享 | 小 |
| 3 | **GitLab CI 模板** | 已有 GitHub Actions | 小 |

### 不需要做的 (2项)

| # | 缺口 | 理由 |
|---|------|------|
| 4 | 配置 AES-GCM | 当前 XOR 够用 |
| 5 | 服务网格/API 网关 | 交给社区（Linkerd/Kong/K8s） |

---

## 判定

**e-cat 已达到生产可用成熟度。** 47 个 crate 涵盖微服务全栈：传输 → 中间件 → 服务发现 → 配置 → 安全 → 数据 → 消息 → 可观测 → DevOps → API 工具。剩余 3 项缺口为小工作量优化，无结构性缺失。

## 数据后端覆盖（15 个）

| 类别 | 数据库 | Crate | 驱动方式 |
|------|--------|-------|----------|
| RDBMS | SQLite/PostgreSQL/MySQL/TiDB | `ecat-data-sqlx` | sqlx（官方异步驱动） |
| 缓存 | Redis | `ecat-data-redis` | redis-rs（官方驱动） |
| 缓存 | Memcached | `ecat-data-memcached` | ⚠️ 内存实现（非生产） |
| 文档 | MongoDB | `ecat-data-mongodb` | mongodb（官方驱动） |
| 对象存储 | S3 / MinIO | `ecat-data-s3` | HTTP/REST（reqwest+rustls，自实现 SigV4） |
| OLAP | ClickHouse | `ecat-data-clickhouse` | HTTP/REST（reqwest） |
| 搜索 | OpenSearch | `ecat-data-opensearch` | HTTP/REST（reqwest） |
| 搜索 | Elasticsearch | `ecat-data-elasticsearch` | HTTP/REST（reqwest） |
| 图 | Neo4j | `ecat-data-neo4j` | HTTP/REST（reqwest） |
| 图 | NebulaGraph | `ecat-data-nebulagraph` | HTTP/REST（reqwest） |
| 图 | ArangoDB | `ecat-data-arangodb` | HTTP/REST（reqwest） |
| 时序 | InfluxDB | `ecat-data-influxdb` | HTTP/REST（reqwest） |
| 时序 | Apache IoTDB | `ecat-data-iotdb` | HTTP/REST（reqwest） |
| 时序 | QuestDB | `ecat-data-questdb` | HTTP/REST（reqwest） |
| 时序 | TDengine | `ecat-data-tdengine` | HTTP/REST（reqwest） |

---

## v4.0 规划（2026-10-05）— 完整 ORM 与 SQL Server

> 状态：**设计中，尚未实现**。下表为规划项，不代表当前可用能力。
> 完整设计见 [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](superpowers/specs/2026-10-05-orm-and-mssql-design.md)。

补齐两项结构性缺口：

1. **完整 ORM**：当前数据层只有手写 SQL 的 RDBMS 客户端（`Row` = 列名 + JSON 值），
   无实体映射、关联、迁移。v4.0 新增 `ecat-orm`（实体宏 / CRUD / 查询构建器 / 关联预加载 /
   连接查询 / 批量 / 分页 / 迁移）+ `ecat-orm-derive`。建在统一 `SqlExecutor` trait 之上，
   **天然覆盖全部 RDBMS 后端**。
2. **SQL Server 后端**：sqlx 主库无 MSSQL 驱动（0.7 前移除，重写未发布），
   新增 `ecat-data-mssql`（`tiberius-ng` 0.13 + `deadpool` 0.13）——
   数据后端从 15 个增至 **16 个**。

连带的地基改动：

| 改动 | 说明 |
|---|---|
| `ecat-data-sqlx` 弃用 `AnyPool` 改原生池 | 修复时间类型限制（不再需要 CAST 绕过）、去掉驱动安装 panic 面、启用 statement cache |
| `ecat-data` 拆 `SqlExecutor` supertrait | 事务内可执行 SQL（当前 `Transaction` 只能 commit/rollback），为 ORM 与读写分离提供基础 |
| 连接池增强 | 查询超时、`warm_up()` 预热、智能 recycle、熔断（复用 `ecat-circuit-breaker`）、读写分离 `RdbmsRouting` |
| 可观测性 | 池指标接 `ecat-metrics`、池探活接 `ecat-health`、慢查询接 `ecat-tracing`（均为 opt-in feature） |

**破坏性变更**：trait 拆分 + `SqlxClient::from_pool` 签名 + 移除 `AnyPool` →
workspace 版本 3.0.3 → **4.0.0**。
