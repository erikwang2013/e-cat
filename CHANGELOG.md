# Changelog

## [7.0.0] — 2026-10-08

### ⚠️ 破坏性变更

- **11 个后端 Config 新增公开字段**：`ArangoConfig` / `Neo4jConfig` / `NebulaGraphConfig` /
  `ElasticsearchConfig` / `OpenSearchConfig` / `InfluxConfig` / `IotdbConfig` /
  `TdengineConfig` / `QuestdbConfig` / `S3Config` 新增 `query_timeout_secs` / `breaker` /
  `max_concurrency`；`MongoConfig` 新增 `query_timeout_secs` / `breaker` / `max_pool_size` /
  `min_pool_size`。这些结构体既没有 `Default` 也没有 `#[non_exhaustive]`，用**结构体字面量**
  构造的用户代码需要补字段（E0063）；走 serde 配置的写法不受影响（新字段全部带
  `#[serde(default)]`，省略即默认）。判定与 6.0.0 对 `RedisConfig` / `ClickhouseConfig`
  的同一处改动一致 —— 同一个改动，同一套标准。

### Added — 出站韧性覆盖全部数据后端（批次 5b）

- 11 个后端接入统一出站外壳 **许可 → 熔断 → 超时**（`ecat_data::run_with_timeout` +
  `ecat_circuit_breaker::Breaker`）：`ecat-data-arangodb` / `neo4j` / `nebulagraph` /
  `elasticsearch` / `opensearch` / `influxdb` / `iotdb` / `tdengine` / `questdb` / `s3` /
  `mongodb`。`transaction()` / `dialect()` / `object_path()` 等**不含 I/O 或纯本地**的
  路径不包 —— 它们失败不是后端故障，包装会把本地错误算进熔断窗口。
- 各 `XxxConfig` 新增可选字段 `query_timeout_secs`（省略 = 30 秒，**`0` = 禁用**）与
  `breaker`；HTTP 后端另有 `max_concurrency`（默认 32）—— reqwest 只有
  `pool_max_idle_per_host`，**没有**最大总连接数旋钮，并发上限由各 crate 自己的
  `tokio::sync::Semaphore` 实现，许可取在**最外层**（排队中的请求还没碰后端，不该计入失败）。
- `MongoConfig` 新增 `max_pool_size` / `min_pool_size`：并发背压交给**驱动连接池**
  （`mongodb` 3.8.0 默认上限 **10**，不是 100），本 crate **没有**信号量层。
- 13 个 crate 各新增 `metrics` feature：把 `ecat_outbound_timeouts_total` /
  `ecat_outbound_breaker_opened_total` / `ecat_outbound_breaker_state` 挂进
  `ecat-metrics` 的共用 collector，标签 = **配置节名**（`"arangodb"` / `"mongodb"` / …）。
  **`from_config` 构造即注册**（本批统一裁决）：`--features metrics` 下用户代码零变化，
  不再需要手动调 `register_outbound_metrics`。覆盖 5b 的 11 个后端**加上 5a 的
  `ecat-data-redis` / `ecat-data-clickhouse`**（后者此前同样只有再导出、无生产调用点）。
  注意：同一配置节在**同一进程内建多个 client** 时，标签只有一份，**最后构造的那个生效**
  —— 这是「标签 = 配置节名」的固有含义，需要区分实例请用不同配置节名。
  另修：聚合 crate `ecat` 的 `metrics` feature 原先只启用 `dep:ecat-metrics`，**不透传**给
  数据后端 —— `ecat = { features = ["metrics", "redis"] }` 会拿到 registry 与 `/metrics`
  端点，但 `ecat-data-redis` 仍以默认特性构建、`from_config` 不注册，一个 `ecat_outbound_*`
  样本都不出。现用弱依赖语法透传（`ecat-data-redis?/metrics` 等）：只开 `metrics` 不会把
  后端拖进来，只开后端也不会被强加 `metrics`（四种组合实测，非破坏性变更）。
  （`ecat-data-sqlx` / `ecat-data-mssql` 的 `register_pool_metrics` **仍为显式调用**：
  它的标签语义是**实例名**（`"primary"` / `"replica-1"`，见 `ecat-metrics/src/rdbms.rs:33-35`），
  自动注册会替用户编一个名字、且读写分离下多个池会互相覆盖。）
- QuestDB 的 `query_timeout_secs` / `breaker` 走 `RdbmsError` 路径（`RdbmsError::Timeout` /
  `RdbmsError::Connection("circuit breaker is open")`），与其余 10 个的
  `ecat_errors::Error`（`DeadlineExceeded` / `Unavailable`）不同 —— 它实现的是 `SqlExecutor`。
- 文档：README「支持的数据库」表超时/熔断列 7 → **18**（19 行中除 memcached 外全部）；
  教程补齐 TDengine / MongoDB / S3 三节与其余 8 节的字段速查（13 份镜像）；
  `config/databases.example.yaml` 同步（并补上 5a 遗留的 Redis / ClickHouse 字段）。

### 说明

- 5a 遗留的「`ecat-data-sqlx` / `ecat-data-mssql` 声明未使用的 prometheus 依赖」本批**未处理**
  （与出站韧性无关，混进来会把版本号变更的 diff 搅浑）。
- `MongoConfig.tls` 仍是**未接线**字段（驱动 3.x 的 TLS 走 URI 选项），本批只加 `TODO` 注释留痕。

## [6.0.0] — 2026-10-08

### ⚠️ 破坏性变更

- **`ecat-data`：`run_with_timeout` 签名变更** —— `(Option<Duration>, F)` →
  `(BackendKind, Option<Duration>, F)`，返回的错误类型由写死的 `RdbmsError` 改为泛型
  `E: TimeoutError`。全仓 13 个既有调用点已迁移。
- **`ecat-data`：删除公开静态量 `QUERY_TIMEOUTS`** —— 由 `TIMEOUTS`（按 `BackendKind`
  分 7 维）+ `timeout_counter(kind)` 取代。它只覆盖 RDBMS，且在新维度下不再被递增 ——
  留着就是一颗哑弹：仍是公开 API，读它的人永远拿到冻结值。
- （次要）`RedisConfig` / `ClickhouseConfig` 新增公开字段。两个结构体都没有 `Default`
  也没有 `#[non_exhaustive]`，用**结构体字面量**构造的用户代码需要补字段；走 serde 配置的
  写法不受影响（新字段全部带 `#[serde(default)]`，省略即默认）。

### Added

- **`ecat-data`：泛型超时助手**（原 `run_with_timeout` 只服务 RDBMS 一条路径）：
  - `BackendKind` —— 7 个后端**类别**（`Rdbms` / `Cache` / `Search` / `Graph` /
    `Document` / `Storage` / `Tsdb`），判别值即 `TIMEOUTS` 下标，**顺序不可改**；
    `BackendKind::slug()` 给出 `Error::reason` 用的短标识。
    维度跟 **trait 家族**走而不是产品品类：ClickHouse 同时实现 `SqlExecutor` 与
    `TsdbClient`，按「调用点包着哪个 trait」分。
  - `TimeoutError` trait + 两个实现：`RdbmsError`（→ `RdbmsError::Timeout`）与
    `ecat_errors::Error`（→ `ErrorCode::DeadlineExceeded`，`reason` 填组件标识）——
    六个非 RDBMS trait 共用后者，不需要六套。
  - `TIMEOUTS: [AtomicU64; 7]` + `timeout_counter(kind)`。取槽走无 `_` 分支的 `match`：
    将来加第 8 个维度时**编译失败**，而不是运行期数错槽。
  - 泛型 `run_with_timeout(kind, timeout, fut)`：`None` = 禁用超时；
    `Some(Duration::ZERO)` = 立刻超时（**不是**禁用，别照抄本仓别处「`0` = 禁用」的惯例）。
- `ecat-data`：熔断器错误的映射公开 —— `map_breaker_error` 由 `pub(crate)` 改为 `pub`，
  并新增 `breaker_error_to_backend_error`（`ecat_errors::Error` 侧的同一套语义）。
  Redis / ClickHouse 的包装层与 `RdbmsRouting` 复用同一套：后端自身的错误**原样透出**，
  熔断打开/探测耗尽时后端根本没被调用，报「连接不可用」。
  分两个函数是因为 `RdbmsError`、`ecat_errors::Error` 与 `BreakerError` 都是外部类型，
  写不出统一的 `From`（孤儿规则）。
- `ecat-circuit-breaker`：
  - `Breaker::opened_total()` —— `Closed → Open` 的累计次数（半开探测失败重新打开也计入），
    供 `ecat_outbound_breaker_open_total`。用 `state()` 轮询猜测「开了几次」是错的
    （轮询间隔决定准确性，两次探测之间开又关抓不到）。
  - `BreakerState::code()` —— 指标数值编码 `0` = closed / `1` = open / `2` = half-open。
    放在枚举定义处而非指标侧，编码器与枚举才不会各改各的。
  - `BreakerConfig` 可反序列化：各字段 `#[serde(default)]`，配置文件里 `breaker: {}`
    与 `breaker: {"failure_ratio": 0.5}` 都能解析。
- `ecat-metrics`：三个出站指标的**全进程唯一** collector ——
  `register_outbound_metrics(backend, timeouts, breaker_opened, breaker_state)`。
  `ecat_outbound_timeouts_total` / `ecat_outbound_breaker_open_total` /
  `ecat_outbound_breaker_state`，维度 `backend`。幂等：同一个 `backend` 重复注册**覆盖**
  （避免同一标签出现两份样本被抓取端判为重复）。collector 必须在 `ecat-metrics` 只建一份 ——
  `registry()` 是全进程一个，每 crate 各建一份会在 `Registry` 里撞名（见 Fixed 第二条）。
- `ecat-metrics`：四个 `ecat_rdbms_*` 家族的唯一 collector ——
  `register_rdbms_metrics(backend, connections, pool_timeouts, query_timeouts,
  transactions_leaked)`；`ecat-data-sqlx` / `ecat-data-mssql` 的 `register_pool_metrics`
  改为往里挂数据源（**签名未变**）。
- **`ecat-data-redis`：`Cache` 路径内置超时 + 熔断**（用户代码零改动）：
  - `RedisConfig` 新增 `query_timeout_secs`（`0` = 禁用；未配置 = 30 秒）与 `breaker`
    （省略即保守默认：失败率 0.5 / 窗口 30 秒 / 打开 10 秒）。
  - `RedisCache::breaker()` 暴露熔断器句柄；超时计数按 `BackendKind::Cache` 维度。
  - opt-in feature `metrics`：`register_outbound_metrics(Arc<Breaker>)`，标签 `backend="redis"`。
- **`ecat-data-clickhouse`：`SqlExecutor` 与 `TsdbClient` 两条路径内置超时 + 熔断 + 并发上限**：
  - `ClickhouseConfig` 新增 `query_timeout_secs` / `breaker` / `max_concurrency`
    （未配置 = 32）。并发上限由本 crate 的信号量实现 —— reqwest 只有
    `pool_max_idle_per_host`（空闲保留数），没有「最大总连接数」，默认无背压。
  - 两条路径**共用一个** `Breaker`（同一个服务器、同一个故障域）；超时维度分别按
    `Rdbms` 与 `Tsdb` 计。
  - `TsdbClient` 实现搬到 `src/tsdb.rs`；`_with` 方法保持**不进熔断器**（见 Fixed 第三条）。
  - opt-in feature `metrics`：标签 `backend="clickhouse"` / `backend="clickhouse-tsdb"`。
- `docs/superpowers/checklists/backend-resilience-onboarding.md` —— 5b 逐 crate 照做的
  出站韧性接入 checklist（8 节）。
- `docs/database-config-tutorial.md` ×13 补 Redis / ClickHouse 的超时与熔断字段，
  并写明 Redis 多路复用的能力边界。

### Fixed

- **`ecat-circuit-breaker`：半开探测名额泄漏（5.0.0 已发布缺陷）。**
  `Breaker::call` 在半开分支于 `f().await` **之前**借出探测名额，而名额的归还只在
  await **之后**的记录逻辑里。于是 future 被**外部取消**（`tokio::select!`、调用方自己的
  超时层、请求处理被 drop）时名额有借无还 —— `half_open_probes`（默认 3）次之后
  `half_open_count >= half_open_probes` 恒成立，**每次调用立即返回 `ProbesExhausted`，
  状态永久停在 `HalfOpen`**，只能靠重启进程恢复。
  修法：探测名额改为 RAII 的 `ProbePermit`，`Drop` 时归还；探测已记入窗口则 `disarm`
  不再归还。归还用 `saturating_sub` 是承重的（探测 A 挂起 → 探测 B 失败重开 → 冷却期
  `Open → HalfOpen` 清零计数 → A 这时被取消，减在 0 上是可达的）。
- **`ecat-data-sqlx` / `ecat-data-mssql`：四个 `ecat_rdbms_*` 同名家族互相顶掉
  （5.0.0 已发布缺陷）。** 两个 crate 的 `register_pool_metrics` 各建一份同名 collector，
  而 `ecat-metrics` 的 `registry()` 是**全进程一个**：`prometheus::Registry` 按名字去重，
  后注册者整份被 `AlreadyReg` 吞掉 ⇒ **它的四个指标一条样本都不输出**，无报错、无日志，
  且单 crate 测试全绿（看不到另一个 crate 的注册）。
  修法：collector 唯一化到 `ecat-metrics::register_rdbms_metrics`，两个后端只挂数据源。
  指标名、HELP 文本与标签语义**一字未改**；新增跨 crate 回归测试
  （`ecat-metrics/tests/rdbms_shared_families.rs`，两个后端同进程时两族样本都在）。
- `ecat-data-clickhouse`：`transaction()`（硬编码「ClickHouse 不支持事务」）与 `_with`
  方法（落到 `SqlExecutor` 的 trait 默认实现）**不进熔断器** —— 两者都不含任何 I/O，
  包进去只会让「本就不支持的调用」被记成后端失败。加了两条把守测试防回归。

### Known limitations

- **熔断没有总开关**：`BreakerConfig` 只有阈值字段，没有 `enabled`。要停用只能把阈值
  调到不可能触发（如 `failure_ratio: 1.1`）。配置教程已写明，别写 `{"enabled": false}`
  （那是反序列化错误）。
- `ecat-data-redis` 仍走 `MultiplexedConnection`（多路复用），本版**不换连接池** ——
  超时 + 熔断下多路复用的边界见配置教程。
- `RedisLock` 的 `DistributedLock` 路径不在本版范围（用 `LockError`，不共用本套映射）。

- **两个后端仍声明未使用的 `prometheus` 直接依赖**：`ecat-data-sqlx` 与 `ecat-data-mssql` 的
  `Cargo.toml` 仍有 `prometheus = { version = "0.13", optional = true }`，`metrics` feature 也仍写着
  `["dep:ecat-metrics", "dep:prometheus"]` —— 但自本版起四个 `ecat_rdbms_*` 家族的 collector 搬到了
  `ecat-metrics`，这两个 crate 已**零 prometheus 引用**。**本版不删**：feature 列表是对外可见的，
  删它超出「换注册方式」的范围，留作后续清理。
- README 的后端能力表未更新（本版只改了版本号）：表里 ClickHouse / QuestDB 两行的
  `✅ 熔断` 仍指「可被 `CircuitBreakerExecutor` 包装」，而 ClickHouse 本版起已是**内置**；
  配置字段表（`ClickhouseConfig` 的 `base_url` 等）也还没列出三个新字段。
  整体留给下一批与其余 10 个后端一并同步 —— 14 份手写镜像同步一次就够，不做两遍。

## [5.0.0] — 2026-10-07

### ⚠️ 破坏性变更

- **`ecat-data`：`RdbmsError` 新增 `NoAvailableReplica` 变体。**
  副本全部熔断且 `RdbmsRouting::fallback_to_primary(false)` 时返回它。
  该 enum **不是 `#[non_exhaustive]`**，因此**下游任何穷举 `match` 会编译失败** ——
  这是本版进位主版本号的**唯一**理由。
  （给它加 `#[non_exhaustive]` 降级不行：对已有的公开 enum 加该属性**本身**也是破坏性变更。）

### Added

- **`ecat-circuit-breaker` 抽出公开的熔断状态机**（批次 5「14 个后端出站韧性」的前置）：
  - `Breaker` + `BreakerConfig` + `BreakerState` + `BreakerError`
  - `Breaker::state()` —— 供调用方读熔断状态；**含冷却期的 `Open → HalfOpen` 转换**
  - 原 tower 的 `CircuitBreakerLayer` / `CircuitBreakerService` 公开签名未变（内部改为委托 `Breaker`），
    12 个既有测试**逐条**保持全绿
  - 该 crate 从单文件 546 行拆为 4 个文件（原文件已超项目 500 行约定）
- `ecat-data`：`CircuitBreakerExecutor<S>` —— 给任意 `SqlExecutor` 逐端点包一层熔断。
  熔断打开时**内层一次都不被调用**；`dialect()` 不经熔断（纯本地判断）。
- `ecat-data`：`RdbmsRouting` —— 读写分离（写落主、读落从轮询、`query_write` 落主、
  事务落主且不经熔断），并**跳过熔断打开的副本**。
  只做逐端点包熔断是不够的：从库挂掉后轮询仍会把 1/N 的读转过去靠熔断快速失败 ——
  那不是故障隔离，是**稳定的 1/N 失败率**。`fallback_to_primary` 控制副本全不可用时降级还是报错。
- `ecat-data-sqlx` / `ecat-data-mssql`：三个 **opt-in feature**（默认关闭，避免把 axum 拖进核心依赖树）：
  - `metrics` —— `register_pool_metrics(backend, pool)`，四个指标
    （`ecat_rdbms_pool_connections` / `_pool_timeouts_total` / `_query_timeout_total` /
    `_transactions_leaked_total`）。后两个接既有的进程级计数 `QUERY_TIMEOUTS` /
    `TRANSACTIONS_LEAKED` —— 把日志变成可告警的指标。
  - `health` —— `RdbmsHealthCheck` 实现 `ecat_health::HealthCheck`（池连通性 `SELECT 1`）。
  - `tracing` —— 超阈值 SQL 打 warn，阈值 `slow_query_ms` 可配；SQL 按**字符**截断
    （不切半个 UTF-8），上限 `SQL_HEAD_CHARS = 200`。
- `ecat`（聚合入口）：两个 opt-in feature `orm` / `mssql`，并补上对应重导出。

### Fixed

- `ecat-circuit-breaker`：`state()` 此前**不做冷却期转换** —— 而 `RdbmsRouting` 靠它跳过
  `Open` 的端点，于是被跳过的端点永远没人调 `call`（转换在 `call` 里），
  冷却期过后也不会被重新放行。后果不是「暂时少一个副本」，是**任何失败过的副本被永久排除**，
  读能力单调缩水。现 `state()` 报告**有效**状态（冷却已过则报 `HalfOpen`）。

### Docs

- 修正多处**文档与代码矛盾**（均为既有问题，非本版引入）：
  - TLS 教程里的 `tls: {}  # 保留字段` —— `SqlxConfig.tls` 实际是**配了就报错**
    （批次 1 改的「响亮失败而非静默忽略」），照抄该 yaml 会启动失败。
  - 「所有后端均支持 `tls` 字段」—— 两处反例：`ecat-data-sqlx` 配了报错、
    `ecat-data-memcached` 声明了但**全 crate 无人读**（静默无效）。
  - 生态规划里「三项破坏性变更尚未发布；3.0.3 → 4.0.0」—— v4.0.0 / v4.1.0 均已发布。
  - README 的 crate 数（51 → 56）、`README.en.md` 标题的后端数（18 → 19）、
    多数据源列表补 SQL Server、目录树删除两个不存在的目录。
  - `docs/i18n/{id,en,es,fr,ja,pt,ru}` 的目录树注释为中文残留 → 各语言译文
    （4.1.0 新增的行本就已翻译，旧 56 行是半截翻译）。
- 新增 `docs/social-preview.png`（1280×640）。
- 12 语言 README / 生态规划 / 数据库配置教程同步更新。

### Known limitations

- **`ecat-data-memcached` 的 `tls` 字段是死字段**：声明了但全 crate 无人读，配了静默无效。
  与批次 1 修掉的 `SqlxConfig.tls` 同族，但改它是行为变更，留作独立任务。
- **`ecat-data-clickhouse`（OLAP）未列入 README 的「多数据源」一行**（表内有），既有遗漏。
- `RdbmsRouting` 的内置 `Endpoint` 会在调用方已用 `CircuitBreakerExecutor` 包装端点时**双重包装**。


## [4.1.0] — 2026-10-06

### Added

- **新 crate `ecat-orm` + `ecat-orm-derive`（完整 ORM）**。建立在 4.0.0 拆出的
  `SqlExecutor` 之上 —— 所有数据操作取 `&impl SqlExecutor`，因此**客户端与 `Transaction` 通吃**。
  - `#[derive(Entity)]`：表名/列/主键/标志位、`from_row`/`to_values`、关联与 `XxxRelation` 枚举、`set_relation` 写回口
  - **方言层** `DialectSpec`：SQLite / PostgreSQL / MySQL / SQL Server / ANSI 五套**纯函数** SQL 生成
  - **查询构建器**：类型状态 `Unfiltered → Filtered`（**无过滤条件无法删改**，编译期保证）+ **标识符白名单**（未声明的列名不拼进 SQL）
  - CRUD（`insert` / `find_by_id` / `find_all` / `update` / `delete_by_id` / `hard_delete_by_id` / `save`）
  - **批量分块**：按各方言参数上限（SQLite 999 / SQL Server 2100 / 其余 65535）切分
  - 分页（`Page<T>` / `paginate` / `paginate_without_count`）
  - **关联预加载**：一次 `IN` 查询取回全部关联，**杜绝 N+1**（三种方向：`has_many` / `has_one` / `belongs_to`）
  - 自动行为：`created_at` / `updated_at` 填充、**软删除**、**乐观锁**（`version` 冲突报 `OptimisticLockConflict`）
  - **迁移系统**：`Migrator`（`add` / `status` / `run` / `down`）+ 实体工厂 `create_table::<E>()` + 版本表 `_ecat_migrations`
- `ecat-data`：`SqlExecutor::execute_then_query`（**有默认实现**，向后兼容）——
  在同一条连接上原子地跑两条语句。`Transaction` 覆写为「直接在自己身上跑」，
  `SqlxClient` 覆写为「开事务跑完提交」。**这是「事务内 insert」能成立的前提**
  （MySQL 的 `LAST_INSERT_ID()` 是连接作用域的）。

### Changed

- `ecat-data`：`DialectSpec::table_exists_sql` 改名 **`create_table_prefix`** 并改语义 ——
  旧名承诺「一条语句」，但在 SQL Server 上返回**空串**，调用方照名字用会生成没有
  `CREATE TABLE` 关键字的非法 SQL。新名**始终返回完整前缀**（MSSQL 为 `CREATE TABLE [x]`），
  另加 `needs_exists_check_before_create()` 表达「建表前需先查存在性」。
- `ecat-data`：`DialectSpec::limit_clause` 的 `limit` 参数改 `Option<u64>` ——
  旧版用 `u64::MAX` 表示「未设置」，`offset()` 不带 `limit()` 时会生成
  `LIMIT 18446744073709551615 OFFSET 20`，**超出 BIGINT、真库拒收**。
- `ecat-data` / `ecat-data-sqlx`：`find_by_id` / `find_all` 改走统一的查询构建器路径
  （列清单、方言引号、占位符编号、**软删除闸门**全在一处，不另写第二条 SQL 生成路径）。

### Known limitations

- **`sqlite::memory:` 不支持跨连接事务场景**：池只有一条被事务占住的连接，事务内第二条
  语句会等到超时。需要多连接时用文件路径（`sqlite:<path>?mode=rwc`）。
- **`join` 的列清单不带表前缀**：被连表与主体有同名列时真库报 `ambiguous column name`。
- **批量 `update_many` 的乐观锁冲突无法定位到行**：只能靠「返回行数 < 传入行数」察觉，
  需要定位时用逐行 `update`。
- `join` 的表名与 ON 条件**不做白名单校验**（`join(table, on)` 收的是字符串，
  字符串里没有类型信息）；与 `filter_raw` 同一信任边界，文档已注明。

### Tests

- `ecat-orm` 的 SQLite 全链路集成测试：实体定义 → 迁移建表 → CRUD → 关联预加载 →
  分页 → 事务提交/回滚 → 软删除 → 乐观锁冲突，外加**时间列无 CAST 往返**
  （断言在**绑定参数**上 —— SQLite 读路径会重新格式化偏移量文本，读回路断言是空的）。
- 发布前 `cargo test --workspace`：**1076 passed / 0 failed**（4.0.0 时为 771）。
  `--doc` 有 **4 条 doctest 实际执行**（2 条 `compile_fail` + 2 条正向对照）——
  编译期断言**必须放在 `src/` 的非 `#[cfg(test)]` 位置**：写在 `tests/*.rs` 里
  rustdoc 不收，写在 `#[cfg(test)] mod` 里 `--doc` 编译时不开 `cfg(test)`，两处都不会执行。


## [4.0.0] — 2026-10-06

### ⚠️ 破坏性变更

- **`ecat-data`：`RdbmsClient` 拆出 `SqlExecutor` supertrait**。执行与方言能力
  （`execute` / `query` / `execute_with` / `query_with` / `query_write` / `dialect`）
  移入 `SqlExecutor`，`RdbmsClient` 只剩 `transaction()`。直接实现过 `RdbmsClient`
  的下游会编译失败。仓库内 4 个实现者（`SqlxClient` / `ClickhouseClient` /
  `QuestdbClient` / 测试桩）已同步适配。
- **`ecat-data`：`TransactionInner` 签名扩张**，`Transaction` 现在实现 `SqlExecutor`
  —— **事务内可执行 SQL**（此前只能 `commit` / `rollback`）。
  空事务（`Transaction::new()`）执行 SQL 现在**报错**而非静默返回 0 行影响。
- **`ecat-data-sqlx`：`from_pool(AnyPool, Dialect)` → `from_pool(Pool)`**，
  并**弃用 `AnyPool`，改用 PG / MySQL / SQLite 三路原生池**。
- **`ecat-data-sqlx`：`SqlxConfig.tls` 设了就报错**（此前被 serde 接受但代码从不读，
  是静默无效的配置项）；TLS 请走 URL 参数（如 `?sslmode=require`）。

### Added

- **新数据后端 `ecat-data-mssql`（SQL Server）** —— 数据后端从 15 个增至 **16 个**。
  基于 `tiberius-ng` 0.13 + `deadpool` 0.13，实现完整 `SqlExecutor` +
  `RdbmsClient`。支持 URL 形态（`mssql://user:pass@host:1433/db`，含
  `encrypt` / `trustservercertificate` 查询参数）与 ADO 形态
  （`Server=host,1433;Database=db;User Id=...`）两种连接串。
- `ecat-data`：`Dialect` 枚举与 `Dialect::from_url`（大小写与首尾空白/控制字符均容忍）。
- `ecat-data`：查询超时助手 `run_with_timeout`，以及进程级超时计数
  `QUERY_TIMEOUTS` / 事务泄漏计数 `TRANSACTIONS_LEAKED`。
- `ecat-data-sqlx`：**连接池参数**（`max_connections` / `min_connections` /
  `acquire_timeout_secs` / `idle_timeout_secs` / `max_lifetime_secs` /
  `query_timeout_secs` / `test_before_acquire`）与 `session_init`
  （会话初始化，默认按方言把库侧时区设为 UTC）、`warm_up()` 预热。
- `docker-compose.dev.yml`：本地联调三库一键起（SQL Server 2022 / PostgreSQL 16 / MySQL 8.0），
  不进 CI。

### Fixed

- `ecat-data-sqlx`：**类型映射的 5 个静默错值**（均由真库 A/B 实测发现）——
  MySQL `UNSIGNED` 整数此前在 `Bool`/`Null` 之间跳（`BIGINT UNSIGNED` 正是最常见的自增主键）、
  PG `smallint` 返回 `Null`、`bool` 分支抢走整数、NULL 无闸门（sqlite 下 NULL 会静默变 `false`）、
  `OffsetDateTime` 的 `Display` 被误当作 RFC3339。未支持类型（`numeric` / `uuid` / `jsonb` 等）
  现在**响亮报错**（带列名与类型名）而非静默返回 null。
- `ecat-data-sqlx`：`DATE` 输出**纯 `YYYY-MM-DD`**，不伪造源数据里不存在的时刻与时区。
- `ecat-data-sqlx`：池默认值单一来源（`pool()` 与 `PoolParams::default()` 此前会静默分叉）；
  `connect()` 与 `from_config()` 的方言默认会话初始化此前不一致。
- `ecat-data-mssql`：`idle_timeout_secs` 是**空配置**（deadpool 不提供空闲回收），已删除；
  `MssqlConfig` 加了 `deny_unknown_fields`（拼错的键名此前被静默忽略）。
- `ecat-data` / `ecat-data-sqlx` / `ecat-data-mssql`：事务方法补上查询超时
  （此前事务内挂死的查询会永久占住连接）。
- `ecat-mq-nats`：**显式安装 rustls crypto provider**。此前依赖「`ring` 与 `aws-lc-rs`
  两个特性恰好启用一个」这一脆弱不变量 —— 任何引入第二个 provider 的依赖都会让它 panic
  （且只在 `cargo test --workspace` 的特性合并下暴露）。全仓其余 4 处早已显式安装，
  此处是唯一遗漏。
- `ecat-data-clickhouse`：建表缓存的 TTL 测试去掉时间依赖（5ms 余量导致 ~0.67% 概率假失败）。

### Security

- 升级 `h2` 0.4.15 → **0.4.19**（RUSTSEC-2026-0258）、`rustls` 0.23.43 → **0.23.45**
  （RUSTSEC-2026-0285，5.3 medium）、`chacha20` 0.10.1 → **0.10.2**（此前版本已 yanked），
  连带 `rustls-webpki` 0.103.13 → 0.103.15。`cargo audit --deny warnings` 闸门转绿。

### Tests

- `ecat-data-sqlx` / `ecat-data-mssql`：env 门控真库集成测试
  （`ECAT_TEST_PG_URL` / `ECAT_TEST_MYSQL_URL` / `ECAT_TEST_MSSQL_URL`；
  `ECAT_REQUIRE_LIVE_DB=1` 让 CI 里「跳过即失败」）。三者与 SQLite 一起覆盖四类后端。
- 真库用例做过**三向验证**：真 URL 通过 / 死端口必须失败（证明真在连库）/ 未设 URL 且
  `REQUIRE` 时 panic 并点名缺失键。
- 发布前 `cargo test --workspace`：**771 passed / 0 failed / 0 panic**（115 个测试二进制，
  67 个套件）。跨 crate 特性合并是必需的一层 —— 本版 `ecat-mq-nats` 的 rustls provider
  panic 只在这一层暴露，单 crate 跑 60 次全过。

## [3.0.3] — 2026-08-27

### Added
- 全球转账打赏：README / README.en 及全部 12 语言 README 新增 ZA Bank 汇款信息（WANG KEXUN / SWIFT AABLHKHHXXX / 银行编号 387），含 Citibank（港元/人民币/美元）与 BNY Mellon（其他币种）跨境汇款代理行附注
- API 参考文档 `docs/api.md`：端口约定、/health /ready /metrics 内置端点、错误格式、GraphQL/OpenAPI/WebSocket/版本路由扩展接口
- 12 语言文档目录 `docs/i18n/{en,ja,ko,ru,de,fr,es,pt,hi,ar,bn,id}/`：全部 24 份文档翻译 + 3 张图片副本，README 顶部语言切换器互链

## [3.0.2] — 2026-08-27

### Fixed
- `ecat-security` SQL 注入扫描绕过：URI 百分号编码载荷（`?q=SELECT%20*%20...`）此前可绕过 header 层检测（检测正则要求字面空白），现先 percent-decode 再匹配，仅检测用解码、转发/日志 URI 不变
- `ecat-data-sqlx` AnyPool 未安装驱动：`connect()/from_config()` 首次连接即 panic "No drivers installed"，现入口处一次安装（幂等，覆盖全部连接路径）
- `ecat-data-influxdb` line protocol 过度转义：字符串 field 值不再转义空格（规范只需转义 `"` 与 `\`）；tag/field 输出经排序保证确定性
- `ecat-data-clickhouse` 建表缓存永不失效：TTL 60s 过期重建；INSERT 报缺表错误时清缓存重试一次
- `ecat-events` dev-dependencies 补 tokio features（macros/rt/time）：此前单独编译该 crate 测试目标必失败，被 workspace feature 并集掩盖

### Tests
- 全面单元测试补写：51 个 crate 全覆盖，+206 测试（核心 40 / data 66 / mq-transport 54 / app 46），workspace 总计 675 测试全绿；测试团队报告见 docs/test-report-2026-08-26.md

## [3.0.1] — 2026-08-17

### Fixed
- `ecat-mq-kafka` auto_commit=true 消息丢失量化告警：消费者流被 drop 时 warn 记录通道内未消费条数（librdkafka 已交付 offset 无法恢复，告警为可达最优解；默认 auto_commit=false 保持安全基线）
- `ecat-data` rdbms rollback 误报：显式 `rollback()` 后 Drop 不再触发 "dropped without commit — rolling back" 误导性 warn（新增 rolled_back 标志）
- `ecat-auth` OAuth2 内省安全：响应体 1MiB 有界读取（防无界内存）、`active=true` 但 sub 缺失/为空即拒绝

### Performance
- 根 Cargo.toml 新增 `[profile.release] lto = "thin"`：60 个跨 crate 热点调用可跨 crate 内联

### Tests
- `ecat-config` FileSource::load() 补 4 个测试（JSON/YAML 值断言、解析错误、顶层非 object 报错）——启动必经路径此前零测试
- `ecat-circuit-breaker` 补状态机迁移测试（open 拒绝请求、half-open 失败重开）

## [3.0.0] — 2026-08-14

### Breaking
- `ecat-mq` MessageStream Bytes 化：`poll_recv` 返回 `Vec<u8>` → `bytes::Bytes`（新增 `bytes = "1"` 依赖）；`publish(&[u8])` 签名不变零迁移。mqtt/nats 原生 Bytes 零拷贝透传、rabbitmq Vec→Bytes 所有权转移；`ecat-events` Handler 消费路径同步 Bytes 化，消除每消息拷贝
- `from_config` 签名统一：12 个数据 crate 统一为 `Result<Self, XxxError>`（保留领域错误）；`ecat-data-memcached` 由 `Self` → `Result<Self, CacheError>`（恒 Ok，唯一功能性破坏），redis/sqlx/mongodb 核实已符合
- workspace 依赖同步：`workspace.dependencies` 15 处 `ecat-*` 版本统一 3.0.0（修复 v2.4.3 发布时的遗漏）

### Added
- `ecat-bench` BenchResult 新增 `pub p95_latency_us`：p95 计算与 p50/p99 同公式（count*0.95 索引界内、空样本 0.0）、`print` 与 `http_bench compare` 补 p95 行、不变式测试 p95_between_p50_and_p99

## [2.4.3] — 2026-08-14

### Added
- `ecat-graphql` 字段参数与嵌套 selection 支持：`FieldRequest`（args/variables/selection）与 `GraphQLField` 富 resolver（`query_field` / `mutation_field` 注册）；两阶段手写解析器（字面量参数、`$var` 解引用、嵌套 selection 树、MAX_DEPTH=32）；legacy resolver 自动获得字段参数（合并进 variables），无参时逐字节兼容旧行为
- `ecat-auth` OAuth2 内省缓存加固：claims 白名单过滤（默认缓存 sub/exp/iat/role + extra 的 iss/aud/scope/roles，`cache_claims_whitelist` 可配置、"*" 逃生门；miss 仍返回完整 claims）；TTL 过期条目写路径主动清除（`purge_expired`）
- CI：`.github/workflows/ci.yml` 新增独立 cargo-audit 闸门 job（预编译 musl 二进制 + `--deny warnings` + `.cargo/audit.toml` 8 项已评估 ignore）

### Fixed
- `ecat-data-s3` quick-xml 0.32.0→0.41.0：修复 2 个高危 CVE（RUSTSEC-2026-0194/0195），`parse_list_xml` 适配新事件模型（文本追加累积 + 实体 GeneralRef 还原）
- CI 原 cargo-audit 步骤失效：`--deny medium` 为 cargo-audit ≥0.20 已废弃语法（被 continue-on-error 掩盖），改为 `--deny warnings` 并移除 continue-on-error

### Docs
- docs/dependency-cve-tracking.md：CI 接入说明、新增 protobuf / instant 评估、各条目补 RUSTSEC 编号
- README×2：Known Limitations 更新（GraphQL 支持字段参数与嵌套 selection；OAuth2 缓存白名单过滤 + TTL 清除）

## [2.4.2] — 2026-08-14

### Added
- `ecat-middleware` M3 `ValidateLayer`：请求校验中间件——`RequestValidator` trait（`validate(&Request) -> Result<(), ValidateError>`）、`ValidateError`（支持自定义 HTTP 状态码，默认 400）、`ValidateLayer::from_fn` 闭包入口（`FnValidator` 包装）；校验失败短路返回错误响应，不进入内层服务
- `ecat-middleware` M4 CORS：`cors` feature 引入 `tower-http`（optional，0.6 线）并 re-export `AllowOrigin` / `Any` / `CorsLayer`
- 示例补齐 U2（3 个）：`examples/databases`（多数据库后端连接）、`examples/middleware`（中间件组合）、`examples/websocket`（WebSocket）
- `ecat-bench`：新增 `http_bench.rs` 正式 bench 示例——bare（裸 axum）/ metrics（+MetricsLayer）/ full（+MetricsLayer+TracingLayer+LoggingLayer）三端点对比，输出 requests/QPS/p50/p99 及相对 bare 的开销；`BENCH_TOTAL` / `BENCH_CONCURRENCY` / `BENCH_WARMUP` / `BENCH_BASE_URL` 环境变量可调

### Fixed
- `ecat-metrics` MetricsLayer 挂载缺陷：`Service::Error` 由 `Box<dyn Error>` 改为透传 `S::Error`——修复 axum `Router::layer` 的 `Into<Infallible>` 约束失败（错误类型不匹配导致 layer 无法挂载）
- `ecat-middleware` ValidateLayer 同款两处：`Service::Error` 透传 `S::Error`；`FnValidator` 手动实现 `Clone`（泛型闭包无法自动 derive）

### Docs
- 新增 docs/dependency-cve-tracking.md：依赖 CVE 跟踪表（rustls-webpki 0.102.8 RUSTSEC-2026-0049 系列、rdkafka-sys cJSON CVE-2025-57052、rustls-pemfile / rsa 低危）+ 跟踪原则
- README×2：构造器命名约定注（`ecat-mq-*` 用 connect、`ecat-data-*` 多数 new，redis/sqlx 例外 connect、mongodb/s3 仅 from_config；既有约定不强制统一，3.0 窗口可评估）

## [2.4.1] — 2026-08-14

### Added
- `ecat-metrics` M1 `MetricsLayer`（tower Layer）：记录请求计数与时延直方图到全局 registry（与 /metrics 端点共享）；指标 `ecat_http_requests_total` / `ecat_http_request_duration_seconds`，标签 method/path/status；`with_path_fn` 自定义 path 标签（高基数路径归一化/脱敏，避免指标基数爆炸）
- `ecat-middleware` M2 `RetryLayer` / `RetryRule`：指数退避重试（`new(max_attempts, base_delay, max_delay)`，含首次共 max_attempts 次）；`RetryRule` trait 自定义重试判定（如按 HTTP 状态码/响应内容），默认规则仅重试服务错误；⚠️ 仅对幂等请求（GET/HEAD/PUT/DELETE）安全
- `ecat` U1 聚合 crate：feature-gated re-export 入口——12 个 feature（http/grpc/middleware/auth/client/events/metrics/tracing/circuit-breaker/consul/remote/redis），默认 http+grpc，`--no-default-features --features <组件>` 精简依赖树

### Fixed
- `ecat-transport-http`：tls_listener accept 通道关闭 panic——accept 循环退出（任务 abort/panic 致 sender 释放、通道关闭）时记录错误并挂起，不再 panic 杀死服务线程；在途连接与优雅停机信号照常处理
- `ecat-middleware` 限流：P1 flaky 测试重构——日志捕获断言改为 limiter 状态断言（消除 writer 捕获竞态）
- `ecat-metrics`：空指标体可区分——无指标注册时输出 `# no metrics registered`（原空响应体无法区分「无数据」与「有数据但输出为空」）
- 数据后端补测（4 个 crate 16 个测试）：`ecat-data-iotdb`（5）、`ecat-data-neo4j`（2）、`ecat-data-arangodb`（4）、`ecat-data-mongodb`（5）

### Security
- `ecat-auth` OAuth2 内省缓存：缓存 key 由 token 明文改为 SHA-256 hash（明文 token 不再驻留内存）；解析出的 claims 仍以明文存于 FIFO 有界缓存（默认 10_000）

### Docs
- README×2：新增聚合 crate（ecat）用法（12 feature 列表/默认 http+grpc）、M1 MetricsLayer 用法（指标名/标签/with_path_fn）、M2 RetryLayer 用法（指数退避/自定义规则/幂等性警告）；已知限制移除 2 条（WebSocket 优雅关闭、熔断判定——均已落地），保留 3 条（GraphQL、OAuth2 内省缓存、Kafka offset）
- README×2 已知限制：Kafka offset 行为说明——默认 `auto_commit=false` 重启从分区末尾（latest）重读、停机期消息被跳过；显式 `auto_commit=true` 才具备 at-least-once 语义
- docs/ecosystem-plan-v3.md：数据后端表按实测逐 crate 核对修正（驱动/能力列）
- CI：新增 cargo-audit 步骤（依赖漏洞扫描，--deny medium，continue-on-error）

## [2.4.0] — 2026-08-14

### Added
- `ecat-auth` JWT：新增 `required_issuer()` / `required_audience()` builder，强制校验 iss/aud 声明（默认不校验，向后兼容）
- `ecat-auth` OAuth2：新增 `cache_capacity(n)` builder（FIFO 有界缓存，默认 10_000，达容量逐出最旧条目）

### Fixed
- `ecat-transport-http`：TLS 握手 DoS 修复——新增 src/tls_listener.rs（后台 accept_loop + 每连接独立 spawn 握手 + 10s 握手超时），慢握手连接不再阻塞其他连接；行为不变，无 API 变更
- `ecat-auth` OAuth2：内省结果缓存由无界改为 FIFO 有界（默认 10_000），防海量唯一 token 内存无界增长
- `ecat-tls`：`skip_verify=true` 与 `ca_cert` 同时配置改为构建报错（跳过校验却配置信任锚的矛盾配置）
- `ecat-events`：消费任务退出（正常/panic）后清理占位，再次 subscribe 可重启消费，修复事件永久静默丢失
- `ecat-data-s3`：TLS 配置面重写——`tls` 字段由 bool 改为 `TlsClientConfig`，复用 `ecat_tls::build_reqwest_client`（rust-s3 → reqwest+rustls）；请求签名改为自实现 AWS SigV4（path-style 寻址，AUTHORIZATION / x-amz-date / x-amz-content-sha256 请求头），修复 S3-1/S3-2 的签名请求头装配与双重 percent-encoding
- `ecat-mq-kafka`：消费改 StreamConsumer（tokio 驱动），消除 ~200ms 固定轮询延迟
- `ecat-mq-kafka`（语义变更，⚠️ 破坏性）：group_id 派生规则——显式配置时派生为 `{group_id}-{topic_hash}`（SHA-256 取 8 位 hex：同一 (group, topic) 跨实例一致，共享消费组负载均衡、offset 组名稳定，hash 后缀消除 "-" 直接拼接的歧义碰撞）；未配置时生成随机组 `ecat-mq-{uuid}`。⚠️ 升级影响：有 group_id 的部署组名由 `{g}-{topic}` 变为 `{g}-{hash8}`，旧 committed offset 孤儿化——升级后按 offset 重置策略（默认 latest）从分区末尾重读，停机期间产生的消息会被跳过；未配置 group_id 的实例各自独立消费组（不再共享负载均衡）。新增 `auto_commit` 配置（默认 false，向后兼容）：true 时 `enable.auto.commit=true`（librdkafka 每 ~5s 自动提交，at-least-once，重启从最近提交点继续，避免停机期消息静默跳过）。消费错误分支新增 tracing::warn
- `ecat-tracing`：TracingLayer span 记录 trace_id（提取自请求头，canonical `x-ecat-trace-id` 优先、`traceparent` 兜底，无 id 时空字段）；⚠️ `TracingService` 的 `Service` 实现由完全泛型特化为 `Service<http::Request<B>>`——使用非 HTTP 请求类型的调用方需调整（编译期变更）
- `ecat-transport`：地址规范化共享——normalize_addr 统一空 host（`:8000`）→ `0.0.0.0:8000`，http/grpc/ws 三端一致，避免无 IPv6 环境绑定失败
- `ecat-scheduler`：任务 panic 韧性——job 改 JoinSet 子任务，panic 记日志后继续下一 tick（不再静默死亡）；run() 同步 panic 记录 warn
- `ecat-versioning`：未知版本 404 路径去掉 builder+unwrap（消除生产 panic 面）

### Docs
- README×2 同步：S3 实现状态表更新（rust-s3 → reqwest+rustls）、JWT 中间件示例补充 `required_issuer` / `required_audience` 用法

## [2.3.5] — 2026-08-07

> 2.3.4 未发布：workspace 版本由 2.3.3 直接跳至 2.3.5（无 v2.3.4 tag）。

### Fixed
- mTLS 测试竞态（2 个 crate）：全量 workspace 测试下 rustls 因同时编译 aws-lc-rs + ring 无法自动选择 CryptoProvider 而 panic——`ecat-transport-grpc` 两个 TLS 测试开头同步调用 `ensure_crypto_provider()`；`ecat-transport-http` 新增 OnceLock 保护的 `ensure_crypto_provider()`，在 `build_server_config`（生产路径）与测试辅助 `client_config` 内调用，一次覆盖全部 3 个 TLS/mTLS 测试
- clippy 告警清零（5 个 crate，11 处）：之前修复引入的嵌套 if / unused_mut / map_or 告警，全部折叠为 let-chain 或等价形式（ecat-cli 5、ecat-auth 2、ecat-events 1、ecat-data-questdb 1、ecat-circuit-breaker 2）

### Docs
- README×2 版本号同步 v2.3.5；docs/alipay.png、docs/weixinpay.png 底部增加 44px 边界并加水印 https://erik.xyz（已验证不遮挡二维码）
- docs/ecosystem-plan-v3.md 更新；Helm Chart appVersion 同步 2.3.5
- 新增团队协作设计（docs/superpowers/specs/2026-08-14-team-design.md）与建队实施计划（docs/superpowers/plans/2026-08-14-team-setup.md）

## [2.3.3] — 2026-08-07

### Added
- mTLS 接入 transport：`HttpServer::tls` / `GrpcServer::tls` 真正生效（tokio-rustls / tonic rustls，支持 CA 校验与强制客户端证书），附自签证书握手测试
- `ecat-cli` proto 子命令真实实现：`proto add` 创建 proto 文件；`proto client/server` 生成 tonic-build `build.rs` 并自动补齐 Cargo.toml 依赖
- `ecat run --watch`：unix 下按进程组终止服务（libc::kill），修复服务二进制成孤儿占端口
- `ecat upgrade`：真实批量升级 ecat-* 依赖版本（改写 Cargo.toml 版本要求 + cargo update）
- `ecat new` 模板：ecat-* 依赖版本与当前版本一致（原硬编码 1.0）
- `ecat-testing` MockServer：真实 axum mock（set_response / received_requests），不再仅翻转布尔标志
- Dockerfile：CMD 改为运行示例服务（helloworld），新增 .dockerignore

### Fixed
- `ecat` App::run()：已有 tracing subscriber 时跳过 `ecat_logging::init()`，修复与 ecat-tracing / ecat-tracing-otlp 的 init 冲突
- `ecat-tracing`：inject/extract 头名统一为 `x-ecat-trace-id`（与 ecat-metadata 一致），trace_id 改用 uuid 生成，TracingLayer span 注入 trace_id
- `ecat-circuit-breaker`：half-open 探活成功后清空滑动窗口，修复闭环后旧失败率立刻再次触发 open
- `ecat-transport-ws`：实现 stop()（关闭信号 + 等待结束），修复 App 关闭时挂起
- `ecat-middleware` 限流：内存/Redis store 放行语义统一（`>=` → `>`）；超限响应状态码 429
- `ecat-security`：攻击拦截响应按 `to_http_status` 映射（403）
- `ecat-auth` OAuth2：introspect 结果按 cache_ttl 缓存，不再每请求打 introspection
- `ecat-encoding` ProtoCodec：真实 prost encode/decode（原恒返回 Err）
- `ecat-transport`：删除无引用的 Request/Response/Context 死代码
- `ecat-data-redis`：Cache 补齐 increment（INCRBY）/ ttl（TTL）/ multi_get（MGET）
- `ecat-registry-etcd`：注册后后台 keepalive 续约（lease_ttl/3 周期），修复 30s 注册自动失效；deregister 取消续约
- `ecat-registry-consul`：register 附带 HTTP 健康检查（/health，10s 间隔）；discover 路径参数 URL 编码
- `ecat-data-influxdb` / `ecat-data-questdb`：query 增加 HTTP 状态码检查，错误不再静默吞掉
- `ecat-data-nebulagraph`：params 非空返回明确错误（不再静默丢弃）
- `ecat-data-tdengine` / `ecat-data-arangodb`：URL 路径段 percent-encoding
- `ecat-events`：remote 模式真实订阅——后台消费循环按事件类型分发到本地 handler（无回环重复）
- `ecat-graphql`：轻量解析器重写（嵌套字段/括号配对/字符串字面量/指令跳过），失败返回明确错误
- `ecat-bench`：修复请求数整除截断与空样本 p50/p99 越界 panic

### Docs
- README×2 同步 v2.3.3；许可证统一 Apache-2.0（与全部 Cargo.toml 一致）
- README 中间件示例修复（补 CircuitBreakerLayer / SecurityLayer 导入、JWT 密钥 ≥32 字节）
- 数据库表：Memcached 标注 ⚠️ 内存实现（非生产）
- 项目结构树补齐 6 个数据后端 crate
- CLI 快速开始对齐 proto/ 目录实际行为
- Helm `appVersion` 同步 2.3.3
- 支付码图片底部边界扩展并添加水印 https://erik.xyz

## [2.3.2] — 2026-08-07

### Fixed
- `ecat-mq` InMemoryMq：`poll_recv` 改用 `Arc<Notify>` 唤醒（`OwnedNotified`），修复空队列时的忙等自旋
- `ecat-middleware` 限流：默认 key 优先取 `ConnectInfo` 客户端地址，不再信任可伪造的转发头
- `ecat-transport-http`：用户 router 与内置 `/metrics` 路径冲突时捕获 panic 并降级为用户路由（原直接 panic）
- `ecat-data-sqlx`：Blob/BYTEA 列以 base64 字符串返回（原静默变 Null）；NaN/Inf 浮点转为字符串；Any 驱动不支持时间类型，fetch 时报错而非静默（调用方需 CAST 成文本）
- `ecat-data-clickhouse`：`write` 按 measurement 分组改为引用传递，消除全量点克隆
- `ecat-config-remote`：watch 首帧强制推送（兼容缺 X-Consul-Index 服务器）；缺 index 时 1s 退避防紧循环；阻塞查询响应缺失 X-Consul-Index 视为错误
- `ecat-registry-consul`：discover 支持 IPv6 地址（方括号）与 `https` service tag 自动切换 scheme

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
