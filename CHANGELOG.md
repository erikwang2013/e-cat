# Changelog

## [2.3.1] — 2026-08-06

### Fixed
- 端口绑定规范化：`HttpServer` 空 host 统一为 `0.0.0.0`，示例/文档/CLI 模板的监听地址从 `:8000` 改为 `0.0.0.0:8000`（修复无 IPv6 环境启动失败）
- 全部 HTTP 数据库适配器（ES/OpenSearch/ClickHouse/InfluxDB/IoTDB/QuestDB/TDengine/Neo4j/NebulaGraph/ArangoDB）与 TLS 客户端统一设置 connect/timeout，修复请求永久悬挂
- `ecat-data-memcached` 标记为内存实现并明确文档警告，禁止生产误用（静默数据丢失风险）
- TDengine 写入 SQL 拼接转义标识符与字符串值（`"`/`\`），修复注入逃逸
- 限流修复：`key_fn` 支持按请求取客户端 key；Redis 限流区分存储错误（fail-open）；内存桶定期清理防止无界增长
- JWT 最小密钥长度校验（≥32 字节随机密钥）与错误泛化；OAuth2 客户端复用、设置超时并强制 HTTPS
- Redis 凭据改为 `ConnectionInfo` 单独传参，错误消息不再泄露口令；锁 TTL 溢出统一钳制
- Elasticsearch `search`/`delete` 补充 HTTP 状态码检查；index/id 路径 URL 编码（IDOR）
- etcd deregister 修正为按完整注册键删除，修复实例退出后注册信息残留
- GitHub Actions CI 增加 `protobuf-compiler` 安装，与 GitLab CI 对齐（修复 protoc 缺失必然失败）
- Dockerfile 修复：拷贝实际 `ecat` 二进制（原 `ecat-app` 不存在）、安装 curl 以支持 HEALTHCHECK、builder 镜像升至 1.85（edition 2024）
- 其他：Helm appVersion 更新为 2.3.0；配置示例默认口令全部注释化；consul 注册端口从端点解析、discover 版本不再硬编码；MQ `from_config` 签名统一为 async；11 处 Cargo.toml 依赖收敛至 `workspace.dependencies`；`ecat new` 增加 crate 名校验（防路径穿越与注入）；README.en.md 同步至 v2.3.0

## [2.3.0] — 2026-08-06

### Added
- `ecat-mq-kafka` 真 Kafka 实现（rdkafka，替换内存存根）
- 消息后端：`ecat-mq-rabbitmq`（lapin）、`ecat-mq-mqtt`（rumqttc）、`ecat-mq-nats`（async-nats）
- 数据后端：`ecat-data-mongodb`（DocumentClient）、`ecat-data-s3`（StorageClient，rust-s3）、`ecat-data-tdengine`（REST 时序）
- `ecat-lock` 分布式锁 trait + `ecat-data-redis` 的 `RedisLock`（SET NX PX + token 校验）
- `ecat-scheduler` tokio 定时任务调度（every / once）
- `ecat-tracing-otlp` OpenTelemetry OTLP/gRPC 追踪导出
- `ecat-data` trait 扩展：`DocumentClient`、`StorageClient`；`Cache::increment/ttl/multi_get`、`SearchClient::bulk_index/update`、`TsdbClient::delete` 加法默认方法
- `ecat-middleware` 限流后端抽象（`RateLimitStore`）+ `RedisRateLimitStore`（可选 feature）
- CLI：`--version`、`upgrade`（批量更新 ecat-* 依赖）、`run --watch`（notify 文件监听 + 500ms 防抖重启）
- `.gitlab-ci.yml`（镜像 GitHub Actions CI）

### Changed
- Workspace 扩展至 55 crates
- 数据库后端增至 18 个（+MongoDB、S3、TDengine）

## [2.1.8] — 2026-08-01

### Added
- Per-crate `license.workspace` and `description` metadata for crates.io publishing
- Workspace `repository` and `documentation` URLs
- `.gitignore` for Rust project conventions

### Changed
- `EncryptedSource` → `ObfuscatedSource` (honest naming: XOR is obfuscation, not encryption)
- Config prefix `enc:` → `obfs:`
- All `from_config()` methods return `Result` instead of panicking on TLS errors
- `RdbmsError` gains `Config` variant
- `execute_with`/`query_with` default impls return error instead of silently dropping params
- QuestDB client: GET → POST for SQL execution
- Redis TTL: `set_ex` → `pset_ex` for sub-second precision
- `ecat-data-memcached`: `std::sync::Mutex` → `tokio::sync::Mutex`
- `ecat-registry-etcd`: hand-rolled base64 → `base64` crate
- `ecat-client`: `RandomBalancer` uses `RandomState` instead of `Instant::now()` hash
- `ecat-client`: `StaticResolver::add_service` uses `blocking_write` instead of `try_write`

### Fixed
- `ecat-versioning` header-based routing now actually validates version headers
- Credential URL encoding in `connect_with_auth` methods
- Missing `json` feature for reqwest in `ecat-data-influxdb` and `ecat-data-clickhouse`
- Content-Type headers on HTTP requests (InfluxDB, ClickHouse, IoTDB)
- Removed `#[allow(dead_code)]` annotations via field renaming

### Split
- `ecat-auth` (540 lines) → `claims.rs` + `jwt.rs` + `apikey.rs` + `oauth2.rs` + `helpers.rs` + `lib.rs`

## [2.1.7] — 2026-07-29

### Added
- 11 new database backends: ArangoDB, ClickHouse, Elasticsearch, InfluxDB, IoTDB,
  Memcached, NebulaGraph, Neo4j, OpenSearch, QuestDB, Redis
- `ecat-tls` crate for shared TLS configuration
- `ecat-transport-ws` WebSocket server
- `ecat-versioning` API version routing
- `ecat-deploy` Docker/K8s/Helm deployment templates
- `ecat-registry-etcd` backend
- `ecat-mq-kafka` backend

### Changed
- All data backend configs include optional TLS fields
- `ecat-data` trait system: RdbmsClient, Cache, GraphClient, SearchClient, TsdbClient
