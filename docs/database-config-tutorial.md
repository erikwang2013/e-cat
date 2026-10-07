# 数据库配置教程

**版本:** 2.4.2 · **日期:** 2026-08-01

e-cat 的 14 个数据后端均支持通过配置文件加载连接信息，无需在代码中硬编码。`username` / `password` 均为可选字段，省略则跳过认证。

---

## 快速开始

### 1. 创建配置文件

复制示例模板并根据实际环境修改：

```bash
cp config/databases.example.yaml databases.yaml
```

编辑 `databases.yaml`，填入真实的连接信息：

```yaml
# databases.yaml
sql:
  url: "postgres://myapp:secret@db.internal:5432/myapp"

redis:
  url: "redis://cache.internal:6379"

clickhouse:
  base_url: "http://ch.internal:8123"
  database: "analytics"
```

### 2. 引入依赖

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
ecat-data-sqlx = { path = "../ecat-data-sqlx" }
ecat-data-redis = { path = "../ecat-data-redis" }
ecat-data-clickhouse = { path = "../ecat-data-clickhouse" }
```

### 3. 加载并使用

```rust
use ecat_data_redis::{RedisCache, RedisConfig};
use ecat_data_sqlx::{SqlxClient, SqlxConfig};
use ecat_data_clickhouse::{ClickhouseClient, ClickhouseConfig};
use serde::Deserialize;

#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: RedisConfig,
    clickhouse: ClickhouseConfig,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 加载 YAML 配置
    let yaml = std::fs::read_to_string("databases.yaml")?;
    let cfg: AppConfig = serde_yaml::from_str(&yaml)?;

    // 创建数据库客户端 — 无硬编码连接信息
    let db = SqlxClient::from_config(cfg.sql).await?;
    let cache = RedisCache::from_config(cfg.redis).await?;
    let ch = ClickhouseClient::from_config(cfg.clickhouse);

    // 使用
    let rows = db.query("SELECT id, name FROM users LIMIT 10").await?;
    cache.set("health", b"ok", std::time::Duration::from_secs(30)).await?;

    Ok(())
}
```

---

## 完整配置参考

### 定义顶层配置结构体

```rust
use serde::Deserialize;

#[derive(Deserialize)]
pub struct DatabasesConfig {
    pub sql: ecat_data_sqlx::SqlxConfig,
    pub redis: ecat_data_redis::RedisConfig,
    pub memcached: ecat_data_memcached::MemcachedConfig,
    pub clickhouse: ecat_data_clickhouse::ClickhouseConfig,
    pub questdb: ecat_data_questdb::QuestdbConfig,
    pub elasticsearch: ecat_data_elasticsearch::ElasticsearchConfig,
    pub opensearch: ecat_data_opensearch::OpenSearchConfig,
    pub neo4j: ecat_data_neo4j::Neo4jConfig,
    pub nebulagraph: ecat_data_nebulagraph::NebulaGraphConfig,
    pub arangodb: ecat_data_arangodb::ArangoConfig,
    pub influxdb: ecat_data_influxdb::InfluxConfig,
    pub iotdb: ecat_data_iotdb::IotdbConfig,
}
```

### YAML 完整示例

见 `config/databases.example.yaml`。

---

## 各后端 Config 字段速查

### RDBMS — SqlxConfig

#### 原生池（PostgreSQL / MySQL / SQLite 三路）

按 URL scheme 自动选择原生驱动，无需额外配置：

| scheme | 驱动 |
|---|---|
| `postgres://` / `postgresql://` | PostgreSQL |
| `mysql://` / `mariadb://` | MySQL |
| `sqlite:` | SQLite |

- **时间类型原生支持**：旧版走 sqlx 的 `AnyPool` 时不支持时间类型，需要自己 `CAST` 成文本；现在时间列可直接读取。
- scheme 的**大小写与首尾空白都会被容忍**（`"POSTGRES://…"`、`" postgres://…"` 都可用）；无法识别的 scheme 会报错并**点名**是哪个。
- `mssql://` **不由本后端提供**（走 `ecat-data-mssql`），传给它会被明确拒绝。

#### 配置示例

```yaml
sql:
  url: "postgres://host:5432/dbname"
  # username: "app_user"    # 可选
  # password: "secret"      # 可选
```

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `url` | `String` | — | sqlx 连接串，支持 SQLite/PG/MySQL/TiDB |
| `username` | `Option<String>` | `None` | 可选：嵌入 URL 认证（与 password 配合） |
| `password` | `Option<String>` | `None` | 可选：嵌入 URL 认证（与 username 配合） |
| `max_connections` | `u32` | `10` | 池内最大连接数 |
| `min_connections` | `u32` | `0` | 保底连接数；会被夹到 ≤ `max_connections` |
| `acquire_timeout_secs` | `u64` | `30` | 等连接的超时 |
| `idle_timeout_secs` | `u64` | `600` | 空闲连接回收 |
| `max_lifetime_secs` | `u64` | `1800` | 连接最长存活 |
| `query_timeout_secs` | `u64` | `30` | 单次查询超时；**0 = 禁用** |
| `slow_query_ms` | `u64` | `1000` | 慢查询告警阈值（毫秒）；未配置 = 1000，**0 = 不打**（仅 `tracing` feature） |
| `test_before_acquire` | `bool` | `false` | 取连接时是否先 ping |
| `session_init` | `string[]` | 按方言 | 每条新连接的会话初始化语句 |

#### `session_init`（会话初始化）

每条新连接建立后，会依次执行这组语句：

| 方言 | 默认 `session_init` |
|---|---|
| PostgreSQL | `SET TIME ZONE 'UTC'`, `SET application_name = 'ecat'` |
| MySQL | `SET time_zone = '+00:00'` |
| SQLite | 无会话概念，默认为空 |

目的是让**库侧直接返回 UTC**，与框架「所有时间统一以 RFC3339 UTC 呈现」的约定对齐。

- **显式空数组 `[]` 表示「主动关闭」**，会覆盖方言默认（与 `query_timeout_secs: 0` 表示禁用是同一种「显式覆盖」约定）。
- 任一条语句失败 → 连接创建失败，**不静默降级**。

```yaml
sql:
  url: "mysql://host:3306/dbname"
  session_init:
    - "SET time_zone = '+00:00'"
    - "SET NAMES utf8mb4"
```

#### `warm_up()` — 预热

启动时主动建满 `min_connections` 再归还，让服务起来就是就绪态：

```rust
let db = SqlxClient::from_config(cfg).await?;
db.warm_up().await?;   // 启动时调用一次
```

为什么需要：sqlx 的 `min_connections` 由**后台任务异步维护**，`connect()` 返回时不保证已建满 —— 启动后的第一波请求会与后台任务抢跑。

#### 可观测性 feature（`metrics` / `health` / `tracing`）

三个 feature **默认关闭**（避免把 axum —— `ecat-metrics` / `ecat-health` 的依赖 —— 拖进核心依赖树），按需开启。`ecat-data-mssql` 提供同样的三个。

```toml
ecat-data-sqlx = { path = "../ecat-data-sqlx", features = ["metrics", "health", "tracing"] }
```

```rust
use std::sync::Arc;
use ecat_data_sqlx::{RdbmsHealthCheck, SqlxClient, SqlxConfig, register_pool_metrics};
use ecat_health::HealthRegistry;

let db = SqlxClient::from_config(cfg).await?;

// metrics：注册后 /metrics 端点自动多出四个指标 ——
// ecat_rdbms_pool_connections（gauge，带 state="idle"/"active"）、
// ecat_rdbms_pool_timeouts_total、ecat_rdbms_query_timeout_total、
// ecat_rdbms_transactions_leaked_total（都是 counter，带 backend 标签）
register_pool_metrics("primary", db.pool());

// health：SELECT 1 连通性探针，注册到 /health 的 readyz
let registry = HealthRegistry::new()
    .with_check(RdbmsHealthCheck::new("sql", Arc::new(db)));
```

`tracing` feature 在查询超过 `slow_query_ms` 时打 warn（耗时 + 截断到前 200 字符的 SQL）。该字段只被这个 feature 读取 —— feature 关闭时仍会解析，但不生效。

#### 时间与日期：统一 RFC3339 UTC

**sqlite 的时间/日期文本会被改写为 RFC3339 UTC**：

- 形如 `2026-10-05 12:34:56` 的文本 → `"2026-10-05T12:34:56Z"`
- 形如 `2026-10-05` 的文本 → `"2026-10-05T00:00:00Z"`（UTC 午夜瞬时）

原因：sqlite 没有类型系统，形如日期/时间的文本本就有歧义，框架统一按时间处理。

**副作用**：版本号、业务编码这类「恰好长成日期形状」的文本列也会被改写。如果不需要这种行为，把该列显式 CAST 成非日期形状，或用别的类型。

PG / MySQL 的真实 `DATE` / `TIMESTAMP` 列同样以 RFC3339 UTC 字符串呈现，**纯日期也带 `T00:00:00Z`** —— 因为 `"2026-10-05"` 不是合法 RFC3339，框架内部统一用一种格式，便于上层解析。

### Redis — RedisConfig

```yaml
redis:
  url: "redis://host:6379"
  # password: "auth_token"  # 可选
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `url` | `String` | Redis 连接 URL |
| `password` | `Option<String>` | 可选：Redis AUTH 密码 |

### Memcached — MemcachedConfig

```yaml
memcached:
  # username: "memcache"    # 可选: 保留字段（当前为内存实现）
  # password: "secret"      # 可选: 保留字段
  {}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `username` | `Option<String>` | 可选：保留字段 |
| `password` | `Option<String>` | 可选：保留字段 |

当前为内存实现，认证字段预留。

### ClickHouse — ClickhouseConfig

```yaml
clickhouse:
  base_url: "http://host:8123"
  database: "default"
  # username: "default"   # 可选
  # password: "secret"    # 可选
```

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `base_url` | `String` | — | HTTP 接口地址 |
| `database` | `String` | `"default"` | 数据库名 |
| `username` | `Option<String>` | `None` | 可选：HTTP Basic Auth 用户名 |
| `password` | `Option<String>` | `None` | 可选：HTTP Basic Auth 密码 |

### QuestDB — QuestdbConfig

```yaml
questdb:
  base_url: "http://host:9000"
  # username: "admin"     # 可选
  # password: "quest"     # 可选
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `base_url` | `String` | HTTP API 地址 |
| `username` | `Option<String>` | 可选：HTTP Basic Auth 用户名 |
| `password` | `Option<String>` | 可选：HTTP Basic Auth 密码 |

### Elasticsearch — ElasticsearchConfig

```yaml
elasticsearch:
  base_url: "http://host:9200"
  # username: "elastic"   # 可选
  # password: "secret"    # 可选
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `base_url` | `String` | REST API 地址 |
| `username` | `Option<String>` | 可选：HTTP Basic Auth 用户名 |
| `password` | `Option<String>` | 可选：HTTP Basic Auth 密码 |

### OpenSearch — OpenSearchConfig

```yaml
opensearch:
  base_url: "http://host:9200"
  # username: "admin"     # 可选
  # password: "secret"    # 可选
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `base_url` | `String` | REST API 地址 |
| `username` | `Option<String>` | 可选：HTTP Basic Auth 用户名 |
| `password` | `Option<String>` | 可选：HTTP Basic Auth 密码 |

### InfluxDB — InfluxConfig

```yaml
influxdb:
  base_url: "http://host:8086"
  org: "myorg"
  bucket: "mybucket"
  token: "my-token"
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `base_url` | `String` | InfluxDB 2.x API 地址 |
| `org` | `String` | 组织名 |
| `bucket` | `String` | 桶名 |
| `token` | `String` | 认证令牌 |

### Neo4j — Neo4jConfig

```yaml
neo4j:
  base_url: "http://host:7474"
  username: "neo4j"
  password: "secret"
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `base_url` | `String` | REST API 地址 |
| `username` | `String` | 用户名 |
| `password` | `String` | 密码 |

### NebulaGraph — NebulaGraphConfig

```yaml
nebulagraph:
  base_url: "http://host:19669"
  space: "my_space"
  # username: "root"      # 可选
  # password: "nebula"    # 可选
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `base_url` | `String` | API 地址 |
| `space` | `String` | 图空间名 |
| `username` | `Option<String>` | 可选：HTTP Basic Auth 用户名 |
| `password` | `Option<String>` | 可选：HTTP Basic Auth 密码 |

### ArangoDB — ArangoConfig

```yaml
arangodb:
  base_url: "http://host:8529"
  db: "mydb"
  username: "root"
  password: "secret"
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `base_url` | `String` | API 地址 |
| `db` | `String` | 数据库名 |
| `username` | `String` | 用户名 |
| `password` | `String` | 密码 |

### IoTDB — IotdbConfig

```yaml
iotdb:
  base_url: "http://host:18080"
  username: "root"
  password: "root"
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `base_url` | `String` | REST API 地址 |
| `username` | `String` | 用户名 |
| `password` | `String` | 密码 |

---

## 程序化创建

### 无需认证

```rust
let es = ElasticsearchClient::new("http://localhost:9200");
let ch = ClickhouseClient::new("http://localhost:8123", "default");
```

### 带认证

```rust
let es = ElasticsearchClient::with_auth("http://es:9200", "elastic", "secret");
let ch = ClickhouseClient::with_auth("http://ch:8123", "default", "admin", "pass");
let qdb = QuestdbClient::with_auth("http://qdb:9000", "admin", "quest");
let ng = NebulaGraphClient::with_auth("http://ng:19669", "space1", "root", "nebula");
```

---

---

## TLS 证书配置

各数据后端普遍支持可选的 TLS 客户端认证（`tls` 字段），但有**两处例外**：`ecat-data-sqlx` 不支持该字段，配了会启动报错（其 TLS 走 URL 参数）；`ecat-data-memcached` 的该字段**静默无效** —— 声明了，但全 crate 无人读取。

### 配置示例

```yaml
clickhouse:
  base_url: "https://ch.internal:8443"
  tls:
    ca_cert: "/etc/ecat/ca.pem"
    client_cert: "/etc/ecat/client.pem"
    client_key: "/etc/ecat/client-key.pem"
    # skip_verify: true  # 仅测试环境
```

### 证书自动生成（ecat-tls）

```rust
use ecat_tls::{generate_ca, generate_server_cert, generate_client_cert};

// 1. 生成 CA
let ca = generate_ca("MyOrg")?;
std::fs::write("ca.pem", &ca.cert_pem)?;
std::fs::write("ca-key.pem", &ca.key_pem)?;

// 2. 生成服务端证书
let srv = generate_server_cert("db.example.com")?;
std::fs::write("server.pem", &srv.cert_pem)?;
std::fs::write("server-key.pem", &srv.key_pem)?;

// 3. 生成客户端证书（mTLS）
let client = generate_client_cert("myapp")?;
std::fs::write("client.pem", &client.cert_pem)?;
std::fs::write("client-key.pem", &client.key_pem)?;
```

### 手动生成（OpenSSL）

```bash
# CA
openssl req -x509 -newkey rsa:4096 -keyout ca-key.pem -out ca.pem -days 3650 -nodes

# 服务端证书
openssl req -new -newkey rsa:4096 -keyout server-key.pem -out server.csr -nodes -subj "/CN=db.example.com"
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -out server.pem -days 365

# 客户端证书 (mTLS)
openssl req -new -newkey rsa:4096 -keyout client-key.pem -out client.csr -nodes -subj "/CN=myapp"
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca-key.pem -out client.pem -days 365
```

### TLS 字段说明

| 字段 | 类型 | 说明 |
|------|------|------|
| `ca_cert` | `Option<String>` | CA 证书 PEM 路径（验证服务端） |
| `client_cert` | `Option<String>` | 客户端证书 PEM 路径（mTLS） |
| `client_key` | `Option<String>` | 客户端私钥 PEM 路径（mTLS） |
| `skip_verify` | `Option<bool>` | 跳过证书验证（仅测试） |

---

## 进阶用法

### 环境变量覆盖

```rust
use std::env;

fn load_config() -> Result<SqlxConfig, Box<dyn std::error::Error>> {
    let mut cfg: SqlxConfig = serde_yaml::from_str(
        &std::fs::read_to_string("databases.yaml")?
    )?;
    if let Ok(url) = env::var("DATABASE_URL") {
        cfg.url = url;
    }
    Ok(cfg)
}
```

### 结合 ecat-config 框架

```rust
use ecat_config::{Config, FileSource};

let mut app_config = Config::new();
app_config.load(&FileSource::new("databases.yaml")).await?;

let redis_cfg: RedisConfig = serde_json::from_value(
    app_config.get::<serde_json::Value>("redis").unwrap()
)?;
let cache = RedisCache::from_config(redis_cfg).await?;
```

### 按需配置

不用的数据库在 YAML 中省略，Rust 结构体用 `Option` 标记：

```rust
#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: Option<RedisConfig>,
    clickhouse: Option<ClickhouseConfig>,
}
```

---

## 相关文档

- [审计报告 r5](audit-report-2026-08-01-r5.md)
- [TLS 证书认证教程](tls-certificate-tutorial.md)
- [示例配置文件](../config/databases.example.yaml)
