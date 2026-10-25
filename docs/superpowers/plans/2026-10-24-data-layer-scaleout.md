# 数据层横向扩展实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在不破坏现有 API、配置和四方言行为的前提下，为 e-cat 数据层加入单机基线、分布式原语、冷热分层与分片能力，并覆盖 MySQL 8.4 LTS / 9.x 的认证、复制与分区差异。

**Architecture:** P0 测量门禁 → P1 单机/分布式原语 → P2 冷热分层 → P3 代理分片 → P4 客户端分片 → P5 outbox → P6 单机/分布式运行基座 → P7 高级分布式事务与迁移。所有新能力放独立 crate 或默认关闭的 feature；分片不解析 SQL，按显式分片键选连接；2PC/Saga 默认关。

**Tech Stack:** Rust 2024、tokio、async-trait、serde、sqlx、ClickHouse HTTP、S3 兼容存储、ShardingSphere-Proxy 5.x。

**Spec:** `docs/superpowers/specs/2026-10-24-data-layer-scaleout-design.md`

## Global Constraints

- 兼容只增不改：不改现有公开签名、pub struct 字段、枚举变体、SQL 模板、默认值。
- 新 feature 默认关闭、不进入 default；新错误类型映射到 `RdbmsError::Database` / `Config`。
- 不解析/改写 SQL；分片键必须显式传入，缺失即报错。
- 默认跨分片用 outbox + Saga + 幂等补偿；2PC/XA 仅作为 opt-in 能力（仅 MySQL/PostgreSQL），不作为默认路径。
- 新 crate 加入根 `Cargo.toml` 的 `members`；`workspace.dependencies` 是半填表，没有的依赖直接写 `path`（见项目记忆）。
- 每个任务结束跑：`cargo fmt -p <crate> -- --check`、`cargo clippy -p <crate> --all-targets -- -D warnings`、`cargo test -p <crate>`。
- 不跑 `cargo fmt --all`（并行工作区会排版别人半成品）。
- 提交用 `git commit --only <本任务路径>`，不要无路径 `git commit`（共享 index 会捎带别人的暂存文件）。
- 文件 <500 行；测试放 `src/<module>/tests.rs` 或 `tests/`。
- 新 crate 的 tracing span 带 `shard` / `mode` / `table` 字段；指标用 `ecat-metrics` 现有 collector，标签 `shard` / `mode` 保持有界，不新增高基数标签。
- P0 门禁未通过时，不启动 P2/P3/P4 的任何实现任务。
- MySQL 目标版本：8.4 LTS 与 9.x（8.0 仅 legacy）。8.4 起 `SHOW SLAVE STATUS` / `SHOW MASTER STATUS` 已移除，统一 `SHOW REPLICA STATUS` / `SHOW BINARY LOG STATUS`。
- 9.x 不得使用 `mysql_native_password`；认证只走 `caching_sha2_password`，TLS 与非 TLS 都要测。
- gh-ost 固定 ≥1.1.8（8.4）；9.x 先验证，未验证前用原生在线 DDL 或 pt-online-schema-change。
- B 阶段在 9.x 上以代理 `caching_sha2_password` 兼容性为门禁；过不了走 A 阶段。
- P7 的远程拓扑/迁移/Saga/2PC 默认关闭或 opt-in；2PC 仅 MySQL/PostgreSQL，不复用 `KeyedClient`/`scatter`。
- Redis cache 与 lock 共享 `RedisPool`；旧 `connect` / `from_config` 单连接路径必须保持可用。
- ClickHouse 与 S3 的在线用例分别用 `ECAT_TEST_CLICKHOUSE_URL`、`ECAT_TEST_S3_*` 门控；未设置即跳过，`ECAT_REQUIRE_LIVE_DB=1` 时跳过即失败。

## Review Focus

- 分片键缺失/非法：必须报错，不得默认落某个分片、不得静默扫全部分片。
- 跨分片查询部分失败：必须返回错误或显式标记，不得把失败分片当空结果。
- 冷热边界重复/遗漏：合并必须按主键去重，边界行只出现一次。
- 标识符注入：分区名、表名、ClickHouse 表名必须白名单校验，不能拼接用户输入。
- feature 全关：SQL、`Row` 值、错误类型必须与基线逐字节一致。
- MySQL 8.4/9：不得出现 `SHOW SLAVE STATUS` / `SHOW MASTER STATUS` / `mysql_native_password`；代理认证门禁未过时不得宣布 B 阶段支持 9.x。

---

## Execution Overview

### 里程碑

| 里程碑 | 包含任务 | 退出条件 |
|---|---|---|
| **M0 基线门禁** | 1 | 基线报告 + 每表 `tune/partition/archive/shard/leave` 处置；未过门禁不启动 P2/P3 |
| **M1 基座可用** | 2–7, 25, 37 | 兼容门禁全绿；MySQL 8.4/9 认证矩阵通过；部署模式校验可用；Redis 共享池可用（可选） |
| **M2 冷热分层** | 8–15, 26 | 归档 dry-run + 校验和 100%；优雅停机；feature-off 不变 |
| **M3 分片与运行基座** | 16–31 | 代理认证门禁通过；分片迁移演练；checksum 校验；池预算；outbox relay；拓扑 reload；健康检查；standalone/distributed 对等 |
| **M4 高级能力** | 32–36 | fencing、远程拓扑、A 阶段迁移执行器、Saga、opt-in 2PC 各自验收通过 |
| **M5 收口** | 全量 | 全量兼容矩阵、备份恢复演练、故障注入、文档与 CHANGELOG 更新 |

### 依赖与并行

| 任务 | 依赖 | 说明 |
|---|---|---|
| 1 | 无 | P0 门禁；先做 |
| 2, 3 | 无 | 可并行 |
| 4 → 5 | 2 | 同一文件，顺序做 |
| 6 | 4, 5 | 使用 `max_replica_lag` / `primary_only` |
| 7 | 2–6 | 兼容与 MySQL 8.4/9 矩阵 |
| 8 → 9 → 10 | 8 → 9 → 10 | tiering 基础链 |
| 11, 12 | 10 | CH/S3 两个 sink 可并行 |
| 13 | 8, 10 | 跨边界查询 |
| 14, 15 | 8 | 可与 9–13 并行 |
| 16 | 无 | 分片目录 |
| 17, 18, 20, 21 | 16 | 可并行（18 是 19 的前置） |
| 19 | 18 | scatter |
| 22 | 16–21 | 聚合 feature |
| 23 | 无 | 迁移校验脚本，可与 16–22 并行 |
| 24 | 17 | 代理认证门禁 |
| 25 | 无 | 部署模式校验 |
| 26 | 10 | 给 `ArchiveJob` 加 `run_until` |
| 27 | 16/18, 10/11, 21 | 分片/冷层/outbox 健康检查 |
| 28 | 2/4, 16/18 | 池预算 |
| 29 | 21, 26 | outbox relay |
| 30 | 25, 26, 27 | 部署清单与对等测试 |
| 31 | 18 | 拓扑 reload |
| 32 | 3 | fencing |
| 33 | 31 | 远程拓扑源 |
| 34 | 23, 31 | 分片迁移执行器 |
| 35 | 21, 26 | Saga |
| 36 | 2/4, 32 | 2PC |
| 37 | 3 | Redis 共享池 |

### 并行轨道

- **Track A（数据层）**：2–15；P0 通过后先做 2–7，再做 8–15。
- **Track B（分片）**：16–24、31；依赖 Track A 的 2–7，不依赖 tiering。
- **Track C（运行基座/部署）**：25–30、37；25/26/37 可与 Track A 并行，27/28/29/30 随依赖推进。
- **Track D（高级）**：32–36；32/37 可早做，33–36 按依赖后置。
- **横切**：MySQL 8.4/9 门禁（1、4、7、24）和兼容门禁（7）在每个里程碑重跑。

### 相对工作量

- **S（≤ 0.5 天等效）**：2、3、5、7、14、15、17、20、22、23、25、28、32、37。
- **M（1–2 天等效）**：1、4、6、8、9、11、12、13、16、18、19、21、24、26、29、30、31、33、35。
- **L（3–5 天等效）**：10、27、34、36。
- 工作量为相对估算，不含真实 MySQL/Proxy/CH 环境的联调与迁移窗口。

### 执行规则

- 每个任务按 TDD 步骤：先写失败测试，再实现，再跑通过，最后 `git commit --only`。
- 每个里程碑结束跑全量 `cargo test --workspace`、`cargo check --workspace --no-default-features`、`cargo clippy`、兼容冒烟测试；更新 CHANGELOG。
- P0 门禁、MySQL 8.4/9 认证门禁、代理认证门禁、2PC opt-in 门禁是硬门禁；未过不得宣布对应阶段完成。
- 所有新 feature 默认关闭；里程碑验收必须同时验证 feature-off 与 feature-on。
- 真实环境门禁用 `ECAT_REQUIRE_LIVE_DB=1` 强制；本地未设 env 时允许 skip，但里程碑验收不允许 skip。

---

## Phase P0 — 测量门禁

### Task 1: P0 基线采集脚本

**Files:**
- Create: `scripts/data-layer/p0-inventory.sql`
- Create: `scripts/data-layer/p0-inventory.sh`
- Test: `scripts/data-layer/p0-inventory.sh --check`

**Interfaces:**
- `p0-inventory.sh` 读 `MYSQL_DSN`（`mysql://user:pass@host:3306/db`），用 `mysql --batch --raw` 执行 SQL，输出八个段：`-- SECTION: VERSION`、`AUTH_PLUGINS`、`TABLES`、`INDEXES`、`SLOW_QUERIES`、`LOCKS`、`REPLICATION`、`PARTITIONS`。
- `VERSION` 用 `SELECT VERSION()`；`AUTH_PLUGINS` 用 `SELECT user, host, plugin FROM mysql.user`（无权限时记录 `unknown` 并继续）；`REPLICATION` 用 `SHOW REPLICA STATUS`（8.4/9 不再使用 `SHOW SLAVE STATUS`）。
- `--check`：校验 SQL 文件包含八个段标记且脚本可执行，成功打印 `p0-inventory: ok`，失败退出 1。
- `--dry-run`：只打印将要执行的 SQL。

- [ ] **Step 1: 写失败检查**

Run: `bash scripts/data-layer/p0-inventory.sh --check`
Expected: FAIL，`No such file or directory`。

- [ ] **Step 2: 写 SQL 骨架**

八个 `-- SECTION:` 标记；VERSION 段 `SELECT VERSION()`；AUTH_PLUGINS 段查 `mysql.user` 的 `plugin`（9.x 若出现 `mysql_native_password` 必须迁移）；REPLICATION 段只发 `SHOW REPLICA STATUS`；TABLES 段查询 `information_schema.TABLES` 的 `TABLE_NAME/TABLE_ROWS/DATA_LENGTH/INDEX_LENGTH`；INDEXES 段查询 `information_schema.STATISTICS` 的索引列与唯一性；SLOW_QUERIES 段查询 `performance_schema.events_statements_summary_by_digest` 按 `SUM_TIMER_WAIT` 降序取 20 条；LOCKS 段查询 `performance_schema.data_lock_waits`；PARTITIONS 段查询 `information_schema.PARTITIONS` 的分区名与边界。

- [ ] **Step 3: 实现脚本**

```bash
#!/usr/bin/env bash
set -euo pipefail
SQL="$(dirname "$0")/p0-inventory.sql"
for s in VERSION AUTH_PLUGINS TABLES INDEXES SLOW_QUERIES LOCKS REPLICATION PARTITIONS; do
  grep -q -- "-- SECTION: $s" "$SQL" || { echo "missing section: $s" >&2; exit 1; }
done
[[ "${1:-}" == "--check" ]] && { echo "p0-inventory: ok"; exit 0; }
[[ "${1:-}" == "--dry-run" ]] && { cat "$SQL"; exit 0; }
: "${MYSQL_DSN:?set MYSQL_DSN}"
mysql --batch --raw --execute="$(cat "$SQL")" "$MYSQL_DSN"
```

- [ ] **Step 4: 验证**

Run: `bash scripts/data-layer/p0-inventory.sh --check`
Expected: `p0-inventory: ok`。
Run: `bash scripts/data-layer/p0-inventory.sh --dry-run | grep -c -- '-- SECTION:'`
Expected: `8`。
Run: `bash scripts/data-layer/p0-inventory.sh --dry-run | grep -c 'SHOW SLAVE STATUS'`
Expected: `0`。
Run: `bash scripts/data-layer/p0-inventory.sh --dry-run | grep -c 'SHOW REPLICA STATUS'`
Expected: `1`。

- [ ] **Step 5: 提交**

```bash
git add scripts/data-layer/p0-inventory.sql scripts/data-layer/p0-inventory.sh
git commit --only scripts/data-layer/p0-inventory.sql scripts/data-layer/p0-inventory.sh -m "chore(data): add P0 baseline inventory script"
```

> P0 的实际执行与每表处置表是人工门禁：拿到表结构 + 慢查询样本后，产出 `tune/partition/archive/shard/leave` 决策，否则不进入 P2/P3/P4。
> MySQL 8.4/9：`SHOW SLAVE STATUS` / `SHOW MASTER STATUS` 已移除，脚本只使用新语句；8.0 <8.0.22 的老库需单独用旧语句，不在主路径。

---

## Phase P1 — 单机基线与分布式原语

### Task 2: `Row` 只读访问器

**Files:**
- Modify: `ecat-data/src/rdbms.rs:6-28`
- Test: `ecat-data/src/rdbms/tests.rs`

**Interfaces:**
- Consumes: 现有 `Row::new(columns, values)`、`Row::get(&str)`。
- Produces:
  ```rust
  impl Row {
      pub fn columns(&self) -> &[String];
      pub fn values(&self) -> &[serde_json::Value];
      pub fn iter(&self) -> impl Iterator<Item = (&str, &serde_json::Value)>;
  }
  ```

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn row_accessors_preserve_column_order() {
    let row = Row::new(
        vec!["id".into(), "name".into()],
        vec![serde_json::json!(1), serde_json::json!("a")],
    );
    assert_eq!(row.columns(), ["id", "name"]);
    assert_eq!(row.values(), [serde_json::json!(1), serde_json::json!("a")]);
    let pairs: Vec<_> = row.iter().map(|(c, v)| (c, v.clone())).collect();
    assert_eq!(pairs, [("id", serde_json::json!(1)), ("name", serde_json::json!("a"))]);
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data row_accessors_preserve_column_order`
Expected: FAIL，`no method named columns`。

- [ ] **Step 3: 实现**

在 `impl Row` 中加三个方法；`iter` 用 `self.columns.iter().map(String::as_str).zip(self.values.iter())`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data row_accessors_preserve_column_order`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data/src/rdbms.rs ecat-data/src/rdbms/tests.rs
git commit --only ecat-data/src/rdbms.rs ecat-data/src/rdbms/tests.rs -m "feat(data): add read-only Row accessors"
```

### Task 3: 分布式锁续租与超时

**Files:**
- Modify: `ecat-lock/src/lib.rs:10-22`
- Modify: `ecat-data-redis/src/lib.rs:271-350`
- Test: `ecat-lock/src/lib.rs`（内联测试）、`ecat-data-redis/src/tests.rs`

**Interfaces:**
- Consumes: `DistributedLock::{acquire, release}`。
- Produces:
  ```rust
  async fn renew(&self, key: &str, token: &str, ttl: Duration) -> Result<(), LockError>
  // 默认实现：Err(LockError::Other("renew not supported by this backend".into()))
  ```
  `RedisLock::renew`：Lua `if redis.call("get", KEYS[1]) == ARGV[1] then return redis.call("pexpire", KEYS[1], ARGV[2]) else return 0 end`；返回 0 时报 `LockError::Other("renew: token mismatch or lock not held")`。
  `RedisLock` 增加私有 `query_timeout: Option<Duration>`：`connect` 默认 30s，`from_config` 读 `RedisConfig.query_timeout_secs`（`0` = 禁用）；`acquire`/`release`/`renew` 都包 `tokio::time::timeout`。

- [ ] **Step 1: 写失败测试**

`ecat-lock` 内联：`default_renew_is_not_supported` 用现有 `MemoryLock`，断言 `renew(...).await.unwrap_err().to_string().contains("renew not supported")`。
`ecat-data-redis/src/tests.rs`：`renew_script_compares_token_and_pexpires` 断言 Lua 字符串同时包含 `redis.call("get"`、`redis.call("pexpire"`、`ARGV[2]`；live 用例 `renew_extends_ttl` 用 `ECAT_TEST_REDIS_URL` 门控；`redis_lock_from_config_reads_query_timeout` 断言配置 `query_timeout_secs: 5` 后内部超时为 5s，`0` 为 `None`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-lock default_renew_is_not_supported`
Expected: FAIL，`no method named renew`。

- [ ] **Step 3: 实现**

在 trait 加默认方法；`RedisLock` 用 `redis::Script` 实现 renew；`acquire`/`release` 原有语义不变，只包超时。Redis cache+lock 共享池在 Task 37 单独实现，本期先明确单连接上限。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-lock && cargo test -p ecat-data-redis renew_`
Expected: PASS（live 用例未设 env 时 skip）。

- [ ] **Step 5: 提交**

```bash
git add ecat-lock/src/lib.rs ecat-data-redis/src/lib.rs ecat-data-redis/src/tests.rs
git commit --only ecat-lock/src/lib.rs ecat-data-redis/src/lib.rs ecat-data-redis/src/tests.rs -m "feat(lock): add renew with Redis token check"
```

### Task 4: 副本延迟探针与跳过

**Files:**
- Modify: `ecat-data/src/rdbms.rs:255-258`
- Modify: `ecat-data/src/routing.rs`
- Modify: `ecat-data-sqlx/src/lib.rs`（`impl RdbmsClient for SqlxClient`）
- Test: `ecat-data/src/routing/tests.rs`、`ecat-data-sqlx/src/tests.rs`（解析函数单测）

**Interfaces:**
- Consumes: `RdbmsRouting::{new, with_breaker_config, fallback_to_primary}`。
- Produces:
  ```rust
  #[async_trait]
  pub trait RdbmsClient: SqlExecutor {
      async fn replica_lag(&self) -> Option<Duration> { None }
  }
  impl RdbmsRouting {
      pub fn max_replica_lag(mut self, lag: Duration) -> Self;
  }
  ```
  私有 `parse_mysql_replica_lag(rows: &[Row]) -> Option<Duration>`：读 `Seconds_Behind_Source`（8.4/9），回退 `Seconds_Behind_Master`（8.0 旧语句），NULL/非数字返回 `None`。
  `pub(crate) fn mysql_replica_status_sql(version: &str) -> &'static str`：≥8.0.22 → `SHOW REPLICA STATUS`；更老 → `SHOW SLAVE STATUS`。

- [ ] **Step 1: 写失败测试**

`ecat-data/src/routing/tests.rs` 给 `FakeClient` 加 `lag: Option<Duration>`：
- `replica_with_lag_above_threshold_is_skipped`：副本 lag 6s、阈值 5s，连续 8 次 `query` 全部落主库。
- `replica_with_lag_below_threshold_is_used`：lag 1s，读落副本。
- `replica_without_lag_probe_keeps_existing_behavior`：`None` 时行为与现有一致。
`ecat-data-sqlx/src/tests.rs`：
- `mysql_lag_parser_reads_seconds_behind_source`：构造 `Row::new(vec!["Seconds_Behind_Source"], vec![json!(3)])`，断言 `Some(3s)`。
- `mysql_lag_parser_returns_none_for_null_or_missing`。
- `mysql_replica_status_sql_uses_replica_on_84_and_9`：`8.4.0` / `9.0.0` 都返回 `SHOW REPLICA STATUS`。
- `mysql_replica_status_sql_uses_slave_only_before_8022`：`8.0.21` 返回 `SHOW SLAVE STATUS`，`8.0.22` 返回 `SHOW REPLICA STATUS`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data replica_with_lag_above_threshold_is_skipped`
Expected: FAIL，`no method named max_replica_lag`。

- [ ] **Step 3: 实现**

`RdbmsRouting` 增加私有字段 `max_replica_lag: Option<Duration>`；`new`/`with_breaker_config` 置 `None`；`pick_replica` 改为 `async`，逐个 `is_available().await`；`Endpoint::is_available` 先看熔断状态，再 `client.replica_lag().await` 与阈值比较。`SqlxClient::replica_lag` 只对 `Dialect::MySql` 执行：先 `SELECT VERSION()`（每 client 缓存），按 `mysql_replica_status_sql` 选语句；8.4/9 绝不再发 `SHOW SLAVE STATUS`；其余方言返回 `None`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data routing && cargo test -p ecat-data-sqlx mysql_lag_parser && cargo test -p ecat-data-sqlx mysql_replica_status_sql`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data/src/rdbms.rs ecat-data/src/routing.rs ecat-data/src/routing/tests.rs ecat-data-sqlx/src/lib.rs ecat-data-sqlx/src/tests.rs
git commit --only ecat-data/src/rdbms.rs ecat-data/src/routing.rs ecat-data/src/routing/tests.rs ecat-data-sqlx/src/lib.rs ecat-data-sqlx/src/tests.rs -m "feat(data): skip replicas above lag threshold"
```

### Task 5: 读己之写 `primary_only`

**Files:**
- Modify: `ecat-data/src/routing.rs`
- Test: `ecat-data/src/routing/tests.rs`

**Interfaces:**
- Produces:
  ```rust
  impl RdbmsRouting {
      pub fn primary_only(&self) -> PrimaryOnly<'_>;
  }
  pub struct PrimaryOnly<'a> { /* 私有：&'a RdbmsRouting */ }
  ```
  `PrimaryOnly` 实现 `SqlExecutor` 的六个方法和 `RdbmsClient::transaction`，全部委托主库；不经过副本轮询，事务仍不经熔断。

- [ ] **Step 1: 写失败测试**

`primary_only_routes_all_reads_to_primary`：主库 + 2 副本，分别调 `query/query_with/query_write/execute_then_query`，断言日志只有 `primary:` 前缀。
`primary_only_transaction_uses_primary`：断言 `transaction` 记在主库。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data primary_only_routes_all_reads_to_primary`
Expected: FAIL，`no method named primary_only`。

- [ ] **Step 3: 实现**

`PrimaryOnly<'a> { router: &'a RdbmsRouting }`；六个方法委托 `router.primary.*`；`transaction` 委托 `router.primary.client.transaction()`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data primary_only`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data/src/routing.rs ecat-data/src/routing/tests.rs
git commit --only ecat-data/src/routing.rs ecat-data/src/routing/tests.rs -m "feat(data): add primary-only read-your-writes handle"
```

### Task 6: `SqlxRoutingConfig` 构建器

**Files:**
- Create: `ecat-data-sqlx/src/routing.rs`
- Create: `ecat-data-sqlx/src/routing/tests.rs`
- Modify: `ecat-data-sqlx/src/lib.rs`（`mod routing; pub use routing::{SqlxRoutingConfig, build_routing};`）

**Interfaces:**
- Consumes: `SqlxConfig`、`SqlxClient::from_config`、`RdbmsRouting`。
- Produces:
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  #[serde(deny_unknown_fields)]
  pub struct SqlxRoutingConfig {
      pub primary: SqlxConfig,
      #[serde(default)] pub replicas: Vec<SqlxConfig>,
      #[serde(default)] pub max_replica_lag_secs: Option<u64>,
      #[serde(default = "default_true")] pub fallback_to_primary: bool,
  }
  pub async fn build_routing(cfg: &SqlxRoutingConfig) -> Result<RdbmsRouting, sqlx::Error>;
  ```

- [ ] **Step 1: 写失败测试**

`routing_config_rejects_unknown_key`：JSON 加 `"replica": []`，断言反序列化失败。
`routing_config_defaults_fallback_to_primary`：省略时 `true`。
`build_routing_without_replicas_reads_primary`：用 `sqlite::memory:` 主库，`build_routing` 后 `query("SELECT 1")` 成功。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sqlx routing_config_rejects_unknown_key`
Expected: FAIL，`cannot find type SqlxRoutingConfig`。

- [ ] **Step 3: 实现**

`build_routing` 逐个 `SqlxClient::from_config`；主库构造 `RdbmsRouting::new`；若配置了 `max_replica_lag_secs` 调 `.max_replica_lag(Duration::from_secs(n))`；调 `.fallback_to_primary(cfg.fallback_to_primary)`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sqlx routing_config`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-sqlx/src/routing.rs ecat-data-sqlx/src/routing/tests.rs ecat-data-sqlx/src/lib.rs
git commit --only ecat-data-sqlx/src/routing.rs ecat-data-sqlx/src/routing/tests.rs ecat-data-sqlx/src/lib.rs -m "feat(data): add Sqlx read-write routing config"
```

### Task 7: 兼容性 CI 门

**Files:**
- Modify: `.github/workflows/ci.yml:1-14`
- Create: `ecat-data/tests/public_api_smoke.rs`

**Interfaces:**
- 新测试只引用现有公开 API，不执行 I/O：`Row::new/get`、`SqlExecutor` 的方法签名、`RdbmsClient` 的 `transaction`、`Cache` 的默认方法、`Dialect` 的五个变体。
- CI 增加：`cargo check --workspace --no-default-features`、`cargo test -p ecat-data --test public_api_smoke`、每个新 feature 的 `cargo check -p <crate> --features <feature>`（在 Task 8+ 后追加）、MySQL 8.4/9 认证矩阵。

- [ ] **Step 1: 写编译期冒烟测试**

```rust
use ecat_data::{Cache, Dialect, RdbmsClient, Row, SqlExecutor};

fn _assert_row(row: &Row) {
    let _: Option<&serde_json::Value> = row.get("id");
}

fn _assert_executor<E: SqlExecutor>(_: &E) {}
fn _assert_client<C: RdbmsClient>(_: &C) {}
fn _assert_cache<C: Cache>(_: &C) {}

#[test]
fn public_api_compiles() {
    let _ = Dialect::Standard;
    let _ = Dialect::Sqlite;
    let _ = Dialect::Postgres;
    let _ = Dialect::MySql;
    let _ = Dialect::Mssql;
}
```

- [ ] **Step 2: 运行确认通过**

Run: `cargo test -p ecat-data --test public_api_smoke`
Expected: PASS。若现有签名已变，编译失败即是兼容性红灯。

- [ ] **Step 3: 改 CI**

在 `cargo test --workspace` 后加 `cargo check --workspace --no-default-features` 和 `cargo test -p ecat-data --test public_api_smoke`；保留现有 `fmt` / `clippy` / `audit`。`cargo semver-checks` 需要 7.1.1 baseline tag；仓库有 tag 后追加，当前以 `public_api_smoke` 作为最低兼容门。

- [ ] **Step 4: MySQL 8.4/9 认证与版本矩阵**

新增 CI job `mysql-matrix`，`mysql-version: ["8.4", "9"]`：

- 用 `docker run mysql:${version}` 启动临时实例，初始化 `ecat` 用户 `IDENTIFIED WITH caching_sha2_password`。
- 设 `ECAT_TEST_MYSQL_URL`、`ECAT_REQUIRE_LIVE_DB=1`，跑 `cargo test -p ecat-data-sqlx`。
- 断言：连接成功；`SHOW REPLICA STATUS` 不报错（单机返回空）；SQL 日志中无 `SHOW SLAVE STATUS`。
- TLS 与非 TLS 各跑一次；非 TLS 若因 `caching_sha2_password` RSA 公钥失败，记录已知限制并强制 TLS，**不得用 `mysql_native_password` 绕过**。
- 记录连接建立耗时；有报告称 sqlx 0.8.6 的 `caching_sha2_password` fast-auth 路径异常（#4244）；用项目实际版本复现，若每次连接走 RSA 全认证，升级/打补丁或接受成本并在 P1 报告中写明。
- TLS 走 URL 参数（如 `?ssl-mode=REQUIRED`）；`SqlxConfig.tls` 字段仍不支持，配了会报错。
- 若连接失败且定位到 sqlx 协议/认证缺陷，升级 sqlx 到支持 8.4/9 的版本；升级后必须重跑 Task 7 的完整兼容矩阵。

- [ ] **Step 5: 提交**

```bash
git add .github/workflows/ci.yml ecat-data/tests/public_api_smoke.rs
git commit --only .github/workflows/ci.yml ecat-data/tests/public_api_smoke.rs -m "ci: add no-default-features and public API compatibility gates"
```

---

## Phase P2 — 冷热分层（P0 门禁通过后启动）

### Task 8: `ecat-data-tiering` 配置与校验

**Files:**
- Create: `ecat-data-tiering/Cargo.toml`
- Create: `ecat-data-tiering/src/lib.rs`
- Create: `ecat-data-tiering/src/config.rs`
- Create: `ecat-data-tiering/src/config/tests.rs`
- Modify: `Cargo.toml`（workspace members 追加 `ecat-data-tiering`）

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  #[serde(deny_unknown_fields)]
  pub struct TieringConfig {
      #[serde(default)] pub enabled: bool,
      #[serde(default = "default_hot_days")] pub hot_days: u32,
      #[serde(default = "default_partition_months")] pub partition_months: u32,
      #[serde(default)] pub tables: Vec<TableTieringConfig>,
      #[serde(default = "default_batch_size")] pub batch_size: u32,
      #[serde(default)] pub max_rows_per_sec: Option<u32>,
      #[serde(default)] pub verify_sample: Option<f32>,
      #[serde(default = "default_retain_s3_days")] pub retain_s3_days: u32,
      #[serde(default)] pub dry_run: bool,
  }
  pub struct TableTieringConfig { pub name: String, pub time_col: String, pub id_col: String, pub retention_days: u32, pub target: ColdTarget, #[serde(default)] pub filter: Option<String> }
  pub enum ColdTarget { Clickhouse, S3 }
  pub enum TieringError { Config(String), Db(ecat_data::RdbmsError), Cold(String), State(String), UnsupportedDialect }
  impl From<ecat_data::RdbmsError> for TieringError { /* Db */ }
  pub fn validate_ident(s: &str) -> Result<(), TieringError>; // ^[A-Za-z_][A-Za-z0-9_]*$
  impl TieringConfig { pub fn validate(&self) -> Result<(), TieringError>; }
  ```

- [ ] **Step 1: 写失败测试**

`config_defaults_are_off_and_180_days`；`config_rejects_unknown_key`；`config_rejects_zero_hot_days`；`config_rejects_empty_table_name_or_columns`；`config_accepts_clickhouse_and_s3_targets`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-tiering config_defaults_are_off_and_180_days`
Expected: FAIL，crate 不存在。

- [ ] **Step 3: 建 crate 并实现**

`Cargo.toml` 依赖 `ecat-data`、`ecat-errors`、`serde`、`serde_json`、`async-trait`、`time`；`validate` 检查 `hot_days > 0`、`partition_months > 0`、表名/列名非空且匹配 `^[A-Za-z_][A-Za-z0-9_]*$`、`retention_days >= hot_days`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-tiering config_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml ecat-data-tiering
git commit --only Cargo.toml ecat-data-tiering -m "feat(tiering): add config and validation"
```

### Task 9: 归档状态表与状态机

**Files:**
- Create: `ecat-data-tiering/src/state.rs`
- Create: `ecat-data-tiering/src/state/tests.rs`
- Modify: `ecat-data-tiering/src/lib.rs`

**Interfaces:**
- Consumes: `SqlExecutor::{execute, query_with}`、`Row` 访问器。
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum ArchivePhase { Pending, Copied, Verified, Dropped }
  pub struct ArchiveJobState { pub table: String, pub partition: String, pub chunk: u64, pub low_id: i64, pub high_id: i64, pub rows: u64, pub checksum: Option<String>, pub phase: ArchivePhase, pub updated_at: time::OffsetDateTime }
  impl ArchiveStateStore {
      pub fn new(db: std::sync::Arc<dyn ecat_data::SqlExecutor>) -> Self;
      pub async fn ensure_schema(&self) -> Result<(), TieringError>;
      pub async fn upsert(&self, s: &ArchiveJobState) -> Result<(), TieringError>;
      pub async fn load(&self, table: &str, partition: &str, chunk: u64) -> Result<Option<ArchiveJobState>, TieringError>;
      pub async fn advance(&self, table: &str, partition: &str, chunk: u64, to: ArchivePhase, checksum: Option<&str>) -> Result<(), TieringError>;
  }
  ```
  合法迁移：`Pending → Copied → Verified → Dropped`；`Dropped` 终态；其余迁移报错。

- [ ] **Step 1: 写失败测试**

用 `ecat-data-sqlx`（dev-dependency，SQLite 内存）：
`state_roundtrip_and_idempotent_upsert`；`invalid_transition_is_rejected`（`Pending → Verified`）；`verified_required_before_dropped`；`load_missing_returns_none`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-tiering state_`
Expected: FAIL。

- [ ] **Step 3: 实现**

`ensure_schema` 建 `tier_archive_jobs(table_name, partition_name, chunk, low_id, high_id, rows, checksum, phase, updated_at, PRIMARY KEY(table_name, partition_name, chunk))`；`upsert` 用 `INSERT ... ON CONFLICT ... DO UPDATE`；SQLite 与 MySQL 语法差异用 `Dialect` 分支（SQLite/Postgres `ON CONFLICT`，MySQL `ON DUPLICATE KEY UPDATE`）。MSSQL 本期返回 `TieringError::UnsupportedDialect`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-tiering state_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-tiering
git commit --only ecat-data-tiering -m "feat(tiering): add archive job state machine"
```

### Task 10: 归档主循环与幂等

**Files:**
- Create: `ecat-data-tiering/src/archive.rs`
- Create: `ecat-data-tiering/src/archive/tests.rs`
- Modify: `ecat-data-tiering/src/lib.rs`
- Modify: `ecat-data-tiering/Cargo.toml`（`ecat-lock` 依赖）

**Interfaces:**
- Produces:
  ```rust
  #[async_trait]
  pub trait ColdSink: Send + Sync {
      async fn write_chunk(&self, table: &str, partition: &str, chunk: u64, rows: &[ecat_data::Row]) -> Result<String, TieringError>;
      async fn verify_chunk(&self, table: &str, partition: &str, chunk: u64, checksum: &str) -> Result<bool, TieringError>;
      async fn health_check(&self) -> Result<(), TieringError>;
  }
  pub struct ArchiveJob { /* hot: Arc<dyn RdbmsClient>, cold: Arc<dyn ColdSink>, state: ArchiveStateStore, config: TieringConfig, lock: Option<Arc<dyn ecat_lock::DistributedLock>> */ }
  impl ArchiveJob {
      pub async fn run_once(&self) -> Result<ArchiveReport, TieringError>;
  }
  ```
  `ArchiveReport { chunks: u64, rows: u64, dropped_partitions: u64, dry_run: bool, skipped: bool }`。
  `lock: Option<Arc<dyn ecat_lock::DistributedLock>>`；未抢到锁时返回 `skipped: true`，不做任何写入。

- [ ] **Step 1: 写失败测试**

`archive_copies_all_chunks_and_verifies`：3 个 chunk，fake hot 返回固定行，fake cold 记录写入与校验；断言状态全部 `Verified`。
`archive_resumes_after_crash_without_duplicates`：预置 `Copied` 状态，重跑只做 verify，不重复写。
`archive_does_not_drop_when_verify_fails`：fake cold 校验返回 false，断言无 `Dropped`。
`dry_run_writes_nothing`：`dry_run=true` 时 fake cold 零调用。
`second_instance_skips_when_lock_held`：fake lock 的 `acquire` 返回 `None`，断言 `run_once().skipped == true` 且 cold 零调用。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-tiering archive_`
Expected: FAIL。

- [ ] **Step 3: 实现**

主循环按 `WHERE id > ? ORDER BY id LIMIT batch` 取 chunk；写冷层 → `Copied`；校验通过 → `Verified`；分区内全部 chunk `Verified` 且超过 `retain_s3_days` → 调 hot 执行 `DROP PARTITION` → `Dropped`。所有表名/分区名先过 `validate_ident`。`dry_run` 只读与校验，不写、不 drop。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-tiering archive_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-tiering
git commit --only ecat-data-tiering -m "feat(tiering): add idempotent archive job"
```

### Task 11: ClickHouse 冷层 sink

**Files:**
- Create: `ecat-data-tiering/src/sink/mod.rs`
- Create: `ecat-data-tiering/src/sink/clickhouse.rs`
- Create: `ecat-data-tiering/src/sink/clickhouse/tests.rs`
- Modify: `ecat-data-tiering/Cargo.toml`（optional `ecat-data-clickhouse`，feature `clickhouse`）
- Modify: `ecat-data-tiering/src/lib.rs`

**Interfaces:**
- Consumes: `ecat_data_clickhouse::{ClickhouseClient, ClickhouseConfig}`、`ColdSink`。
- Produces:
  ```rust
  #[cfg(feature = "clickhouse")]
  pub struct ClickhouseSink { /* client + table whitelist */ }
  #[cfg(feature = "clickhouse")]
  impl ColdSink for ClickhouseSink { ... }
  pub fn render_sql_value(v: &serde_json::Value) -> Result<String, TieringError>;
  // 标识符校验复用 Task 8 的 validate_ident
  ```
  `render_sql_value`：`Null → NULL`；布尔/数字原样；字符串单引号转义（`\` 与 `'`）；数组/对象用 JSON 文本再转义。

- [ ] **Step 1: 写失败测试**

`sql_value_escapes_quotes_and_backslashes`；`sql_value_renders_null_and_numbers`；`validate_ident_rejects_semicolon_and_dash`；`clickhouse_write_chunk_returns_stable_sha256`（同一批行按主键排序后哈希稳定）；`cold_rows_preserve_value_wire_format`（RFC3339 UTC 时间、base64 bytes、date 字符串经 NDJSON 往返后逐字节一致）。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-tiering --features clickhouse clickhouse_`
Expected: FAIL。

- [ ] **Step 3: 实现**

用 `sha2` 计算规范化 NDJSON 的 SHA-256（按主键排序后逐行 `serde_json::to_string`）；`write_chunk` 生成 `INSERT INTO <validated_table> VALUES (...)` 并调 `ClickhouseClient::execute`，同时写 `ecat_tier_checksums(table, partition, chunk, checksum)`；`verify_chunk` 查该表比对 checksum；`health_check` 执行 `SELECT 1`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-tiering --features clickhouse clickhouse_`
Expected: PASS。live 用例未设 `ECAT_TEST_CLICKHOUSE_URL` 时 skip。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-tiering
git commit --only ecat-data-tiering -m "feat(tiering): add ClickHouse cold sink"
```

### Task 12: S3 冷层 sink

**Files:**
- Create: `ecat-data-tiering/src/sink/s3.rs`
- Create: `ecat-data-tiering/src/sink/s3/tests.rs`
- Modify: `ecat-data-tiering/Cargo.toml`（optional `ecat-data-s3`，feature `s3`）
- Modify: `ecat-data-tiering/src/lib.rs`

**Interfaces:**
- Consumes: `ecat_data::StorageClient::{put, get, delete, list}`、`ColdSink`。
- Produces:
  ```rust
  #[cfg(feature = "s3")]
  pub struct S3Sink { /* Arc<dyn StorageClient>, bucket, prefix */ }
  #[cfg(feature = "s3")]
  impl ColdSink for S3Sink { ... }
  ```
  每个 chunk 写 `s3://<bucket>/<prefix>/<table>/<partition>/<chunk>.ndjson`；先写 chunk，最后写 `manifest.json`（作为提交点）。

- [ ] **Step 1: 写失败测试**

内存 `FakeStorage` 实现 `StorageClient`：
`write_chunk_puts_ndjson_and_manifest`；`manifest_written_last`；`existing_key_skips_rewrite`；`checksum_matches_downloaded_bytes`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-tiering --features s3 s3_`
Expected: FAIL。

- [ ] **Step 3: 实现**

NDJSON 每行 `serde_json::to_string(row_pairs)`；chunk 大小上限 64MB；`put` 整对象；`verify_chunk` 用 `get` 后重新计算 SHA-256；`health_check` 调 `list(bucket, prefix)` 验证可达。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-tiering --features s3 s3_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-tiering
git commit --only ecat-data-tiering -m "feat(tiering): add S3 cold sink"
```

### Task 13: `TieredQuery` 跨边界查询

**Files:**
- Create: `ecat-data-tiering/src/query.rs`
- Create: `ecat-data-tiering/src/query/tests.rs`
- Modify: `ecat-data-tiering/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  #[async_trait]
  pub trait ColdQuery: Send + Sync {
      async fn query_range(&self, table: &str, from: time::OffsetDateTime, to: time::OffsetDateTime, params: &[serde_json::Value], limit: u64, offset: u64) -> Result<Vec<ecat_data::Row>, TieringError>;
  }
  pub struct TieredTable { pub name: String, pub time_col: String, pub id_col: String, pub hot_days: u32 }
  pub struct TieredQuery { /* hot: Arc<dyn RdbmsClient>, cold: Arc<dyn ColdQuery>, tables: Vec<TieredTable> */ }
  impl TieredQuery {
      pub fn new(hot: Arc<dyn ecat_data::RdbmsClient>, cold: Arc<dyn ColdQuery>, tables: Vec<TieredTable>) -> Self;
      pub async fn query_range(&self, table: &str, from: time::OffsetDateTime, to: time::OffsetDateTime, params: &[serde_json::Value], limit: u64, offset: u64) -> Result<Vec<ecat_data::Row>, TieringError>;
  }
  ```
  MSSQL 的 OFFSET/FETCH 分页本期不实现；遇到 `Dialect::Mssql` 返回 `TieringError::UnsupportedDialect`。

- [ ] **Step 1: 写失败测试**

`hot_only_range_reads_hot`；`cold_only_range_reads_cold`；`boundary_rows_appear_once`（hot 与 cold 各返回同一 id 的行，结果去重为一行）；`merge_sorts_by_pk_and_paginates`；`cold_failure_returns_error_not_empty`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-tiering query_`
Expected: FAIL。

- [ ] **Step 3: 实现**

根据 `hot_days` 计算热冷边界；全热/全冷单侧查询；跨边界两侧查（下推 `limit+offset`），按主键合并去重、排序后再分页；cold 错误直接返回。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-tiering query_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-tiering
git commit --only ecat-data-tiering -m "feat(tiering): add hot-cold range query"
```

### Task 14: 分区 DDL 助手与 `ecat-tier` CLI

**Files:**
- Create: `ecat-data-tiering/src/partition.rs`
- Create: `ecat-data-tiering/src/partition/tests.rs`
- Create: `ecat-data-tiering/src/bin/ecat-tier.rs`
- Modify: `ecat-data-tiering/Cargo.toml`（`clap` 依赖、`[[bin]]`）
- Modify: `ecat-data-tiering/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct PartitionSpec { pub table: String, pub column: String, pub months: u32 }
  pub fn plan_range_partitions(spec: &PartitionSpec, from: time::OffsetDateTime, to: time::OffsetDateTime) -> Result<String, TieringError>;
  pub async fn drop_partition(db: &dyn ecat_data::SqlExecutor, table: &str, partition: &str) -> Result<u64, TieringError>;
  pub struct RestorePlan { pub table: String, pub partition: String, pub source: RestoreSource }
  pub enum RestoreSource { Clickhouse, S3 }
  pub async fn prepare_staging(db: &dyn ecat_data::SqlExecutor, plan: &RestorePlan) -> Result<String, TieringError>;
  pub async fn restore_partition(db: &dyn ecat_data::SqlExecutor, plan: &RestorePlan) -> Result<u64, TieringError>;
  ```
  `prepare_staging` 执行 `DROP TABLE IF EXISTS staging` → `CREATE TABLE staging LIKE ...` → `REMOVE PARTITIONING`，返回 staging 表名；调用方（Task 39）从 `RestoreSource` 装载数据后再调用 `restore_partition`。`restore_partition` 只做守卫（staging 非空、目标分区存在）→ `EXCHANGE PARTITION` → 成功后才 `reset_partition`；表名/分区名先过 `validate_ident`。
  CLI：`ecat-tier plan|run|verify|restore --config <path> [--dry-run]`。
  MySQL 8.4/9 的 `RANGE COLUMNS` 语法未变；首次 `PARTITION BY` 的算法/锁级别需在 8.4/9 实测，gh-ost 不用于分区转换。

- [ ] **Step 1: 写失败测试**

`partition_sql_uses_utc_boundaries_and_validated_identifiers`；`partition_sql_rejects_bad_ident`；`drop_partition_rejects_semicolon`；`restore_uses_staging_and_exchange_partition`；`cli_plan_parses_config_path`（用 `try_parse_from`，不启动任务）。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-tiering partition_`
Expected: FAIL。

- [ ] **Step 3: 实现**

`plan_range_partitions` 生成 `ALTER TABLE \`t\` PARTITION BY RANGE COLUMNS(\`ts\`) (...)`，边界用 UTC；`drop_partition` 先 `validate_ident`，再 `ALTER TABLE ... DROP PARTITION`；CLI 只做参数解析与调用，不新增业务逻辑。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-tiering partition_ && cargo run -p ecat-data-tiering --bin ecat-tier -- --help`
Expected: PASS，帮助信息列出四个子命令。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-tiering
git commit --only ecat-data-tiering -m "feat(tiering): add partition DDL and CLI"
```

### Task 15: 聚合 crate 可选 feature

**Files:**
- Modify: `ecat/Cargo.toml`
- Modify: `ecat/src/reexports.rs`
- Test: `cargo check -p ecat --features tiering`

**Interfaces:**
- `ecat` 新增 optional 依赖 `ecat-data-tiering`，feature `tiering = ["dep:ecat-data-tiering"]`；不改 default。
- `ecat::tiering` 仅在 feature 开启时存在。

- [ ] **Step 1: 写失败检查**

Run: `cargo check -p ecat --features tiering`
Expected: FAIL，feature 不存在。

- [ ] **Step 2: 加 feature 与重导出**

`ecat/Cargo.toml` 加 optional 依赖与 feature；`ecat/src/reexports.rs` 在 `#[cfg(feature = "tiering")]` 下 `pub use ecat_data_tiering as tiering;`。

- [ ] **Step 3: 验证默认构建不变**

Run: `cargo check -p ecat && cargo check -p ecat --features tiering`
Expected: 均 PASS。

- [ ] **Step 4: 提交**

```bash
git add ecat/Cargo.toml ecat/src/reexports.rs
git commit --only ecat/Cargo.toml ecat/src/reexports.rs -m "feat(ecat): add opt-in tiering re-export"
```

---

## Phase P3/P4 — 分片（P0/P3 门禁通过后启动）

> P3 的代理部署、双写/CDC、切流是运维任务，不在本计划代码范围内；本阶段只实现 e-cat 侧的配置、目录、校验和 A 阶段客户端分片。

### Task 16: `ecat-data-sharding` 目录与校验

**Files:**
- Create: `ecat-data-sharding/Cargo.toml`
- Create: `ecat-data-sharding/src/lib.rs`
- Create: `ecat-data-sharding/src/config.rs`
- Create: `ecat-data-sharding/src/catalog.rs`
- Create: `ecat-data-sharding/src/catalog/tests.rs`
- Modify: `Cargo.toml`（workspace members 追加 `ecat-data-sharding`）

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Deserialize)]
  #[serde(deny_unknown_fields)]
  pub struct ShardingConfig { pub mode: ShardingMode, pub catalog: ShardCatalog }
  pub enum ShardingMode { Proxy, Client }
  pub struct ShardCatalog { pub tables: Vec<ShardTable> }
  pub struct ShardTable { pub name: String, pub key: String, pub pk: String, pub algorithm: ShardAlgorithm, pub shards: Vec<ShardEndpoint> }
  pub struct ShardEndpoint { pub name: String, #[serde(default)] pub url: Option<String> }
  pub enum ShardAlgorithm { HashMod { shards: u32 }, Range { boundaries: Vec<i64> } }
  pub enum ShardError { Config(String), MissingKey, UnknownTable(String), ShardCountMismatch, PartialFailure { failed: Vec<String> }, ClockRollback, IdOutOfRange, UnsupportedDialect }
  pub fn stable_hash64(key: &serde_json::Value) -> Result<u64, ShardError>; // FNV-1a，canonical string
  impl ShardCatalog {
      pub fn validate(&self) -> Result<(), ShardError>;
      pub fn shard_for(&self, table: &str, key: &serde_json::Value) -> Result<usize, ShardError>;
      pub fn table(&self, name: &str) -> Result<&ShardTable, ShardError>;
  }
  ```
  `HashMod` 必须与代理侧配置的算法一致；若代理使用 Java hashCode 等其它算法，A 阶段按设计 §4.1 重新分片，不假设可移植。`stable_hash64` 用 FNV-1a 的已知向量测试钉住。

- [ ] **Step 1: 写失败测试**

`catalog_rejects_duplicate_table_names`；`catalog_rejects_empty_shards`；`hash_mod_routes_same_key_to_same_shard`；`range_routes_boundaries`；`unknown_table_is_error`；`null_key_is_error`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sharding catalog_`
Expected: FAIL，crate 不存在。

- [ ] **Step 3: 建 crate 并实现**

`validate` 检查表名/键/主键非空、分片非空、端点名唯一、`Client` 模式要求每个端点有 `url`；`shard_for` 对 `Null`/缺失键返回 `ShardError::MissingKey`，不猜。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sharding catalog_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml ecat-data-sharding
git commit --only Cargo.toml ecat-data-sharding -m "feat(sharding): add shard catalog and validation"
```

### Task 17: 代理模式校验

**Files:**
- Create: `ecat-data-sharding/src/proxy.rs`
- Create: `ecat-data-sharding/src/proxy/tests.rs`
- Modify: `ecat-data-sharding/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn validate_proxy(cfg: &ShardingConfig) -> Result<(), ShardError>;
  ```
  规则：`mode == Proxy`；`catalog.tables` 非空；端点 `url` 必须为空（B 阶段单端点由 `SqlxConfig` 提供，不在 catalog 里配多个）；`Client` 模式调用直接报错。
  > MySQL 8.4/9 门禁：代理必须能用 `caching_sha2_password` 连接后端。第三方报告称 ShardingSphere-Proxy 5.4.x 不稳定（官方矩阵未确认）；9.x 已移除 `mysql_native_password`，不能靠旧插件绕过。验证通过前不得开启 B 阶段；9.x 默认走 A 阶段。

- [ ] **Step 1: 写失败测试**

`proxy_rejects_client_mode`；`proxy_rejects_empty_tables`；`proxy_rejects_per_shard_urls`；`proxy_accepts_single_endpoint_catalog`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sharding proxy_`
Expected: FAIL。

- [ ] **Step 3: 实现**

纯校验函数；错误用 `ShardError::Config(String)`，不新增 `RdbmsError` 变体。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sharding proxy_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-sharding
git commit --only ecat-data-sharding -m "feat(sharding): validate proxy topology"
```

### Task 18: `ShardRouter::for_key` 与 `KeyedClient`

**Files:**
- Create: `ecat-data-sharding/src/router.rs`
- Create: `ecat-data-sharding/src/client.rs`
- Create: `ecat-data-sharding/src/router/tests.rs`
- Modify: `ecat-data-sharding/src/lib.rs`

**Interfaces:**
- Consumes: `ShardCatalog`、`Arc<dyn RdbmsClient>`。
- Produces:
  ```rust
  pub struct ShardRouter { /* catalog + clients */ }
  impl ShardRouter {
      pub fn new(catalog: ShardCatalog, clients: std::collections::HashMap<String, Arc<dyn RdbmsClient>>) -> Result<Self, ShardError>;
      pub fn for_key(&self, table: &str, key: &serde_json::Value) -> Result<KeyedClient, ShardError>;
  }
  pub struct KeyedClient { /* client: Arc<dyn RdbmsClient>, table: String */ }
  impl ecat_data::SqlExecutor for KeyedClient { /* 六个方法全部委托 */ }
  impl ecat_data::RdbmsClient for KeyedClient { /* transaction 委托 */ }
  ```

- [ ] **Step 1: 写失败测试**

`for_key_routes_all_six_methods_to_same_shard`；`for_key_transaction_uses_same_shard`；`unknown_table_is_error`；`missing_key_is_error`；`shard_count_mismatch_is_error`；`missing_client_is_error`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sharding for_key_`
Expected: FAIL。

- [ ] **Step 3: 实现**

`for_key` 查 catalog → 算 shard index → 取 client；`KeyedClient` 实现 `SqlExecutor` 六个方法与 `RdbmsClient::transaction`，全部原样委托；不做 SQL 解析。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sharding for_key_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-sharding
git commit --only ecat-data-sharding -m "feat(sharding): add key-bound client routing"
```

### Task 19: 显式 scatter 查询

**Files:**
- Create: `ecat-data-sharding/src/scatter.rs`
- Create: `ecat-data-sharding/src/scatter/tests.rs`
- Modify: `ecat-data-sharding/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct ScatterRequest { pub table: String, pub sql: String, pub params: Vec<serde_json::Value>, pub order_by: String, pub limit: u64, pub offset: u64, pub timeout: std::time::Duration, pub max_concurrency: usize }
  impl ShardRouter {
      pub async fn scatter(&self, req: &ScatterRequest) -> Result<Vec<ecat_data::Row>, ShardError>;
  }
  ```
  语义：所有分片执行；失败分片返回 `ShardError::PartialFailure { failed: Vec<String> }`，不返回部分结果；成功时按 `order_by` 排序、按 catalog 的 `pk` 去重、再 `offset/limit`。

- [ ] **Step 1: 写失败测试**

`scatter_merges_and_dedupes_by_pk`；`scatter_sorts_before_paginating`；`scatter_partial_failure_returns_error`；`scatter_unknown_table_is_error`；`scatter_timeout_returns_error`（一个分片 sleep 超过 `timeout`，整次调用报错，不挂死）；`scatter_concurrency_is_bounded`（同时执行的分片数不超过 `max_concurrency`）。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sharding scatter_`
Expected: FAIL。

- [ ] **Step 3: 实现**

用 `tokio::time::timeout` 包住整次 scatter；用 `JoinSet` + `tokio::sync::Semaphore` 限制 `max_concurrency`；任一分片错误或总超时 → 错误（`PartialFailure` / `RdbmsError::Timeout`），不返回部分结果；合并后排序、去重、分页。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sharding scatter_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-sharding
git commit --only ecat-data-sharding -m "feat(sharding): add explicit scatter query"
```

### Task 20: Snowflake 全局 ID

**Files:**
- Create: `ecat-data-sharding/src/id.rs`
- Create: `ecat-data-sharding/src/id/tests.rs`
- Modify: `ecat-data-sharding/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct Snowflake { /* worker_id, epoch_ms, clock, last_ms, sequence */ }
  impl Snowflake {
      pub fn new(worker_id: u64, epoch: time::OffsetDateTime) -> Result<Self, ShardError>;
      pub fn with_clock(worker_id: u64, epoch: time::OffsetDateTime, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Result<Self, ShardError>;
      pub fn next_id(&self) -> Result<i64, ShardError>;
  }
  ```
  位布局：41 位毫秒时间戳 + 10 位 worker id + 12 位序列；`worker_id < 1024`；时钟回拨返回 `ShardError::ClockRollback`。

- [ ] **Step 1: 写失败测试**

`ids_are_strictly_increasing`（fake clock 前进 1ms，连续 5000 个 id）；`ids_are_unique_under_concurrency`（8 个任务 × 1000 个 id，`HashSet` 长度 8000）；`clock_rollback_is_rejected`；`worker_id_out_of_range_is_rejected`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sharding snowflake_`
Expected: FAIL。

- [ ] **Step 3: 实现**

`AtomicI64` 存 last_ms，`AtomicU64` 存 sequence；同一毫秒内 sequence 自增，溢出则自旋到下一毫秒；时钟回拨直接报错，不回退等待。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sharding snowflake_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-sharding
git commit --only ecat-data-sharding -m "feat(sharding): add snowflake id generator"
```

### Task 21: outbox 存储与幂等消费

**Files:**
- Create: `ecat-data-sharding/src/outbox.rs`
- Create: `ecat-data-sharding/src/outbox/tests.rs`
- Modify: `ecat-data-sharding/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum OutboxStatus { Pending, Processing, Done, Failed }
  pub struct OutboxRecord { pub id: String, pub aggregate: String, pub payload: Vec<u8>, pub status: OutboxStatus, pub attempts: u32, pub next_attempt_at: time::OffsetDateTime }
  impl OutboxStore {
      pub fn new(db: Arc<dyn ecat_data::RdbmsClient>) -> Self;
      pub async fn ensure_schema(&self) -> Result<(), ShardError>;
      pub async fn append(&self, r: &OutboxRecord) -> Result<(), ShardError>;
      pub async fn claim(&self, limit: u32, lease: time::Duration) -> Result<Vec<OutboxRecord>, ShardError>;
      pub async fn mark_done(&self, id: &str) -> Result<(), ShardError>;
      pub async fn mark_failed(&self, id: &str, retry_after: time::Duration) -> Result<(), ShardError>;
      pub async fn pending_count(&self) -> Result<u64, ShardError>;
  }
  ```

- [ ] **Step 1: 写失败测试**

用 SQLite 内存库：`append_then_claim_returns_pending`；`claim_skips_future_retry`；`concurrent_claims_are_disjoint`（两个任务同时 claim，id 集合无交集）；`stale_processing_is_reclaimed`（`Processing` 超过 lease 后可再次 claim）；`mark_done_is_idempotent`；`mark_failed_increments_attempts_and_sets_next_attempt`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sharding outbox_`
Expected: FAIL。

- [ ] **Step 3: 实现**

`ensure_schema` 建 `ecat_outbox(id TEXT PRIMARY KEY, aggregate TEXT, payload BLOB, status TEXT, attempts INTEGER, next_attempt_at TEXT, claimed_at TEXT)`；`claim` 在事务里先回收 `status='processing' AND claimed_at < now - lease` 的记录，再 `SELECT ... WHERE status='pending' AND next_attempt_at <= ? LIMIT ?`，最后按主键集合 `UPDATE` 为 `Processing` 并写 `claimed_at`；SQLite 不支持 `UPDATE LIMIT`，用事务 + 主键集合更新。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sharding outbox_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-sharding
git commit --only ecat-data-sharding -m "feat(sharding): add outbox store with idempotent claim"
```

### Task 22: 聚合 crate 分片 feature

**Files:**
- Modify: `ecat/Cargo.toml`
- Modify: `ecat/src/reexports.rs`
- Test: `cargo check -p ecat --features sharding`

**Interfaces:**
- `ecat` 新增 optional 依赖 `ecat-data-sharding`，feature `sharding = ["dep:ecat-data-sharding"]`；不改 default。
- `ecat::sharding` 仅在 feature 开启时存在。

- [ ] **Step 1: 写失败检查**

Run: `cargo check -p ecat --features sharding`
Expected: FAIL，feature 不存在。

- [ ] **Step 2: 加 feature 与重导出**

`ecat/Cargo.toml` 加 optional 依赖与 feature；`ecat/src/reexports.rs` 在 `#[cfg(feature = "sharding")]` 下 `pub use ecat_data_sharding as sharding;`。

- [ ] **Step 3: 验证默认构建不变**

Run: `cargo check -p ecat && cargo check -p ecat --features sharding`
Expected: 均 PASS。

- [ ] **Step 4: 提交**

```bash
git add ecat/Cargo.toml ecat/src/reexports.rs
git commit --only ecat/Cargo.toml ecat/src/reexports.rs -m "feat(ecat): add opt-in sharding re-export"
```

---

### Task 23: 分片迁移校验脚本

**Files:**
- Create: `scripts/sharding/checksum.sql`
- Create: `scripts/sharding/verify.sh`
- Test: `scripts/sharding/verify.sh --check`

**Interfaces:**
- `checksum.sql` 四个 `-- SECTION:`：`ROW_COUNTS`、`PK_RANGES`、`CHECKSUMS`、`ORPHANS`；每表按主键区间计算 `COUNT(*)` 与 `SUM(CRC32(CONCAT_WS('|', ...)))`。
- `verify.sh --check`：校验四个段标记存在，成功打印 `sharding-checksum: ok`。
- `verify.sh --source-dsn <dsn> --target-dsn <dsn> --table <name>`：对比两侧输出；任一区间计数/校验和不一致则退出 1，打印差异表。
- MySQL 8.4+ 的 binlog 位点用 `SHOW BINARY LOG STATUS`；`SHOW MASTER STATUS` 仅 8.0；复制变更用 `CHANGE REPLICATION SOURCE TO`，不用 `CHANGE MASTER TO`。

- [ ] **Step 1: 写失败检查**

Run: `bash scripts/sharding/verify.sh --check`
Expected: FAIL，`No such file or directory`。

- [ ] **Step 2: 写 SQL 与脚本**

SQL 段用 `information_schema` 与 `CRC32`；脚本用 `mysql --batch --raw --skip-column-names` 取两侧结果，`diff` 后非零即失败。表名只接受 `^[A-Za-z_][A-Za-z0-9_]*$`，拒绝分号/反引号。

- [ ] **Step 3: 验证**

Run: `bash scripts/sharding/verify.sh --check`
Expected: `sharding-checksum: ok`。
Run: `bash scripts/sharding/verify.sh --table 'bad;drop' --source-dsn x --target-dsn y`
Expected: 退出 1，`invalid table identifier`。

- [ ] **Step 4: 提交**

```bash
git add scripts/sharding/checksum.sql scripts/sharding/verify.sh
git commit --only scripts/sharding/checksum.sql scripts/sharding/verify.sh -m "chore(sharding): add migration checksum verifier"
```

---

### Task 24: MySQL 8.4/9 代理认证门禁（P3 前置）

**Files:**
- Create: `scripts/sharding/proxy-auth-check.sh`
- Test: `scripts/sharding/proxy-auth-check.sh --check`

**Interfaces:**
- `--check`：校验脚本包含 `mysql:8.4` / `mysql:9` 两个分支与 `caching_sha2_password` 用户创建语句，成功打印 `proxy-auth-check: ok`。
- 运行模式：`PROXY_DSN` 指向 ShardingSphere-Proxy；脚本通过代理执行 `SELECT 1`，成功退出 0，失败打印代理版本与 MySQL 版本。
- 脚本中不得出现 `mysql_native_password`（MySQL 9.x 已移除，8.4 默认禁用）。

- [ ] **Step 1: 写失败检查**

Run: `bash scripts/sharding/proxy-auth-check.sh --check`
Expected: FAIL，`No such file or directory`。

- [ ] **Step 2: 实现**

脚本按 `MYSQL_VERSION` 选择 `mysql:8.4` / `mysql:9` 镜像；用 `CREATE USER ... IDENTIFIED WITH caching_sha2_password` 建代理账号；启动目标 ShardingSphere-Proxy 版本，通过 `PROXY_DSN` 执行 `SELECT 1`；记录代理版本。

- [ ] **Step 3: 验证**

Run: `bash scripts/sharding/proxy-auth-check.sh --check`
Expected: `proxy-auth-check: ok`。
Run: `grep -c mysql_native_password scripts/sharding/proxy-auth-check.sh`
Expected: `0`。

- [ ] **Step 4: 提交**

```bash
git add scripts/sharding/proxy-auth-check.sh
git commit --only scripts/sharding/proxy-auth-check.sh -m "chore(sharding): add MySQL 8.4/9 proxy auth gate"
```

> 门禁不过：MySQL 9.x 走 A 阶段；MySQL 8.4 换支持 `caching_sha2_password` 的代理版本或走 A 阶段。不得为了过门禁启用 `mysql_native_password`。

---

## Phase P6 — 单机/分布式运行基座（跨阶段补充）

> Task 25–27、29–30 建议在 P2/P3 之前完成；Task 28、31 可与 P3/P4 并行。Task 31 是 P4 门控项。

### Task 25: 部署模式与启动校验

**Files:**
- Create: `ecat/src/deployment.rs`
- Create: `ecat/src/deployment/tests.rs`
- Modify: `ecat/src/lib.rs`（`mod deployment; pub use deployment::{DeploymentConfig, DeploymentError, DeploymentMode};`）
- Modify: `ecat/Cargo.toml`（`serde = { workspace = true }`）

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum DeploymentMode { Standalone, Distributed }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum CacheMode { #[default] Memory, Redis }

  #[derive(Debug, Clone, serde::Deserialize)]
  #[serde(deny_unknown_fields)]
  pub struct DeploymentConfig {
      pub mode: DeploymentMode,
      pub node_id: String,
      #[serde(default)] pub worker_id: Option<u16>,
      #[serde(default)] pub cache: CacheMode,
      #[serde(default)] pub sharding_enabled: bool,
      #[serde(default)] pub tiering_enabled: bool,
  }

  pub enum DeploymentError { StandaloneWithSharding, DistributedWithoutNodeId, DistributedWithMemoryCache, InvalidWorkerId }
  impl DeploymentConfig { pub fn validate(&self) -> Result<(), DeploymentError>; }
  impl ecat::AppBuilder {
      pub fn deployment(mut self, cfg: DeploymentConfig) -> Self;
  }
  ```
  规则：`Standalone` 禁止 `sharding_enabled`、允许 `CacheMode::Memory`；`Distributed` 要求 `node_id` 非空且 `CacheMode::Redis`；分布式分片要求 `worker_id < 1024`。`AppBuilder::build()` 在设置 deployment 时调用 `validate()`，失败返回错误。

- [ ] **Step 1: 写失败测试**

`standalone_rejects_sharding`；`standalone_allows_memory_cache`；`distributed_requires_node_id`；`distributed_rejects_memory_cache`；`distributed_sharding_requires_worker_id`；`standalone_allows_tiering`；`distributed_with_node_and_worker_ok`；`unknown_key_is_rejected`；`app_builder_rejects_invalid_deployment`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat deployment_`
Expected: FAIL，`cannot find type DeploymentConfig`。

- [ ] **Step 3: 实现**

纯校验，无 I/O；只新增 `serde` 直接依赖；`DeploymentError` 手写 `Display`/`Error`，不引入 thiserror，也不并入现有错误枚举；`AppBuilder` 加私有字段 `deployment: Option<DeploymentConfig>`，`build()` 校验。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat deployment_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat/src/deployment.rs ecat/src/deployment/tests.rs ecat/src/lib.rs ecat/Cargo.toml
git commit --only ecat/src/deployment.rs ecat/src/deployment/tests.rs ecat/src/lib.rs ecat/Cargo.toml -m "feat(ecat): add deployment mode validation"
```

> `node_id` 可由 `ecat-registry` 的 `ServiceInfo.metadata` 提供；`worker_id` 仍用静态配置或数据库租约，不依赖 registry 分配（见 Task 20）。

### Task 26: 后台任务优雅停机

**Files:**
- Create: `ecat-data/src/worker.rs`
- Create: `ecat-data/src/worker/tests.rs`
- Modify: `ecat-data/src/lib.rs`
- Modify: `ecat-data-tiering/src/archive.rs`
- Test: `ecat-data-tiering/src/archive/tests.rs`

**Interfaces:**
- Consumes: `ArchiveJob::{run_once, config}`。
- Produces:
  ```rust
  pub struct Shutdown { /* tokio::sync::watch::Receiver<bool> */ }
  pub struct ShutdownHandle { /* tokio::sync::watch::Sender<bool> */ }
  impl Shutdown {
      pub fn new() -> (ShutdownHandle, Shutdown);
      pub async fn cancelled(&mut self);
      pub fn is_cancelled(&self) -> bool;
  }
  impl ShutdownHandle { pub fn shutdown(&self); }
  impl ArchiveJob { pub async fn run_until(&self, shutdown: &mut Shutdown) -> Result<(), TieringError>; }
  ```
  `run_until` 在 `interval` 与 `shutdown.cancelled()` 之间 `select!`；收到停机后完成当前 chunk 再返回，不中断写入。

- [ ] **Step 1: 写失败测试**

`shutdown_cancels_archive_within_100ms`（fake job sleep 10s，发停机后 100ms 内返回）；`run_until_finishes_current_chunk_before_exit`（fake job 记录 `copied` 后才退出）；`shutdown_handle_is_idempotent`（重复 `shutdown()` 不 panic）。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data worker_ && cargo test -p ecat-data-tiering shutdown_`
Expected: FAIL，`cannot find type Shutdown`。

- [ ] **Step 3: 实现**

`Shutdown` 用 `tokio::sync::watch`；`run_until` 用 `tokio::select!`；不新增依赖（tokio 已在 workspace）。`ecat-data-tiering` 依赖 `ecat-data` 已存在。App 集成不改 `App` API：在 `on_start` 里 `tokio::spawn(run_until)`，把 `ShutdownHandle` 与 `JoinHandle` 放进 `on_stop` 闭包，停机时先 `shutdown()` 再 `await`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data worker_ && cargo test -p ecat-data-tiering shutdown_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data/src/worker.rs ecat-data/src/worker/tests.rs ecat-data/src/lib.rs ecat-data-tiering/src/archive.rs ecat-data-tiering/src/archive/tests.rs
git commit --only ecat-data/src/worker.rs ecat-data/src/worker/tests.rs ecat-data/src/lib.rs ecat-data-tiering/src/archive.rs ecat-data-tiering/src/archive/tests.rs -m "feat(data): add graceful shutdown for background jobs"
```

### Task 27: 数据层健康检查

**Files:**
- Create: `ecat-data-sharding/src/health.rs`
- Create: `ecat-data-sharding/src/health/tests.rs`
- Create: `ecat-data-tiering/src/health.rs`
- Create: `ecat-data-tiering/src/health/tests.rs`
- Modify: `ecat-data-sharding/Cargo.toml`（optional `ecat-health`，feature `health`）
- Modify: `ecat-data-tiering/Cargo.toml`（同上）
- Modify: 两个 `src/lib.rs`

**Interfaces:**
- Produces（均为 `#[cfg(feature = "health")]`）：
  ```rust
  pub struct ShardHealthCheck { /* Arc<ShardRouter> + timeout */ }
  impl ecat_health::HealthCheck for ShardHealthCheck { /* name = "data.shards"; SELECT 1 on each shard */ }
  impl ShardRouter {
      pub async fn check_health(&self, timeout: std::time::Duration) -> Result<(), ShardError>;
  }

  pub struct TieringHealthCheck { /* cold: Arc<dyn ColdSink>, max_lag */ }
  impl ecat_health::HealthCheck for TieringHealthCheck { /* name = "data.tiering"; calls ColdSink::health_check */ }

  pub struct OutboxHealthCheck { /* store + max backlog */ }
  impl ecat_health::HealthCheck for OutboxHealthCheck { /* name = "data.outbox" */ }
  ```
  `ShardHealthCheck`：拓扑校验失败或任一分片 `SELECT 1` 失败 -> `Err`；`TieringHealthCheck`：冷层不可达或归档延迟超阈值 -> `Err`；`OutboxHealthCheck`：pending 数超阈值 -> `Err`。

- [ ] **Step 1: 写失败测试**

`shard_health_ok_when_all_shards_answer`；`shard_health_fails_when_one_shard_is_down`；`tiering_health_fails_when_cold_store_unreachable`；`outbox_health_fails_over_backlog`；`health_feature_off_still_compiles`（`cargo check -p ecat-data-sharding --no-default-features`）。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sharding --features health health_`
Expected: FAIL。

- [ ] **Step 3: 实现**

用 `ecat_health::FnCheck` 或直接实现 `HealthCheck`；超时用 `tokio::time::timeout`；错误信息包含失败分片名/后端名。feature 默认关，不影响现有构建。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sharding --features health health_ && cargo test -p ecat-data-tiering --features health health_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-sharding ecat-data-tiering
git commit --only ecat-data-sharding ecat-data-tiering -m "feat(data): add shard/tiering/outbox health checks"
```

### Task 28: 分片连接池预算

**Files:**
- Modify: `ecat-data/src/rdbms.rs`（`RdbmsClient` 加默认 `fn pool_size(&self) -> Option<u32> { None }`）
- Modify: `ecat-data-sqlx/src/lib.rs`（`SqlxClient::pool_size` 返回 `self.pool.size()`）
- Create: `ecat-data-sharding/src/budget.rs`
- Create: `ecat-data-sharding/src/budget/tests.rs`
- Modify: `ecat-data-sharding/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct PoolBudget { pub max_total_connections: u32 }
  pub fn validate_pool_budget(
      clients: &[std::sync::Arc<dyn ecat_data::RdbmsClient>],
      instances: u32,
      budget: &PoolBudget,
  ) -> Result<(), ShardError>;
  ```
  规则：对 `pool_size()` 为 `Some(n)` 的客户端求和，再乘 `instances`；总和 > `max_total_connections` -> `ShardError::Config`；`None` 的客户端跳过（并在返回的错误/日志里注明未知）。`instances` 是同一部署的 app 副本数，避免 10 副本 × 10 分片 × 10 连接把 MySQL 打爆。

- [ ] **Step 1: 写失败测试**

`pool_budget_rejects_oversubscription`（3 × 10 > 20）；`pool_budget_accepts_exact`（3 × 10 == 30）；`pool_budget_multiplies_by_instance_count`（3 × 10 × 2 = 60；预算 50 拒绝、60 通过）；`pool_budget_ignores_unknown_pool_size`（`None` 不参与求和）；`pool_size_default_is_none`（fake client 不覆写）。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sharding pool_budget_`
Expected: FAIL。

- [ ] **Step 3: 实现**

`RdbmsClient` 加默认方法（只增不改）；`SqlxClient` 覆写；`validate_pool_budget` 纯计算，无 I/O。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sharding pool_budget_ && cargo test -p ecat-data pool_size_default_is_none`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data/src/rdbms.rs ecat-data-sqlx/src/lib.rs ecat-data-sharding
git commit --only ecat-data/src/rdbms.rs ecat-data-sqlx/src/lib.rs ecat-data-sharding -m "feat(sharding): validate total connection pool budget"
```

### Task 29: outbox relay 与幂等处理器

**Files:**
- Create: `ecat-data-sharding/src/outbox/relay.rs`
- Create: `ecat-data-sharding/src/outbox/relay/tests.rs`
- Modify: `ecat-data-sharding/src/outbox.rs`（`mod relay; pub use relay::{OutboxHandler, OutboxRelay, RelayReport};`）

**Interfaces:**
- Consumes: `OutboxStore::{claim, mark_done, mark_failed}`、`ecat_data::Shutdown`。
- Produces:
  ```rust
  #[async_trait]
  pub trait OutboxHandler: Send + Sync {
      async fn handle(&self, record: &OutboxRecord) -> Result<(), String>;
  }
  pub struct OutboxRelay { /* store, handler, batch, lease, interval */ }
  impl OutboxRelay {
      pub async fn run_once(&self) -> Result<RelayReport, ShardError>;
      pub async fn run_until(&self, shutdown: &mut ecat_data::Shutdown) -> Result<(), ShardError>;
  }
  pub struct RelayReport { pub claimed: u32, pub done: u32, pub failed: u32 }
  ```
  `run_once`：claim 一批 -> 逐条 `handle` -> 成功 `mark_done`，失败 `mark_failed`（指数退避）-> 返回计数；`run_until` 按 `interval` 循环，收到停机后完成当前批再退出。

- [ ] **Step 1: 写失败测试**

`relay_marks_done_on_success`；`relay_marks_failed_and_backs_off`；`relay_shutdown_stops_after_current_batch`；`relay_duplicate_delivery_is_idempotent`（handler 用 id 去重，第二次不产生副作用）。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sharding relay_`
Expected: FAIL。

- [ ] **Step 3: 实现**

claim 用 Task 21 的租约；`handle` 串行执行（首版不做并发），失败调用 `mark_failed`；`run_until` 用 `tokio::select!` 与 `Shutdown`；日志带 `outbox_id`/`attempts`。handler 可由用户接 `ecat-mq`（Kafka/NATS/RabbitMQ），框架不内置投递语义。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sharding relay_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-sharding
git commit --only ecat-data-sharding -m "feat(sharding): add outbox relay with idempotent handler"
```

### Task 30: 单机/分布式部署清单与对等测试

**Files:**
- Modify: `docker-compose.dev.yml`
- Modify: `ecat-deploy/helm/values.yaml`
- Modify: `ecat-deploy/k8s-deployment.yaml`
- Create: `scripts/deploy/parity.sh`
- Test: `scripts/deploy/parity.sh --check`

**Interfaces:**
- Compose 增加两个 profile：
  - `standalone`：`mysql:8.4` + `redis`；无 proxy/CH。
  - `distributed`：`mysql:8.4`/`mysql:9` + `redis` + `clickhouse` + `shardingsphere-proxy` + 2 个 app 实例；健康检查与 `ECAT_DEPLOYMENT_MODE`/`ECAT_NODE_ID`。
- Helm values 增加 `deploymentMode`、`replicaCount`、`nodeId`（从 Pod 字段注入）、`readinessProbe`/`livenessProbe`；MySQL 镜像 tag 可切 8.4/9。
- `parity.sh --check`：校验 compose 两个 profile 与 Helm values 含 `standalone`/`distributed`/`deploymentMode`；`--run`（env 门控）对同一套 CRUD/ORM SQL 在 standalone 与 distributed 上跑，比较结果。
- Helm 口令用 Secret 注入，不写进 `values.yaml`；distributed profile 的验收包含每分片备份 + binlog 位点 + 恢复演练。

- [ ] **Step 1: 写失败检查**

Run: `bash scripts/deploy/parity.sh --check`
Expected: FAIL，`No such file or directory`。

- [ ] **Step 2: 实现**

compose 用 `profiles: [standalone]` / `[distributed]`；Helm 用 `deploymentMode` 条件渲染；`parity.sh` 先做静态校验，运行模式需 `ECAT_STANDALONE_DSN`/`ECAT_DISTRIBUTED_DSN`，未设置时 skip（`ECAT_REQUIRE_LIVE_DB=1` 时失败）。

- [ ] **Step 3: 验证**

Run: `bash scripts/deploy/parity.sh --check`
Expected: `deploy-parity: ok`。
Run: `docker compose -f docker-compose.dev.yml config --quiet`
Expected: 退出 0（若本机无 docker，记录 skip）。
Run: `helm lint ecat-deploy/helm`
Expected: 退出 0（若本机无 helm，记录 skip）。

- [ ] **Step 4: 提交**

```bash
git add docker-compose.dev.yml ecat-deploy/helm/values.yaml ecat-deploy/k8s-deployment.yaml scripts/deploy/parity.sh
git commit --only docker-compose.dev.yml ecat-deploy/helm/values.yaml ecat-deploy/k8s-deployment.yaml scripts/deploy/parity.sh -m "chore(deploy): add standalone and distributed profiles with parity check"
```

### Task 31: 拓扑热更新（P4 门控）

**Files:**
- Create: `ecat-data-sharding/src/topology.rs`
- Create: `ecat-data-sharding/src/topology/tests.rs`
- Modify: `ecat-data-sharding/src/router.rs`
- Modify: `ecat-data-sharding/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  #[async_trait]
  pub trait TopologySource: Send + Sync {
      async fn load(&self) -> Result<ShardCatalog, ShardError>;
  }
  pub struct StaticTopology { /* catalog */ }
  impl ShardRouter {
      pub fn reload(&self, catalog: ShardCatalog) -> Result<(), ShardError>;
      pub fn catalog(&self) -> ShardCatalog;
  }
  ```
  `ShardRouter` 内部用 `Arc<std::sync::RwLock<ShardCatalog>>`；`for_key` 取读锁后克隆 `Arc<dyn RdbmsClient>`；`reload` 先 `validate`，再替换。旧 `KeyedClient` 持有旧 `Arc`，不受影响。

- [ ] **Step 1: 写失败测试**

`reload_routes_new_key_to_new_shard`；`reload_rejects_invalid_catalog`（重复表名/空分片）；`reload_rejects_missing_client`（新 catalog 的端点没有对应 client）；`in_flight_keyed_client_keeps_old_snapshot`；`static_topology_loads_catalog`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sharding topology_`
Expected: FAIL。

- [ ] **Step 3: 实现**

`StaticTopology` 直接返回 clone；`reload` 先校验新 catalog 的每个端点都有对应 client，再持写锁只做 swap；`for_key` 的读锁不跨 `.await`（先取 client，再释放锁）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sharding topology_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-sharding
git commit --only ecat-data-sharding -m "feat(sharding): add topology source and hot reload"
```

> 远程 `TopologySource` 见 Task 33，分片数据迁移/扩容执行见 Task 34；本任务只做静态源与路由热更新。

---

## Phase P7 — 高级分布式事务与迁移（门控）

> 全部默认关闭 / opt-in；Task 32 依赖 Task 3，Task 33/34 依赖 Task 31/23，Task 35 依赖 Task 21/26，Task 37 依赖 Task 3。2PC 仅 MySQL/PostgreSQL，不作为默认跨分片路径。

### Task 32: Fencing token 锁

**Files:**
- Modify: `ecat-lock/src/lib.rs`（新增 `FencedLock` / `FenceGuard`，不改 `DistributedLock`）
- Modify: `ecat-data-redis/src/lib.rs`（`RedisLock` 实现 `FencedLock`）
- Test: `ecat-lock/src/lib.rs`、`ecat-data-redis/src/tests.rs`

**Interfaces:**
- Produces:
  ```rust
  #[async_trait]
  pub trait FencedLock: DistributedLock {
      async fn acquire_fenced(&self, key: &str, ttl: Duration) -> Result<Option<FenceGuard>, LockError>;
      async fn validate_fence(&self, key: &str, fence: u64) -> Result<bool, LockError>;
  }
  #[derive(Debug, Clone)]
  pub struct FenceGuard { pub token: String, pub fence: u64 }
  ```
  Redis 实现用 Lua 原子执行 `INCR ecat:fence:<key>` + `SET lock <token> NX PX <ttl>`；SET 失败时 fence 仍递增（单调，浪费可接受），返回 `None`。`validate_fence` 读当前 fence，等于传入值才返回 `true`。

- [ ] **Step 1: 写失败测试**

`fence_increases_across_acquire_release_reacquire`；`only_one_concurrent_acquire_wins`；`stale_fence_is_rejected`（释放后重取，旧 fence `validate_fence` 为 false）；`renew_does_not_change_fence`；`release_does_not_decrease_fence`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-lock fence_`
Expected: FAIL，`cannot find trait FencedLock`。

- [ ] **Step 3: 实现**

`FencedLock` 是新 trait，不改 `DistributedLock`；`RedisLock` 用 `redis::Script` 实现两段 Lua；fence key 与 lock key 分开，避免 `release` 删除 fence。归档/outbox 可选使用：把 `fence` 写进状态表，写状态/`DROP PARTITION`/`mark_done` 前 `validate_fence`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-lock fence_ && cargo test -p ecat-data-redis fence_`
Expected: PASS（live 用例未设 `ECAT_TEST_REDIS_URL` 时 skip）。

- [ ] **Step 5: 提交**

```bash
git add ecat-lock/src/lib.rs ecat-data-redis/src/lib.rs ecat-data-redis/src/tests.rs
git commit --only ecat-lock/src/lib.rs ecat-data-redis/src/lib.rs ecat-data-redis/src/tests.rs -m "feat(lock): add fenced lock with monotonic tokens"
```

### Task 33: Consul/etcd 远程拓扑源

**Files:**
- Create: `ecat-config-remote/src/etcd.rs`（`EtcdConfigSource`）
- Modify: `ecat-config-remote/src/lib.rs`、`ecat-config-remote/Cargo.toml`（feature `etcd`）
- Create: `ecat-data-sharding/src/topology/remote.rs`
- Create: `ecat-data-sharding/src/topology/remote/tests.rs`
- Modify: `ecat-data-sharding/Cargo.toml`（optional `ecat-config-remote`，features `consul` / `etcd`）

**Interfaces:**
- Produces:
  ```rust
  // ecat-config-remote
  pub struct EtcdConfigSource { /* endpoints + prefix + auth */ }
  impl EtcdConfigSource {
      pub fn new(endpoints: Vec<String>, prefix: impl Into<String>) -> Self;
      pub fn with_auth(self, user: &str, password: &str) -> Self;
  }

  // ecat-data-sharding
  #[cfg(feature = "consul")]
  pub struct ConsulTopologySource { /* ConsulConfigSource */ }
  #[cfg(feature = "etcd")]
  pub struct EtcdTopologySource { /* EtcdConfigSource */ }
  impl ConsulTopologySource {
      pub async fn watch(&self) -> Result<tokio::sync::watch::Receiver<ShardCatalog>, ShardError>;
  }
  impl EtcdTopologySource {
      pub async fn watch(&self) -> Result<tokio::sync::watch::Receiver<ShardCatalog>, ShardError>;
  }
  impl TopologySource for ConsulTopologySource / EtcdTopologySource { /* load -> parse + validate */ }
  ```
  Consul 用 `ConsulConfigSource` 的 blocking query/watch；etcd 首版用 `/v3/kv/range` 轮询（`watch_interval`），后续可换 `/v3/watch` 流。启动无拓扑 -> `ShardError::Config`（fail-closed）；坏 JSON -> 错误；watch 失败保留最后已知拓扑。

- [ ] **Step 1: 写失败测试**

`consul_topology_loads_and_validates_catalog`；`etcd_topology_loads_and_validates_catalog`；`bad_json_is_rejected`；`missing_topology_fails_closed`；`watch_emits_new_catalog`（mock HTTP 两次返回不同拓扑）；`watch_failure_keeps_last_known`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sharding --features consul,etcd topology_`
Expected: FAIL。

- [ ] **Step 3: 实现**

Consul 复用 `ecat-config-remote`；etcd 用 `reqwest` 直接调 v3 HTTP（版本与 `ecat-data-s3` 对齐，直接写依赖，不塞 workspace 表）。`watch` 用 `tokio::sync::watch`，解析后 `ShardCatalog::validate`；错误不覆盖上次成功值。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sharding --features consul,etcd topology_`
Expected: PASS（mock HTTP，不连真 Consul/etcd）。

- [ ] **Step 5: 提交**

```bash
git add ecat-config-remote ecat-data-sharding
git commit --only ecat-config-remote ecat-data-sharding -m "feat(sharding): add Consul and etcd topology sources"
```

### Task 34: 分片迁移 / 扩缩容执行器（P4 门控）

**Files:**
- Create: `ecat-data-sharding/src/migrate/mod.rs`
- Create: `ecat-data-sharding/src/migrate/plan.rs`
- Create: `ecat-data-sharding/src/migrate/backfill.rs`
- Create: `ecat-data-sharding/src/migrate/verify.rs`
- Create: `ecat-data-sharding/src/migrate/cutover.rs`
- Create: `ecat-data-sharding/src/migrate/tests.rs`
- Modify: `ecat-data-sharding/src/lib.rs`
- Modify: `ecat-data-sharding/Cargo.toml`（feature `migrate`，默认关）

**Interfaces:**
- Produces:
  ```rust
  pub struct ShardMove { pub table: String, pub from: String, pub to: String, pub key_range: Option<KeyRange> }
  pub struct ShardPlan { pub moves: Vec<ShardMove> }
  impl ShardPlan { pub fn diff(old: &ShardCatalog, new: &ShardCatalog) -> Result<Self, ShardError>; }

  pub struct ShardMigrator { /* source/target clients, state store, plan */ }
  impl ShardMigrator {
      pub async fn backfill(&self) -> Result<MigrationReport, ShardError>;
      pub async fn verify(&self) -> Result<(), ShardError>;
      pub async fn cutover(&self, router: &ShardRouter, new_catalog: ShardCatalog) -> Result<(), ShardError>;
      pub async fn rollback(&self) -> Result<(), ShardError>;
  }
  pub struct MigrationReport { pub moved_rows: u64, pub chunks: u64, pub verified: bool }
  ```
  状态表 `shard_migration_jobs(table, from, to, key_low, key_high, phase, cursor, rows, checksum, updated_at)`；阶段 `pending → backfilling → verified → cutover → done`（或 `rolled_back`）。hash-mod 变更按全表双写 + 回填；range 变更按区间搬移。

- [ ] **Step 1: 写失败测试**

`plan_diff_detects_range_moves`；`plan_diff_detects_hash_remap_as_full_copy`；`backfill_is_idempotent`；`verify_mismatch_blocks_cutover`；`cutover_reloads_router`；`rollback_restores_old_catalog`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-sharding migrate_`
Expected: FAIL。

- [ ] **Step 3: 实现**

回填按 key 游标分批（`WHERE key > ? ORDER BY key LIMIT ?`），目标端幂等 upsert；校验用行数 + PK 区间 checksum + 抽样；`cutover` 只在 `verified` 后调 `router.reload(new)`；旧分片保留 N 天；`rollback` 反向搬移或 `reload(old)`。迁移任务用 `FencedLock` 保证单执行，状态写 `shard_migration_jobs`；整个模块 `#[cfg(feature = "migrate")]`。B 阶段用 ShardingSphere-Scaling，本任务只覆盖 A 阶段。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-sharding migrate_`
Expected: PASS（SQLite/fake clients；真库用例 env 门控）。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-sharding
git commit --only ecat-data-sharding -m "feat(sharding): add shard migration and rebalance executor"
```

### Task 35: Saga 引擎

**Files:**
- Create: `ecat-saga/Cargo.toml`
- Create: `ecat-saga/src/lib.rs`
- Create: `ecat-saga/src/saga.rs`
- Create: `ecat-saga/src/state.rs`
- Create: `ecat-saga/src/saga/tests.rs`
- Modify: `Cargo.toml`（workspace members）
- Modify: `ecat/Cargo.toml`、`ecat/src/reexports.rs`（feature `saga`）

**Interfaces:**
- Produces:
  ```rust
  #[async_trait]
  pub trait SagaStep: Send + Sync {
      fn name(&self) -> &str;
      async fn execute(&self, ctx: &mut SagaContext) -> Result<(), SagaError>;
      async fn compensate(&self, ctx: &mut SagaContext) -> Result<(), SagaError>;
  }
  pub struct SagaContext { pub saga_id: String, pub idempotency_key: String, pub data: serde_json::Value }
  pub enum SagaStatus { Running, Compensating, Completed, Compensated, Failed }
  pub enum StepStatus { Pending, Done, Compensated, Failed }
  pub struct SagaState { pub saga_id: String, pub status: SagaStatus, pub current_step: usize, pub context: serde_json::Value }
  #[async_trait]
  pub trait SagaStateStore: Send + Sync {
      async fn load(&self, saga_id: &str) -> Result<Option<SagaState>, SagaError>;
      async fn save(&self, state: &SagaState) -> Result<(), SagaError>;
      async fn step_status(&self, saga_id: &str, step: &str) -> Result<StepStatus, SagaError>;
      async fn set_step(&self, saga_id: &str, step: &str, status: StepStatus) -> Result<(), SagaError>;
  }
  pub struct Saga { /* steps + state store */ }
  impl Saga {
      pub fn new(steps: Vec<Arc<dyn SagaStep>>, store: Arc<dyn SagaStateStore>) -> Self;
      pub async fn run(&self, saga_id: &str, ctx: SagaContext) -> Result<(), SagaError>;
      pub async fn resume(&self, saga_id: &str) -> Result<(), SagaError>;
  }
  ```
  状态表 `saga_instances` / `saga_steps`；状态 `Running / Compensating / Completed / Compensated / Failed`；步骤按顺序执行，失败后逆序补偿；补偿失败 -> `CompensationFailed` + 告警，不无限重试。

- [ ] **Step 1: 写失败测试**

`saga_success_runs_steps_in_order`；`saga_failure_compensates_in_reverse`；`saga_resume_continues_from_persisted_step`；`compensation_failure_is_reported`；`duplicate_run_is_idempotent`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-saga saga_`
Expected: FAIL，crate 不存在。

- [ ] **Step 3: 实现**

状态存 `SqlExecutor`（SQLite/MySQL 分支）；步骤/补偿要求幂等，`SagaContext` 带 `idempotency_key`；`resume` 读状态续跑；步骤可写 outbox，补偿走业务幂等接口；不引入工作流引擎。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-saga saga_ && cargo check -p ecat --features saga`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml ecat-saga ecat/Cargo.toml ecat/src/reexports.rs
git commit --only Cargo.toml ecat-saga ecat/Cargo.toml ecat/src/reexports.rs -m "feat(saga): add lightweight Saga engine"
```

### Task 36: 2PC / XA 协调器（opt-in，仅 MySQL/PostgreSQL）

**Files:**
- Create: `ecat-data-2pc/Cargo.toml`
- Create: `ecat-data-2pc/src/lib.rs`
- Create: `ecat-data-2pc/src/coordinator.rs`
- Create: `ecat-data-2pc/src/xa.rs`
- Create: `ecat-data-2pc/src/recovery.rs`
- Create: `ecat-data-2pc/src/coordinator/tests.rs`
- Modify: `Cargo.toml`（workspace members）
- Modify: `ecat/Cargo.toml`、`ecat/src/reexports.rs`（feature `2pc`）

**Interfaces:**
- Produces:
  ```rust
  #[async_trait]
  pub trait XaParticipant: Send + Sync {
      async fn xa_start(&self, xid: &str) -> Result<(), XaError>;
      async fn xa_end(&self, xid: &str) -> Result<(), XaError>;
      async fn xa_prepare(&self, xid: &str) -> Result<(), XaError>;
      async fn xa_commit(&self, xid: &str) -> Result<(), XaError>;
      async fn xa_rollback(&self, xid: &str) -> Result<(), XaError>;
      async fn xa_recover(&self) -> Result<Vec<String>, XaError>;
  }
  pub struct RecoveryReport { pub recovered: u32, pub failed: u32 }
  pub struct TwoPhaseCoordinator { /* coordinator db + participants */ }
  impl TwoPhaseCoordinator {
      pub async fn begin(&self, participants: &[(&str, Arc<dyn XaParticipant>)]) -> Result<DistributedTx, XaError>;
      pub async fn recover(&self) -> Result<RecoveryReport, XaError>;
  }
  pub struct DistributedTx { /* id + participants */ }
  impl DistributedTx {
      pub async fn execute(&self, participant: &str, sql: &str, params: &[serde_json::Value]) -> Result<u64, XaError>;
      pub async fn commit(self) -> Result<(), XaError>;
      pub async fn rollback(self) -> Result<(), XaError>;
  }
  ```
  协调日志表 `distributed_tx(id, status, participants, decision, created_at)`；流程 `START/END → PREPARE 全部 → 写 COMMIT 决策 → COMMIT 全部`；任一 prepare 失败 -> 全部 ROLLBACK。恢复扫 `PREPARED/COMMITTING`；MySQL 用 `XA RECOVER`，PostgreSQL 查 `pg_prepared_xacts`。仅 `Dialect::{MySql, Postgres}`；其他方言返回 `XaError::UnsupportedDialect`。

- [ ] **Step 1: 写失败测试**

`all_prepare_success_commits_all`；`prepare_failure_rolls_back_all`；`coordinator_crash_after_prepare_recovers_commit`；`commit_and_rollback_are_idempotent`；`xa_recover_lists_in_doubt`（fake participant）；`unsupported_dialect_is_rejected`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-2pc xa_`
Expected: FAIL，crate 不存在。

- [ ] **Step 3: 实现**

协调器状态机 + 恢复 worker；xid 用 UUID；不把 2PC 接进 `KeyedClient`/`scatter`，只作为显式 API；coordinator 库单点，文档要求外部 HA；恢复 worker 启动时用 `FencedLock` 保证单实例；监控 prepared 事务。MySQL 8.4/9 的 XA 语法以目标版本实测；PostgreSQL 要求 `max_prepared_transactions > 0`。默认 feature 关闭。

**连接作用域是硬约束**：XA 的 `START/END/PREPARE/COMMIT` 必须落在同一条物理连接上（和 `LAST_INSERT_ID()` 同类问题）。`XaParticipant` 实现必须用 `SqlxClient::pool().acquire()` 固定一条连接，并在这条连接上执行整个 XA 生命周期；不得走池化的 `execute_with`/`query_with`。`ecat-data-2pc` 因此依赖 `ecat-data-sqlx` + `sqlx`（feature `sqlx`），其他后端返回 `UnsupportedDialect`。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-2pc xa_ && cargo check -p ecat --features 2pc`
Expected: PASS；MySQL/PG live 用例用 `ECAT_TEST_*_URL` 门控。

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml ecat-data-2pc ecat/Cargo.toml ecat/src/reexports.rs
git commit --only Cargo.toml ecat-data-2pc ecat/Cargo.toml ecat/src/reexports.rs -m "feat(2pc): add opt-in XA coordinator"
```

### Task 37: Redis cache + lock 共享连接池

**Files:**
- Modify: `ecat-data-redis/Cargo.toml`（`deadpool = { workspace = true, features = ["rt_tokio_1"] }`）
- Create: `ecat-data-redis/src/redis_pool.rs`
- Create: `ecat-data-redis/src/redis_pool/tests.rs`
- Modify: `ecat-data-redis/src/lib.rs`（`RedisCache` / `RedisLock` 改为 `RedisBackend`，保留旧构造器）

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, serde::Deserialize)]
  #[serde(deny_unknown_fields)]
  pub struct RedisPoolConfig {
      pub url: String,
      #[serde(default)] pub password: Option<String>,
      #[serde(default)] pub tls: Option<TlsClientConfig>,
      #[serde(default)] pub query_timeout_secs: Option<u64>,
      #[serde(default)] pub max_size: Option<usize>,          // 默认 10
      #[serde(default)] pub acquire_timeout_secs: Option<u64>, // 默认 30
      #[serde(default)] pub create_timeout_secs: Option<u64>,  // 默认 30
      #[serde(default)] pub recycle_timeout_secs: Option<u64>, // 默认 30
  }
  #[derive(Clone)]
  pub struct RedisPool { /* Arc<deadpool::managed::Pool<RedisConnectionManager>> */ }
  impl RedisPool {
      pub async fn connect(cfg: RedisPoolConfig) -> Result<Self, Error>;
      pub fn cache(&self) -> RedisCache;
      pub fn lock(&self) -> RedisLock;
      pub fn status(&self) -> deadpool::Status;
  }
  impl RedisCache { pub fn from_pool(pool: RedisPool) -> Self; }
  impl RedisLock  { pub fn from_pool(pool: RedisPool) -> Self; }
  ```
  内部 `enum RedisBackend { Single(MultiplexedConnection), Pooled(RedisPool) }`；`cache()` / `lock()` 克隆同一个 `RedisPool`（内部 `Arc`）。`connect` / `from_config` / `from_connection` 保持原签名，走 `Single` 兼容路径。`max_size` 等池参数在 `RedisPoolConfig`，不碰 `RedisConfig`。

- [ ] **Step 1: 写失败测试**

`pool_config_defaults_are_sane`（10 / 30 / 30 / 30）；`from_pool_config_rejects_bad_url`；`cache_and_lock_share_one_pool`（两者内部的 `RedisPool` 指向同一个 `Arc`）；`old_connect_and_from_config_still_compile`（旧签名不变）；`pool_status_reports_size`；live 用例 `pool_handles_concurrent_cache_and_lock` 用 `ECAT_TEST_REDIS_URL` 门控。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-redis pool_`
Expected: FAIL，`cannot find type RedisPoolConfig`。

- [ ] **Step 3: 实现**

`RedisConnectionManager` 实现 `deadpool::managed::Manager`：`create` 用 `redis::Client::get_multiplexed_async_connection`，`recycle` 用 `PING`。`RedisConnGuard` 对 `Single` / `Pooled` 枚举 `DerefMut` 到 `MultiplexedConnection`，所有命令代码改走 `self.conn().await?`，不复制两套命令逻辑。`RedisPool::connect` 只建池（deadpool 惰性建连）；真正 I/O 在首次取连接。`cache()` / `lock()` 把 `RedisPoolConfig.query_timeout_secs` 分别写进 `RedisCache` / `RedisLock` 的超时字段；池的 acquire/create/recycle 由 deadpool 管。`from_config` / `connect` 仍走单连接，行为不变；`from_pool_config` / `from_pool` 才启用共享池。共享池是默认用法；如果 cache 与 lock 需要故障隔离，调两次 `RedisPool::connect` 建两个池即可。池指标用 `status()` 暴露，由应用注册，不在本 crate 自动注册 Prometheus。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-redis`
Expected: PASS（live 用例未设 `ECAT_TEST_REDIS_URL` 时 skip）。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-redis
git commit --only ecat-data-redis -m "feat(redis): add shared cache and lock connection pool"
```

---

### Task 38: ClickHouse `ColdQuery` 适配器（执行期补充）

> Discovered during Task 13 review: design §7.5 requires `TieredQuery` to read the cold layer from ClickHouse, but the original plan only defined the `ColdQuery` trait. This task adds the concrete adapter. Depends on Task 11 and Task 13.

**Files:**
- Create: `ecat-data-tiering/src/query/clickhouse.rs`
- Create: `ecat-data-tiering/src/query/clickhouse/tests.rs`
- Modify: `ecat-data-tiering/src/query.rs`（`#[cfg(feature = "clickhouse")] pub mod clickhouse;` + re-export）
- Modify: `ecat-data-tiering/Cargo.toml`（reuse the existing optional `ecat-data-clickhouse` / `clickhouse` feature）

**Interfaces:**
- Produces:
  ```rust
  #[cfg(feature = "clickhouse")]
  pub struct ClickhouseColdQuery { /* client + validated table/time_col/id_col */ }
  #[cfg(feature = "clickhouse")]
  impl ClickhouseColdQuery {
      pub fn new(client: ecat_data_clickhouse::ClickhouseClient, table: String, time_col: String, id_col: String) -> Result<Self, TieringError>;
  }
  #[cfg(feature = "clickhouse")]
  impl ColdQuery for ClickhouseColdQuery { /* query_range */ }
  pub fn build_range_sql(table: &str, time_col: &str, id_col: &str, from: &str, to: &str, limit: u64, offset: u64) -> Result<String, TieringError>;
  ```
  `query_range` must return rows **PK-ascending before applying limit/offset** (the precondition documented in Task 13). `ClickhouseClient` has no parameter binding, so all identifiers go through `validate_ident`, and values are rendered with the existing `render_sql_value`; no arbitrary SQL fragments. Query: `SELECT * FROM <table> WHERE <time_col> >= <from> AND <time_col> < <to> ORDER BY <id_col> ASC LIMIT <limit> OFFSET <offset>`.

- [ ] **Step 1: 写失败测试**

`clickhouse_cold_query_builds_validated_sql`（table/time_col/id_col、from/to、limit/offset 都出现在固定位置）；`clickhouse_cold_query_rejects_bad_ident`；`clickhouse_cold_query_escapes_time_values`；`clickhouse_cold_query_orders_by_pk_before_limit`；live 用例 `clickhouse_cold_query_roundtrip` 用 `ECAT_TEST_CLICKHOUSE_URL` 门控（未设置时 skip）。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-tiering --features clickhouse clickhouse_cold_query_`
Expected: FAIL，`cannot find type ClickhouseColdQuery`。

- [ ] **Step 3: 实现**

用 `validate_ident` 校验 table/time_col/id_col；用 `render_sql_value` 转义 from/to；`ClickhouseClient::query` 执行；把返回的 `Vec<Row>` 原样交给 `TieredQuery`。不要新增参数绑定抽象；标识符和值都必须白名单/转义。MSSQL/SQLite/MySQL 不走这个适配器。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-tiering --features clickhouse clickhouse_cold_query_`
Expected: PASS（live 用例未设 env 时 skip）。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-tiering docs/superpowers/plans/2026-10-24-data-layer-scaleout.md
git commit --only ecat-data-tiering docs/superpowers/plans/2026-10-24-data-layer-scaleout.md -m "feat(tiering): add ClickHouse ColdQuery adapter"
```

---

### Task 39: `ecat-tier` CLI 运行接线（执行期补充）

> Discovered during Task 14 review: `run|verify|restore` parse config then return an explicit error. This task wires `run` and `verify`; `restore` remains an explicit "load staging first" workflow because the cold sinks have no read API. Depends on Tasks 9–14, 38.

**Files:**
- Modify: `ecat-data-tiering/src/bin/ecat-tier.rs`
- Create: `ecat-data-tiering/src/cli.rs`
- Create: `ecat-data-tiering/src/cli/tests.rs`
- Modify: `ecat-data-tiering/Cargo.toml`（feature `cli` enables `clap`, `ecat-data-sqlx`, `clickhouse`/`s3` sinks as selected）

**Interfaces:**
- Produces:
  ```rust
  #[derive(serde::Deserialize)]
  #[serde(deny_unknown_fields)]
  pub struct CliConfig {
      pub database: ecat_data_sqlx::SqlxConfig,
      pub tiering: crate::TieringConfig,
  }
  pub async fn run_cli(config: CliConfig, command: Command, dry_run: bool) -> Result<(), TieringError>;
  ```
  `run`: build `SqlxClient` from `database`; for each table, build the `ClickhouseSink`/`S3Sink` from `TieringConfig`/feature; build `ArchiveJob`; call `run_once` (or `run_until`). `verify`: load `tier_archive_jobs` rows and call `ColdSink::verify_chunk`; report mismatches. `restore`: keep the explicit error that staging must be loaded by the operator via `restore_partition` after the cold data is in staging.
- CLI tests: parse config; `plan` unchanged; `run`/`verify` dispatch with a fake executor/sink behind a trait; no live DB.

- [ ] **Step 1: 写失败测试**

`cli_config_parses_database_and_tiering`；`cli_run_dispatches_to_archive_job`（fake hot/cold）；`cli_verify_reports_checksum_mismatch`；`cli_restore_still_requires_staging`；`cli_rejects_unknown_key`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-tiering --features cli cli_`
Expected: FAIL。

- [ ] **Step 3: 实现**

Config: reuse `SqlxConfig` + `TieringConfig`; no new DB abstraction. Sink selection by `TableTieringConfig.target` and feature. Keep `restore` explicit (cold sinks have no read API; `RestoreSource` is a caller-side loader). Do not add a general CLI framework.

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-tiering --features cli cli_`
Expected: PASS（no live DB）。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-tiering docs/superpowers/plans/2026-10-24-data-layer-scaleout.md
git commit --only ecat-data-tiering docs/superpowers/plans/2026-10-24-data-layer-scaleout.md -m "feat(tiering): wire ecat-tier run and verify"
```

---

### Task 40: `App` 后台 worker 接线（执行期补充）

> Discovered during Task 26 review: `ArchiveJob::run_until` has no production caller; the Task 26 brief's Files list excluded `ecat/`. This task adds a generic `AppBuilder` background-worker hook so the shutdown path is usable in a real app.

**Files:**
- Create: `ecat/src/worker.rs`
- Create: `ecat/src/worker/tests.rs`
- Modify: `ecat/src/lib.rs`
- Modify: `ecat/Cargo.toml`（optional `ecat-data` dependency, feature `worker`）

**Interfaces:**
- Produces:
  ```rust
  #[cfg(feature = "worker")]
  pub struct WorkerHandle { /* ShutdownHandle + JoinHandle */ }
  #[cfg(feature = "worker")]
  impl ecat::AppBuilder {
      pub fn background_worker<F, Fut>(self, make: F) -> Self
      where
          F: FnOnce(ecat_data::Shutdown) -> Fut + Send + 'static,
          Fut: std::future::Future<Output = ()> + Send + 'static;
  }
  ```
  `background_worker` registers an `on_start` hook that spawns `make(shutdown)`, and an `on_stop` hook that calls `shutdown()` then awaits the task (bounded by the worker's own run deadline). It is generic over the future, so `ArchiveJob::run_until` can be passed without `ecat` depending on `ecat-data-tiering` by default. The feature is default-off; `worker = ["dep:ecat-data"]`.

- [ ] **Step 1: 写失败测试**

`background_worker_starts_and_stops`（fake future waits on shutdown; `App` start/stop hooks or the helper's `stop` path; assert it exits）；`background_worker_is_additive_default_off`（default build unchanged）；`worker_handle_shutdown_is_idempotent`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat --features worker worker_`
Expected: FAIL。

- [ ] **Step 3: 实现**

Use `ecat_data::{Shutdown, ShutdownHandle}`; `on_start` spawns; `on_stop` calls `shutdown()` and awaits. Do not add a new scheduler; the worker future owns its loop. No production unwrap.

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat --features worker worker_ && cargo check -p ecat`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat docs/superpowers/plans/2026-10-24-data-layer-scaleout.md
git commit --only ecat docs/superpowers/plans/2026-10-24-data-layer-scaleout.md -m "feat(ecat): add background worker lifecycle hook"
```

---

### Task 41: 归档延迟健康检查（执行期补充）

> Discovered during Task 27 review: `TieringHealthCheck` could only probe the cold sink; the design requires archive-lag detection. This task adds a real lag query to the state store and wires it into the health check. Depends on Task 9 and Task 27.

**Files:**
- Modify: `ecat-data-tiering/src/state.rs`
- Modify: `ecat-data-tiering/src/state/tests.rs`
- Modify: `ecat-data-tiering/src/health.rs`
- Modify: `ecat-data-tiering/src/health/tests.rs`

**Interfaces:**
- Produces:
  ```rust
  impl ArchiveStateStore {
      /// Latest `updated_at` for a table (any partition/chunk), or None if no state rows.
      pub async fn latest_updated_at(&self, table: &str) -> Result<Option<time::OffsetDateTime>, TieringError>;
  }
  #[cfg(feature = "health")]
  impl TieringHealthCheck {
      pub fn with_archive_lag(self, store: std::sync::Arc<ArchiveStateStore>, max_lag: time::Duration) -> Self;
  }
  ```
  `TieringHealthCheck::check` still probes the cold sink; when an archive-lag store is configured, it also fails if `now - latest_updated_at(table) > max_lag` for any configured table. No state rows -> Ok (no archive history yet). Keep `probe_timeout` separate from `max_archive_lag`.

- [ ] **Step 1: 写失败测试**

`archive_lag_recent_state_is_ok`；`archive_lag_old_state_is_error`；`archive_lag_no_state_rows_is_ok`；`archive_lag_state_query_error_propagates`；`tiering_health_without_archive_lag_keeps_reachability_only`.

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ecat-data-tiering --features health health_`
Expected: FAIL。

- [ ] **Step 3: 实现**

`latest_updated_at` uses a parameterized `SELECT MAX(updated_at) FROM <state table> WHERE table_name = ?` (or the state table's constant); parse RFC3339. `TieringHealthCheck` holds `Option<(Arc<ArchiveStateStore>, Duration, Vec<String>)>`; check cold first, then lag. No new dependencies.

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ecat-data-tiering --features health health_ && cargo test -p ecat-data-tiering state_`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add ecat-data-tiering docs/superpowers/plans/2026-10-24-data-layer-scaleout.md
git commit --only ecat-data-tiering docs/superpowers/plans/2026-10-24-data-layer-scaleout.md -m "feat(tiering): add archive-lag health check"
```

---

### Task 42: 分片备份/恢复演练脚本（执行期补充）

> Discovered during Task 30 review: the distributed acceptance item "per-shard backup + binlog position + restore drill" had no artifact. Depends on Task 23/30.

**Files:**
- Create: `scripts/deploy/sharded-backup-drill.sh`
- Test: `scripts/deploy/sharded-backup-drill.sh --check`

**Interfaces:**
- `--check` validates the script contains: per-shard `mysqldump`/snapshot step, binlog position capture (`SHOW BINARY LOG STATUS` for 8.4/9; `SHOW MASTER STATUS` only for 8.0), restore into a scratch schema, and checksum verification. Prints `sharded-backup-drill: ok`.
- `--run` is env-gated by `SHARD_DSNS` (comma-separated) and `RESTORE_DSN`; no live run in CI. Secrets stay out of argv (use a defaults file or `MYSQL_PWD` with a documented /proc caveat).
- No `SHOW SLAVE STATUS`, no `mysql_native_password`; table/shard identifiers validated.

- [ ] **Step 1: 写失败检查**

Run: `bash scripts/deploy/sharded-backup-drill.sh --check`
Expected: FAIL，`No such file or directory`。

- [ ] **Step 2: 实现**

Script contains a `--check` marker list, read-only backup commands, binlog position capture, restore/verify steps, and a clear failure if `--run` lacks env. Do not execute destructive DDL during `--check`.

- [ ] **Step 3: 验证**

Run: `bash scripts/deploy/sharded-backup-drill.sh --check`
Expected: `sharded-backup-drill: ok`。
Run: `grep -c 'SHOW SLAVE STATUS' scripts/deploy/sharded-backup-drill.sh`
Expected: `0`。

- [ ] **Step 4: 提交**

```bash
git add scripts/deploy/sharded-backup-drill.sh docs/superpowers/plans/2026-10-24-data-layer-scaleout.md
git commit --only scripts/deploy/sharded-backup-drill.sh docs/superpowers/plans/2026-10-24-data-layer-scaleout.md -m "chore(deploy): add sharded backup/restore drill"
```

---

## 验收清单

- [ ] `cargo test --workspace` 全绿；`cargo check --workspace --no-default-features` 通过。
- [ ] `ecat-data --test public_api_smoke` 通过；feature 全关时 SQL/行/错误与基线一致。
- [ ] P1：副本延迟超阈值时读不落该副本；`primary_only` 读主；锁续租 token 校验通过。
- [ ] P2：归档 100% 校验和一致、0 重复；kill -9 后各阶段可续跑；verify 失败不 drop；冷读边界行只出现一次。
- [ ] P3：代理配置校验拒绝多端点/Client 模式；业务 SQL 零修改经代理跑通（需外部环境）。
- [ ] P4：无键/未知表报错；六方法同分片；scatter 部分失败返回错误；Snowflake 并发唯一。
- [ ] P5：outbox 并发 claim 不重复；重放幂等。
- [ ] MySQL 8.4/9：`caching_sha2_password` TLS/非 TLS 连接通过；8.4/9 上不出现 `SHOW SLAVE STATUS`；9.x 无 `mysql_native_password` 账号。
- [ ] MySQL 8.4/9：`RANGE COLUMNS` 分区 DDL 在目标版本实测；gh-ost ≥1.1.8（8.4）或已确认 9.x 替代方案。
- [ ] P6：standalone 禁止 sharding、distributed 要求 node_id/worker_id；单机与分布式跑同一套 CRUD/ORM 结果一致。
- [ ] P6：归档/outbox worker 收到 shutdown 后 100ms 内退出；readiness 覆盖分片/冷层/outbox/归档。
- [ ] P6：实例数 × 分片池总量超预算启动失败；outbox relay 幂等重放；拓扑 reload 后新键走新分片、旧句柄不受影响。
- [ ] P7：fencing 单调递增、陈旧 fence 拒绝；Redis cache/lock 共享池上限生效，旧单连接路径不变。
- [ ] P7：Consul/etcd 拓扑 load/watch、坏 JSON fail-closed、失败保留最后已知。
- [ ] P7：分片迁移 plan/backfill/verify/cutover/rollback 通过，checksum 不一致不切流。
- [ ] P7：Saga 成功/逆序补偿/resume/补偿失败告警通过；2PC 全准备提交、部分失败回滚、协调器崩溃恢复、commit 幂等。
- [ ] P7：2PC 仅 MySQL/PostgreSQL，默认 feature 关闭；`KeyedClient`/`scatter` 不自动使用 2PC。
