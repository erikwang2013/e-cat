<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Ecat API 参考

本页汇总 Ecat 框架的接口（API）面：端口约定、内置端点、错误格式与扩展接口。业务路由由各服务自行注册。

## 端口约定

| 协议 | 监听地址 | 说明 |
|------|----------|------|
| HTTP | `0.0.0.0:8000` | axum 路由，默认示例端口 |
| gRPC | `0.0.0.0:9000` | tonic Server，默认示例端口 |

## 内置端点

以下端点由生态 crate 提供，随服务挂载：

| 端点 | 来源 | 说明 |
|------|------|------|
| `/health` | ecat-health | 存活检查（返回服务名、版本、启动时间） |
| `/ready` | ecat-health | 就绪检查（依赖就绪后返回 200） |
| `/metrics` | ecat-metrics | Prometheus 指标暴露（`ecat_http_requests_total` / `ecat_http_request_duration_seconds`） |
| `/{service}/{method}` | 用户路由 | 示例：`/helloworld/ecat` |

> 指标端点路径含 ID 等高基数场景请用 `MetricsLayer::new().with_path_fn(...)` 归一化，避免指标基数爆炸。

## 请求处理流程

```
客户端请求
  ├─ HTTP :8000 ──→ axum::Router ─┐
  └─ gRPC :9000 ──→ tonic::Server ─┤
                              ┌─────┴──────┐
                              │ Middleware │  Recovery→Tracing→Logging→Auth→Metrics→Security→CircuitBreaker
                              └─────┬──────┘
                                    ▼
                               Handler（tower::Service）
                                    ▼
                               Response（JSON/Protobuf 编码）
```

## 错误格式

`ecat-errors` 提供 `ErrorCode` + `Error`，编译期映射 HTTP 状态码：

```rust
use ecat_errors::{Error, ErrorCode};

Error::new(ErrorCode::InvalidArgument, "bad_request", "user id must be positive");
```

错误响应经 middleware 编码为 JSON（或 Protobuf），携带 code / reason / message。

## 扩展接口

| 能力 | Crate | 接口 |
|------|-------|------|
| GraphQL | ecat-graphql | `/graphql` 端点；支持字段参数与嵌套 selection，不支持别名、fragment 与多顶层字段 |
| OpenAPI | ecat-openapi | 从路由生成 OpenAPI spec |
| WebSocket | ecat-transport-ws | 升级的 WS 传输 |
| API 版本路由 | ecat-versioning | `/v1/...` 前缀版本路由 |
| 认证 | ecat-auth | JWT / API Key 中间件；JWT 密钥需 ≥32 字节，可链式 `required_issuer`/`required_audience` |
| gRPC 客户端 | ecat-transport-grpc | 集成服务发现与负载均衡 |

## 服务间通信

- `HttpClient`（ecat-client）：集成服务发现与负载均衡，CircuitBreaker 熔断保护
- `GrpcClient`（ecat-transport-grpc）：同上，gRPC 协议
- 中间件统一使用 `tower::ServiceBuilder` 组合（Recovery / Tracing / Logging / Timeout / RateLimit / Security / CircuitBreaker / Metrics / Retry / Validate / CORS）

## 数据后端接口

所有数据后端（`ecat-data-*`）通过统一 trait（`RdbmsClient` 管事务、`SqlExecutor` 管执行与方言 / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`）抽象；REST 类后端（Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine / OpenSearch / Elasticsearch / S3）基于 `base_url` 访问对应 HTTP 接口。连接配置见 [数据库配置教程](database-config-tutorial.md)。

## ORM（ecat-orm）

`ecat-orm` 提供实体派生宏、类型安全的查询构建器、CRUD、关联预加载与迁移。数据操作统一取
`&impl SqlExecutor`，因此**客户端与 `Transaction` 通吃** —— 同一套实体定义与 API：

```rust
User::find_by_id(&db, 1).await?;              // db: SqlxClient / MssqlClient
let tx = db.transaction().await?;
User::update(&tx, &user).await?;              // tx: Transaction
User::insert(&tx, &user).await?;              // 事务里也能插入：两条语句跑在调用方的事务内
tx.commit().await?;
```

SQL 由方言层生成：SQLite / PostgreSQL / MySQL / TiDB（`ecat-data-sqlx`）与 SQL Server
（`ecat-data-mssql`）共用同一套实体定义。

### 实体定义与 `#[entity(...)]` 属性文法

`#[derive(Entity)]` 生成 `Entity::META`（表名 / 列 / 关联 / 标志位）、`from_row` / `to_values` /
`pk_value`，以及每个实体一个 `XxxRelation` 枚举（变体名 = 关联字段名的 PascalCase）。

容器属性：

| 写法 | 含义 |
|------|------|
| `#[entity(table = "users")]` | 表名；省略时取结构体名的 snake_case（`User` → `user`，`UserProfile` → `user_profile`） |

列字段：

| 写法 | 含义 |
|------|------|
| `#[entity(column = "user_name")]` | 列名覆盖，默认取字段名 |
| `#[entity(pk)]` | 主键 |
| `#[entity(auto_increment)]` | 自增（隐含 pk）；自增主键不进插入列清单，由数据库生成 |
| `#[entity(created_at)]` / `#[entity(updated_at)]` | 自动时间戳：插入时两列都填，更新时只刷新 `updated_at` |
| `#[entity(soft_delete)]` | 软删除列：读取路径自动加该列 `IS NULL` 闸门 |
| `#[entity(version)]` | 乐观锁列：更新写 `version + 1` 并比对旧值，冲突返回 `OrmError::OptimisticLockConflict` |

关联字段（**容器类型是硬约束**，裸实体类型在编译期即被拒 —— 它无法表达「没查到」）：

| 写法 | 字段类型 | 含义 |
|------|----------|------|
| `#[entity(has_many = "Post", foreign_key = "user_id")]` | `Vec<Post>` | 一对多：本表 `local_key` 的值去目标表 `foreign_key` 列里匹配 |
| `#[entity(has_one = "Profile", foreign_key = "user_id")]` | `Option<Profile>` | 一对一，单值关联只取第一行 |
| `#[entity(belongs_to = "Tag", foreign_key = "tag_code")]` | `Option<Tag>` | 多对一：本表 `foreign_key` 的值去**目标表主键**匹配 |

`local_key` 可省略：`has_many` / `has_one` 默认取本表主键，`belongs_to` 默认取目标表主键。
关联字段**不进列清单**。字段类型到列类型的映射只存在于 `value::ColumnValue` 的 impl 里
（宏不复制映射表），不认识的类型会得到指向该字段的 `T: ColumnValue` 未满足错误。

### CRUD

| 方法 | 说明 |
|------|------|
| `Entity::insert(&db, &e) -> i64` | 插入并返回新主键。MySQL 的两步式（`INSERT` 后 `SELECT LAST_INSERT_ID()`，而它是**连接作用域**的）由 `SqlExecutor::execute_then_query` 包进同一个事务 |
| `Entity::insert_many(&db, &[e]) -> u64` | 批量插入，返回受影响行数（不回吐主键：`LAST_INSERT_ID()` 只给首行、SQLite 给末行，跨后端语义不可靠） |
| `Entity::save(&db, &e)` | 主键「未设置」（自增且值为 0）则插入，否则更新；返回 `()` |
| `Entity::update(&db, &e) -> u64` | 按主键整行更新。影响 0 行时报错：无 `version` 的实体报 `OrmError::NotFound`，有 `version` 的报 `OrmError::OptimisticLockConflict`（不追加一次查询就分不清「行不存在」与「版本已过期」） |
| `Entity::update_many(&db, &[e]) -> u64` | 逐行更新并累加行数；批量**报不出**是哪一行冲突（返回行数 < 传入行数即有人被版本闸门挡下） |
| `Entity::upsert(&db, &e) -> u64` | 按主键 upsert（各方言 `ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE`） |
| `Entity::find_by_id(&db, pk) -> Option<Self>` | 按主键取一行，找不到返回 `Ok(None)` |
| `Entity::find_all(&db) -> Vec<Self>` | 不带任何过滤的全量查询（**大表上就是全表扫描**，分页请走 `paginate`） |
| `Entity::delete_by_id(&db, pk) -> u64` | 声明了 `soft_delete` 时发 `UPDATE` 置删除时刻；行仍在表里，重复删除返回 `NotFound` 且不刷新删除时刻 |
| `Entity::hard_delete_by_id(&db, pk) -> u64` | 绕过软删除，真的发 `DELETE` |

### 查询构建器

`User::query()` 起手，返回 `Query<User, Unfiltered>`；加上过滤条件后变
`Query<User, Filtered>`（类型状态）—— 「没有 WHERE 就删除」在编译期就不成立。

```rust
use ecat_orm::query::{Op, Order};

let users = User::query()
    .filter("name", Op::Like, "alice%")?        // Eq / Ne / Lt / Le / Gt / Ge / Like
    .filter("email", Op::NotNull, serde_json::json!(null))?
    .filter("id", Op::In, serde_json::json!([1, 2, 3]))?  // In / NotIn 取数组值
    .order_by("id", Order::Desc)?
    .limit(10)
    .offset(20)
    .fetch(&db)
    .await?;
```

- 列名按 `EntityMeta.columns` 白名单校验（`OrmError::UnknownColumn`）；关联表的列不在白名单里，
  需要时用 `filter_raw("...")` —— **它不做任何校验，输入必须可信**。
- `with_trashed()` 关掉软删除闸门（连已软删的一起查回来）。
- `join(JoinType::Left, "posts", "posts.user_id = users.id")` 支持 `Inner` / `Left`，用于在
  `filter_raw` 里按被连表的列过滤。**列清单不带表前缀**，所以被连表与主体有同名列时真库会报
  `ambiguous column name` —— 连表的列名需与主体错开（实测 SQLite）。
- `delete_where(&db)` / `hard_delete_where(&db)` 按同一套条件删除（软删除实体走 `UPDATE`）。
- `fetch(&db)` 是执行入口，`find_by_id` / `find_all` / `paginate` 都复用它背后的同一份 SQL 生成路径。

### 分块与分页

- **批量写自动分块**：`insert_many` / `update_many` 按方言的单语句参数上限
  （如 SQL Server 2100）切分，各块行数累加。手动分块请用同一份额度，避免「本机能跑、到生产超上限」。
- **分页**：`paginate(&db, page, per_page)` 发 2 条查询（COUNT + 取页），页码**从 1 开始**；
  COUNT 复用同一个 WHERE / JOIN，但去掉 ORDER BY / LIMIT / OFFSET。返回 `Page { items, total, page, per_page }`，
  `total_pages()` / `has_next()` 由 `total` 算出。
- 大表深层翻页用 `paginate_without_count(&db, page, per_page)`（1 条查询）：`total` 为 `None`，
  `total_pages()` / `has_next()` 也给不出答案 —— 算不出来就如实说算不出来。

### 关联预加载

```rust
let users = User::query()
    .with(&[UserRelation::Posts, UserRelation::Profile])
    .fetch(&db)
    .await?;
```

- 每个关联发**一条** `IN` 查询（键超过单语句参数上限时按块切分），主体查询本身不 JOIN ——
  **杜绝 N+1**：取 3 个用户及其 posts 一共 2 条 SELECT（1 条主体 + 1 条 `IN`），不是 4 条。
- 未声明 `with()` 时关联字段是空的（`Vec::new()` / `None`），不会带出上一次加载的旧值；
  `set_relation` 对每个主体都会被调用（含空结果），正是为了清掉残留。
- 单值关联（`has_one` / `belongs_to`）只取第一行。

### 迁移

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Migrator, create_table};

Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .add("002_posts", create_table::<Post>())
    .status().await?;      // MigrationStatus { applied, pending, unknown }
    .run().await?;         // 应用 pending，逐条记入版本表；重复 run 幂等
    .down(1).await?;       // 执行 001 的反向 SQL 并从版本表删掉那一行
```

- 迁移名的数字前缀即版本号（`"001_users"` → 1），非数字前缀报 `OrmError::InvalidMigrationName`，**不猜 0**。
- `create_table::<E>()` / `drop_table::<E>()` 由 `EntityMeta` 生成 DDL，**方言在 `run()` 时按连接的
  `dialect()` 解析**，所以迁移列表与连接串解耦。自定义 SQL（ALTER、数据回填）用
  `MigrationBuilder::new(|d| ...)`。
- 没有反向 SQL 的迁移调 `down` 会报 `OrmError::MigrationIrreversible` —— 「DROP 再 CREATE 回来」会丢数据，不能替调用方猜。
- 版本表由 `Migrator` 自动建（MSSQL 没有 `CREATE TABLE IF NOT EXISTS`，会先查存在性）。
