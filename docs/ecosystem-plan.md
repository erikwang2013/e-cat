# e-cat 生态规划

**版本:** 1.0.8  
**日期:** 2026-08-01

## 现状覆盖

| 领域 | 已覆盖 | 缺失 |
|------|--------|------|
| 传输层 | HTTP (axum), gRPC (tonic) | — |
| 编码 | JSON, Protobuf | — |
| 中间件 | 日志、追踪、超时、恢复、限流 | 熔断、重试、认证 |
| 配置 | 环境变量、文件 (JSON/YAML) | 远程配置中心 |
| 注册 | 内存（单机） | Consul, etcd, DNS |
| 安全 | 攻击检测 | 认证、授权、mTLS |
| 数据 | 5 种抽象 + SQLx 实现 | Redis, ES, TSDB 实现 |
| 可观测 | 日志, Prometheus metrics | 分布式追踪, 健康检查 |
| 通信 | — | 客户端, 消息队列, 事件总线 |
| DevOps | CLI 脚手架 | Docker, K8s, CI/CD |

---

## 缺口最大的四个领域

1. **服务间通信与弹性** — 没有 HTTP/gRPC 客户端、熔断器、重试策略，微服务之间只能手动调用
2. **认证与授权** — 没有 JWT/OAuth2/mTLS，生产环境不可用
3. **注册中心后端** — 只有内存实现，多实例部署时无法发现彼此
4. **数据层后端** — 只有 SQLx RDBMS，没有 Redis 缓存、搜索引擎、时序库实现

---

## 分期规划

### 第一期：生产就绪（必须）

目标：3 个服务可以安全地互相调用，有认证、能容错

| 新 crate | 用途 | 核心内容 | 预计行数 |
|----------|------|----------|----------|
| `ecat-client` | 服务间调用 | HttpClient + GrpcClient，集成服务发现、负载均衡、超时 | ~400 |
| `ecat-circuit-breaker` | 熔断器 | CircuitBreakerLayer，三态 + 滑动窗口统计 | ~200 |
| `ecat-auth` | 认证中间件 | JWT 验证、OAuth2、API Key、mTLS 证书提取 | ~350 |
| `ecat-health` | 健康检查 | /health + /ready 端点，可注册自定义检查器 | ~150 |
| `ecat-registry-consul` | Consul 注册 | ConsulRegistry 实现 Registry trait | ~200 |

#### ecat-client

```
ecat-client
├── HttpClient (服务发现 + 负载均衡 + 超时)
├── GrpcClient (同上)
├── ServiceResolver trait (可插拔：consul/dns/static)
└── LoadBalancer (round-robin / random / least-conn)
```

```rust
// 使用示例
let client = HttpClient::builder()
    .resolver(ConsulResolver::new("http://consul:8500"))
    .load_balancer(RoundRobin::new())
    .timeout(Duration::from_secs(5))
    .build();

let resp = client.get("user-service", "/api/users/42").await?;
```

#### ecat-circuit-breaker

```
ecat-circuit-breaker
├── CircuitBreakerLayer (Tower Layer)
├── 三态：Closed → Open (阈值触发) → HalfOpen (探测)
└── 滑动窗口统计 (可配置窗口大小和阈值)
```

```rust
let layer = CircuitBreakerLayer::builder()
    .failure_ratio(0.5)
    .window(Duration::from_secs(30))
    .half_open_probes(3)
    .build();
```

#### ecat-auth

```
ecat-auth
├── JwtAuthLayer (验证 JWT，提取 claims 到 Context)
├── OAuth2Layer (token introspection)
├── ApiKeyLayer (header/query 提取验证)
└── 统一 AuthClaims 传递用户身份
```

```rust
let jwt = JwtAuthLayer::new(jwk_set_url)
    .require_claims(&["sub", "role"])
    .extract_to_context(true);

let apikey = ApiKeyLayer::new(validator)
    .header_name("X-API-Key");
```

#### ecat-health

```
ecat-health
├── HealthRegistry (注册健康检查)
├── /health (liveness: 进程存活)
├── /ready (readiness: 依赖就绪)
└── 内置检查器: DB ping, Redis ping, upstream 可达
```

```rust
let health = HealthRegistry::new()
    .with_check("db", || db.ping())
    .with_check("redis", || redis.ping())
    .with_check("user-service", || client.health("user-service"));

let app = axum::Router::new()
    .route("/health", health.liveness_handler())
    .route("/ready", health.readiness_handler());
```

#### ecat-registry-consul

```rust
let registry = ConsulRegistry::new("http://consul:8500")
    .with_health_check(HealthCheck::http("/health", Duration::from_secs(10)));

let reg = registry.register(
    ServiceInfo::new("user-service", "1.0.0")
        .with_endpoint("http://10.0.1.5:8000")
).await?;
```

---

### 第二期：数据与消息

目标：完整的数据后端覆盖 + 异步消息

| 新 crate | 用途 | 核心内容 |
|----------|------|----------|
| `ecat-data-redis` | Redis 缓存 | RedisCache 实现 Cache trait |
| `ecat-data-opensearch` | 搜索引擎 | OpenSearchClient 实现 SearchClient trait |
| `ecat-data-influxdb` | 时序数据库 | InfluxClient 实现 TsdbClient trait |
| `ecat-mq` | 消息队列抽象 | MessageQueue trait + Kafka 适配 |
| `ecat-events` | 事件总线 | 进程内 + 跨服务事件 Pub/Sub |
| `ecat-config-remote` | 远程配置 | Consul KV / etcd 配置源，支持热加载 |

#### ecat-data-redis

```rust
let cache = RedisCache::connect("redis://localhost:6379").await?;
cache.set("key", "value", Some(Duration::from_secs(300))).await?;
let val: Option<String> = cache.get("key").await?;
```

#### ecat-mq

```rust
pub trait MessageQueue: Send + Sync {
    async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), MqError>;
    async fn subscribe(&self, topic: &str) -> Result<Box<dyn MessageStream>, MqError>;
}

// 适配器
pub struct KafkaMq { /* ... */ }
pub struct RabbitMq { /* ... */ }
pub struct NatsMq { /* ... */ }
```

#### ecat-events

```rust
// 进程内
let bus = EventBus::local();
bus.subscribe::<UserCreated>(|event| async move {
    tracing::info!("user {} created", event.user_id);
}).await;

// 跨服务
let bus = EventBus::remote(kafka_mq);
bus.publish(UserCreated { user_id: 42 }).await?;
```

---

### 第三期：体验与运维

目标：一键部署、一键压测、API 文档自动生成

| 新 crate | 用途 | 核心内容 |
|----------|------|----------|
| `ecat-testing` | 集成测试工具 | Mock transport、测试 fixture、chaos 注入 |
| `ecat-deploy` | 部署资源 | Dockerfile 模板、K8s manifests、Helm charts |
| `ecat-bench` | 性能基准 | 延迟/吞吐压测，与 actix-go 对比 |
| `ecat-openapi` | API 文档 | 从 protobuf 生成 OpenAPI spec |

---

## 各期产出统计

| 期次 | 新增 crate | 预计代码行数 | 关键交付 |
|------|-----------|-------------|----------|
| 一期 | 5 | ~1,300 | 多服务安全通信、健康检查、Consul 注册 |
| 二期 | 6 | ~1,800 | Redis、消息队列、事件总线、远程配置 |
| 三期 | 4 | ~1,200 | 部署模板、性能基准、API 文档 |

---

## 依赖选型

| 组件 | 推荐 crate | 理由 |
|------|-----------|------|
| HTTP 客户端 | `reqwest` | 异步、成熟、TLS 原生支持 |
| Consul 客户端 | `consul-rs` | 轻量、异步 |
| JWT | `jsonwebtoken` | 纯 Rust、零依赖 |
| Redis | `fred` | 异步、集群支持 |
| Kafka | `rdkafka` | 基于 librdkafka，性能最优 |
| mTLS | `rustls` | 纯 Rust TLS，无 OpenSSL 依赖 |
| OpenSearch | `opensearch` | 官方客户端 |
| InfluxDB | `influxdb2` | 官方 Rust 客户端 |

---

## 实施路线图

```
第一期 (v1.1.x)
  ecat-health       ──────── 1-2天
  ecat-client       ──────── 2-3天
  ecat-circuit-breaker ───── 1天
  ecat-auth         ──────── 2天
  ecat-registry-consul ──── 1天
  ─────────────────────────────
  合计: 7-9天

第二期 (v1.2.x)
  ecat-data-redis   ──────── 1天
  ecat-data-opensearch ──── 1天
  ecat-data-influxdb ────── 1天
  ecat-mq + Kafka   ──────── 2天
  ecat-events       ──────── 1天
  ecat-config-remote ────── 1天
  ─────────────────────────────
  合计: 7天

第三期 (v1.3.x)
  ecat-testing      ──────── 2天
  ecat-deploy       ──────── 2天
  ecat-bench        ──────── 1天
  ecat-openapi      ──────── 1天
  ─────────────────────────────
  合计: 6天
```

---

## 不纳入生态的部分

以下不纳入 e-cat 生态，使用 Rust 社区现有方案：

| 需求 | 推荐方案 | 理由 |
|------|---------|------|
| API 网关 | Kong / Envoy | 成熟，与语言无关 |
| 服务网格 | Linkerd / Istio | Rust 社区无成熟方案 |
| 容器编排 | Kubernetes | 行业标准 |
| 日志收集 | Vector / Fluentd | Rust 原生，已有 |
| CI/CD | GitHub Actions | 生态最完善 |
