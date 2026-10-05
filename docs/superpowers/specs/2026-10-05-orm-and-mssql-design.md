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
| 时间类型 | `time::OffsetDateTime`，序列化为 RFC3339 字符串 |
| 迁移 CLI | 挂到既有 `ecat-cli`（`ecat migrate up`），不新建二进制 |
| 版本 | workspace 3.0.3 → **4.0.0**（trait 拆分为破坏性变更） |

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

外部若直接实现过 `RdbmsClient` 会编译失败（方法移到 supertrait）。仓库内实现者只有
`SqlxClient` 与测试桩 `RawOnlyClient`。按 4.0.0 发布。

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
}

pub struct MssqlClient { pool: deadpool::managed::Pool<MssqlManager> }

impl MssqlClient {
    pub async fn connect(url: &str) -> Result<Self, RdbmsError>;
    pub async fn from_config(cfg: MssqlConfig) -> Result<Self, RdbmsError>;
    pub fn from_pool(pool: Pool<MssqlManager>) -> Self;        // 对齐 SqlxClient::from_pool
}
```

- **参数绑定**：`serde_json::Value` → `&dyn tiberius_ng::ToSql`，SQL 内占位符为
  `@P1..@Pn`。沿用现有契约（调用方写后端原生占位符，与 `SqlxClient` 一致）。
- **参数生命周期**：tiberius 的 `query(sql, &[&dyn ToSql])` 需要借用，实现内先把
  `Value` 物化成 `enum Bind { I64(i64), F64(f64), Str(String), Bool(bool), Null }`
  持有所有值，再构造引用切片。
- **行转换**：tiberius `Row` → `ecat_data::Row`。类型分派用 `ColumnData` 匹配
  （`I64`/`I32`/`I16`/`U8`/`F64`/`F32`/`Bit`/`String`/`Guid`/`Numeric`/`DateTime`/
  `DateTime2`/`Bytes`/`Null`），时间类型转 RFC3339 字符串，`Bytes` 转 base64
  （与 `cell_to_json` 的既有约定一致，`ecat-data-sqlx/src/lib.rs:83-121`）。
- **池**：`MssqlManager` 实现 `deadpool::managed::Manager`，`create` 建 TCP +
  `Client::connect`（`rustls` TLS），`recycle` 跑 `SELECT 1` 健康检查。
- **构造器命名**：遵循 README 既有约定，主构造器 `connect`（与 `ecat-data-sqlx` 一致）。

## 4. `ecat-data-sqlx` 改动

- 实现 `SqlExecutor`（方法体不变），新增 `dialect()`：从 URL scheme 判定
  （`postgres://` / `postgresql://` → Postgres，`mysql://` → MySql，`sqlite:` → Sqlite，
  其余 → Standard），存为字段。
- `SqlxConfig` 增加池参数 `max_connections` / `min_connections` /
  `acquire_timeout_secs` / `idle_timeout_secs`（`Option`，缺省沿用 sqlx 默认值），
  `connect` 内部改用 `AnyPoolOptions` 构造池；`from_pool` 保持不变。
- `SqlxTransactionWrapper` 补齐四个执行方法（委托 `sqlx::Transaction`），
  实现 `TransactionInner` 新签名。

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

`EntityMeta` 由 derive 生成（`const` 数组），供查询构建器与迁移 DDL 共用，
避免宏生成大段重复代码。

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

// 批量
User::insert_many(&db, &users).await?;
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

## 6. 方言差异矩阵

| | SQLite | PostgreSQL | MySQL | SQL Server |
|---|---|---|---|---|
| 标识符引号 | `"` | `"` | `` ` `` | `[ ]` |
| 占位符 | `?` | `$n` | `?` | `@Pn` |
| LIMIT | `LIMIT n OFFSET m` | `LIMIT n OFFSET m` | `LIMIT n OFFSET m` | 无 ORDER BY → `TOP n`；有 OFFSET → `OFFSET m ROWS FETCH NEXT n ROWS ONLY`（无 ORDER BY 时补 `ORDER BY (SELECT NULL)`） |
| 取回自增 ID | `SELECT last_insert_rowid()` | `INSERT ... RETURNING id` | `SELECT LAST_INSERT_ID()` | `INSERT ... OUTPUT INSERTED.[id]` |
| UPSERT | `ON CONFLICT DO UPDATE` | `ON CONFLICT DO UPDATE` | `ON DUPLICATE KEY UPDATE` | `MERGE` |
| 布尔字面量 | `1/0` | `TRUE/FALSE` | `1/0` | `1/0` |
| 时间类型 DDL | `TEXT` | `TIMESTAMPTZ` | `DATETIME` | `DATETIME2` |
| 自增 DDL | `INTEGER PRIMARY KEY AUTOINCREMENT` | `BIGSERIAL` | `BIGINT AUTO_INCREMENT` | `BIGINT IDENTITY(1,1)` |
| 字符串 DDL | `TEXT` | `TEXT` | `TEXT` | `NVARCHAR(MAX)` |
| 建表存在性检查 | `CREATE TABLE IF NOT EXISTS` | 同左 | 同左 | 无该语法 → 先查 `INFORMATION_SCHEMA.TABLES` |
| 主键回填 | 两步（insert + select） | `RETURNING` 一步 | 两步（insert + select） | `OUTPUT` 一步 |

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
}

pub enum InsertPlan { Single { sql: String }, InsertThen { insert: String, fetch: String } }
```

### 时间类型策略（正面处理既有坑）

sqlx `Any` 驱动不认时间类型，timestamp 列 fetch 直接报错
（`ecat-data-sqlx/src/lib.rs:84-86` 已注明）。ORM 生成 SELECT 时，对
`EntityMeta` 中标记为时间类型的列按方言包一层转换：

- SQLite / PostgreSQL：`CAST(col AS TEXT)`
- MySQL：`CAST(col AS CHAR)`
- SQL Server：tiberius 原生支持 `DATETIME2` → **不加 cast**

统一以 RFC3339 字符串进入 `Row`，`ecat-orm/src/time.rs` 解析为 `OffsetDateTime`。
结果：时间字段在五个后端行为一致。

## 7. 迁移系统

- 版本表 `_ecat_migrations(version BIGINT PRIMARY KEY, name <文本类型>, applied_at <时间类型>)`，
  两种类型均按方言映射（§6 矩阵）：`TEXT`/`TIMESTAMPTZ`/`TEXT`/`NVARCHAR(MAX)` 与
  `TEXT`/`TIMESTAMPTZ`/`DATETIME`/`DATETIME2`，避开 sqlx `Any` 的时间类型限制
  （SQLite / MySQL 侧存 RFC3339 文本）。
- DDL 由 `EntityMeta` 生成（`ColType` → 方言类型），支持 `create_table` / `drop_table`。
- `Migrator::new(&db, dialect).up()`：读已应用版本 → 过滤未应用 → 按版本号顺序执行
  → 记录版本；单个迁移失败即中止（不吞错）。
- MSSQL 建表先查 `INFORMATION_SCHEMA.TABLES`（无 `IF NOT EXISTS` 语法）。
- CLI：`ecat-cli` 新增 `migrate up` / `migrate status` 子命令（clap derive），
  连接信息由 `--config <file> [--section sql|mssql]` 指定，文件结构与
  `config/databases.example.yaml` 同构（`sql:` / `mssql:` 段 + `url`/`username`/`password`）。
- **不做**：回滚 SQL 自动生成（DDL 的反向不可靠）。`down()` 仅支持显式提供的
  反向 SQL，未提供则返回 `OrmError::MigrationIrreversible`。

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

`ecat-orm` 的 SQLite 集成测试使用 `sqlite::memory:` + `AnyPool`，
沿用 `SqlxClient::connect` 路径，无需额外依赖。

## 9. 影响面清单

| 文件 / crate | 改动 |
|---|---|
| `Cargo.toml`（根） | 加 3 个 member + workspace dependencies（tiberius-ng / deadpool / tokio-util / time） |
| `ecat-data` | 新增 `dialect.rs`；`rdbms.rs` 拆 `SqlExecutor`；`TransactionInner` 扩方法 |
| `ecat-data-sqlx` | 实现 `SqlExecutor` + `dialect()`；`SqlxConfig` 池参数；事务 wrapper 补执行方法 |
| `ecat-data-mssql` | **新建** |
| `ecat-orm` | **新建** |
| `ecat-orm-derive` | **新建** |
| `ecat-cli` | `migrate up` / `migrate status` 子命令 |
| `ecat`（聚合） | 新增 feature `orm` / `mssql` |
| `config/databases.example.yaml` | 补 `mssql:` 段与 sql 池参数注释 |
| `README.md` / `README.en.md` | 后端表补 SQL Server（第 16 个），补 ORM 用法段 |
| `docs/ecosystem-plan-v3.md` | 后端覆盖表补一行 |
| 版本 | workspace 3.0.3 → 4.0.0 |

## 10. 非目标（本次不做）

- 不开源 ORM 覆盖之外的文档库 ORM（MongoDB 保持 `DocumentClient` 手写查询）
- 不做关联的延迟加载（lazy load）—— 只做显式 `with()` 预加载，避免隐式 N+1
- 不做迁移的自动回滚 DDL 推导
- 不做查询缓存 / 二级缓存
- 不做 SQL Server 的 Windows 集成认证（Kerberos）—— 仅 SQL 账号密码认证

## 11. 风险

| 风险 | 缓解 |
|---|---|
| `tiberius-ng` 是社区续作，API 可能变动 | 锁 0.13.x；驱动细节全部封装在 `ecat-data-mssql` 内，不外泄 |
| 破坏性 trait 拆分影响外部使用者 | 仓库内唯一实现者 `SqlxClient` 同步改；发 4.0.0 并写 CHANGELOG |
| 五个方言的 SQL 生成正确性 | 方言层纯函数 + 字符串断言单测（无需数据库即可全量覆盖） |
| ORM 体量大（约 3000–4000 行） | 按 <500 行/文件拆模块；先落 SQLite 全链路集成测试再扩方言 |
| MSSQL 无法在 CI 实跑 | env 门控 + docker-compose 本地验证；方言生成逻辑由单测兜底 |

## 12. 验收标准

1. `cargo test --workspace` 全绿（含新增方言单测与 SQLite ORM 集成测试）
2. `cargo clippy --workspace -- -D warnings` 与 `cargo fmt --check` 通过
3. `cargo audit --deny warnings` 通过（证明 tiberius-ng 路径无 advisory）
4. SQLite 上跑通：实体定义 → 迁移建表 → CRUD → 关联预加载 → 分页 → 事务 →
   软删除 → 乐观锁冲突
5. `SqlxClient::dialect()` 对 `postgres://` / `mysql://` / `sqlite:` 三种 URL 返回正确方言
6. MSSQL 客户端可编译、可配置，且有 env 门控集成测试（本地 docker 实跑通过）
