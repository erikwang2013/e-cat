# ecat-orm 与 SQL Server 后端设计 — 完整 ORM + 第 16 个数据后端

日期：2026-10-05
状态：已批准（用户确认「确认」）

## 背景

现状（`ecat-data` + `ecat-data-sqlx`）只提供手写 SQL 的 RDBMS 客户端：

- **无 ORM**：仓库内 grep 不到 diesel / sea-orm / entity / migration，查询结果为通用
  `Row`（列名 + `serde_json::Value`），无实体映射、关联、迁移。
- **无 SQL Server**：sqlx 主库无 MSSQL 驱动（0.7 前移除，重写未发布），
  `AnyPool` 仅覆盖 SQLite / PostgreSQL / MySQL。
- **事务不能执行 SQL**：`TransactionInner` 只有 `commit` / `rollback`
  （`ecat-data/src/rdbms.rs:31-34`），`Transaction` 无任何查询方法 —— ORM 的
  「事务集成」在当前抽象下无法实现。
- **连接池参数未暴露**：`SqlxConfig` 只有 `url` / `username` / `password` / `tls`
  （`ecat-data-sqlx/src/lib.rs:10-21`），池大小走 sqlx 默认值。

## 查证结论（2026-10-05）

| 候选 | 事实 | 结论 |
|---|---|---|
| `sqlx` 主库 MSSQL | 0.7 前移除，重写未发布 | ❌ 不可用 |
| `sea-orm` / `diesel` 开源版 | 仅 PG / MySQL / SQLite；MSSQL 在商业版 SeaORM X | ❌ 不可用 |
| `tiberius` | **RUSTSEC-2020-0010「unmaintained」** | ❌ CI 跑 `cargo-audit --deny warnings`，会挂 |
| `tiberius-ng` 0.13.1 | 维护续作，0 条 advisory，Apache-2.0 | ✅ 采用 |
| `deadpool-tiberius` / `bb8-tiberius` | 停在 2025，均依赖原版 `tiberius` | ❌ 会引入 unmaintained 传递依赖 |
| `chrono` | RUSTSEC-2020-0159 | ❌ 改用 `time` 0.3（已在 Cargo.lock） |

## 决策

| 项 | 决策 |
|---|---|
| ORM 路线 | 自研 `ecat-orm`，建在统一 `SqlExecutor` trait 之上 → 天然覆盖全部 RDBMS |
| SQL Server 驱动 | `tiberius-ng` 0.13 + `deadpool` 自写 Manager（≈50 行） |
| 方言抽象 | `Dialect` 枚举放 `ecat-data`，SQL 生成规则放 `ecat-orm` |
| 事务执行 | 拆出 `SqlExecutor` supertrait，`Transaction` 也实现它 |
| 时间类型 | `time::OffsetDateTime`，在 `Row` 内统一序列化为 RFC3339 字符串，存前归一化 UTC |
| **sqlx 驱动** | **弃用 `AnyPool`，改原生池**（PG / MySQL / SQLite 三路分派）→ 时间类型原生可用，**不写任何 CAST 绕过** |
| **连接池增强** | 查询超时 + 预热 `warm_up()` + 智能 recycle + 熔断（复用 `ecat-circuit-breaker`）+ 读写分离 + 事务泄漏计数 |
| **可观测性** | 池指标 → `ecat-metrics`、池探活 → `ecat-health`、慢查询 → `ecat-tracing`（均为后端 crate 的 opt-in feature） |
| 迁移执行 | **程序内 `Migrator`**（用户 `main.rs` / `src/bin/migrate.rs`）。**取消 CLI 方案** —— `ecat-cli` 不链接用户代码，看不到实体定义 |
| 版本 | workspace 3.0.3 → **4.0.0**（trait 拆分 + `from_pool` 签名变更为破坏性变更） |

## 0. 评审补充（2026-10-05 第二轮，用户确认全收）

初版设计评审后追加的项，正文已并入：

| 项 | 位置 |
|---|---|
| 弃用 `AnyPool` 改原生池 → 删除 CAST 绕过机制 | §4、§6 时间策略 |
| 查询超时（池耗尽的头号原因） | §2.6 |
| 池预热 `warm_up()` + 智能 recycle | §3、§4 |
| 熔断（复用 `ecat-circuit-breaker`，抽出公开 `Breaker`） | §2.5 |
| 读写分离 `RdbmsRouting` + `query_write` 写路径 | §2.5、§2.2 |
| 批量分块（MSSQL 2100 参数上限） | §5.5(b) |
| 标识符白名单（`filter`/`order_by`/`join` 的注入面） | §5.5(a) |
| MySQL 取回自增 ID 必须包事务（连接池下的静默错误） | §6 |
| `insert_many` 改为返回行数；分页补 COUNT 查询 | §5.4、§5.6 |
| 时间归一化 UTC；`EntityMeta` 改 `&'static` | §5.4、§5.3 |
| 可观测性三 feature（metrics / health / tracing） | §7.5 |
| **取消迁移 CLI**（`ecat-cli` 看不到用户实体） | §7 |

第三轮（同日，用户追问「连接池还有提升吗」）：

| 项 | 位置 |
|---|---|
| 路由跳过已熔断端点（避免稳定 1/N 失败率）+ 副本全挂时降级策略 | §2.5 |
| sqlx `test_before_acquire` 默认改 `false`，与 deadpool 侧智能 recycle 对齐 | §2.8 |
| 会话初始化钩子（PG/MySQL 会话级 UTC、MSSQL `ARITHABORT ON`） | §2.7 |

## 1. 新增 crate

```
ecat-data-mssql/     SQL Server 后端（tiberius-ng + deadpool），实现 SqlExecutor
ecat-orm/            ORM 本体：实体、查询、关联、批量、迁移
ecat-orm-derive/     proc-macro：#[derive(Entity)] / #[derive(FromRow)]
```

proc-macro 必须是独立 crate，这是 3 个而非 2 个的原因。

依赖（workspace）：
```toml
# default-features = false 是必需的：默认开 native-tls + winauth，
# 与项目 rustls 栈冲突，且 winauth 会引入 SSPI 依赖。
tiberius-ng = { version = "0.13", default-features = false, features = ["rustls", "tds73", "time"] }
deadpool = "0.13"
tokio-util = { version = "0.7", features = ["compat"] }   # TCP → AsyncRead/Write 适配
time = { version = "0.3", features = ["formatting", "parsing", "serde"] }
tokio = { version = "1", features = ["sync"] }            # ecat-data 新增（见 §2.3）
```

`tiberius-ng` 自带 `time` feature（其 `chrono` feature 不用，避开 chrono 的
RUSTSEC-2020-0159）。

## 2. `ecat-data` 核心改动

### 2.1 方言枚举

```rust
// ecat-data/src/dialect.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect { Standard, Sqlite, Postgres, MySql, Mssql }
```

**必须放 `ecat-data`**：`ecat-data-sqlx` 需要上报自己的方言，而它不能依赖 `ecat-orm`。

### 2.2 trait 拆分

```rust
// ecat-data/src/rdbms.rs
#[async_trait]
pub trait SqlExecutor: Send + Sync {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError>;
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError>;
    async fn execute_with(&self, sql: &str, params: &[serde_json::Value]) -> Result<u64, RdbmsError>;
    async fn query_with(&self, sql: &str, params: &[serde_json::Value]) -> Result<Vec<Row>, RdbmsError>;
    fn dialect(&self) -> Dialect;                       // 新增

    /// 写路径且需返回结果（`INSERT ... RETURNING` / `OUTPUT INSERTED`）。
    /// 默认委托 `query_with`；只有 `RdbmsRouting` 覆写为强制走主库
    /// —— 否则写语句会被路由到从库。
    async fn query_write(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        self.query_with(sql, params).await
    }
}

#[async_trait]
pub trait RdbmsClient: SqlExecutor {                    // 只剩这一个方法
    async fn transaction(&self) -> Result<Transaction, RdbmsError>;
}
```

四个执行方法的默认实现（返回「不支持参数化」错误）从 `RdbmsClient` 迁到 `SqlExecutor`，
语义不变。

### 2.3 事务内可执行 SQL

`TransactionInner` 增加四个执行方法，`Transaction` 实现 `SqlExecutor`：

```rust
#[async_trait]
pub trait TransactionInner: Send {
    async fn execute(&mut self, sql: &str) -> Result<u64, RdbmsError>;
    async fn query(&mut self, sql: &str) -> Result<Vec<Row>, RdbmsError>;
    async fn execute_with(&mut self, sql: &str, p: &[Value]) -> Result<u64, RdbmsError>;
    async fn query_with(&mut self, sql: &str, p: &[Value]) -> Result<Vec<Row>, RdbmsError>;
    fn dialect(&self) -> Dialect;
    async fn commit(&mut self) -> Result<(), RdbmsError>;
    async fn rollback(&mut self) -> Result<(), RdbmsError>;
}
```

`Transaction` 内部改为 `tokio::sync::Mutex<Box<dyn TransactionInner>>`
（`SqlExecutor::execute` 取 `&self`，而 inner 需要 `&mut`；std `Mutex` 跨 await 持锁
不安全）。**`ecat-data` 因此新增 `tokio = { features = ["sync"] }` 依赖** ——
目前它只有 dev-dependency tokio。`commit` / `rollback` 保持 `self` 消费语义与现有签名。
`Drop` 告警逻辑不变（`ecat-data/src/rdbms.rs:74-84`）。

### 2.4 破坏性影响

外部若直接实现过 `RdbmsClient` 会编译失败（方法移到 supertrait）。仓库内实现者共 **4 个**：

| 实现者 | 位置 | `dialect()` 取值 |
|---|---|---|
| `SqlxClient` | `ecat-data-sqlx/src/lib.rs:142` | 由池的类型决定 |
| `ClickhouseClient` | `ecat-data-clickhouse/src/lib.rs:228` | `Standard` |
| `QuestdbClient` | `ecat-data-questdb/src/lib.rs:71` | `Standard` |
| `RawOnlyClient`（测试桩） | `ecat-data/src/rdbms.rs:268` | `Standard` |

> 初版 spec 写的「仓库内实现者只有 `SqlxClient` 与测试桩」是**错的**（照 2026-08-01 的审计
> 报告抄的，未 grep `impl`）。ClickHouse 的实现在 2026-08-06「四项缺口」那次工作里加的。
> 由实现者在本批次发现并订正。

另有 `ecat-data-clickhouse/src/tests.rs:246/267/278` 三处 UFCS 调用
（`ecat_data::RdbmsClient::query(&client, ..)`）需改为 `SqlExecutor::query`。

**ClickHouse / QuestDB 取 `Standard` 的理由**：两者都走 HTTP 接口、**不支持参数绑定**
（其 `_with` 方法落到默认的「not supported」错误）。若声明为 `Postgres`（QuestDB 的 SQL 是
Postgres 味），ORM 会生成 `$1` 占位符发给一个无法绑定参数的传输层 —— 承诺不存在的能力。
取 `Standard` 则错误来自真实的能力缺失，语义准确。将其纳入 ORM 方言层是另开一次的决策
（见 §10 非目标）。

按 4.0.0 发布。

### 2.5 组合式包装器（新增两个模块）

三个 `SqlExecutor` 包装器均放 `ecat-data`（同一 trait 所在处，且不依赖 ORM）：

```rust
// ecat-data/src/routing.rs —— 读写分离
pub struct RdbmsRouting {
    primary: Arc<dyn RdbmsClient>,
    replicas: Vec<Arc<dyn RdbmsClient>>,
    next: AtomicUsize,                                    // round-robin，无需锁
}
#[async_trait] impl SqlExecutor for RdbmsRouting {
    // execute / execute_with            → primary（写）
    // query / query_with                → replicas 轮询（读）
    // query_write                       → primary（写路径的返回行查询）
    // dialect()                         → primary.dialect()
}
#[async_trait] impl RdbmsClient for RdbmsRouting {
    // transaction()                     → primary（事务天然读主，规避副本延迟）
}

// ecat-data/src/breaker.rs —— 熔断
pub struct CircuitBreakerExecutor<S> { inner: S, breaker: Breaker }
```

**熔断复用 `ecat-circuit-breaker`**：该 crate 目前只暴露 tower 的
`CircuitBreakerLayer` / `CircuitBreakerService`（`ecat-circuit-breaker/src/lib.rs:80,158`），
状态机 `BreakerInner` 是私有的。本次把状态机抽成公开 `Breaker` 类型 + 泛型异步方法：

```rust
// ecat-circuit-breaker 新增（tower 层改为调用它，行为不变）
impl Breaker {
    pub fn new(cfg: BreakerConfig) -> Self;
    pub async fn call<F, Fut, T, E>(&self, f: F) -> Result<T, BreakerError<E>>
    where F: FnOnce() -> Fut, Fut: Future<Output = Result<T, E>>;
}
```

熔断器**逐端点包装**（每个 primary / replica 各一个），而非包在路由外层 —— 否则单个从库
故障会误熔断整条链路。组合顺序：

```rust
let primary  = CircuitBreakerExecutor::new(sqlx_primary,  cfg.clone());
let replicas = replica_clients.into_iter()
    .map(|c| CircuitBreakerExecutor::new(c, cfg.clone())).collect();
let db = RdbmsRouting::new(primary, replicas);
```

**路由必须跳过已熔断的端点。** 只做"逐端点包熔断"是不够的：从库挂掉后，轮询仍会把
1/N 的读请求转过去，靠熔断快速失败 —— 那不是故障隔离，是**稳定的 1/N 失败率**。
因此 `RdbmsRouting` 选端点时读取各端点的熔断状态，`Open` 的直接跳过：

```rust
pub struct RdbmsRouting {
    primary: Endpoint,                    // Endpoint = CircuitBreakerExecutor<...>
    replicas: Vec<Endpoint>,
    next: AtomicUsize,
    fallback_to_primary: bool,            // 副本全不可用时是否降级读主，默认 true
    // fallback_to_primary = false 时直接返回 RdbmsError::NoAvailableReplica
}
```

副本全部熔断时：`fallback_to_primary = true`（默认）读请求降级到主库；
`false` 则快速失败。两种行为都要有测试。

### 2.6 查询超时（在 `RdbmsClient` 实现内部，不做成包装器）

超时若做成包装器，`transaction()` 的返回类型会变形（`TimeoutExecutor<S>::transaction()`
返回的 `Transaction` 需要额外包一层 `TransactionInner`），代价大于收益。改为在
**后端实现内部**应用，两端（客户端与事务 wrapper）都覆盖：

```rust
// ecat-data/src/timeout.rs —— 两个后端共用
pub(crate) async fn with_timeout<F, T>(t: Option<Duration>, fut: F) -> Result<T, RdbmsError>
where F: Future<Output = Result<T, RdbmsError>>;
// 超时 → RdbmsError::Timeout，并递增 ecat_rdbms_query_timeout_total 计数
```

配置字段 `query_timeout_secs`（后端 config 内，默认 30，`0` = 禁用）。
sqlx 侧另设服务端兜底：PG 连接参数 `statement_timeout`、MySQL `max_execution_time`。

### 2.7 会话初始化钩子

sqlx 侧用 `PoolOptions::after_connect`、deadpool 侧用 `PoolBuilder::post_create`，
在每条新连接建立后执行一组会话设置语句。配置字段
`session_init: Option<Vec<String>>`（`None` 时用各后端默认值）：

| 后端 | 默认语句 | 作用 |
|---|---|---|
| PostgreSQL | `SET TIME ZONE 'UTC'`、`SET application_name = 'ecat'` | 库侧直接返回 UTC（加固 §5.4 的 UTC 约定）；`application_name` 让连接在 `pg_stat_activity` 里可辨识 |
| MySQL | `SET time_zone = '+00:00'` | 同上 |
| SQL Server | `SET ARITHABORT ON` | 避免**计划缓存污染**（ARITHABORT 取值不同会让同一查询产生多份执行计划），也是索引视图可用性的前提 |
| SQLite | 无 | 无会话概念 |

任一条语句执行失败 → **连接创建失败**（`Manager::create` 返回 Err），不静默降级。

### 2.8 取连接的探活策略（两个后端对齐）

两个池的取用路径都有一次多余往返，必须都处理：

| 后端 | 机制 | 默认 | 决策 |
|---|---|---|---|
| sqlx | `test_before_acquire`（每次 acquire 发一次 ping，不论上次 ping 多近 —— sqlx issue #1743） | `true` | 暴露为 `test_before_acquire` 配置，**默认改 `false`** |
| deadpool | 我们自己的 `Manager::recycle` | — | 按空闲时长决定是否 `SELECT 1`（§3） |

关闭每次 ping 后，死连接的兜底由三件事共同保证：`max_lifetime` 轮换、
查询超时（§2.6）、以及首次使用时的报错。这正是本设计要"两个后端行为一致"的原因
—— 只优化一侧会造成同一问题只解决一半。

## 3. `ecat-data-mssql`

```
src/lib.rs      MssqlClient + SqlExecutor 实现 + 参数绑定
src/config.rs   MssqlConfig + URL 解析（mssql://user:pass@host:1433/db?encrypt=...）
src/pool.rs     deadpool Manager
```

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct MssqlConfig {
    pub url: String,
    #[serde(default)] pub username: Option<String>,
    #[serde(default)] pub password: Option<String>,
    #[serde(default)] pub max_connections: Option<usize>,      // 默认 10
    #[serde(default)] pub min_connections: Option<usize>,      // 默认 0
    #[serde(default)] pub acquire_timeout_secs: Option<u64>,   // 默认 30
    #[serde(default)] pub idle_timeout_secs: Option<u64>,      // 默认 600
    #[serde(default)] pub query_timeout_secs: Option<u64>,     // 默认 30，0 = 禁用
    #[serde(default)] pub session_init: Option<Vec<String>>,   // 默认 SET ARITHABORT ON（§2.7）
}

pub struct MssqlClient { pool: deadpool::managed::Pool<MssqlManager> }

impl MssqlClient {
    pub async fn connect(url: &str) -> Result<Self, RdbmsError>;
    pub async fn from_config(cfg: MssqlConfig) -> Result<Self, RdbmsError>;
    pub fn from_pool(pool: Pool<MssqlManager>) -> Self;
    /// 启动时预热：并发取 min_connections 个连接再放回。
    pub async fn warm_up(&self) -> Result<(), RdbmsError>;
    pub fn pool_status(&self) -> PoolStatus;                   // 供 metrics
}
```

- **参数绑定**：`serde_json::Value` → `&dyn tiberius_ng::ToSql`，SQL 内占位符为
  `@P1..@Pn`。沿用现有契约（调用方写后端原生占位符，与 `SqlxClient` 一致）。
- **参数生命周期**：tiberius 的 `query(sql, &[&dyn ToSql])` 需要借用，实现内先把
  `Value` 物化成 `enum Bind { I64(i64), F64(f64), Str(String), Bool(bool), Null }`
  持有所有值，再构造引用切片。
- **行转换**：tiberius `Row` → `ecat_data::Row`。类型分派用 `ColumnData` 匹配
  （`I64`/`I32`/`I16`/`U8`/`F64`/`F32`/`Bit`/`String`/`Guid`/`Numeric`/`DateTime`/
  `DateTime2`/`Bytes`/`Null`），时间类型转 RFC3339 字符串（**统一转 UTC**），
  `Bytes` 转 base64（与 `cell_to_json` 的既有约定一致）。
- **池**：
  - `create`：TCP 连接（`tokio_util::compat` 适配）→ `Client::connect`（`rustls` TLS）。
  - `recycle`：**智能探活** —— `Metrics` 显示空闲时长低于阈值（默认 5s）直接返回
    `Ok`，超阈值才跑 `SELECT 1`。省掉高频场景下每次归还的一个往返。
  - `warm_up()`：SQL Server 建连含 TDS 握手 + TLS + 认证（几十~几百 ms），
    `min_connections` 不会自动建满，必须主动预热。
- **超时**：每个执行方法经 `with_timeout`（§2.6）。
- **构造器命名**：遵循 README 既有约定，主构造器 `connect`（与 `ecat-data-sqlx` 一致）。
- **可选 feature**（默认关，避免把 axum 拖进核心依赖）：
  - `metrics` → `register_pool_metrics()` 注册 `ecat_rdbms_pool_connections{backend,state}`
  - `health` → `RdbmsHealthCheck` 实现，接 `/health`

## 4. `ecat-data-sqlx` 改动（弃用 `AnyPool`，改原生池）

### 4.1 动机

`AnyPool` 的四处代价（`ecat-data-sqlx/src/lib.rs:34-41, 83-121`）：

| 代价 | 换原生池后 |
|---|---|
| 时间类型不可用 → 需要靠 `CAST(... AS TEXT)` 绕过 | 原生池支持 `time` 类型 → **绕过机制整个删除** |
| 每次查询走动态分派，statement cache 受限 | 原生分派 + 池级 statement cache |
| `install_default_drivers` 的 Once 保护 + 「No drivers installed」panic 面 | 整段消失 |
| 方言靠 URL 猜测 | 池的类型即方言 |

### 4.2 结构

```rust
enum Pool {                                  // 内部私有
    Pg(sqlx::PgPool),
    My(sqlx::MySqlPool),
    Sq(sqlx::SqlitePool),
}

pub struct SqlxClient { pool: Pool }

impl SqlxClient {
    pub async fn connect(url: &str) -> Result<Self, sqlx::Error>;   // 按 scheme 分派
    pub async fn from_config(cfg: SqlxConfig) -> Result<Self, sqlx::Error>;
    pub fn from_pool(pool: Pool) -> Self;        // ⚠️ 公开签名变更（原 AnyPool）
    pub async fn warm_up(&self) -> Result<(), RdbmsError>;
}
```

`SqlExecutor` 的每个方法做一次三路 `match` 分派（`Pg/My/Sq` 各自的原生
`query`/`execute`），行为与现状一致。`dialect()` 由 `Pool` 变体直接决定，
不再解析 URL。

### 4.3 `SqlxConfig` 扩展

```rust
pub struct SqlxConfig {
    pub url: String,
    #[serde(default)] pub username: Option<String>,
    #[serde(default)] pub password: Option<String>,
    #[serde(default)] pub tls: Option<TlsClientConfig>,
    #[serde(default)] pub max_connections: Option<usize>,      // 默认 10
    #[serde(default)] pub min_connections: Option<usize>,      // 默认 0
    #[serde(default)] pub acquire_timeout_secs: Option<u64>,   // 默认 30
    #[serde(default)] pub idle_timeout_secs: Option<u64>,      // 默认 600
    #[serde(default)] pub max_lifetime_secs: Option<u64>,      // 默认 1800
    #[serde(default)] pub query_timeout_secs: Option<u64>,     // 默认 30，0 = 禁用
    #[serde(default)] pub test_before_acquire: Option<bool>,   // 默认 false（§2.8）
    #[serde(default)] pub session_init: Option<Vec<String>>,   // 默认按方言取（§2.7）
}
```

### 4.4 行转换与事务

- `cell_to_json` 的类型链改为：`bool → i64 → i32 → f64 → OffsetDateTime(→RFC3339 UTC)
  → String → Vec<u8>(base64) → Null`（`sqlx` 加 `time` feature）。
  **这就是删除 CAST 机制的那一步**。
- `SqlxTransactionWrapper` 补齐四个执行方法 + `dialect()`（委托 `sqlx::Transaction`），
  并同样应用 `with_timeout`。
- 可选 feature `metrics` / `health` 与 §3 同构。

## 5. `ecat-orm` 本体

### 5.1 用户代码形态

```rust
#[derive(Entity)]
#[entity(table = "users")]
pub struct User {
    #[entity(pk, auto_increment)] pub id: i64,
    pub name: String,
    pub email: Option<String>,
    #[entity(created_at)]  pub created_at: Option<OffsetDateTime>,
    #[entity(updated_at)]  pub updated_at: Option<OffsetDateTime>,
    #[entity(soft_delete)] pub deleted_at: Option<OffsetDateTime>,
    #[entity(version)]     pub version: i64,
    #[entity(has_many = "Post", foreign_key = "user_id")]
    pub posts: Vec<Post>,
}
```

### 5.2 模块与文件布局

```
src/lib.rs             re-export + crate 文档
src/error.rs           OrmError（thiserror）
src/entity.rs          Entity trait / ColumnMeta / EntityMeta
src/crud.rs            insert / find_by_id / find_all / update / delete / save
src/batch.rs           insert_many / update_many / delete_where / upsert
src/page.rs            Page<T> { items, total, page, per_page }
src/relation.rs        RelationMeta trait + 预加载执行（一次 IN 查询，杜绝 N+1）
                       derive 为每个实体生成 `XxxRelation` 枚举（如 UserRelation::Posts）
src/query/mod.rs       Query 构建器（类型状态：Unfiltered → Filtered）
src/query/filter.rs    Op（Eq/Ne/Lt/Le/Gt/Ge/Like/In/NotIn/IsNull/NotNull）+ 表达式树
src/query/sql.rs       SELECT 生成 + join + 分页
src/dialect/mod.rs     DialectSpec trait + lookup(Dialect) -> &'static dyn DialectSpec
src/dialect/standard.rs
src/dialect/sqlite.rs
src/dialect/postgres.rs
src/dialect/mysql.rs
src/dialect/mssql.rs
src/migrate/mod.rs     Migrator（up / down / status）
src/migrate/ddl.rs     实体元数据 → CREATE TABLE / DROP TABLE / 类型映射
src/migrate/version.rs _ecat_migrations 版本表读写
src/time.rs            OffsetDateTime ↔ RFC3339 字符串互转
```

每个文件 < 500 行（项目规则）。

### 5.3 核心 trait

```rust
pub trait Entity: Sized + Send + Sync {
    const TABLE: &'static str;
    const PK: &'static str;
    const META: &'static EntityMeta;                 // 列名、类型、标志位
    fn from_row(row: &Row) -> Result<Self, OrmError>;
    fn to_values(&self) -> Vec<(&'static str, serde_json::Value)>;
    fn pk_value(&self) -> serde_json::Value;
}
```

`EntityMeta` 由 derive 生成，供查询构建器与迁移 DDL 共用，避免宏生成大段重复代码。
内部必须是 `const` 可构造的静态切片，不能用 `Vec`：

```rust
pub struct EntityMeta {
    pub table: &'static str,
    pub pk: &'static str,
    pub columns: &'static [ColumnMeta],        // 非 Vec —— 否则无法 const
    pub relations: &'static [RelationMeta],
    pub flags: EntityFlags,                    // created_at / updated_at / soft_delete / version
}
```

### 5.4 API 面

```rust
// CRUD —— 全部取 &impl SqlExecutor，事务与任意后端通吃
User::insert(&db, &user).await?;                  // 返回 i64 主键
User::find_by_id(&db, 1).await?;                  // Option<User>
User::query().filter("name", Op::Like, "%e%").fetch(&db).await?;

// 分页
User::query().order_by("id", Order::Desc).paginate(&db, 1, 20).await?;   // Page<User>

// 关联预加载 —— UserRelation 由 #[derive(Entity)] 依据 #[entity(has_many/has_one/belongs_to)]
// 标记自动生成，每个关联一个变体
User::query().with(&[UserRelation::Posts, UserRelation::Profile]).fetch(&db).await?;

// 批量 —— 返回受影响行数，不返回 id 列表。
// 理由：MySQL 的 LAST_INSERT_ID() 只给批量首行、SQLite 给末行，跨后端语义不可靠。
// 需要逐行 id 时循环调用 insert（或包在一个事务里）。
User::insert_many(&db, &users).await?;            // -> u64
User::query().filter("age", Op::Lt, 18).delete_where(&db).await?;
User::upsert(&db, &user).await?;

// join
User::query().join(JoinType::Left, "posts", "posts.user_id = users.id").fetch(&db).await?;

// 事务
let tx = db.transaction().await?;
User::insert(&tx, &user).await?;                  // Transaction 实现 SqlExecutor
Post::insert(&tx, &post).await?;
tx.commit().await?;
```

自动行为（由 `EntityMeta` 标志位驱动，仅在 `update` / `save` / `delete` 路径生效）：
- `created_at`：insert 时填当前时间（字段为 `None` 才填，显式值优先）
- `updated_at`：insert 与 update 时均填
- `soft_delete`：`delete` 改为 `UPDATE ... SET deleted_at = ?`；查询自动加
  `WHERE deleted_at IS NULL`（`Query::with_trashed()` 可关闭）
- `version`：`update` 生成 `UPDATE ... WHERE id = ? AND version = ?`，影响行数为 0
  返回 `OrmError::OptimisticLockConflict`，成功则 `version + 1`

自动填充的时间**一律归一化为 UTC 再写入**。SQLite / MySQL 侧存 RFC3339 文本，
带偏移量的字符串比较会错乱，必须统一。

### 5.5 两个安全/正确性约束

**（a）标识符白名单校验。** `filter(column, ...)` / `order_by(column, ...)` /
`group_by(...)` 收的是 `&str` —— **值是绑定的，标识符不是**。因此这些 API 必须把列名
对照 `EntityMeta.columns` 校验，未命中即返回 `OrmError::UnknownColumn`，绝不拼进 SQL。
需要原生表达式时走显式逃生口 `Query::filter_raw(expr)`（文档注明「输入必须可信」）。

`join(table, on)` 的表名与 ON 条件同理：表名对照 `EntityMeta.table` 与已声明关联校验，
`on` 条件仅接受编译期字面量（文档注明信任边界）。

**（b）批量操作按参数上限分块。** 各后端单语句参数上限差异很大：

| 后端 | 单语句参数上限 |
|---|---|
| SQL Server | **2100** |
| SQLite | 999（旧）/ 32766（3.32+） |
| PostgreSQL | 65535 |
| MySQL | 65535（受 `max_allowed_packet` 约束） |

`insert_many` / `update_many` / `delete_where(Op::In, ...)` / 关联预加载的 `IN (...)`
全部按 `chunk = max_params_per_stmt / 每行参数数` 切分后循环执行。
该上限作为 `DialectSpec::max_params_per_stmt()` 由方言层提供。
**不分块 = 几千行批量插入必然报错。**

### 5.6 分页

`Page<T> { items, total, page, per_page }` 的 `total` 来自一条独立的 COUNT 查询
（复用同一 WHERE 与 JOIN，强制去掉 ORDER BY / LIMIT / OFFSET）。

```rust
User::query().paginate(&db, page, per_page).await?;              // 2 次查询
User::query().paginate_without_count(&db, page, per_page).await?; // 1 次，total = None
```

`paginate_without_count` 用于大表深层翻页（`COUNT(*)` 全表扫描代价高于取一页数据）。

## 6. 方言差异矩阵

| | SQLite | PostgreSQL | MySQL | SQL Server |
|---|---|---|---|---|
| 标识符引号 | `"` | `"` | `` ` `` | `[ ]` |
| 占位符 | `?` | `$n` | `?` | `@Pn` |
| LIMIT | `LIMIT n OFFSET m` | `LIMIT n OFFSET m` | `LIMIT n OFFSET m` | 无 ORDER BY → `TOP n`；有 OFFSET → `OFFSET m ROWS FETCH NEXT n ROWS ONLY`（无 ORDER BY 时补 `ORDER BY (SELECT NULL)`） |
| 取回自增 ID | `INSERT ... RETURNING id`（SQLite 3.35+） | `INSERT ... RETURNING id` | `SELECT LAST_INSERT_ID()` | `INSERT ... OUTPUT INSERTED.[id]` |
| UPSERT | `ON CONFLICT DO UPDATE` | `ON CONFLICT DO UPDATE` | `ON DUPLICATE KEY UPDATE` | `MERGE` |
| 布尔字面量 | `1/0` | `TRUE/FALSE` | `1/0` | `1/0` |
| 时间类型 DDL | `TEXT` | `TIMESTAMPTZ` | `DATETIME` | `DATETIME2` |
| 自增 DDL | `INTEGER PRIMARY KEY AUTOINCREMENT` | `BIGSERIAL` | `BIGINT AUTO_INCREMENT` | `BIGINT IDENTITY(1,1)` |
| 字符串 DDL | `TEXT` | `TEXT` | `TEXT` | `NVARCHAR(MAX)` |
| 建表存在性检查 | `CREATE TABLE IF NOT EXISTS` | 同左 | 同左 | 无该语法 → 先查 `INFORMATION_SCHEMA.TABLES` |
| 主键回填 | 一步（`RETURNING`） | 一步（`RETURNING`） | **两步 + 包事务**（见下） | 一步（`OUTPUT`） |
| 单语句参数上限 | 999 / 32766 | 65535 | 65535 | **2100** |

**MySQL 为什么必须包事务**：`LAST_INSERT_ID()` 是**连接作用域**的。在连接池下，
`INSERT` 与 `SELECT LAST_INSERT_ID()` 是两次独立的池取用，**可能落到不同连接，
取回的是别的会话的值（静默错误）**。因此 MySQL 的一步式不可用，`InsertPlan::InsertThen`
必须把两条语句包进同一个事务（`BEGIN → INSERT → SELECT LAST_INSERT_ID() → COMMIT`）
以保证同连接。SQLite / PG / MSSQL 均可用一步式，无此问题。

`DialectSpec` 接口（全部为纯函数，便于字符串断言测试）：

```rust
pub trait DialectSpec: Send + Sync {
    fn quote(&self, ident: &str) -> String;
    fn placeholder(&self, index: usize) -> String;        // 1-based
    fn limit_clause(&self, limit: u64, offset: u64, has_order: bool) -> String;
    /// n_params = 绑定参数个数（生成 @P1..@Pn / $1..$n / ?）。
    /// 返回一步式（RETURNING / OUTPUT）或两步式（insert + select last id）。
    fn insert_plan(&self, table: &str, cols: &[String], pk: &str, n_params: usize) -> InsertPlan;
    fn upsert(&self, table: &str, cols: &[String], pk: &str, n_params: usize) -> String;
    fn bool_literal(&self, b: bool) -> String;
    fn col_type(&self, ty: ColType) -> String;
    fn table_exists_sql(&self, table: &str) -> String;
    fn autoincrement_ddl(&self, ty: ColType) -> String;
    fn max_params_per_stmt(&self) -> usize;               // §5.5(b) 分块依据
}

pub enum InsertPlan { Single { sql: String }, InsertThen { insert: String, fetch: String } }
```

**`InsertPlan::InsertThen` 的 `fetch` 语句必须在事务内执行**（MySQL 路径），
否则取到的是别的连接的 `LAST_INSERT_ID()`（§6 已说明）。`crud::insert` 在
`InsertThen` 分支自动包事务。

### 时间类型策略

**不写任何 CAST 绕过机制。** 原方案的 `CAST(col AS TEXT)` 只是为了绕开 sqlx `Any`
驱动不支持时间类型（`ecat-data-sqlx/src/lib.rs:84-86` 注明的坑）。本设计已弃用
`AnyPool`（§4），PG / MySQL / SQLite 的原生池原生支持 `time` 类型，
tiberius-ng 也原生支持 —— 五个后端统一，**零特例**。

统一约定：所有后端的时间值在 `Row` 内以 **RFC3339 UTC 字符串** 呈现
（后端各自完成转换），`ecat-orm/src/time.rs` 负责与 `OffsetDateTime` 互转。
写入路径同样归一化为 UTC（§5.4）。

## 7. 迁移系统

**执行位置：用户代码。取消 CLI 方案。**

`ecat-cli` 是项目脚手架（`new` / `proto` / `run` / `build` / `upgrade`），靠
`cargo run` 驱动用户工程，**不链接用户代码 —— 看不到实体定义，无从知道建什么表**
（`ecat-cli/src/main.rs:20-44`）。因此迁移必然产生在用户 crate 内，与 Rust 生态
惯例一致（diesel / sea-orm 的 CLI 靠扫描源码目录才能做到）。

```rust
// 用户侧：main.rs 或 src/bin/migrate.rs
let db = SqlxClient::connect(&url).await?;
let m = Migrator::new(&db)
    .add("001_users", create_table::<User>())
    .add("002_posts", create_table::<Post>());

m.status().await?;      // 打印已应用 / 待应用
m.run().await?;         // 按序执行未应用项，失败即中止
```

- 版本表 `_ecat_migrations(version BIGINT PRIMARY KEY, name <文本类型>, applied_at <时间类型>)`，
  类型按方言映射（`TEXT`/`TEXT`/`TEXT`/`NVARCHAR(MAX)` 与
  `TEXT`/`TIMESTAMPTZ`/`DATETIME`/`DATETIME2`），时间统一存 RFC3339 UTC。
- DDL 由 `EntityMeta` 生成（`ColType` → 方言类型），支持 `create_table` / `drop_table`。
- `run()`：读已应用版本 → 过滤未应用 → 按版本号顺序执行 → 记录版本；
  单个迁移失败即中止（不吞错）。
- MSSQL 建表先查 `INFORMATION_SCHEMA.TABLES`（无 `IF NOT EXISTS` 语法）。
- **不做**：回滚 SQL 自动生成（DDL 的反向不可靠）。`down()` 仅支持显式提供的
  反向 SQL，未提供则返回 `OrmError::MigrationIrreversible`。

## 7.5 可观测性（三个 opt-in feature）

均为后端 crate（`ecat-data-sqlx` / `ecat-data-mssql`）的可选 feature，默认关闭 ——
避免把 axum（`ecat-metrics` / `ecat-health` 的依赖）拖进核心依赖树。

| feature | 产出 | 接口 |
|---|---|---|
| `metrics` | `ecat_rdbms_pool_connections{backend,state="idle\|active"}`、`ecat_rdbms_pool_timeouts_total`、`ecat_rdbms_query_timeout_total`、`ecat_rdbms_transactions_leaked_total` | `register_pool_metrics()` → `ecat_metrics::registry()`，自动出现在已有 `/metrics` 端点 |
| `health` | 池连通性探针（`SELECT 1`） | `RdbmsHealthCheck` 实现 `ecat_health::HealthCheck`（`ecat-health/src/lib.rs:13`），接 `/health` readyz |
| `tracing` | 超阈值 SQL 打 warn（耗时 + 截断 SQL），阈值 `slow_query_ms` 可配 | 复用 `ecat-tracing` |

`ecat_rdbms_transactions_leaked_total` 接的是既有的「事务未提交即 Drop」告警
（`ecat-data/src/rdbms.rs:74-84`）—— 把日志变成可告警的指标。

## 8. 测试策略

CI 无数据库服务（`.github/workflows/ci.yml` 仅 `cargo test --workspace`），分三层：

| 层 | 手段 | 覆盖 |
|---|---|---|
| 方言（无数据库） | 纯字符串断言单测 | §6 矩阵全部分支（引号/占位符/LIMIT/UPSERT/DDL 类型/建表检查） |
| ORM 全功能 | SQLite in-memory 集成测试 | 实体/CRUD/查询/关联预加载/批量/分页/事务/软删除/乐观锁/迁移全链路 |
| PG / MySQL / MSSQL | env 门控（`ECAT_TEST_MSSQL_URL` 等，未设则跳过并打印提示） | 连接/查询/事务/池回收/方言实跑 |

配套 `docker-compose.dev.yml`（sqlserver 2022 + postgres + mysql）供本地一键起，
不接入 CI。MSSQL 驱动的最小集成测试覆盖：连接、参数化查询、事务提交/回滚、
池复用（连续取用两次连接）。

`ecat-orm` 的 SQLite 集成测试使用 `sqlite::memory:`，走 `SqlxClient::connect`
的 `SqlitePool` 路径，无需额外依赖。

**新增覆盖点**（对应本次评审补充）：
- 每后端一枚方言单测：批量分块边界（2100 / 999 / 65535 各超一行的输入）
- MySQL `InsertThen` 路径断言其包在事务内执行（用一个记录调用序列的假 executor）
- `RdbmsRouting`：写语句落 primary、读语句落 replica、`query_write` 落 primary、
  **端点熔断后轮询跳过它**、副本全熔断时 `fallback_to_primary` 两种取值的行为
- `CircuitBreakerExecutor`：失败率超阈值后快速失败，冷却后半开探测
- 会话初始化：假 executor 断言连接建立后按序执行了初始化语句；
  某条语句失败 → 连接创建失败
- 标识符白名单：未声明的列名被拒（不拼进 SQL）
- 时间归一化：带 `+08:00` 偏移的输入在写前被转为 UTC

## 9. 影响面清单

| 文件 / crate | 改动 |
|---|---|
| `Cargo.toml`（根） | 加 3 个 member + workspace dependencies（tiberius-ng / deadpool / tokio-util / time / tokio-sync） |
| `ecat-data` | 新增 `dialect.rs` / `routing.rs` / `breaker.rs` / `timeout.rs`；`rdbms.rs` 拆 `SqlExecutor` + `query_write`；`TransactionInner` 扩方法；新增依赖 `tokio`(sync) / `ecat-circuit-breaker` |
| `ecat-data-sqlx` | **弃用 `AnyPool` 改三路原生池**；`time` 类型映射；`dialect()`；`SqlxConfig` 池参数 + `query_timeout_secs`；`warm_up()`；事务 wrapper 补执行方法；`metrics`/`health` feature |
| `ecat-data-mssql` | **新建**（含 `warm_up()` / 智能 recycle / `metrics` / `health` feature） |
| `ecat-orm` | **新建** |
| `ecat-orm-derive` | **新建** |
| `ecat-circuit-breaker` | 抽出公开 `Breaker` 类型（tower 层改为调用它，行为不变） |
| `ecat`（聚合） | 新增 feature `orm` / `mssql` |
| `docker-compose.dev.yml` | **新建**（本地三库联调，不进 CI） |
| 版本 | workspace 3.0.3 → 4.0.0 |

### 9.1 文档更新清单

**文档更新必须与代码同批落地。** README 系文档带状态列（`✅ 已实现` /
`✅ Реализовано` / `✅ 実装済み`），代码未落地前更新即为不实描述 ——
用户照文档使用会直接失败。因此本清单随实现批次执行，不提前写。

| 文档 | 根文件 | i18n 副本 | 更新内容 |
|---|---|---|---|
| README | `README.md` / `README.en.md` | `docs/i18n/{12}/README.md` | 后端表补 SQL Server 行（第 16 个）；`RDBMS \| sqlx` 依赖行补 `tiberius-ng`；新增 ORM 用法段；目录树补 3 个新 crate |
| 数据库配置教程 | `docs/database-config-tutorial.md` | `docs/i18n/{12}/database-config-tutorial.md` | 补 `MssqlConfig` 段、sql 池参数（`max_connections` / `query_timeout_secs` / `session_init` 等）、`RdbmsRouting` 与熔断组合示例 |
| API 参考 | `docs/api.md` | `docs/i18n/{12}/api.md` | 补 ORM / 迁移的公开 API；`/metrics` 新增的池指标名 |
| TLS 教程 | `docs/tls-certificate-tutorial.md` | `docs/i18n/{12}/tls-certificate-tutorial.md` | SQL Server 的 TLS 连接串参数（`encrypt=mandatory` / `trust_server_certificate`） |
| 依赖 CVE 跟踪 | `docs/dependency-cve-tracking.md` | `docs/i18n/{12}/dependency-cve-tracking.md` | `tiberius-ng` 进入 `Cargo.lock` 后按常规流程补录（选型排除记录已在 2026-10-05 提前写入根文件） |
| 生态规划 | `docs/ecosystem-plan-v3.md` | `docs/i18n/{12}/ecosystem-plan-v3.md` | v4.0 规划段去掉「尚未实现」标记，并入「当前覆盖」 |
| 配置示例 | `config/databases.example.yaml` | — | 补 `mssql:` 段与 sql 池参数注释 |
| 变更日志 | `CHANGELOG.md` | — | 4.0.0 段：新增 / 变更 / 破坏性变更 / 迁移指引 |

i18n 共 12 个语言目录（`ar` `bn` `de` `en` `es` `fr` `hi` `id` `ja` `ko` `pt` `ru`），
按上表逐份同步 —— **受影响文件约 80 个**，是本次改动中体量最大的单项。
`docs/i18n/*/audit-report-*.md` 等历史记录文档**不更新**（审计报告是历史存档）。

> `ecat-cli` **不改动**（迁移 CLI 方案已取消，见 §7）。

## 10. 非目标（本次不做）

- 不开源 ORM 覆盖之外的文档库 ORM（MongoDB 保持 `DocumentClient` 手写查询）
- 不做关联的延迟加载（lazy load）—— 只做显式 `with()` 预加载，避免隐式 N+1
- 不做迁移的自动回滚 DDL 推导
- 不做查询缓存 / 二级缓存
- **不支持复合主键** —— 单列主键（`i64` / `uuid` / `String`）
- **不支持 JSON / 数组列的类型化映射** —— 按文本处理
- 不做 SQL Server 的 Windows 集成认证（Kerberos）—— 仅 SQL 账号密码认证
- 不做 SQL Server 死锁（错误 1205）自动重试
- 读写分离不做「写后读一致性」保证 —— 事务内自动读主，事务外读到副本延迟属预期行为

## 11. 风险

| 风险 | 缓解 |
|---|---|
| `tiberius-ng` 是社区续作，API 可能变动 | 锁 0.13.x；驱动细节全部封装在 `ecat-data-mssql` 内，不外泄 |
| 破坏性变更（trait 拆分 + `from_pool` 签名 + `AnyPool` 移除） | 仓库内实现者 4 个（`SqlxClient` / `ClickhouseClient` / `QuestdbClient` / 测试桩）+ 3 处 UFCS 调用，全部随批次 1 一并机械适配；发 4.0.0 并写 CHANGELOG |
| 五方言 SQL 生成正确性 | 方言层纯函数 + 字符串断言单测（无需数据库即可全量覆盖） |
| **原生池重写后回归** | 现有 `ecat-data-sqlx` 测试全部保留并通过；SQLite 集成测试覆盖所有执行路径 |
| ORM 体量大（约 4000–5000 行，含池增强） | 按 <500 行/文件拆模块；先落 SQLite 全链路集成测试再扩方言 |
| MSSQL 无法在 CI 实跑 | env 门控 + docker-compose 本地验证；方言生成逻辑由单测兜底 |
| `Breaker` 抽取改动既有 crate | tower 层的既有 12 个测试（`ecat-circuit-breaker/src/lib.rs:275-546`）必须保持全绿 |

## 11.5 交付批次（用户确认，2026-10-05）

分 4 批。**每批结束时 `cargo test --workspace` 必须全绿**（CI 闸门），
每批可独立提交与回滚；文档与代码**同批落地**（§9.1）。版本号在批次 4 统一
bump 到 4.0.0，分支内不发布中间版本。

| 批次 | 代码 | 文档 |
|---|---|---|
| **1 地基** | `ecat-data`：`Dialect`、`SqlExecutor` 拆分、`query_write`、`Transaction` 可执行、`timeout.rs` 助手<br>`ecat-data-sqlx`：**原生池重写**、`time` 类型映射、池参数、`warm_up()`、`test_before_acquire`、`session_init` | `database-config-tutorial.md` ×13（含 12 语言） |
| **2 驱动** | `ecat-data-mssql`：tiberius-ng + deadpool、参数绑定、行转换、`warm_up()`、智能 recycle | `README.md` / `README.en.md` + `docs/i18n/{12}/README.md`（后端表补第 16 行、目录树、依赖行） |
| **3 ORM** | `ecat-orm` + `ecat-orm-derive`：实体宏、CRUD、查询构建器、关联预加载、join、批量（含分块）、分页、标识符白名单、软删除/时间戳/乐观锁、迁移系统 | `api.md` ×13、README ORM 用法段 ×14、`config/databases.example.yaml` |
| **4 收尾** | 包装器（`CircuitBreakerExecutor` / `RdbmsRouting`）+ `ecat-circuit-breaker` 抽 `Breaker` + 可观测性三 feature（metrics/health/tracing）+ `ecat` 聚合 feature + `docker-compose.dev.yml` | `CHANGELOG.md`（4.0.0 段）、`ecosystem-plan-v3.md` 去掉「尚未实现」标记 ×13、`tls-certificate-tutorial.md` ×13 |

批次 1 是唯一含破坏性 API 改动的一批（trait 拆分 + 原生池），
但它本身不自成发布 —— 4.0.0 只在批次 4 后整体发布。

## 12. 验收标准

1. `cargo test --workspace` 全绿（含新增方言单测与 SQLite ORM 集成测试）
2. `cargo clippy --workspace -- -D warnings` 与 `cargo fmt --check` 通过
3. `cargo audit --deny warnings` 通过（证明 tiberius-ng 路径无 advisory）
4. SQLite 上跑通：实体定义 → 迁移建表 → CRUD → 关联预加载 → 分页 → 事务 →
   软删除 → 乐观锁冲突
5. `SqlxClient` 对 `postgres://` / `mysql://` / `sqlite:` 三种 URL 建出对应原生池，
   `dialect()` 返回正确方言；**时间列无需 CAST 即可正确读写**
6. MSSQL 客户端可编译、可配置，且有 env 门控集成测试（本地 docker 实跑通过）
7. 池增强逐项可验证：查询超时触发并计数、`warm_up()` 建满 `min_connections`、
   智能 recycle 在短空闲时跳过探活、熔断在失败率超阈后快速失败、
   `RdbmsRouting` 写落主读落从、**副本熔断时读请求被跳过该副本**
   （`fallback_to_primary` 两种取值各一个用例）
8. 会话初始化生效可验证：PG 连接后 `SHOW timezone` 为 `UTC`；
   MSSQL 连接后 `ARITHABORT` 为 ON；初始化语句失败时连接创建失败（不静默）
9. `ecat-circuit-breaker` 既有 12 个测试保持全绿（`Breaker` 抽取未改行为）
