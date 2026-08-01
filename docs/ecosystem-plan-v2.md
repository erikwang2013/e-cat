# e-cat 生态规划 v2 — 已完成与后续

**版本:** 2.3.0  
**日期:** 2026-08-01  
**状态:** 全部规划已完成，39 crates

---

## 一、已完成（三期全部交付）

| 期次 | Crate | 能力 | 测试 |
|------|-------|------|------|
| 一期 | `ecat-health` | 健康检查（/health、/ready） | 4 |
| 一期 | `ecat-client` | HTTP 客户端 + 服务发现 + 负载均衡 | 7 |
| 一期 | `ecat-circuit-breaker` | 三态熔断器（Tower Layer） | 4 |
| 一期 | `ecat-auth` | JWT + API Key 认证中间件 | 8 |
| 一期 | `ecat-registry-consul` | Consul 服务注册 | 2 |
| 二期 | `ecat-data-redis` | Redis 缓存（Cache trait） | 1 |
| 二期 | `ecat-mq` | 消息队列抽象 + InMemoryMq | 2 |
| 二期 | `ecat-events` | 本地 + 远程事件总线 | 2 |
| 二期 | `ecat-config-remote` | Consul KV 远程配置 | 2 |
| 三期 | `ecat-testing` | MockServer + ChaosConfig | 5 |
| 三期 | `ecat-openapi` | OpenAPI 3.0 spec 生成 | 2 |
| 三期 | `ecat-bench` | 并发性能基准 | 2 |
| 三期 | `ecat-deploy` | Dockerfile + K8s manifests | — |

---

## 二、剩余缺口

### 高优先级（生产必需）

| 缺口 | 影响 | 方案 |
|------|------|------|
| **gRPC 客户端** | 无法通过 gRPC 调用其他服务 | `ecat-client` 增加 `GrpcClient`，集成 tonic channel + 服务发现 |
| **OAuth2 认证** | 不支持第三方登录 | `ecat-auth` 增加 `OAuth2Layer`，token introspection |
| **mTLS 支持** | 服务间无双向 TLS | `ecat-transport` 增加 rustls 配置入口 |
| **分布式追踪** | 无法跨服务追踪请求 | 集成 opentelemetry（Export traces） |

### 中优先级（体验提升）

| 缺口 | 影响 | 方案 |
|------|------|------|
| **etcd 注册** | 仅支持 Consul | `ecat-registry-etcd` 实现 Registry trait |
| **Kafka 适配器** | MQ 仅有内存实现 | `ecat-mq` 增加 `KafkaMq`（rdkafka） |
| **OpenSearch 后端** | SearchClient trait 无实现 | `ecat-data-opensearch` |
| **InfluxDB 后端** | TsdbClient trait 无实现 | `ecat-data-influxdb` |
| **Helm Charts** | 无 Helm 部署 | `ecat-deploy` 增加 Chart.yaml + values |
| **WebSocket 支持** | 无实时推送 | `ecat-transport-ws` 基于 axum ws |

### 低优先级（锦上添花）

| 缺口 | 影响 | 方案 |
|------|------|------|
| **GraphQL 集成** | 无 GraphQL endpoint | `ecat-graphql` 基于 async-graphql |
| **API 版本管理** | 无版本策略 | `ecat-versioning` 基于 header/path 路由 |
| **速率限制后端** | RateLimitLayer 仅内存 | Redis 后端 |
| **配置加密** | 敏感配置明文 | `ecat-config` 增加加密 source |
| **CI/CD 模板** | 无自动化流水线 | GitHub Actions / GitLab CI 模板 |
| **性能对比报告** | 无生态对比 | Bench 对比 actix-web / tonic 原生 |

---

## 三、后续四期规划

### 第四期：通信与安全强化（v2.1.0）✅ 已完成

| Crate | 内容 | 状态 |
|-------|------|------|
| `ecat-client` 扩展 | GrpcClient | ✅ |
| `ecat-auth` 扩展 | OAuth2Layer | ✅ |
| `ecat-transport` 扩展 | mTLS config (TlsConfig) | ✅ |
| `ecat-tracing`（新） | 分布式追踪（span + trace_id） | ✅ |

### 第五期：数据后端补齐（建议 v2.2.0）

| Crate | 内容 | 预计行数 |
|-------|------|----------|
| `ecat-registry-etcd`（新） | etcd Registry | ~150 |
| `ecat-mq-kafka`（新） | Kafka 适配器 | ~200 |
| `ecat-data-opensearch`（新） | SearchClient 实现 | ~150 |
| `ecat-data-influxdb`（新） | TsdbClient 实现 | ~150 |
| 合计 | 4 个新 crate | ~650 |

### 第六期：运维与体验（建议 v2.3.0）

| Crate | 内容 | 预计行数 |
|-------|------|----------|
| `ecat-deploy` 扩展 | Helm Charts | ~80 |
| `ecat-transport-ws`（新） | WebSocket 支持 | ~200 |
| `ecat-versioning`（新） | API 版本路由 | ~120 |
| CI/CD 模板 | GitHub Actions | ~60 |
| 合计 | 4 项 | ~460 |

---

## 四、版本路线图

```
v1.0.x  核心骨架（18 crates）                    ✅ 已完成
v2.0.x  生态一期～三期（+13 crates = 31 total）   ✅ 已完成
v2.1.x  通信与安全强化（+1 crate, 3 扩展）        ✅ 已完成
v2.2.x  数据后端补齐（+4 crates）                 ✅ 已完成
v2.3.x  运维与体验（+2 crates, 2 扩展）           📋 规划中
```

---

## 五、不纳入生态的部分

| 需求 | 方案 | 理由 |
|------|------|------|
| API 网关 | Kong / Envoy | 语言无关，成熟 |
| 服务网格 | Linkerd | Rust 无成熟方案 |
| 容器编排 | Kubernetes | 行业标准 |
| 日志收集 | Vector | Rust 原生，已有 |
