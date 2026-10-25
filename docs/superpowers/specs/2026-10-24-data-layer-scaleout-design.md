# 数据层横向扩展设计：分片、单机、分布式、冷热分层

- 日期：2026-10-24
- 状态：待评审（设计稿，不含实现）
- 适用版本：e-cat 7.1.1 之后，按兼容契约以 7.2.0 增量发布
- 场景：MySQL 8.4 LTS / 9.x，千万级，慢查询、写锁、磁盘容量、备份/DDL 时间全部吃紧

## 0. 摘要
本设计给出一条从单 MySQL 到「分区 + 冷热分层 + 代理分片 + 可选客户端分片」的演进路径，四项能力（分库分表、单机、分布式、冷数据）统一在一条路线图里交付，不做四套割裂实现。

三条硬决策：

1. **先 B 后 A**：B 阶段复用 MySQL 原生分区与 ShardingSphere-Proxy 5.x，e-cat 只提供配置、目录、观测和兼容层；A 阶段再提供可选的进程内客户端分片。
2. **不改 SQL、不改表名**：分片路由按显式分片键选连接，不解析/改写 SQL；ORM 的 `Entity::TABLE` 与 derive 生成的表名保持不变。
3. **只增不改**：现有 `ecat-data` / `ecat-orm` 的公开 API、配置字段、四方言行为不变；新能力全部放进默认关闭的 feature 和新 crate。

**先泼冷水**：千万行本身通常不构成必须分片的理由。P0 必须先证明瓶颈不是索引、Schema、锁或连接池；如果 P0 能靠调优解决，后续阶段不启动。分片是最后手段，不是第一手段。

**实施门禁**：本设计覆盖 P0–P5 的完整路线，但每阶段独立批准；P0 的退出条件是一票否决门。P2–P4 依赖仓库外的 ClickHouse、ShardingSphere 与真实分片环境，没有环境时只交付设计与脚本，不做无法验证的实现。

## 1. 背景与目标
### 1.1 现状（已实测）

- 数据层无分片：全仓 `*.rs` 搜索 `shard|分库|分表` 零命中。
- 只有读写分离：`RdbmsRouting`（`ecat-data/src/routing.rs:103`），写主读从、副本轮询、逐端点熔断、可降级读主。
- ORM 表名是编译期常量：`Entity::TABLE`（`ecat-orm/src/entity.rs:116`），derive 生成于 `ecat-orm-derive/src/expand.rs:326`。
- 冷数据无内建支持：`StorageClient` 只有 put/get/delete/list（`ecat-data/src/storage.rs:7`）。
- 分布式锁只有单连接 Redis 实现（`ecat-data-redis/src/lib.rs:271`）；调度器是进程内 tokio（`ecat-scheduler/src/lib.rs:7`）。
- `Row` 字段私有，只有按列名读取（`ecat-data/src/rdbms.rs:7-28`）。

### 1.2 目标

- 业务 SQL 尽量不改，能从单库平滑演进到分片。
- 热数据留在 MySQL/Redis，冷数据可验证地下沉到 ClickHouse/S3。
- 单机仍是开发与最小部署形态；分布式是主路径，两者共用同一套代码路径。
- 每一步可回滚、可验证、可观测。

### 1.3 非目标（anti-goals）

- 不重造分布式数据库，不实现 Raft/Paxos。
- 默认不使用 2PC/XA；跨分片一致性用分片键亲和 + outbox + 幂等补偿 + Saga。另提供 **opt-in** XA 协调器（仅 MySQL/PostgreSQL），用于确实需要强一致的跨库场景，不作为默认路径。
- 不解析或改写任意 SQL；跨分片查询只走显式 API。
- 不兼容 ShardingSphere/Vitess 的管理面；只把它们当 MySQL 端点。
- 不改现有 API 语义来做「顺手优化」。

### 1.4 硬约束

- 兼容性：只增不改（见 §3）。
- 现有四种方言：SQLite / PostgreSQL / MySQL（8.4 LTS 与 9.x 为主，8.0 legacy）/ MSSQL，行为不变。
- 默认构建零新增依赖、零行为变化。
- 不新增 `RdbmsError` / `OrmError` / `ColType` / `Dialect` 枚举变体（均无 `#[non_exhaustive]`）。
- MySQL 目标版本：8.4 LTS（主目标）与 9.x Innovation（逐版本验证）；8.0 仅 legacy best-effort（2026-04 已 EOL）。

### 1.5 MySQL 版本矩阵

| 版本 | 定位 | 支持级别 | 关键差异 |
|---|---|---|---|
| MySQL 8.0 | 旧版 | legacy，只修不新增 | `SHOW SLAVE STATUS` 在 <8.0.22 可用；8.0 已于 2026-04 EOL |
| MySQL 8.4 LTS | 主目标 | 完整支持 | `mysql_native_password` 默认禁用；`SHOW SLAVE STATUS` / `SHOW MASTER STATUS` 移除；必须 `SHOW REPLICA STATUS` / `SHOW BINARY LOG STATUS` |
| MySQL 9.x Innovation | 目标 | 完整支持（逐个小版本验证） | `mysql_native_password` 移除；仅 `caching_sha2_password`；代理/驱动需先过认证兼容门 |

## 2. 四项能力与交付阶段
| 能力 | 现状 | 目标 | 主交付阶段 |
|---|---|---|---|
| 分库分表 | 无 | B：代理分片；A：可选客户端分片 | P3 / P4 |
| 单机 | SQLite / 单 MySQL 可用，但无 profile 与失败模式说明 | 明确的 dev / standalone 形态，零语义新增 | P1 |
| 分布式 | 读写分离、Redis 锁、进程内调度 | 延迟感知、锁续租、幂等调度、outbox、故障转移边界 | P1 / P5 |
| 冷数据 | S3 只有对象 CRUD，无分层 | 时间分区 + 归档 + 冷读 + 可验证恢复 | P2 |

## 3. 兼容契约（只增不改）

### 3.1 允许

- 新 crate、新模块、新类型、新自由函数、只读访问器。
- trait 加**带默认实现**的方法（先例：`CHANGELOG.md:286`）。
- `Row` 加只读访问器（`columns()` / `values()` / `iter()`），供跨分片合并与冷存序列化使用。
- 新 derive 可选属性，并同步更新宏；旧属性缺省行为不变。
- 新顶层配置段（`sharding:` / `tiering:`），独立类型、`#[serde(default)]`。
- 新 feature，默认关闭、不进入 default、不改名。
- 新错误类型映射到既有 `RdbmsError::Database` / `Config`，不新增变体。

### 3.2 禁止

- 加枚举变体：`RdbmsError`、`OrmError`、`ColType`、`Dialect`、`InsertPlan`。
- 给现有 pub struct 加字段：`SqlxConfig`、`MssqlConfig`、`PoolParams`、`MssqlParams`、`EntityMeta` 等（会破坏下游结构体字面量）。
- 加必需 trait 方法、改签名/返回类型、改默认实现语义（`query_write` / `execute_then_query` 尤其不能动）。
- 改 SQL 模板、占位符、分页、主键回填、软删、读写分离路径的现有行为。
- 改默认值：30s 查询超时、1s 慢查询阈值、`session_init`、`0 = 禁用` 等。
- 给现有配置加 `deny_unknown_fields`。
- 把新 feature 放进 default，或把运行时 profile 做成 feature（Cargo feature 是全图合并的）。

### 3.3 配置与 feature

- `SqlxConfig` 静默忽略未知键（`ecat-data-sqlx/src/config.rs:8-51`）；因此新键**不能**塞进它，否则一边静默失效、一边启动报错。
- `MssqlConfig` 有 `deny_unknown_fields`（`ecat-data-mssql/src/config.rs:27`）；新键塞进去会让旧二进制直接拒绝启动。
- 结论：分片/分层配置放独立顶层段，由新 crate 自己反序列化；旧程序不读、不感知。
- 聚合 crate `ecat` 新增可选 feature `sharding` / `tiering`，默认关闭（参考 `ecat/Cargo.toml:33-37` 的弱依赖写法）。

### 3.4 版本与测试门

- 仅新增：7.2.0；任何枚举变体/公开字段：8.0.0（本设计不允许）。
- CI 门：`cargo test --workspace` + `cargo semver-checks` 对比 7.1.1 + 旧配置/旧实体样例编译。
- 当前 CI 只跑默认特性、无真库（`.github/workflows/ci.yml:10-13`），P0 必须补：默认构建、`--no-default-features`、每个新 feature 单开与组合、SQLite 常跑；PG/MySQL 用 `ECAT_TEST_*_URL` + `ECAT_REQUIRE_LIVE_DB`（`ecat-data-sqlx/src/live_tests.rs:21-26`），MSSQL 同机制在 `ecat-data-mssql/src/live_tests.rs`。
- MySQL 版本矩阵：**8.4 LTS 与 9.x 必跑**，8.0 legacy 只跑冒烟；认证至少覆盖 `caching_sha2_password` 的 TLS 与非 TLS 两种连接；复制探针在 8.4/9 必须只发 `SHOW REPLICA STATUS`。

## 4. 总体架构

### 4.1 先 B 后 A

```
业务 / ORM
   │  SqlExecutor / RdbmsClient（现有签名不变）
   ├─ B：SqlxClient ──► ShardingSphere-Proxy 5.x ──► MySQL 分片
   │                     （读写分离、分片、全局 ID 归代理）
   └─ A：ShardedClient（可选 feature）──► 每个分片一个 RdbmsClient
```

- B 阶段代理是唯一入口：**只许单端点，禁止在代理外再套 `RdbmsRouting`**，否则双路由。
- A 阶段与 B 阶段共用 `ShardCatalog` 与同一套分片算法，便于影子双读和灰度切换。
- B 阶段拓扑的权威源是 ShardingSphere-Proxy 配置；e-cat 的 `ShardTopology` 只是只读镜像，用于启动校验与观测，不反向驱动代理。
- A 阶段必须能复现代理的分片算法才共用 `ShardCatalog`；若代理用了不可移植的自定义算法，A 阶段按「重新分片 + 双写迁移」处理，不假设两者天然一致。
- e-cat 在 B 阶段不解析 SQL，只做：配置校验、拓扑目录、连接选择（A）、观测装饰。
- **MySQL 8.4/9 的 B 阶段有额外门禁**：代理必须能用 `caching_sha2_password` 连接后端。第三方报告称 ShardingSphere-Proxy 5.4.x 对 `caching_sha2_password` 不稳定（官方矩阵未确认），且 MySQL 9.x 已移除 `mysql_native_password`，不能靠启用旧插件绕过。过不了门禁时，9.x 走 A 阶段（客户端直连），8.4 走 A 阶段或换支持 `caching_sha2_password` 的代理版本。

### 4.2 新增边界

| 组件 | 落点 | 默认 |
|---|---|---|
| `ShardingMode` / `ShardCatalog` / `ShardTopology` / 配置校验 / 指标装饰 | 新 crate `ecat-data-sharding` | feature `catalog` |
| `ShardedClient: RdbmsClient`（按请求分片键绑定的句柄） | `ecat-data-sharding` | feature `client`（A 阶段） |
| `TieringConfig` / `ArchiveJob` / `TieredQuery` / `RestorePlan` / 归档状态表 | 新 crate `ecat-data-tiering` | feature `tiering` |
| `Row` 只读访问器 | `ecat-data` | 无 feature |
| 延迟探针 / 读己之写 pin | `ecat-data` 或 `ecat-data-sharding` | feature `replica-lag` |
| 分布式锁 `renew` 默认方法 | `ecat-lock` / `ecat-data-redis` | 无 feature（默认方法） |
| `FencedLock` / `FenceGuard` | `ecat-lock` / `ecat-data-redis` | 无 feature |
| 远程拓扑源 | `ecat-data-sharding/src/topology/` + `ecat-config-remote` | feature `consul` / `etcd` |
| `ShardMigrator` | `ecat-data-sharding/src/migrate/` | feature `migrate` |
| `Saga` 引擎 | 新 crate `ecat-saga` | feature `saga`，默认关 |
| `TwoPhaseCoordinator` | 新 crate `ecat-data-2pc` | feature `2pc`，默认关 |
| Redis 共享连接池（cache + lock） | `ecat-data-redis` | `RedisPoolConfig`；旧单连接路径保留 |

不新建 `ecat-data` 大模块，避免把 catalog、归档、代理适配都塞进 trait crate；新 crate 通过现有 `SqlExecutor` / `RdbmsClient` / `StorageClient` / `Cache` 组合。

## 5. 分阶段路线图

### P0 — 测量与基线（1–2 周）

- 盘点：每表行数/大小、PK/FK/UNIQUE、Top 慢查询、锁等待、缓冲池命中、备份时长、DDL 时长、磁盘增长、QPS。
- 分类：每张表标记 `tune / partition / archive / shard / leave`，并记录理由。
- 产出：基线报告 + 容量预测 + 每表处置表 + 分片触发阈值。
- 退出条件：能回答「慢在查询、锁、容量还是 DDL」；否则不进入 P2/P3。
- **10M 行的特别说明**：如果 P0 发现是索引/Schema/锁问题，先修，不因「已经千万」直接上分片。

### P1 — 单机基线与读写分离（2–4 周）

- 单机 profile：dev = SQLite；standalone = 单 MySQL 主库 + 可选 Redis；不做新语义，只提供配置样例和失败模式说明。
- 基线加固：连接池参数、慢查询告警、索引、Redis 热读缓存（`Cache::set` 带 TTL）。
- 读写分离接线：`RdbmsRouting` 已有能力，补配置加载、健康检查、延迟感知；**不改其既有语义**。
- 副本延迟：`>1s` 告警，`>5s` 从读降级主库；读己之写用请求级 pin（新 API，不改默认路由）。
- 退出条件：热查询 p95 不退化；恢复演练通过；feature 全关时行为与基线逐字节一致。

### P2 — 冷热分层（4–8 周）

- 每表定义保留策略：`hot_days`（默认 180）、`partition_months`（默认 1）、`retention_days`、白名单过滤。
- 分区策略见 §7.3；优先能保住单列主键的 `RANGE(id)`，严格按时间的 `RANGE COLUMNS(ts)` 需单独评审。
- 归档到 ClickHouse（可查询冷层）或 S3（恢复/审计）；先 dry-run，再小表试跑。
- 冷读走 `TieredQuery::query_range`，不解析任意 SQL；跨边界两侧查询后按 PK 合并。
- 退出条件：行数与校验和 100% 一致、0 重复；`DROP PARTITION` 前必须 `verified`；冷层故障只影响冷查询。

### P3 — B 阶段：代理分片（6–12 周，按 P0 结论决定是否启动）

- 部署 ShardingSphere-Proxy 5.x（首选）；e-cat 用现有 `SqlxConfig` 连代理，单端点。
- 分片键、分片算法、全局 ID（代理 Snowflake/序列）在代理侧配置；e-cat 只校验拓扑。
- 迁移：新集群 → 双写或 CDC 追平 → 区间回填 → checksum 校验 → 短只读窗切写 → 观察 → 切读；旧库保留 N 天可秒级回滚。
- 进入阈值（默认，P0 可调）：单实例 ≥1TB、持续写 ≥3000 TPS、单表 ≥3 亿行，或复制延迟 >5s 每周 ≥3 次。
- 退出条件：业务 SQL 零修改经代理跑通；跨分片查询 <5%；分片容量偏差 <20%；复制延迟 p99 <1s；切换 RTO <5min。

### P4 — A 阶段：可选客户端分片（8–12+ 周，按需）

- `ShardedClient` 实现 `RdbmsClient`，按显式分片键选连接；逻辑表名不变，无键即报错，不猜。
- 与 B 共用 `ShardCatalog`/算法；先影子双读比对，再按租户灰度切连接。
- 进入阈值：代理 QPS >5 万、代理额外 p99 >2ms，或需要租户级故障域。
- 退出条件：路由分布卡方检验通过；跨分片事务明确拒绝；scatter 归并正确；全局 ID 并发唯一；补偿重放幂等。

### P5 — 分布式运维能力（可与 P3/P4 并行）

- 分布式锁：`DistributedLock` 加 `renew` 默认方法；`RedisLock` 实现续租；锁 TTL 必须大于单次运行。
- 分布式调度：不引入调度引擎；任务内抢锁 + 幂等 + 断点续跑，多实例只有一个执行。
- outbox + Saga：框架提供 outbox 表、relay 与轻量 Saga 引擎（`ecat-saga`）；复杂编排仍可留在业务侧。
- 故障转移：主库切换交给 Orchestrator/MHA/ProxySQL 或代理；e-cat 不实现选主。
- 观测：`shard` / `mode` 有界标签、查询直方图、扇出/补偿积压、代理指标单独采集。

### P6 — 单机/分布式运行基座（跨阶段，建议与 P1/P3 并行）

- 部署模式：`DeploymentMode::{Standalone, Distributed}` + 启动校验；单机禁止 sharding，分布式要求 `node_id`。
- 生命周期：归档/outbox 等后台任务支持优雅停机；`App` 的 on_start/on_stop 统一管理。
- 健康检查：分片拓扑、冷层、outbox 积压、归档延迟；分布式节点 readiness 必须包含这些检查。
- 资源预算：N 分片 × 连接池上限不得超过总预算；scatter 有总超时与并发上限。
- outbox relay：租约 claim + 幂等 handler + 退避重试 + 停机。
- 部署清单：standalone / distributed 两套 compose/Helm profile；同一套 CRUD/ORM 对等测试。
- 拓扑热更新（P4 门控）：`TopologySource` + `ShardRouter::reload`；旧句柄保持旧快照。

### P7 — 高级分布式事务与迁移（门控）

- fencing token：单调 fence + 陈旧持有者拒绝；归档/outbox 写前校验。
- 远程拓扑：Consul/etcd `TopologySource` + watch → `ShardRouter::reload`；启动 fail-closed，失败保留最后已知。
- 分片迁移/扩缩容：A 阶段 `ShardMigrator`（plan/backfill/verify/cutover/rollback）；B 阶段用 ShardingSphere-Scaling。
- Saga：`ecat-saga` 轻量引擎（步骤 + 逆序补偿 + 状态持久化 + resume）。
- 2PC：`ecat-data-2pc` opt-in XA（仅 MySQL/PostgreSQL）；协调日志 + 恢复；默认关。
- Redis 锁连接池：`RedisLockPoolConfig` + deadpool；旧 `connect`/`from_config` 不变。

## 6. 分片详细设计

### 6.1 分片键

| 场景 | 键 | 说明 |
|---|---|---|
| SaaS | `tenant_id` | 大租户（>20% 容量）独立分片，或用 `(tenant_id, user_id)` |
| 用户中心 | `user_id` | 子表冗余 `user_id` 保持亲和 |
| 订单 | `user_id` 路由 + `created_at` 分区/归档 | 商家维度建二级索引表 |
| 日志/时序 | 时间分区 | 不以时间做分片键（写热点） |

通则：高基数、不可变、查询必带；忌自增、可空、低频列；一致性哈希 + 虚拟节点，按 2 倍预留。

### 6.1.1 A 阶段的分片键入口

现有 `SqlExecutor`/`RdbmsClient` 方法只有 sql+params，`transaction()` 没有参数，所以分片键不能藏在全局客户端里。采用**按请求绑定**的句柄：

`ShardRouter::for_key(&key) -> Result<KeyedClient, ShardError>`；`KeyedClient: RdbmsClient` 把六个方法都发往该键对应的分片，事务天然同分片。无键时 `for_key` 直接报错，不猜。跨分片查询走独立的显式 `scatter` API，不伪装成 `RdbmsClient`。ORM 签名不用改：`User::find_by_id(&keyed, 1)`。

### 6.2 跨分片查询

- 禁止跨分片 JOIN；`Query::join`（`ecat-orm/src/query/mod.rs:155`）在 A 阶段加同分片校验。
- 小字典表广播；二级索引表两跳；仅显式 scatter（归并 ORDER BY/LIMIT、限流、超时）。
- 不得静默漏行：部分分片失败必须返回明确错误，不能当空结果。

### 6.3 事务与一致性

- 同分片：复用现有 `Transaction`（单 inner，`ecat-data/src/rdbms.rs:56`）。
- 跨分片默认：outbox + 幂等补偿 + Saga，补偿键全局唯一。需要强一致时可选 `ecat-data-2pc` 的 XA 协调器（仅 MySQL/PostgreSQL，opt-in，见 §8.5），不作为默认路径。
- 写路径的 `query_write` / `execute_then_query` 必须落主库且同分片；A 阶段漏覆写会把 MySQL 两步插入发错库（模板见 `ecat-data/src/routing.rs:162-234`）。
- `RdbmsClient::transaction()` 返回具体 `Transaction`，跨分片只能走新 API，不改现有签名。

### 6.4 全局 ID

- B：代理 Snowflake/序列，保留 `auto_increment`。
- A：i64 Snowflake；worker id 用静态 `worker_id` 配置或数据库租约表分配，**不依赖 `ecat-registry`**（`Registry` 只有 register/deregister/discover/list_services，没有租约或原子分配，`ecat-registry/src/lib.rs:68-73`）。
- `Entity::insert` 固定返回 i64（`ecat-orm/src/crud.rs:106-149`）；分布式 ID 不改变该契约，新增显式 ID 注入口，不复用旧签名。

### 6.5 迁移与回滚

1. 单库 → 分区：在线 DDL（`ALGORITHM=INPLACE, LOCK=NONE`）；不支持则用与目标版本兼容的工具（MySQL 8.4 需 gh-ost ≥1.1.8；MySQL 9.x 尚未验证，优先原生在线 DDL 或 pt-online-schema-change），影子表回填后 rename。首次分区转换可能只能 COPY，必须维护窗口（P0 实测）。
2. → Proxy：新集群同构 schema；双写（失败入 outbox）或 CDC（Debezium/Canal）追平；按分片键区间回填；校验 = 行数 + 主键区间 checksum + 抽样字段 diff = 0。
3. 切流：短只读窗 → 切写 → 观察 → 切读；旧库保留并反向 CDC N 天；配置/DNS 秒级切回。
4. → 客户端分片：与 Proxy 共用 `ShardCatalog`，影子双读后按租户灰度。
5. MySQL 8.4+ 的复制/运维语句：`SHOW REPLICA STATUS`、`SHOW BINARY LOG STATUS`、`CHANGE REPLICATION SOURCE TO`、`START/STOP REPLICA`；不要在新版本上使用 `SHOW SLAVE STATUS` / `SHOW MASTER STATUS` / `CHANGE MASTER TO`。

### 6.6 迁移工具边界

- `Migrator` 每条一条语句、非事务（`ecat-orm/src/migrate/mod.rs:202-218`），且版本表每库独立（`ecat-orm/src/migrate/version.rs:18`）。
- 分片 DDL 不走 `Migrator`：由管理 CLI 逐分片执行并校验；ORM 只做 CRUD。
- 分区表 DDL 也必须外部脚本维护，因为 MySQL/PG/MSSQL 要求分区列进入唯一键，而现有 ORM 主键是单列。

## 7. 冷热分层详细设计

### 7.1 单机边界

| 形态 | 组成 | 失败模式 |
|---|---|---|
| dev | SQLite URL（`Dialect::from_url`，`ecat-data/src/dialect.rs:25-43`） | 单写者，并发写 BUSY；无分区/归档 |
| standalone | 1 个 MySQL 主库 + 可选 Redis | MySQL 单点 = 全站不可用；无 HA |
| 进程内 | HashMap 缓存（`ecat-data-memcached`）、tokio 调度（`ecat-scheduler`） | 重启即失；调度重叠；多实例重复 |

不新增 profile 语义：用不同 YAML 选择形态，新顶层 `tiering:` 缺省关闭，旧配置零变化。分布式部署必须用 Redis（或外部缓存）；进程内 `HashMap` 只允许单机 dev/small-prod，否则跨实例不一致。

### 7.2 热冷判定

- 主判据：时间，`ts < now() - hot_days`（默认 180 天，按月分区）。
- 业务白名单：只归档写后不变的行（事件/日志/已关闭订单）；可更新表排除或用 `status` 过滤。
- 访问频率：一期只观测（慢查询日志/performance_schema），不自动迁移；确需时用 Redis `increment`，放三期。
- 配置：`enabled`、`hot_days`、`partition_months`、`tables[{name,time_col,id_col,retention_days,target,filter}]`、`batch_size`、`max_rows_per_sec`、`window`、`verify_sample`、`retain_s3_days`、`dry_run`。

### 7.3 分区策略（关键取舍）

MySQL 限制：`RANGE COLUMNS(ts)` 要求每个唯一键（含 PK、全部业务 UNIQUE）都含 `ts`；InnoDB 分区表不能有外键，也不能被外键引用；子表要删 FK 并冗余 `ts`。

两个方案：

| 方案 | 保留单列 PK | 适用 | 代价 |
|---|---|---|---|
| `RANGE(id)` + 时间→max(id) 边界表 | 是 | append-only 表（日志/事件） | 回填/长事务会破坏 id↔时间序 |
| `RANGE COLUMNS(ts)`，PK 改 `(id, ts)` | 否 | 严格按时间、允许重建 PK | 首次转换重建表；业务 UNIQUE 需含 ts 或降级为普通索引 + 应用校验 |

**默认选 `RANGE(id)`**；`RANGE COLUMNS(ts)` 必须逐表评审，并在影子表验证。分区键选错是整套方案里唯一不能「只增不改」的点，代价高于本期所有新增代码。

MySQL 8.4/9 的 `RANGE COLUMNS` 语法未变，但首次 `ALTER ... PARTITION BY` 的算法/锁级别必须在目标版本实测；gh-ost 不用于分区转换。

### 7.4 归档任务

- 状态表：`tier_archive_jobs(table, partition, chunk, low_id, high_id, rows, checksum, phase, updated_at)`，主键前三元组。
- 阶段：`pending → copied → verified → dropped`；`DROP PARTITION` 前必须 `verified`。
- 幂等：chunk 级 upsert；ClickHouse 用 `ReplacingMergeTree ORDER BY id`（或去重 token）；S3 key 固定、已存在跳过；manifest 最后写作为提交点。
- 断点：`WHERE id > ? ORDER BY id LIMIT batch`，不用 OFFSET；重启只补未完成阶段。
- 限流：批大小 + 令牌桶 + 夜间窗口 + 单次超时；`Scheduler` 会重叠，任务内再加互斥并抢 `RedisLock`；锁无续租，TTL 要大于单次运行，靠缩小 chunk 保证。
- 校验：行数 + 规范化 NDJSON 的 xxhash/CRC32、1% 抽样回读、ClickHouse count 与 S3 `get` 后哈希全部通过才 `verified`；等待 `retain_s3_days` 后再 `DROP PARTITION`。
- 备份：冷数据恢复必须跨层；`DROP PARTITION` 是元数据级、秒级，但只有 `innodb_file_per_table=ON` 才归还磁盘；预建未来 3 个月分区，避免 MAXVALUE 重组。

### 7.5 冷读

- 新 API：`TieredQuery::query_range(table, from, to, params)`，只接受区间与参数，不解析任意 SQL。
- 全热 → MySQL；全冷 → ClickHouse；跨边界 → 两侧查询（下推 offset+limit）后按 PK 合并去重排序再分页。
- S3 只用于恢复/审计，不参与在线查询。
- 约束：`ClickhouseClient` 已是 `SqlExecutor/RdbmsClient`（`ecat-data-clickhouse/src/lib.rs:359-430`），但无参数绑定（`ecat-data/src/rdbms.rs:206-219`），标识符必须白名单校验；`StorageClient` 整对象在内存（`ecat-data/src/storage.rs:7-18`），chunk 限 64–128MB。

### 7.6 恢复

- `DROP PARTITION` 前删 ClickHouse 分区/S3 对象并重置状态即可回滚。
- `DROP PARTITION` 后：把分片灌入同构 staging 表，`EXCHANGE PARTITION` 换回（需无 FK、结构一致）；ClickHouse 副本作第二来源。
- 恢复演练必须进 CI/运维清单，不能只写在文档里。

## 8. 分布式能力

### 8.1 读写分离与延迟

- 现有 `RdbmsRouting` 语义不变：写主、读副本轮询、逐端点熔断、副本全挂可降级读主。
- 已知事实纠正：`Breaker::state()` 在冷却后返回 `HalfOpen`（`ecat-circuit-breaker/src/breaker.rs:243-252`），副本不会被永久排除；`routing.rs:146-148` 的旧注释应作为文档修复，不应据此设计。
- 新增：副本延迟探针（`RdbmsClient::replica_lag`）+ 请求级读己之写 pin；延迟 `>1s` 告警，`>5s` 读降级主库。
- MySQL 8.4/9 只发 `SHOW REPLICA STATUS`，读取 `Seconds_Behind_Source`；`SHOW SLAVE STATUS` 在 8.4 已移除，不得作为回退路径。8.0 <8.0.22 才允许旧语句。
- 事务天然走主、不过熔断（`ecat-data/src/routing.rs:228-233`），保持不变。

### 8.2 分布式锁与调度

- `DistributedLock` 只有 acquire/release（`ecat-lock/src/lib.rs:10-22`）；加 `renew` 默认方法，`RedisLock` 实现续租。
- Redis cache 与 lock 共享 `RedisPool`（deadpool，复用 workspace 依赖）；新增 `RedisPoolConfig`，不改现有 `RedisConfig` / `connect` / `from_config`（继续单连接兼容）。`RedisLock` 另加命令超时与续租。
- 加 fencing token：`FencedLock::acquire_fenced` 返回单调递增的 fence；归档/outbox 在写状态与 `DROP PARTITION` 前校验 fence，拒绝陈旧持有者。
- 调度器不持久化、多实例会重复；归档/迁移任务必须锁 + 幂等 + 断点续跑。
- 后台任务必须有优雅停机；归档/outbox 使用 fencing token 拒绝陈旧持有者，drop 前必须 verified。

### 8.3 outbox 与补偿

- 框架提供 outbox 表、租约 claim、幂等消费钩子与 relay；另提供 `ecat-saga` 轻量编排引擎（正向步骤 + 逆序补偿 + 状态持久化 + resume），复杂业务编排仍可留在业务侧。
- 补偿键全局唯一；重放幂等；跨分片事务默认拒绝，不静默降级；需要强一致时显式使用 §8.5 的 opt-in XA。

### 8.4 故障转移与可观测

- 主库切换交外部（Orchestrator/MHA/ProxySQL/云 RDS）；代理至少 2 实例 + LB。
- readiness 包含分片拓扑、冷层、outbox/归档积压；连接池预算按 **实例数 × 分片数 × 每池上限** 校验；scatter 总超时/并发上限。
- 指标：`shard` / `mode` 有界标签、查询直方图、扇出数、补偿积压、代理指标单采；避免高基数标签。

### 8.5 高级事务：Saga 与 2PC

**Saga（默认跨分片一致性方案）**

- 新 crate `ecat-saga`：`SagaStep`（`execute` / `compensate`）+ `Saga` 编排器；正向顺序执行，失败后逆序补偿。
- 状态表 `saga_instances(id, status, current_step, context, updated_at)` 与 `saga_steps(saga_id, step, status, attempts, error)`；支持 `resume` 断点续跑。
- 语义：步骤幂等；补偿幂等；补偿失败进入 `CompensationFailed`，必须人工介入并留告警；不自动重试到无限。
- 默认关闭，聚合 crate 用 feature `saga` 开启。

**2PC / XA（opt-in，强一致）**

- 新 crate `ecat-data-2pc`：仅支持 MySQL 8.0+（`XA START/END/PREPARE/COMMIT/ROLLBACK`）与 PostgreSQL（`PREPARE TRANSACTION`，要求 `max_prepared_transactions > 0`）。
- 协调日志表 `distributed_tx(id, status, participants, decision, created_at)`；流程：`BEGIN` → 执行 → 全部 `PREPARE` → 写 `COMMIT` 决策 → 逐参与者 `COMMIT`；任一 prepare 失败则全部 `ROLLBACK`。
- 恢复：启动时扫描 `PREPARED`/`COMMITTING`；参与者侧用 `XA RECOVER` / `pg_prepared_xacts` 检查 in-doubt；commit/rollback 幂等。
- 边界：不支持 ClickHouse/S3/SQLite/MSSQL；不用于热路径；没有 coordinator 选主（coordinator 库是单点，需外部 HA）；必须监控 prepared 事务与协调日志。
- XA 命令是连接作用域：participant 必须固定一条物理连接（`SqlxClient::pool().acquire()`）执行整个 XA 生命周期，不能走池化短连接。
- 默认关闭，聚合 crate 用 feature `2pc` 开启；现有 `RdbmsClient` 不加方法，XA 能力走新 trait `XaParticipant`。

### 8.6 分片迁移 / 扩缩容

- **B 阶段**：数据迁移交给 ShardingSphere-Scaling / 代理侧工具；e-cat 只做迁移前校验、迁移后 `ShardRouter::reload`，不重复造迁移器。
- **A 阶段**：新 `ShardMigrator`：
  1. `ShardPlan::diff(old, new)` 计算需要搬移的 `(table, from, to, key_range)`。
  2. 双写窗口：应用写旧分片 + 新分片（或 outbox 补偿）。
  3. 按 key range 分批回填，幂等 upsert，限速。
  4. 校验：行数 + PK 区间 checksum + 抽样 diff；不一致不切流。
  5. 切流：`ShardRouter::reload(new_catalog)`；旧分片保留 N 天。
  6. 回滚：反向搬移 / 拓扑切回；迁移状态表 `shard_migration_jobs` 记录阶段与游标。
- 约束：hash-mod 扩缩容会移动大部分 key；range 分片只移动连续区间；迁移期间必须能双写；P0 门禁未过不执行。

### 8.7 远程拓扑源

- `TopologySource` 增加远程实现：
  - Consul：复用 `ecat-config-remote::ConsulConfigSource` 的 blocking query/watch，读取拓扑 key，解析 `ShardCatalog`。
  - etcd：在 `ecat-config-remote` 增加 `EtcdConfigSource`（v3 `/v3/kv/range`；首版轮询，后续换 `/v3/watch`），复用同样的 JSON 解析。
- 行为：启动时无拓扑 -> fail-closed（不猜默认分片）；watch 到新拓扑 -> `validate` -> `ShardRouter::reload`；失败保留最后已知可用拓扑并告警。
- 安全：Consul ACL token / etcd 认证走配置；TLS 用现有 `TlsClientConfig`。

## 9. 兼容性与验收矩阵

### 9.1 必须全绿的矩阵

| 维度 | 用例 |
|---|---|
| 构建 | default workspace；`--no-default-features`；每个新 feature 单开 + 组合 |
| 方言 | SQLite 常跑；PG/MSSQL 用 `ECAT_TEST_*_URL`；MySQL 8.4 与 9.x 必跑，8.0 冒烟 |
| MySQL 认证 | `caching_sha2_password` × TLS/非 TLS；9.x 不得使用 `mysql_native_password` |
| MySQL 复制探针 | 8.4/9 上 `SHOW REPLICA STATUS` 成功，且不调用 `SHOW SLAVE STATUS` |
| MySQL 分区 | `RANGE COLUMNS` 在 8.4/9 实测；首次转换窗口可接受 |
| 路径 | 迁移、CRUD、批量分块、乐观锁、软删、分页 COUNT、关联预加载、事务提交/回滚、读写分离、熔断 |
| 兼容 | feature 全关时 SQL/行/错误与基线逐字节一致；旧配置/旧实体样例编译；`cargo semver-checks` 对 7.1.1 |
| 分片 | 2 分片键路由、跨片排序分页、跨片唯一、部分失败、扩缩容、跨片事务默认拒绝 |
| 锁/缓存池 | fencing 单调递增；陈旧 fence 被拒绝；RedisCache 与 RedisLock 共享池、池上限生效；旧单连接路径不变 |
| 拓扑源 | Consul/etcd 加载与 watch；坏 JSON fail-closed；保留最后已知拓扑 |
| 迁移 | plan/backfill/verify/cutover/rollback；checksum 不一致不切流 |
| Saga | 成功、逆序补偿、resume、补偿失败告警 |
| 2PC | 全 prepare 成功提交；部分失败回滚；协调器崩溃恢复；commit 幂等 |
| 分层 | 冷热同值、NULL/缺失、失效/TTL、分片键碰撞、冷层不可用降级 |
| 部署模式 | standalone（SQLite/单 MySQL）与 distributed（proxy/2 分片）跑同一套 CRUD/ORM，结果一致 |
| 生命周期 | 归档/outbox worker 收到 shutdown 后 100ms 内退出，不留下半完成状态 |
| 资源 | 分片池总量超预算时启动失败；scatter 超时返回错误 |
| 拓扑 | reload 后新键走新分片，旧句柄不受影响 |

### 9.2 迁移验收

- 回填校验 0 差异；双跑 ≥7 天零 diff；回滚演练 <5min。
- 归档：kill -9 各阶段续跑无重无漏；双进程仅一个执行（Barrier 测试）；限速达标。
- 冷读：跨边界 golden set 与归档前逐行一致。
- MySQL 8.4/9：`caching_sha2_password` TLS/非 TLS 连接、`SHOW REPLICA STATUS`、`RANGE COLUMNS` 分区 DDL 在目标版本实测通过；gh-ost/代理版本门禁通过。
- 高级事务：Saga 补偿/resume 通过；2PC 在 MySQL/PostgreSQL 真库通过，in-doubt 恢复演练通过。
- 迁移：分片迁移回填 0 差异、切流/回滚演练 <5min；远程拓扑源断线保留最后已知拓扑。

## 10. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| 分区键选错 | 需再次重建表，代价最高 | P0 逐表核对 PK/FK/UNIQUE；影子表验证；`RANGE(id)` 优先 |
| 首次分区转换非在线 | 阻塞写，需维护窗口 | 在目标版本实测算法与锁级别；原生在线 DDL / gh-ost ≥1.1.8 / 影子表 |
| MySQL 9.x 移除 `mysql_native_password` | 旧账号/旧驱动直接连不上 | P0 迁移到 `caching_sha2_password`；TLS/非 TLS 都测；禁止新账号用旧插件 |
| 有报告称 sqlx 0.8.6 `caching_sha2_password` fast-auth 路径异常（#4244） | 每次连接走 RSA 全认证，连接风暴 | P0 用项目实际 sqlx 版本压测；必要时升级/打补丁，不能只信报告 |
| 第三方报告称 ShardingSphere-Proxy 5.4.x 对 `caching_sha2_password` 不稳定（官方矩阵未确认） | B 阶段在 8.4/9 可能不可用 | P3 认证门禁；9.x 走 A 阶段或换代理版本；不要依赖 `mysql_native_password` |
| gh-ost <1.1.8 不支持 MySQL 8.4 | 在线迁移工具直接失败 | 固定 gh-ost ≥1.1.8；9.x 先验证，否则原生在线 DDL/pt-osc |
| `SHOW SLAVE STATUS` 在 8.4 移除 | 监控/迁移脚本报语法错 | 统一 `SHOW REPLICA STATUS`；`SHOW MASTER STATUS` → `SHOW BINARY LOG STATUS` |
| 业务 UNIQUE 不含分区键 | 约束语义变化 | 降级为普通索引 + 应用校验，单独评审 |
| 单列 PK 与时间分区冲突 | ORM 建表不兼容 | ORM 只做 CRUD；分区 DDL 外部脚本 |
| `insert` 固定返回 i64 | 分布式 ID 不能复用 | 新增显式 ID API，不改旧签名 |
| `Row` 私有 | 跨分片/冷存无法安全合并 | 加只读访问器（线格式保持不变） |
| ClickHouse 无参数绑定 | 注入/拼接风险 | 标识符白名单；参数安全编码；不拼任意 SQL |
| S3 整对象内存 | 大对象 OOM | chunk 64–128MB；不用于在线查询 |
| 跨库无事务 | 冷热不一致 | 状态机 + 校验和；verified 前不 drop |
| 锁无续租 | 长任务被抢 | 缩小 chunk；TTL > 单次运行；fail-closed |
| 调度重叠 | 重复归档 | 锁 + 幂等 + 断点 |
| 单 MySQL/代理单点 | 全站不可用 | 外部 HA；代理 2 实例 + LB |
| 实例数 × N 分片 × 池上限 | 连接数打爆 MySQL | 启动时校验总预算；按实例/分片调小池 |
| 后台任务未停机 | 半完成归档/outbox | 优雅停机 + 断点状态 + 幂等 |
| 拓扑变更未热更新 | 新分片不可达/写错分片 | TopologySource + reload；旧句柄快照 |
| 锁过期无 fencing | 陈旧写者 | 本期加 fencing token；写状态/drop 前校验 |
| XA 协调器崩溃 | in-doubt 事务、prepared 泄漏 | 协调日志 + 启动恢复 + 幂等 commit/rollback；仅 opt-in |
| Saga 补偿失败 | 数据半完成 | 补偿重试 + 人工介入标记；补偿幂等 |
| 远程拓扑源不可用 | 无法 reload/启动 | 本地缓存最后已知拓扑；启动时无拓扑 fail-closed |
| 分片迁移切流失败 | 数据不一致 | 双写 + checksum + 可回滚；verify 通过才切 |
| Redis 单连接队头阻塞 | 缓存/锁热路径吞吐受限 | cache+lock 共享 `RedisPool`；旧单连接仅兼容路径 |
| 共享 RedisPool 故障域绑定 | 缓存打满时锁也排队 | 预留容量，或拆成 cache/lock 两个 `RedisPool`（同一配置类型） |
| 高基数指标 | 监控爆炸 | `shard`/`mode` 有界标签 |
| 配置双轨 | 新键静默失效或启动失败 | 新顶层配置段，不塞旧 struct |

## 11. 已确认决策与剩余前提

1. **冷查询落点：已确认 ClickHouse 已具备**。冷读走 ClickHouse，S3 只做恢复/审计；`TieredQuery` 包含 CH 适配与跨边界归并分页。同库冷表降级分支保留，但本期不实现。
2. **代理选型：默认 ShardingSphere-Proxy 5.x**；只有 K8s + 专职 DBA + 10 倍增长才考虑 Vitess。
3. **首批表：业务方将提供表结构 + 慢查询样本**。P0 可据此产出每表处置、分区键与分片键候选；缺数据时不进入 P2/P3。
4. **交付节奏：已确认先出实现计划，不写代码**。实现计划按本设计的阶段拆任务、文件落点、测试与验收。
5. **MySQL 8.4/9 是硬目标**；B 阶段在 9.x 上以代理认证门禁为前提，过不了则 9.x 走 A 阶段。
6. **单机与分布式双模式**：部署模式、停机、健康、预算、outbox relay、部署清单、拓扑热更新纳入计划 Task 25–31。
7. **高级分布式事务与迁移**：fencing token、Consul/etcd 远程拓扑、分片迁移/扩缩容、Saga、opt-in 2PC、Redis cache+lock 共享池纳入计划 Task 32–37；2PC 默认关，仅 MySQL/PostgreSQL。

## 12. 工作分解（供后续实现计划展开）

| 工作包 | 主要文件/落点 | 依赖 |
|---|---|---|
| P0 基线工具 | 新增 `scripts/` 只读 SQL + 报告模板 | 无 |
| 延迟探针/读己之写 | `ecat-data` / `ecat-data-sharding` | RdbmsRouting |
| `Row` 只读访问器 | `ecat-data/src/rdbms.rs` | 无 |
| 分布式锁续租 | `ecat-lock`、`ecat-data-redis` | 默认方法 |
| 归档状态表 + 任务 | 新 crate `ecat-data-tiering` | Scheduler、RedisLock、S3/CH |
| 分区 DDL 管理 | 新 CLI 子命令（`ecat-cli`） | 外部 DDL |
| ShardCatalog/拓扑校验 | 新 crate `ecat-data-sharding` | SqlxConfig |
| 代理接入与迁移工具 | `ecat-data-sharding` + 运维脚本 | ShardingSphere-Proxy |
| ShardedClient（A） | `ecat-data-sharding` feature `client` | ShardCatalog |
| 观测与标签 | `ecat-metrics` 各后端 metrics feature | 有界标签约定 |
| 部署模式校验 | `ecat/src/deployment.rs` | 无 |
| 后台任务停机 | `ecat-data/src/worker.rs`、归档/outbox | Tokio watch |
| 健康检查 | 两个新 crate 的 `health` feature | `ecat-health` |
| 连接池预算 | `ecat-data-sharding/src/budget.rs` | `RdbmsClient::pool_size` |
| outbox relay | `ecat-data-sharding/src/outbox/relay.rs` | `OutboxStore`、`Shutdown` |
| 部署清单/对等测试 | `docker-compose.dev.yml`、`ecat-deploy/*`、`scripts/deploy/` | 外部环境 |
| 拓扑热更新 | `ecat-data-sharding/src/topology.rs` | `ShardRouter` |
| fencing token | `ecat-lock` / `ecat-data-redis` | `DistributedLock` |
| Redis 共享池（cache + lock） | `ecat-data-redis` | deadpool、`RedisPoolConfig` |
| 远程拓扑源 | `ecat-data-sharding/src/topology/`、`ecat-config-remote` | Consul/etcd |
| 分片迁移/扩缩容 | `ecat-data-sharding/src/migrate/` | `ShardRouter::reload`、checksum |
| Saga 引擎 | 新 crate `ecat-saga` | outbox、状态表 |
| 2PC/XA 协调器 | 新 crate `ecat-data-2pc` | MySQL/PostgreSQL XA |

## 13. 明确不做
- 不做 SQL 解析/改写中间件。
- 不做 Raft/选主；2PC 仅提供 opt-in XA 协调器（MySQL/PostgreSQL），不作为默认路径。
- 不做跨分片 JOIN 的通用支持。
- 不做自动冷热迁移（首期只 dry-run + 白名单定时任务）。
- 不做 S3 range/multipart/预签名（本期不需要）。
- 不改 `Entity` / `Dialect` / `SqlExecutor` / 现有配置结构体。
