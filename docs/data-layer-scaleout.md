# Ecat 数据层横向扩展

> 状态：`7.2.0`（已发布）。本页描述 7.2.0 新增、默认关闭的数据层能力。
> 兼容性约束：既有 `SqlExecutor` / `RdbmsClient` / `Cache` / `Entity` / `Dialect` 与配置字段不变；
> 新能力通过新 crate 和 feature 接入，不改变单机默认行为。

![Ecat 数据层横向扩展架构](data-layer-scaleout.svg)

## 能力与开关

| 能力 | Crate | Feature / 入口 | 默认 |
|---|---|---|---|
| 分库分表、scatter、Snowflake、outbox、迁移 | `ecat-data-sharding` | `health` / `consul` / `etcd` / `migrate` | 关 |
| 冷热分层、ClickHouse/S3 sink、跨边界查询、CLI | `ecat-data-tiering` | `clickhouse` / `s3` / `health` / `cli` | 关 |
| Saga 引擎 | `ecat-saga` | 独立 crate | 可选依赖 |
| opt-in XA 2PC | `ecat-data-2pc` | `sqlx`（默认开，仅 MySQL/PostgreSQL） | 默认关（聚合 crate `2pc`） |
| 后台 worker 生命周期 | `ecat` | `worker` | 关 |
| 共享 Redis cache+lock 池 | `ecat-data-redis` | 恒可用 | 旧构造器仍走单连接 |

聚合 crate 的对应 feature：`sharding` / `tiering` / `saga` / `"2pc"` / `worker`。
MySQL 版本目标为 8.4 LTS / 9.x；8.0 仅保留兼容分支。

## 1. 分库分表

### 1.1 拓扑

```yaml
sharding:
  mode: proxy            # proxy: ShardingSphere-Proxy 决定路由；client: e-cat 直连分片
  catalog:
    tables:
      - name: orders
        key: user_id
        pk: id
        algorithm:
          hash_mod: { shards: 4 }
        shards:
          - { name: s0 }
          - { name: s1 }
          - { name: s2 }
          - { name: s3 }
```

`proxy` 模式禁止每个分片自带 `url`；`client` 模式要求每个分片有 `url`。
`ShardCatalog::validate` 校验表名/键/主键、分片数量、端点唯一性与区间边界。
哈希算法必须与代理配置逐字节一致；不可移植的 Java `hashCode` 等算法不能假设天然一致。

### 1.2 按请求绑定分片

```rust
let keyed = router.for_key("orders", &json!(42))?;
// 六个方法（execute/query/execute_with/query_with/dialect/transaction）都走同一分片
let rows = keyed.query("SELECT * FROM orders WHERE user_id = 42").await?;
let tx = keyed.transaction().await?;
```

没有分片键就报错，不猜默认分片。跨分片读取必须显式使用 `scatter`：

```rust
let rows = router.scatter(&ScatterRequest {
    table: "orders".into(),
    sql: "SELECT * FROM orders WHERE status = ?".into(),
    params: vec![json!("paid")],
    order_by: "id".into(),
    limit: 100,
    offset: 0,
    timeout: Duration::from_secs(5),
    max_concurrency: 8,
}).await?;
```

`scatter` 任一分片失败或总超时即整体报错；排序、按主键去重与分页在内存中完成。
`Snowflake` 生成全局 ID；`outbox` 存储与 relay 负责跨分片事件的幂等投递。

### 1.3 迁移 / 扩缩容

```rust
let plan = ShardPlan::diff(&old_catalog, &new_catalog)?;
let migrator = ShardMigrator::new(plan, clients, state_db, old_catalog, new_catalog)?
    .with_chunk_size(1000);

migrator.backfill().await?;   // 主键游标、幂等 upsert、可断点续跑
migrator.verify().await?;     // 源/目标行数 + XOR checksum；不一致不切流
migrator.cutover(&router, new_catalog.clone()).await?;
// 出问题时：
migrator.rollback(&router).await?; // 只切回旧拓扑，旧分片保留

// hash 重分片期间由调用方双写（切流前：旧为主、新为镜像）：
let dual = DualWrite::before_cutover(&old_router, &new_router);
dual.for_key("orders", &json!(id))?
    .execute_with(
        "UPDATE orders SET status = ? WHERE id = ?",
        &[json!("paid"), json!(id)],
    )
    .await?;
```

- hash 变更按全表复制并逐行按新拓扑路由；range 变更按区间搬移。
- 回填/校验/切流前会检查源表结构（MySQL/PostgreSQL 用 `information_schema`，SQLite 用
  `pragma_table_info`）；BLOB/BYTEA 等二进制列直接报错，绝不把 base64 文本写进目标表。
- hash 重分片期间用 `DualWrite`：切流前 `before_cutover(&old_router, &new_router)`（旧为主、
  新为镜像），切流后 `after_cutover(&new_router, &old_router)`（新为主、旧为镜像，便于
  `rollback`）。两侧同名的分片只写一次；镜像写失败返回 `dual-write mirror failed` 错误。
- 状态表 `shard_migration_jobs` 记录 phase/cursor/rows/checksum；迁移可用 `FencedLock` 保证单执行。
- 回滚后再次迁移会重新进入回填/校验流程。
- B 阶段直接使用 ShardingSphere-Scaling；本执行器覆盖 A 阶段。

## 2. 冷热分层

### 2.1 配置

```yaml
tiering:
  enabled: true
  hot_days: 180
  partition_months: 1
  batch_size: 1000
  verify_sample: 0.01
  tables:
    - name: events
      time_col: ts
      id_col: id
      retention_days: 365
      target: clickhouse   # 或 s3
```

热层本期为 MySQL（`RANGE COLUMNS` 分区）；冷层为 ClickHouse（可查询）或 S3/MinIO
（恢复/审计）。ClickHouse 与 S3 的 sink 都实现 `ColdSink`：

```rust
pub trait ColdSink {
    async fn write_chunk(&self, table: &str, partition: &str, chunk: u64, rows: &[Row]) -> Result<String, TieringError>;
    async fn verify_chunk(&self, table: &str, partition: &str, chunk: u64, checksum: &str) -> Result<bool, TieringError>;
    async fn health_check(&self) -> Result<(), TieringError>;
}
```

### 2.2 跨边界查询

```rust
let query = TieredQuery::new(hot_db, cold_query, vec![TieredTable {
    name: "events".into(),
    time_col: "ts".into(),
    id_col: "id".into(),
    hot_days: 180,
}]);
let page = query.query_range("events", from, to, &[], 100, 0).await?;
```

全热只查 MySQL，全冷只查冷层；跨边界两侧各下推 `limit + offset`，按主键合并去重再分页。
ClickHouse 冷读适配器为 `ClickhouseColdQuery`；`ClickhouseClient` 无参数绑定，因此只接受
由表元数据与时间区间构造的 SQL，非空 `params` 直接拒绝（不静默忽略）。

### 2.3 CLI 与 readiness

```bash
# 配置文件需包含顶层 database: 与 tiering:
ecat-tier plan   --config app.yaml
ecat-tier run    --config app.yaml --dry-run
ecat-tier verify --config app.yaml
# restore 保持显式：先把冷数据装入 staging，再调用 prepare_staging + restore_partition
ecat-tier restore --config app.yaml
```

`run` 与 `verify` 需要 `cli` feature；冷后端连接通过环境变量提供：
`ECAT_CLICKHOUSE_URL` / `ECAT_CLICKHOUSE_DATABASE`，
`ECAT_S3_ENDPOINT` / `ECAT_S3_REGION` / `ECAT_S3_ACCESS_KEY` /
`ECAT_S3_SECRET_KEY` / `ECAT_S3_BUCKET` / `ECAT_S3_PREFIX`。

`TieringHealthCheck::with_archive_lag(store, max_lag)` 在冷层可达性之外检查状态表最新
`updated_at`；没有状态行视为尚无归档历史。`probe_timeout` 只约束冷层探针。

## 3. 分布式原语

### 3.1 Fencing lock

```rust
let guard = lock.acquire_fenced("migration:orders", ttl).await?
    .ok_or_else(|| LockError::Other("另一个执行者持有锁".into()))?;
// 资源侧记录已见的最大 fence，拒绝更小的写入
assert!(lock.validate_fence("migration:orders", guard.fence).await?);
lock.release("migration:orders", &guard.token).await?;
```

获取失败也会递增 fence（保持单调）；因此迁移的排他性依赖 `renew` 的 token 校验，
`validate_fence` 用于资源侧的 max-seen 检查，而不是“当前是否仍持锁”的唯一判据。

### 3.2 Saga

```rust
let saga = Saga::new(vec![step_a, step_b, step_c], store);
saga.run("order-42", ctx).await?;       // 失败：逆序补偿；补偿失败 -> CompensationFailed
saga.resume("order-42").await?;         // 从持久化 current_step 续跑
```

步骤/补偿必须幂等；`SagaContext` 带 `idempotency_key`；状态存 `saga_instances` /
`saga_steps`。`Failed` 状态不自动重试，需要人工处理。

### 3.3 opt-in XA 2PC（仅 MySQL/PostgreSQL）

```rust
let coordinator = TwoPhaseCoordinator::new(coordinator_db, participants)
    .with_fence(lock, "2pc:recovery", Duration::from_secs(60));
let tx = coordinator.begin(&[("orders", orders_xa), ("stock", stock_xa)]).await?;
tx.execute("orders", "INSERT INTO orders ...", &[json!(42)]).await?;
tx.commit().await?;                      // END -> PREPARE all -> decision -> COMMIT all
```

- 协调日志 `distributed_tx(id, status, participants, decision, created_at)`。
- 任一 prepare 失败全部回滚；恢复扫描 `prepared` / `committing` / `rolling_back`。
- XA 生命周期固定在一条物理连接上；协调器数据库是单点，生产需外部 HA。
- 不把 2PC 接进 `KeyedClient` / `scatter`，只作为显式 API。

### 3.4 共享 Redis 池与远程拓扑

```rust
let cfg: RedisPoolConfig = serde_json::from_value(json!({
    "url": "redis://localhost:6379",
    "max_size": 16
}))?;
let pool = RedisPool::connect(cfg).await?;
let cache = pool.cache();   // 与 lock 共享 Arc<Pool>
let lock = pool.lock();
```

旧 `connect` / `from_config` / `from_connection` 仍走单条多路复用连接。
远程拓扑用 `ConsulTopologySource` / `EtcdTopologySource` 的 `load/watch`；启动无拓扑
fail-closed，后续坏 JSON 保留最后已知拓扑。

### 3.5 后台 worker

```rust
App::builder()
    .background_worker(|mut shutdown| async move {
        archive_job.run_until(&mut shutdown).await.ok();
    })
    .build()?
    .run().await?;
```

`on_start` 启动任务，`on_stop` 先发停机信号再等待退出；worker future 自己负责单次运行预算。

## 4. MySQL 8.4 / 9 与兼容性

- 复制状态用 `SHOW REPLICA STATUS`；`SHOW BINARY LOG STATUS` 取代 8.0 的 `SHOW MASTER STATUS`。
- 认证仅使用 `caching_sha2_password`；9.x 已移除 `mysql_native_password`，不得用它绕过连接失败。
- `RANGE COLUMNS` 分区语法未变；首次分区转换的算法/锁级别需在 8.4/9 实测。
- 分片代理认证、TLS、Helm/compose、真实 XA/冷层/备份演练仍属于 live gate，未在离线测试中声称通过。

## 5. 已知限制

- 迁移执行器只搬移标量列：回填/校验/切流前发现 BLOB/BYTEA 等二进制列即拒绝。SQLite
  只能按声明类型判断，声明为非 BLOB affinity 的列里若实际存了 blob 值仍可能漏检。
- `DualWrite` 只镜像单条写语句；主写与镜像写不在同一事务内（镜像失败会报错，调用方需重试），
  `transaction()` 只在主拓扑执行，跨库事务内的双写需调用方自行拆分。
- `rollback` 只恢复路由并保留旧分片，不做反向数据搬移。
- 2PC 协调器数据库单点；恢复未用 participant `xa_recover` 交叉核对。
- `ecat-tier` 冷后端通过环境变量配置；当前没有 ClickHouse 认证环境变量。
- readiness 的归档滞后状态查询不受 `probe_timeout` 约束。

## 相关文档

- [设计文档](superpowers/specs/2026-10-24-data-layer-scaleout-design.md)
- [实现计划](superpowers/plans/2026-10-24-data-layer-scaleout.md)
- [API 参考](api.md)
- [数据库配置教程](database-config-tutorial.md)
