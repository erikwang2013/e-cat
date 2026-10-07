# 批次 5a 实施计划：出站韧性试点 —— 泛型超时助手 + Redis + ClickHouse

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `run_with_timeout` 泛型化（多一个「后端类别」维度、支持 `ecat_errors::Error`），并在 **Redis**（非 HTTP 客户端路径）与 **ClickHouse**（同时实现 `RdbmsClient` 与 `TsdbClient` 的最难 case）两个后端上试点「超时 + 熔断 + 指标」的完整接法；产出一份供 5b 逐 crate 照做的接入 checklist。

**Architecture:** 三个非 RDBMS trait 里，Redis 走 `Cache`、ClickHouse 走 `SqlExecutor` + `TsdbClient`，都是 `ecat-data` 已有 trait 的**实现体内**包装（不是 opt-in 包装器），用户代码零改动。`ecat-data` 只出**泛型助手**（`BackendKind` / `TimeoutError` / `run_with_timeout` / `TIMEOUTS`）与**熔断器错误映射**；`ecat-circuit-breaker::Breaker` 逐 client 持有一个（批次 4 已公开，直接复用）。指标按 `BackendKind` 分维度、按 opt-in `metrics` feature 注册。

**Tech Stack:** `ecat-data`（`SqlExecutor` / `RdbmsError` / `Cache` / `TsdbClient`）· `ecat-errors`（`Error` / `ErrorCode`）· `ecat-circuit-breaker`（`Breaker` / `BreakerConfig` / `BreakerState` / `BreakerError`）· `ecat-metrics` + `prometheus`（opt-in）· `tokio`（`time::timeout`、`sync::Semaphore`）· `redis` 0.27 · `reqwest` 0.12。

**设计依据:** `docs/superpowers/specs/2026-10-06-outbound-resilience-design.md` §1–§4、§6、§7.5、§8；写法与严格度参照 `docs/superpowers/plans/2026-10-07-batch4-pool-and-observability.md`。

---

## ⚠️ 与 spec 的实测出入（**先读这节，不要按 spec 原文照做**）

写本计划时逐条实测。**数量对不上的一律以实测为准**，并在下文据此展开。

### 出入 1：版本 —— spec 说的「4.0.0 窗口」已过期

| | |
|---|---|
| spec §1 原文 | 「与批次 1 的 4.0.0 一同发布即可（`ecat-data` 尚未进入 4.0.0 发布窗口）」 |
| 实测（2026-10-07） | `Cargo.toml:70` `version = "5.0.0"`；`git log` 显示 `4.1.0 → 5.0.0` 的 bump 已提交（`2ed667a`）。**4.0.0 与 5.0.0 都已发布** |

**结论**：本次签名变更进**下一个版本 = 6.0.0**（`run_with_timeout` 签名变更 + `QUERY_TIMEOUTS` 删除都是破坏性的）。本批执行 bump（Task 7）—— 与批次 4 的先例一致（破坏性变更与 bump 同批），且 `main` 不能长期挂着「标着 5.0.0 的破坏性变更」。

### 出入 2：调用点不是 16 个，是 **10 个生产 + 3 个测试 = 13 处**

spec §1 的表按「4 个客户端方法 × 2 个 crate + 4 个事务方法 × 2 个 crate = 16」计数。**实测两个客户端侧各只有一个调用点** —— 它们早就有共享的 `timed()` 外壳：

| 文件:行 | 数量 | 说明 |
|---|---|---|
| `ecat-data-sqlx/src/lib.rs:152` | **1**（spec 说 4） | `SqlxClient::timed()` 辅助函数，四个客户端方法都走它 |
| `ecat-data-sqlx/src/transaction.rs:37,48,61,94` | 4 | 宏 `tx_wrapper!` 内的四个方法（宏在 `:141-143` 展开 3 次，但只有 4 行文本要改） |
| `ecat-data-mssql/src/client.rs:121` | **1**（spec 说 4） | `MssqlClient::timed()` 辅助函数，同上 |
| `ecat-data-mssql/src/client.rs:288,296,304,314` | 4 | `MssqlTransaction` 的四个方法 |
| `ecat-data/src/timeout.rs:47,54,61` | 3 | **spec 没数**：`timeout.rs` 自己的测试调用 |

实测命令与输出（`rg -n 'run_with_timeout' --type rust`，去掉 `use`/`pub use`/文档注释行）：
`ecat-data/src/timeout.rs` 定义 1 处 + 测试 3 处；`ecat-data-sqlx` 5 处（含 1 处 import）；`ecat-data-mssql` 5 处（含 1 处 import）。**注释里提到它的还有 4 处**（`sqlx/src/tracing.rs:12`、`sqlx/src/tests.rs:364`、`mssql/src/tests.rs:440`、`mssql/src/client.rs:264,286`）—— 这些不改。

**spec 还漏了一项**：`QUERY_TIMEOUTS` 的**消费者**（见出入 3）。

### 出入 3：`TIMEOUTS` 与既有的 `QUERY_TIMEOUTS` 不能并存

spec §1 引入 `TIMEOUTS: [AtomicU64; 7]`，但没说既有的 `QUERY_TIMEOUTS`（`ecat-data/src/timeout.rs:11`）怎么办。**留着就是一颗哑弹**：它不再被递增，却仍是公开 API，读它的人永远拿到冻结值。

实测消费者（都在 `metrics` feature 门控下）：

| 文件:行 | 用法 |
|---|---|
| `ecat-data-sqlx/src/metrics.rs:20,136` | `use` + `count(QUERY_TIMEOUTS.load(..))` |
| `ecat-data-mssql/src/metrics.rs:24,152` | 同上 |

（两处的 `:281-287` / `:323-329` 是测试里的 `fetch_add`/`load`，也要一起改。）

**结论**：删除 `QUERY_TIMEOUTS`，两个消费者改读 `TIMEOUTS[BackendKind::Rdbms as usize]`。⚠️ **这两处只在 `--features metrics` 下才编译** —— 默认 `cargo test -p ecat-data-sqlx` 根本碰不到它们，改坏了也是绿的。Task 1 有专门一步带 feature 验证。

### 出入 4：**嵌套顺序与 spec 相反**（本批最重要的判断）

spec §3 定的是「超时在外、熔断在内」：

```rust
run_with_timeout(kind, t, async { self.breaker.call(|| inner).await.map_err(..) }).await
```

**这个顺序有两个实测可推导的问题**（源码证据在 `ecat-circuit-breaker/src/breaker.rs`）：

1. **熔断器会对「卡死的后端」永久失明。** 超时触发时 `tokio::time::timeout` 会 **drop 掉内层 future**；此时 `Breaker::call` 正停在 `let result = f().await;`（`:113`），它后面那句 `inner.window.record(false)`（`:130`）**永远不会执行**。于是每次「后端在预算内没答」都是一次 **drop**，窗口里什么都不记 —— 一个每次都挂满超时的后端，熔断器**永远不开**，所有请求继续排队打过去。而「一个卡死的 Redis GET 会无限期挂着」（spec §背景）恰恰是本设计要防的头号场景。

2. **半开探测会漏，漏满 3 次熔断器永久锁死。** 半开分支先占用探测名额再调用（`:86-92`：`half_open_count += 1` 发生在 `f().await` **之前**），而名额的归还只发生在 `f()` 返回之后的记录逻辑里。future 被 drop ⇒ 名额**有借无还**。`half_open_probes` 默认 3，于是三次被超时掐断的半开探测之后，`:88` 的 `half_open_count >= half_open_probes` 恒成立 ⇒ **每次调用立即返回 `ProbesExhausted`，状态永远停在 `HalfOpen`**（没有任何路径能从 `HalfOpen` 退出而不经过一次成功/失败的记录）。这是**永久性故障**。

3. spec §3 给的**理由与它自己的代码相反**：原文说「若熔断在内，被超时掐断的调用仍会被记为一次待定/失败」。实测正好相反 —— 熔断在内时被掐断的调用**一次都不记**；是「熔断在外」才会把超时记成一次失败。

**结论**：本计划采用 **熔断在外、超时在内**：

```rust
self.breaker.call(|| run_with_timeout(kind, t, fut)).await.map_err(..)
```

- 超时变成一次普通的 `Err`，被熔断器**如实记为后端失败** ⇒ 卡死的后端能打开熔断器（这才是本设计的目的）；
- 没有 future 被 drop ⇒ 半开探测名额不漏、不会锁死；
- 语义同一句话说得清：**熔断器数的是「我们是否在预算内拿到一个可接受的答复」，超时就是「没有」。**

> **需 lead 裁决。** 这一条**偏离已批准的 spec**。若 lead 裁决维持 spec 的顺序（超时在外），则**必须先修 `ecat-circuit-breaker`**：给半开探测名额加 RAII 归还（drop 时 `half_open_count -= 1`，成功/失败记录后 disarm），否则问题 2 会在 5b 被复制 11 份。问题 1 无解 —— 那正是该顺序的固有语义。**本计划按「熔断在外」展开。**

### 出入 5：`ecat_outbound_breaker_open_total` 在 `Breaker` 上**没有数据源**

spec §4 要求注册 `ecat_outbound_breaker_open_total`（counter，维度 `backend`），但实测 `Breaker` 的公开面（`ecat-circuit-breaker/src/breaker.rs`）只有 `new` / `call` / `state` / `lock`(pub(crate))。状态迁移只写 `tracing::warn!`（`:135-141`、`:150-152`），**没有计数器**。

用 `state()` 轮询猜测是错的（轮询间隔决定准确性，且熔断器可能开又关再没人抓）。**结论**：Task 2 给 `Breaker` 加 `opened_total()`，在两处 `state = BreakerState::Open` 之后 +1。

### 出入 6：ClickHouse 的「两条路径」不是 `RdbmsClient` + `TsdbClient`

spec §7.5 说 ClickHouse「同时实现 `RdbmsClient` 与 `TsdbClient`，两条路径都要包」。实测 `RdbmsClient` 只有一个方法且 ClickHouse 的实现是**硬编码常量错误**：

```rust
// ecat-data-clickhouse/src/lib.rs:296-300
async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
    Err(RdbmsError::Database("ClickHouse does not support transactions".into()))
}
```

它**不含任何 I/O**，包进熔断器只会让一次「本就不支持的调用」被记成后端失败。**结论**：`transaction()` 不包。真正要包的是**两条做 I/O 的路径**：`SqlExecutor`（`execute` / `query`）与 `TsdbClient`（`write` / `query` / `delete`）。两个路径**共用一个 `Breaker`**（同一个服务器、同一个故障域）。

### 出入 7：ClickHouse 的 `_with` 方法 —— 正确的处理是「什么都不做」

spec §7.5 说它的 `_with` 方法「本就不支持（落到默认错误），超时/熔断要在这个前提下仍然语义正确」。**实测这个前提下语义天然正确**：`ClickhouseClient` 根本没有覆写 `execute_with` / `query_with` / `query_write`（`ecat-data-clickhouse/src/lib.rs:229-292` 只实现了 `execute` / `query` / `dialect`），调用会落到 trait 默认实现（`ecat-data/src/rdbms.rs:201-217`），返回 `RdbmsError::Database("parameterized execute not supported by this backend")` —— **默认实现不经过本 crate 的任何代码，所以永不触碰熔断器**。

**结论**：不加代码，**加一条把守测试**（`unsupported_with_methods_do_not_trip_the_breaker`）。这条测试是防回归的：将来有人"顺手"给 ClickHouse 补 `_with`，会把「不支持」喂给熔断器。

### 出入 8：ClickHouse 已经有 reqwest 的 30 秒总超时（但只在部分构造器上）

`ecat-tls/src/lib.rs:83-84` 与 `:97-98` 两个分支都设了 `.connect_timeout(5s).timeout(30s)`，而 `ClickhouseClient::from_config`（`:72-84`）走的就是它。但 `ClickhouseClient::new()`（`:43`）与 `with_auth()`（`:55`）用的是**裸 `reqwest::Client::new()` —— 一个超时都没有**。

**结论**：新加的外层 `run_with_timeout` 与 reqwest 内层**共存**，`from_config` 建出的 client 实际预算是两层取先到者。要写进 rustdoc。测试必须让两层可区分：`query_timeout_secs: 1` + mock 睡 5 秒 —— 外层 1 秒先开火，返回 `RdbmsError::Timeout`（reqwest 那层只会给 `RdbmsError::Database`，且它 30 秒根本来不及）。**若外层没接上，mock 5 秒后返回成功 ⇒ 断言失败**，不是空验收。

### 出入 9：`RedisLock` 不在 5a 范围

`ecat-data-redis` 还有一个 `impl DistributedLock for RedisLock`（`:196-242`）。它**不在** spec「六个共用 `Error` 的 trait」清单里（用 `LockError`），spec §7 也没点名。**结论**：5a 只包 `Cache` 路径，`RedisLock` 不动。这是**范围决策，不是遗漏**。

### 出入 10：README 的「超时/熔断」列已存在，5a 不动它

批次 4 已加该列（`README.md:136-157`），且 `ClickHouse` / `QuestDB` 两行现在写着 `✅ 熔断` —— 那指的是「可被 `ecat_data::CircuitBreakerExecutor` 包装」，不是内置。5a 后 ClickHouse 变成**内置**，但表格另 16 行 5b 才动。**结论**：表格更新整体留给 5b（只改 2 行的表在 14 份 README 镜像间同步一次、5b 再同步一次，是白干一遍）。

---

## 已核实的事实（**不要重新猜**）

均为写本计划时实测。

### `ecat-data` 现状

```
ecat-data/src/timeout.rs    70 行   ← 本批重写
ecat-data/src/breaker.rs   318 行   ← 加两个公开映射函数
ecat-data/src/lib.rs        25 行   ← 私有 mod + pub use（不是 pub mod）
ecat-data/Cargo.toml                ← 无 [features] 段；已依赖 ecat-errors + ecat-circuit-breaker
```

- **公开面**：`pub use timeout::{QUERY_TIMEOUTS, TRANSACTIONS_LEAKED, run_with_timeout};`（`:24`）
- `ecat_errors::Error` 是**结构体**（非 enum）：`Error::new(code, reason, message)`（`ecat-errors/src/lib.rs:25`），字段 `code` / `reason` / `message` / `cause` / `metadata`
- `ecat_errors::ErrorCode` 是 `ecat_protos::errors::ErrorCode` 的重导出（`ecat-errors/src/lib.rs:5`）。可用值：`InvalidArgument`=1001 … `Unavailable`=1008、**`DeadlineExceeded`=1009**（`ecat-protos/proto/errors.proto`）
- `ecat-data/src/breaker.rs:29-35` 已有 `pub(crate) fn map_breaker_error(BreakerError<RdbmsError>) -> RdbmsError` —— **本批要把它改成 `pub`**，Redis/ClickHouse 才用得上

### `ecat-circuit-breaker`（批次 4 已抽取，**直接复用**）

```rust
// ecat-circuit-breaker/src/lib.rs:12
pub use breaker::{Breaker, BreakerConfig, BreakerError, BreakerState};
```

- `Breaker::new(BreakerConfig)` / `call<F, Fut, T, E>(&self, f: F) -> Result<T, BreakerError<E>>`（`breaker.rs:70`）/ `state() -> BreakerState`（`:175`）
- `BreakerConfig` 默认 **0.5 / 30s / 3 / 10s**（`breaker.rs:22-30`）
- `BreakerError<E>` 有**三个**变体：`Open`、`ProbesExhausted`、`Inner(E)`（`:190-199`）
- `state()` 在冷却期已过时**报告** `HalfOpen` 但不改内部状态（`:175-190`）—— 指标 gauge 直接用它是安全的
- `Breaker::call` 要求 `T: 'static`、`E: std::fmt::Display`（`:74-75`）

### `ecat-data/src/routing.rs` 的既有接法（照抄它）

```rust
self.breaker
    .call(|| self.client.execute(sql))
    .await
    .map_err(map_breaker_error)
```

闭包**按需构造 future**（熔断打开时内层根本没被碰）。逐端点各一个 `Breaker`。**Endpoint 层不套 `run_with_timeout`** —— 超时在更内层的客户端里，本批沿用这个分层。

### 两个后端 crate 现状

```
ecat-data-redis/src/lib.rs        395 行（含内联 `mod tests` 约 150 行；12 个测试）
ecat-data-clickhouse/src/lib.rs   450 行   ← 紧贴 500 行硬规则
ecat-data-clickhouse/src/tests.rs 425 行（24 个测试，`#[cfg(test)] mod tests;` 挂在 lib.rs:449-450）
```

- 两个 crate **都没有 `[features]` 段**，都**没依赖** `ecat-circuit-breaker` / `ecat-metrics`
- `ecat-data-clickhouse` 的 `tokio` **只在 `[dev-dependencies]`** —— 要用 `Semaphore` 必须先加进 `[dependencies]`
- ClickHouse 测试用的是 **进程内 axum mock**：`spawn_mock(captured, status, body, summary_header)`（`tests.rs:144-195`），第 4 个参数是响应头、**不是延迟**
- Redis 测试全部是「连不上就报错」型（`redis://nonexistent:9999`），没有可控的假服务端

### 配置字段的既有约定（**照抄，5b 也照它**）

`ecat-data-sqlx/src/config.rs:37-40` + `:146-154`：

```rust
/// `0` = 禁用查询超时。
#[serde(default)]
pub query_timeout_secs: Option<u64>,
```
```rust
/// `0` 表示显式禁用超时；未配置时为 30 秒。
pub fn query_timeout(&self) -> Option<Duration> {
    match self.query_timeout_secs {
        None => Some(Duration::from_secs(30)),
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s)),
    }
}
```

**`0` = 关闭，不是「0 秒立刻超时」** —— 这是全仓约定，两个新后端必须一致。

### 全批次的硬约束（沿用批次 3/4，**都实测踩过**）

1. **假绿灯**：新建 `.rs` **先加 `mod` 声明再写内容**。判据是**测试总数增加**，不是「看到 ok」。
2. **空验收**：**把本任务新增的东西整行删掉，验收还会通过吗？会 → 它是空验收。** 已知变体：无测试覆盖 / 无调用点 / 注册表 fallback 共用 / 引用了不存在的东西 / doctest 放在不执行的位置 / 只断言 `contains`。
3. **计划片段不保证过 rustfmt**。落码后跑 `cargo fmt`，`fmt --check` 必须 rc=0。计划管语义，rustfmt 管排版。
4. **每个源文件 < 500 行**（含新文件）。收尾复核。
5. 测试命令用 `cargo test -p <crate>`，**不用 `--workspace`**（全量很慢）。

---

## 文件结构

```
ecat-data/src/
  timeout.rs        重写：BackendKind / TimeoutError / TIMEOUTS / 泛型 run_with_timeout（删 QUERY_TIMEOUTS）
  breaker.rs        已有；两个映射函数改为公开 + 新增一个
  lib.rs            重导出更新

ecat-circuit-breaker/src/
  breaker.rs        已有；加 opened_total()

ecat-data-redis/src/
  lib.rs            RedisCache / RedisLock（变薄）+ 三个新字段 + guarded 外壳
  tests.rs          新建：既有 12 个测试（从 lib.rs 搬出）+ 假 RESP 服务端 + 超时/熔断测试
  metrics.rs        新建（feature = "metrics"）：注册三个出站指标

ecat-data-clickhouse/src/
  lib.rs            ClickhouseClient（变薄）+ 三个新字段 + guarded 外壳 + 并发信号量
  tsdb.rs           新建：TsdbClient 实现（从 lib.rs 抽出，145 行）
  tests.rs          已有（加一行 `mod resilience;`）
  tests/resilience.rs  新建：超时 / 熔断 / 并发上限 / `_with` 不熔断
  metrics.rs        新建（feature = "metrics"）：同 redis 的三指标

docs/superpowers/checklists/
  backend-resilience-onboarding.md   新建：5b 的接入 checklist（Task 5 产出）
```

---

## Task 1: `ecat-data` 泛型超时助手 + 迁移 13 个调用点

**Files:**
- Modify: `ecat-data/src/timeout.rs`（重写）
- Modify: `ecat-data/src/breaker.rs`（两个映射函数公开）
- Modify: `ecat-data/src/lib.rs`
- Modify: `ecat-data-sqlx/src/lib.rs`（`:22` import、`:152`）
- Modify: `ecat-data-sqlx/src/transaction.rs`（`:12` import、`:37,48,61,94`）
- Modify: `ecat-data-mssql/src/client.rs`（`:17` import、`:121,288,296,304,314`）
- Modify: `ecat-data-sqlx/src/metrics.rs`（`:20,136` + 测试 `:281-287`）
- Modify: `ecat-data-mssql/src/metrics.rs`（`:24,152` + 测试 `:323-329`）

**这一个任务必须原子完成** —— 改完 `run_with_timeout` 签名后，13 个调用点一起编译失败，中间态不可编译。

- [ ] **Step 1: 先跑基线，记下四个 crate 的测试数与测试名**

```bash
for p in ecat-data ecat-data-sqlx ecat-data-mssql; do
  echo "=== $p ==="
  cargo test -p $p 2>&1 | grep -E '^test |^test result'
done
cargo test -p ecat-data-sqlx -p ecat-data-mssql --features metrics,health,tracing 2>&1 | grep -E '^test result'
```

**把每个 crate 的测试数与测试名记下来**，Task 末尾要逐条比对。最后一行是 `metrics` 门控代码的基线（默认跑不到它）。

- [ ] **Step 2: 重写 `ecat-data/src/timeout.rs`**

整文件替换为：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use crate::rdbms::RdbmsError;
use ecat_errors::{Error, ErrorCode};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 未提交即 Drop 的事务累计数（`Transaction` 的 Drop guard 递增）。
pub static TRANSACTIONS_LEAKED: AtomicU64 = AtomicU64::new(0);

/// 出站调用的后端类别，用于把超时计数分维度 ——
/// 否则只有一个总数，看不出是哪个后端在出事。
///
/// 判别值即 [`TIMEOUTS`] 的下标，**顺序不可改**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Rdbms = 0,
    Cache = 1,
    Search = 2,
    Graph = 3,
    Document = 4,
    Storage = 5,
    Tsdb = 6,
}

/// 各类后端的累计超时次数，下标即 [`BackendKind`] 的判别值。
///
/// 用进程级静态量而非依赖 `ecat-metrics`：本 crate 保持零外部依赖，
/// 指标的读取方按需接入（各后端的 `metrics` feature）。
///
/// 本量取代了 5.0.0 的 `QUERY_TIMEOUTS`（只覆盖 RDBMS）。
pub static TIMEOUTS: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];

/// 可被超时包装的错误类型。
///
/// 六个非 RDBMS trait（`Cache` / `SearchClient` / `GraphClient` /
/// `DocumentClient` / `StorageClient` / `TsdbClient`）共用 `ecat_errors::Error`，
/// RDBMS 路径用 [`RdbmsError`] —— 两个实现就够，不需要六套。
pub trait TimeoutError: Sized {
    fn from_timeout(d: Duration) -> Self;
}

impl TimeoutError for RdbmsError {
    fn from_timeout(d: Duration) -> Self {
        RdbmsError::Timeout(format!("query exceeded {d:?}"))
    }
}

impl TimeoutError for Error {
    fn from_timeout(d: Duration) -> Self {
        Error::new(
            ErrorCode::DeadlineExceeded,
            "timeout",
            format!("call exceeded {d:?}"),
        )
    }
}

/// 给一次出站调用套一层超时。
///
/// `None` 表示禁用超时，直接透传结果。超时发生时递增 [`TIMEOUTS`] 的对应维度，
/// 并返回 `E::from_timeout`。
///
/// 这是池耗尽的头号防线：`acquire_timeout` 只约束「等连接」，
/// 拿到连接后卡死的查询会一直占着它。
pub async fn run_with_timeout<F, T, E>(
    kind: BackendKind,
    timeout: Option<Duration>,
    fut: F,
) -> Result<T, E>
where
    F: std::future::Future<Output = Result<T, E>>,
    E: TimeoutError,
{
    match timeout {
        None => fut.await,
        Some(d) => match tokio::time::timeout(d, fut).await {
            Ok(result) => result,
            Err(_) => {
                TIMEOUTS[kind as usize].fetch_add(1, Ordering::Relaxed);
                Err(E::from_timeout(d))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdbms::RdbmsError;
    use std::sync::atomic::Ordering;

    /// 该维度的当前值。测试用 `SeqCst`：别的测试也在并发地加同一个槽。
    fn count(kind: BackendKind) -> u64 {
        TIMEOUTS[kind as usize].load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn none_timeout_passes_result_through() {
        let r: Result<u64, RdbmsError> =
            run_with_timeout(BackendKind::Rdbms, None, async { Ok(42) }).await;
        assert_eq!(r.unwrap(), 42);
    }

    #[tokio::test]
    async fn fast_future_completes_within_timeout() {
        let r: Result<u64, RdbmsError> = run_with_timeout(
            BackendKind::Rdbms,
            Some(Duration::from_secs(5)),
            async { Ok(1) },
        )
        .await;
        assert_eq!(r.unwrap(), 1);
    }

    #[tokio::test]
    async fn slow_future_times_out_and_counts() {
        let before = count(BackendKind::Rdbms);
        let r: Result<(), RdbmsError> = run_with_timeout(
            BackendKind::Rdbms,
            Some(Duration::from_millis(10)),
            async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(())
            },
        )
        .await;
        let err = r.unwrap_err();
        assert!(matches!(err, RdbmsError::Timeout(_)), "got: {err:?}");
        assert_eq!(count(BackendKind::Rdbms), before + 1);
    }

    /// `ecat_errors::Error` 路径：必须是 `DeadlineExceeded` 而不是笼统的 `Internal`，
    /// 否则调用方没法把「超时」与「后端报错」分开处理。
    #[tokio::test]
    async fn error_path_maps_to_deadline_exceeded() {
        let r: Result<(), Error> = run_with_timeout(
            BackendKind::Cache,
            Some(Duration::from_millis(10)),
            async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(())
            },
        )
        .await;
        let err = r.unwrap_err();
        assert_eq!(err.code, ErrorCode::DeadlineExceeded, "got: {err:?}");
    }

    /// 分维度计数**互不串**（spec §8 判据 4）。
    /// 两个维度都真开火，而不是只断言「某维度 += 1」—— 后者在没有分维度时也会过。
    #[tokio::test]
    async fn timeout_counters_are_per_backend_kind() {
        let rdbms_before = count(BackendKind::Rdbms);
        let tsdb_before = count(BackendKind::Tsdb);
        let slow = || async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok::<(), Error>(())
        };
        let _: Result<(), Error> =
            run_with_timeout(BackendKind::Tsdb, Some(Duration::from_millis(10)), slow()).await;
        assert_eq!(count(BackendKind::Tsdb), tsdb_before + 1);
        assert_eq!(
            count(BackendKind::Rdbms),
            rdbms_before,
            "Tsdb 的超时不得落到 Rdbms 槽"
        );
    }
}
```

- [ ] **Step 3: `ecat-data/src/breaker.rs` 两个映射函数改为公开，并新增一个**

把 `:29-35` 的 `pub(crate) fn map_breaker_error` 改成 `pub fn`（函数体一行不改）。

在它**后面**追加（`use` 段加 `use ecat_errors::{Error, ErrorCode};`）：

```rust
/// `ecat_errors::Error` 侧的同一套映射（六个非 RDBMS trait 用的错误类型）。
///
/// 与 [`map_breaker_error`] 分开是因为 `ecat_errors::Error` 与 `BreakerError`
/// **都是外部类型**，写不出统一的 `From` 实现（孤儿规则），只能各来一个函数。
/// `reason` 是后端标识（如 `"redis"`），进 `Error::reason`。
pub fn breaker_error_to_backend_error(e: BreakerError<Error>, reason: &'static str) -> Error {
    match e {
        BreakerError::Inner(inner) => inner,
        other => Error::new(ErrorCode::Unavailable, reason, other.to_string()),
    }
}
```

- [ ] **Step 4: `ecat-data/src/lib.rs` 重导出更新**

```rust
pub use breaker::{CircuitBreakerExecutor, breaker_error_to_backend_error, map_breaker_error};
```
（其余行不动；`pub use timeout::{QUERY_TIMEOUTS, TRANSACTIONS_LEAKED, run_with_timeout};` 改成：）

```rust
pub use timeout::{BackendKind, TIMEOUTS, TRANSACTIONS_LEAKED, TimeoutError, run_with_timeout};
```

- [ ] **Step 5: 迁移 `ecat-data-sqlx`（5 处）**

`src/lib.rs:20-23` 的 import 加一项：

```rust
use ecat_data::{
    BackendKind, Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction, TransactionInner,
    run_with_timeout,
};
```

`:152` 改成（在 `SqlxClient::timed` 里）：

```rust
        crate::tracing::timed(
            self.slow_query,
            sql,
            run_with_timeout(BackendKind::Rdbms, self.query_timeout, fut),
        )
        .await
```

`src/transaction.rs:12` 的 import 加一项：

```rust
use ecat_data::{BackendKind, Dialect, RdbmsError, Row, TransactionInner, run_with_timeout};
```

`:37,48,61,94` 四处，把 `run_with_timeout(self.query_timeout, async {` 逐个改成：

```rust
                run_with_timeout(BackendKind::Rdbms, self.query_timeout, async {
```

（四处缩进各不相同，**按原位缩进**；`transaction.rs` 里这四行在宏体内，照原样改即可。）

- [ ] **Step 6: 迁移 `ecat-data-mssql`（6 处）**

`src/client.rs:16-19` 的 import 加一项：

```rust
use ecat_data::{
    BackendKind, Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction, TransactionInner,
    run_with_timeout,
};
```

`:121` 改成：

```rust
        crate::tracing::timed(
            self.slow_query,
            sql,
            run_with_timeout(BackendKind::Rdbms, self.query_timeout, fut),
        )
        .await
```

`:288,296,304,314` 四处同理，`run_with_timeout(self.query_timeout, async {` → `run_with_timeout(BackendKind::Rdbms, self.query_timeout, async {`。

- [ ] **Step 7: 改 `QUERY_TIMEOUTS` 的两个消费者（**必须带 `--features metrics` 验证**）**

`ecat-data-sqlx/src/metrics.rs`：`:20` 的 `use ecat_data::{QUERY_TIMEOUTS, TRANSACTIONS_LEAKED};` 改成

```rust
use ecat_data::{BackendKind, TIMEOUTS, TRANSACTIONS_LEAKED};
```

`:136` 的 `count(QUERY_TIMEOUTS.load(Ordering::Relaxed))` 改成

```rust
        let timeouts = count(TIMEOUTS[BackendKind::Rdbms as usize].load(Ordering::Relaxed));
```

`:281,286` 两处测试里的 `QUERY_TIMEOUTS` 同样换成 `TIMEOUTS[BackendKind::Rdbms as usize]`。

`ecat-data-mssql/src/metrics.rs`：`:24` / `:152` / `:323` / `:328` 四处**同构**替换。

`:16` / `:133-135` 的文档注释里提到 `QUERY_TIMEOUTS` 的地方一并改（用 `` [`TIMEOUTS`] ``）。

- [ ] **Step 8: 编译 + 逐条比对测试输出**

```bash
cargo test -p ecat-data 2>&1 | grep -E '^test |^test result'
cargo test -p ecat-data-sqlx 2>&1 | grep -E '^test result'
cargo test -p ecat-data-mssql 2>&1 | grep -E '^test result'
cargo test -p ecat-data-sqlx -p ecat-data-mssql --features metrics,health,tracing 2>&1 | grep -E '^test |^test result'
```

**与 Step 1 逐条比对**：既有测试名一个不少、全绿；`ecat-data` 的测试数比基线**多 2**（`error_path_maps_to_deadline_exceeded`、`timeout_counters_are_per_backend_kind`）。**没多就是新测试没被编译。**

- [ ] **Step 9: 空验收自证（必做）**

临时把 `run_with_timeout` 的 `TIMEOUTS[kind as usize].fetch_add(..)` 那一行整行删掉，重跑：

```bash
cargo test -p ecat-data 2>&1 | grep -E '^test .*timeout_counters|^test result'
```

**必须看到 `timeout_counters_are_per_backend_kind` FAILED**（`Tsdb` 槽没加）与 `slow_future_times_out_and_counts` FAILED。记录实际输出，还原。

- [ ] **Step 10: 行数复核 + 闸门 + 提交**

```bash
find ecat-data/src ecat-data-sqlx/src ecat-data-mssql/src -name '*.rs' -exec awk 'END{if(NR>500) print FILENAME": "NR}' {} \;
cargo fmt --all
cargo fmt --all -- --check; echo "fmt rc=$?"
cargo clippy -p ecat-data -p ecat-data-sqlx -p ecat-data-mssql --all-targets --all-features -- -D warnings; echo "clippy rc=$?"
git add ecat-data/src ecat-data-sqlx/src ecat-data-mssql/src
git commit -m "feat(ecat-data): run_with_timeout 泛型化（BackendKind 维度 + ecat_errors::Error），迁移 13 个调用点"
```

### ⚠️ Task 1 必读

1. **`QUERY_TIMEOUTS` 是删不是留。** 留着它 = 公开一个永远不再增长的计数器。
2. **`--features metrics` 那一步不能省。** 两个 `metrics.rs` 改动在默认构建里**根本不参与编译**，`cargo test -p ecat-data-sqlx` 全绿完全说明不了它们是对的。
3. **`QUERY_TIMEOUTS` 的删除是破坏性变更**，Task 7 的 CHANGELOG 要点名。
4. `ecat-data/Cargo.toml` **不需要**改 —— `ecat-errors` 与 `ecat-circuit-breaker` 都已是依赖。

---

## Task 2: `ecat-circuit-breaker` 加 `opened_total()`

**Files:**
- Modify: `ecat-circuit-breaker/src/breaker.rs`

理由见「出入 5」：`ecat_outbound_breaker_open_total` 没有数据源。**只加一个只读计数器，不改状态机** —— 批次 4 的既有 15 个测试必须逐条保持全绿。

- [ ] **Step 1: 基线**

```bash
cargo test -p ecat-circuit-breaker 2>&1 | grep -E '^test |^test result'
```

记下**测试名**（不是只记总数）。

- [ ] **Step 2: `Inner` 加字段**

```rust
pub(crate) struct Inner {
    pub(crate) state: BreakerState,
    pub(crate) window: SlidingWindow,
    pub(crate) opened_at: Option<Instant>,
    pub(crate) half_open_count: u32,
    /// `Closed → Open` 的累计次数。半开探测失败**重新打开也算一次**
    /// （那是「后端还是不行」的第二次确认，与首次打开同等重要）。
    pub(crate) opened_total: u64,
}
```

`Breaker::new` 的 `Inner { .. }` 字面量加 `opened_total: 0,`。

- [ ] **Step 3: 两处打开点各 +1**

`call` 里 `inner.state = BreakerState::Open;` 出现在**两处**（`state = Open` 紧跟 `opened_at = Some(Instant::now())`）：Closed 分支按失败率打开、HalfOpen 分支探测失败重新打开。**两处都在 `opened_at = Some(..)` 后面加一行**：

```rust
                    inner.opened_total += 1;
```

- [ ] **Step 4: 公开读取方法**

放在 `state()` 之后：

```rust
    /// `Closed → Open` 的累计次数（半开探测失败重新打开也计入）。
    ///
    /// 供 `metrics` feature 注册 `ecat_outbound_breaker_open_total`。
    /// 用 `state()` 轮询猜测「开了几次」是错的 —— 轮询间隔决定准确性，
    /// 而且熔断器可能开又关，两次探测之间发生的事抓不到。
    pub fn opened_total(&self) -> u64 {
        self.lock().opened_total
    }
```

- [ ] **Step 5: 测试**

加在既有 `mod tests` 里：

```rust
    /// `opened_total` 数的是**打开次数**，不是当前状态 ——
    /// 只断言「打开后 ≥1」是空验收（一个恒返回 1 的实现也能过）。
    /// 这里驱动两次独立的打开：首次按失败率、之后由半开探测失败重新打开。
    #[tokio::test]
    async fn opened_total_counts_each_open_not_the_state() {
        let cfg = BreakerConfig {
            open_duration: Duration::from_millis(20),
            ..BreakerConfig::default()
        };
        let b = Breaker::new(cfg);
        assert_eq!(b.opened_total(), 0, "初始不得已经计过数");

        let fail = || async { Err::<(), &str>("backend down") };
        for _ in 0..5 {
            let _ = b.call(fail).await;
        }
        assert_eq!(b.state(), BreakerState::Open);
        assert_eq!(b.opened_total(), 1);

        // 冷却 → 半开 → 探测失败 → 重新打开，计到 2。
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = b.call(fail).await;
        assert_eq!(b.opened_total(), 2, "半开探测失败重新打开必须再计一次");
    }
```

（`mod tests` 顶部已 `use super::*;`；若没有 `Duration` 就补 `use std::time::Duration;`。）

- [ ] **Step 6: 逐条比对 + 空验收自证**

```bash
cargo test -p ecat-circuit-breaker 2>&1 | grep -E '^test |^test result'
```

**与 Step 1 逐条比对**：既有测试名一个不少、全绿；总数 +1。

然后把 Step 3 加的两行 `inner.opened_total += 1;` 全部注释掉，重跑 —— **`opened_total_counts_each_open_not_the_state` 必须 FAILED**。记录输出，还原。

- [ ] **Step 7: 闸门 + 提交**

```bash
cargo fmt --all -- --check; echo "fmt rc=$?"
cargo clippy -p ecat-circuit-breaker --all-targets -- -D warnings; echo "clippy rc=$?"
git add ecat-circuit-breaker/src
git commit -m "feat(ecat-circuit-breaker): Breaker 暴露 opened_total()，供出站熔断指标使用"
```

---

## Task 3: `ecat-data-redis` —— 超时 + 熔断 + 指标

**Files:**
- Create: `ecat-data-redis/src/tests.rs`（`mod tests` 从 lib.rs 搬出并扩展）
- Create: `ecat-data-redis/src/metrics.rs`（feature 门控）
- Modify: `ecat-data-redis/src/lib.rs`
- Modify: `ecat-data-redis/Cargo.toml`
- Modify: `ecat-data-redis/README.md`（若存在；不存在跳过）

**范围**：只包 `impl Cache`（见「出入 9」，`RedisLock` 不动）。

- [ ] **Step 1: 基线**

```bash
cargo test -p ecat-data-redis 2>&1 | grep -E '^test |^test result'
```

记下**12 个**测试名。

- [ ] **Step 2: `Cargo.toml` 加依赖与 feature**

```toml
[dependencies]
# ...（既有依赖不动）
ecat-circuit-breaker.workspace = true
# 指标依赖是 optional：默认不把 axum（ecat-metrics 的依赖）拖进核心依赖树。
ecat-metrics = { workspace = true, optional = true }
prometheus = { version = "0.13", optional = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt", "net", "time"] }

[features]
metrics = ["dep:ecat-metrics", "dep:prometheus"]
```

（`[dev-dependencies]` 的 tokio 现有为 `["macros", "rt"]`，加 `net` + `time`：假 RESP 服务端要 `TcpListener`，测试要 `sleep`。）

- [ ] **Step 3: 建 `src/tests.rs`，把 `mod tests` 整块搬过去**

在 `lib.rs` 里把 `#[cfg(test)] mod tests { ... }`（`:244-395`）整块剪出，粘到新文件，去掉外面那层 `mod tests { }` 与它的缩进，文件头如下：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 测试独立成文件：lib.rs 有 500 行硬上限（项目约定，见批次 4 的同类拆分）。
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
```

`lib.rs` 末尾只留：

```rust
#[cfg(test)]
mod tests;
```

**⚠️ 假绿防线**：`mod tests;` 必须在 `lib.rs` 里，且 `src/tests.rs` 必须同时存在于磁盘上 —— 先建文件再加声明。加完立刻跑 Step 4 的测试，**断言 12 个测试名一个不少**。

- [ ] **Step 4: 确认搬运无损**

```bash
cargo test -p ecat-data-redis 2>&1 | grep -E '^test |^test result'
```

**与 Step 1 的 12 个名字逐条比对。** 少一个或多一个都停下查 —— 常见原因是 `use super::*` 的可见性变了（`super` 仍是 crate 根，不该有变化）。

- [ ] **Step 5: `RedisConfig` 加两个字段**

在 `tls` 字段后加（**语义与 `ecat-data-sqlx/src/config.rs:146-154` 完全一致**）：

```rust
    /// 单次命令超时秒数。`0` = 禁用；未配置 = 30 秒。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    /// 默认**开启** —— 保守阈值下只在持续失败时打开。设 `{"enabled": false}` 关闭。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
```

配一个转换（与 sqlx 同款）：

```rust
/// `0` 表示显式禁用超时；未配置时为 30 秒。
fn query_timeout(secs: Option<u64>) -> Option<Duration> {
    match secs {
        None => Some(Duration::from_secs(30)),
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s)),
    }
}
```

⚠️ **`BreakerConfig` 要可反序列化**。实测它现在只有 `#[derive(Debug, Clone)]`（`ecat-circuit-breaker/src/breaker.rs:20`），**不能 `Deserialize`**。所以在 `ecat-circuit-breaker/src/breaker.rs` 给它加：

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BreakerConfig {
    #[serde(default = "default_failure_ratio")]
    pub failure_ratio: f64,
    #[serde(default = "default_window", with = "duration_secs")]
    pub window: Duration,
    #[serde(default = "default_half_open_probes")]
    pub half_open_probes: u32,
    #[serde(default = "default_open_duration", with = "duration_secs")]
    pub open_duration: Duration,
}
```

并补 `ecat-circuit-breaker/Cargo.toml` 的 `serde` 依赖（**先读现有 `[dependencies]` 再写：若已有 serde 就只加 `derive` feature**）+ 三个默认值函数 + 一个把秒数转 `Duration` 的 `mod duration_secs`（`serde` 没有内置的 `Duration` 支持）。**默认值必须与 `impl Default for BreakerConfig` 里的数字逐字相同**（0.5 / 30 / 3 / 10），否则配置省略时行为与 `Breaker::new(BreakerConfig::default())` 不一致。

⚠️ 这一改动在 `ecat-circuit-breaker` 里，**Task 2 的测试要重跑**。

- [ ] **Step 6: `RedisCache` 加三个字段 + 装配**

```rust
pub struct RedisCache {
    conn: MultiplexedConnection,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
}
```

三个构造器（`connect` / `connect_with_password` / `from_connection`）都补上默认装配。`connect` / `connect_with_password` 用：

```rust
        Ok(Self {
            conn,
            query_timeout: Some(Duration::from_secs(30)),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
        })
```

`from_connection` 同理。`from_config` 判掉配置值：

```rust
    pub async fn from_config(cfg: RedisConfig) -> Result<Self, Error> {
        let url = build_url(&cfg);
        let client = match &cfg.password {
            Some(pw) if !pw.is_empty() => Self::connect_with_password(&url, pw).await?,
            _ => Self::connect(&url).await?,
        };
        Ok(Self {
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
            ..client
        })
    }
```

> ⚠️ `{ ..client }` 这里**不能用**（`client` 是 `Self` 不是字段结构体）—— 上面那段是把 `RedisCache` 的其余字段留给默认值的写法，实际写：

```rust
        Ok(Self {
            conn: client.conn,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
        })
```

**`breakers()` 读取口**（metrics 用）：

```rust
    /// 本 client 的熔断器。`metrics` feature 注册指标时要读它的状态与打开次数。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }
```

- [ ] **Step 7: 加 `guarded` 外壳并改造 `impl Cache` 的六个方法**

```rust
/// 一次出站调用的公共外壳：**熔断在外、超时在内**。
///
/// 顺序与 spec §3 相反（理由见批次 5a 计划的「与 spec 的出入 4」）：
/// 超时若在外层，`tokio::time::timeout` 会把熔断器的 future 直接 drop 掉，
/// 于是每次「后端没在预算内作答」都**什么都不记** —— 卡死的后端永远打不开熔断器，
/// 而卡死正是本设计要防的头号场景。熔断在外时超时是一次普通的 `Err`，如实计入失败。
impl RedisCache {
    async fn guarded<F, T>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        self.breaker
            .call(|| run_with_timeout(BackendKind::Cache, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "redis"))
    }
}
```

`impl Cache` 六个方法各改三行。以 `get` 为例，原体（`:82-87`）整段挪进闭包、不加任何逻辑改动：

```rust
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        self.guarded(async {
            let mut conn = self.conn.clone();
            conn.get(key)
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "redis", format!("redis get: {e}")))
        })
        .await
    }
```

`set` / `delete` / `increment` / `ttl` / `multi_get` **同构**：`self.guarded(async { <原体原样> }).await`。

⚠️ **不要**给 `ttl_to_duration` 或 `build_url` 套 `guarded` —— 它们是纯本地函数，不发起 I/O。

- [ ] **Step 8: import 更新**

`lib.rs` 顶部：

```rust
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::breaker_error_to_backend_error;
use ecat_data::{BackendKind, run_with_timeout};
use std::sync::Arc;
```

- [ ] **Step 9: 假 RESP 服务端 + 五条新测试（写进 `src/tests.rs`）**

Redis 的「内层」是 `MultiplexedConnection`，**造不出假实现**（无公开构造器），所以用进程内假服务端 —— 与 `ecat-data-mssql/src/tests.rs:440` 的假 `TcpListener` 同一套路。

```rust
/// 假 Redis 服务端：**应答握手、对数据命令装死**。
///
/// 数据命令不回应 ⇒ 调用方在超时前一直挂着。这正是「一个卡死的 Redis GET」。
/// 握手命令必须应答，否则 `get_multiplexed_async_connection` 就卡在建连上了，
/// 测到的是建连超时而不是命令超时。
async fn spawn_silent_redis() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let Ok(n) = tokio::io::AsyncReadExt::read(&mut sock, &mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    // RESP 数组：`*<n>\r\n` 之后是 n 个 `$<len>\r\n<bytes>\r\n`
                    for cmd in drain_commands(&mut buf) {
                        let is_data = matches!(
                            cmd.as_str(),
                            "GET" | "MGET" | "SET" | "PSETEX" | "DEL" | "INCR" | "INCRBY" | "TTL"
                        );
                        if !is_data {
                            let _ = tokio::io::AsyncWriteExt::write_all(&mut sock, b"+OK\r\n").await;
                            let _ = tokio::io::AsyncWriteExt::flush(&mut sock).await;
                        }
                    }
                }
            });
        }
    });
    format!("redis://{addr}")
}

/// 从 `buf` 头部切出完整的 RESP 命令（命令名大写），不完整的留在 `buf` 里。
/// 返回本次切出的命令名列表。
fn drain_commands(buf: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let Some(star) = buf.iter().position(|b| *b == b'*') else {
            return out;
        };
        let Some(nl) = buf[star..].iter().position(|b| *b == b'\n') else {
            return out;
        };
        let argc: usize = match std::str::from_utf8(&buf[star + 1..star + nl])
            .ok()
            .and_then(|s| s.trim_end_matches('\r').parse().ok())
        {
            Some(n) => n,
            None => return out,
        };
        let mut pos = star + nl + 1;
        let mut first: Option<String> = None;
        let mut complete = true;
        for i in 0..argc {
            let Some(nl) = buf[pos..].iter().position(|b| *b == b'\n') else {
                complete = false;
                break;
            };
            let len: usize = match std::str::from_utf8(&buf[pos + 1..pos + nl])
                .ok()
                .and_then(|s| s.trim_end_matches('\r').parse().ok())
            {
                Some(n) => n,
                None => {
                    complete = false;
                    break;
                }
            };
            let start = pos + nl + 1;
            if buf.len() < start + len + 2 {
                complete = false;
                break;
            }
            if i == 0 {
                first = Some(String::from_utf8_lossy(&buf[start..start + len]).to_uppercase());
            }
            pos = start + len + 2;
        }
        if !complete {
            return out;
        }
        if let Some(name) = first {
            out.push(name);
        }
        buf.drain(..pos);
    }
}
```

五条新测试：

```rust
/// 建连必须能完成 —— 这一条是后面三条的前提。
/// **若它失败，不要改断言**：把实际错误原样回报给 lead。
#[tokio::test]
async fn connect_succeeds_against_fake_server() {
    let url = spawn_silent_redis().await;
    let cache = RedisCache::connect(&url).await.unwrap();
    assert_eq!(cache.breaker().state(), BreakerState::Closed);
}

/// 超时真的开火（spec §8 判据 2）：假服务端对 GET 永不回应。
#[tokio::test]
async fn get_times_out_with_deadline_exceeded() {
    let url = spawn_silent_redis().await;
    let cache = RedisCache::connect(&url).await.unwrap();
    let mut cache = cache;
    cache.query_timeout = Some(Duration::from_millis(50));

    let before = TIMEOUTS[BackendKind::Cache as usize].load(Ordering::SeqCst);
    let err = cache.get("k").await.expect_err("GET 不该返回");
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "got: {err}");
    assert!(
        TIMEOUTS[BackendKind::Cache as usize].load(Ordering::SeqCst) > before,
        "超时必须计入 Cache 维度"
    );
}

/// 熔断真的打开（spec §8 判据 3）：连续超时后**快速失败**，
/// 且不再等满超时 —— 这是「快」的可观测判据，不是只断言 is_err。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let url = spawn_silent_redis().await;
    let mut cache = RedisCache::connect(&url).await.unwrap();
    cache.query_timeout = Some(Duration::from_millis(20));

    for _ in 0..5 {
        let _ = cache.get("k").await;
    }
    assert_eq!(cache.breaker().state(), BreakerState::Open);
    assert_eq!(cache.breaker().opened_total(), 1);

    let start = std::time::Instant::now();
    let err = cache.get("k").await.expect_err("熔断已打开");
    assert!(
        start.elapsed() < Duration::from_millis(20),
        "熔断打开后必须立即返回，实际耗时 {:?}",
        start.elapsed()
    );
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
}

/// 超时与熔断**不是**同一件事：超时由 `run_with_timeout` 报 `DeadlineExceeded`，
/// 熔断打开由映射函数报 `Unavailable`。两者混成一个码，调用方就没法区分
/// 「这次慢」和「后端已经放弃了」。
#[tokio::test]
async fn timeout_and_breaker_open_have_distinct_codes() {
    let url = spawn_silent_redis().await;
    let mut cache = RedisCache::connect(&url).await.unwrap();
    cache.query_timeout = Some(Duration::from_millis(20));
    let err = cache.get("k").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "got: {err}");
    for _ in 0..5 {
        let _ = cache.get("k").await;
    }
    let err = cache.get("k").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
}

/// `query_timeout_secs: 0` 是**禁用**而非「0 秒立刻超时」（全仓约定）。
/// 断言成功即可 —— 若 0 被当成零超时，这里会拿到 `DeadlineExceeded`。
#[tokio::test]
async fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));

    let url = spawn_silent_redis().await;
    let cfg: RedisConfig = serde_json::from_str(&format!(
        r#"{{"url": "{url}", "query_timeout_secs": 0}}"#
    ))
    .unwrap();
    let cache = RedisCache::from_config(cfg).await.unwrap();
    assert_eq!(cache.query_timeout, None);
}
```

> `cache.query_timeout = ...` 需要 `let mut cache`。若 clippy 对 `let mut cache = cache;` 有意见，改成一步 `let mut cache = RedisCache::connect(&url).await.unwrap();`。

- [ ] **Step 10: 确认新测试真的在跑**

```bash
cargo test -p ecat-data-redis 2>&1 | grep -E '^test |^test result'
```

**12 → 17**（+5）。没多就是 `mod tests` 没生效。

- [ ] **Step 11: 空验收自证（两条）**

1. 把 `impl Cache for RedisCache` 里 `get` 的 `self.guarded(async { .. })` 换回直接 `self.conn.clone().get(key).await.map_err(..)`（绕过外壳）→ **`get_times_out_with_deadline_exceeded` 必须 FAILED**（不再有超时）。记录输出，还原。
2. 把 `repeated_timeouts_open_the_breaker_and_fail_fast` 里的 `start.elapsed()` 断言**去掉**再跑 → 仍然通过。**这说明该断言是这条测试唯一的「快」判据**，不能删。

- [ ] **Step 12: `src/metrics.rs`（feature 门控）**

在 `lib.rs` 加（放在其他 `mod` 声明附近）：

```rust
#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;
```

`src/metrics.rs`：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "metrics"：把出站超时与熔断状态接进 `ecat_metrics::registry()`，
//! 已有的 `/metrics` 端点自动多出这三个指标：
//!
//! | 指标 | 类型 | 维度 |
//! |---|---|---|
//! | `ecat_outbound_timeouts_total` | counter | `backend` |
//! | `ecat_outbound_breaker_open_total` | counter | `backend` |
//! | `ecat_outbound_breaker_state` | gauge | `backend`（0=closed 1=open 2=half-open）|
//!
//! 超时数读的是 `ecat-data` 的进程级静态量 [`TIMEOUTS`]，**按后端类别**而非实例 ——
//! 多个 Redis 实例共享同一个 `Cache` 计数，这是 spex §4 明确的取舍
//! （按实例的维度是独立一期）。熔断两项是**按实例**的：每 client 一个 `Breaker`。

use ecat_circuit_breaker::{Breaker, BreakerState};
use ecat_data::{BackendKind, TIMEOUTS};
use prometheus::{IntCounterVec, IntGaugeVec, Opts};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

/// 全局 collector 三件套。`Registry` 按「指标名 + 常量标签」去重，
/// 同名注册第二次直接 `AlreadyReg` —— 多后端共用一个 registry 时
/// 只有「注册一次、按 label 取子指标」这一条路。
static METRICS: OnceLock<Metrics> = OnceLock::new();

struct Metrics {
    timeouts: IntCounterVec,
    opened: IntCounterVec,
    state: IntGaugeVec,
    breakers: Mutex<Vec<(&'static str, Arc<Breaker>)>>,
}

/// 注册本后端的三个出站指标。`backend` 是标签值（如 `"redis"` / `"clickhouse"`）。
///
/// 幂等：指标家族只注册一次；同一个 `backend` 重复注册则**替换**它的熔断器
/// （避免同一标签出现两份样本）。
pub fn register_outbound_metrics(backend: &'static str, breaker: Arc<Breaker>) {
    let metrics = METRICS.get_or_init(|| {
        let timeouts = IntCounterVec::new(
            Opts::new("ecat_outbound_timeouts_total", "Outbound call timeouts"),
            &["backend"],
        )
        .expect("valid metric");
        let opened = IntCounterVec::new(
            Opts::new("ecat_outbound_breaker_open_total", "Circuit breaker openings"),
            &["backend"],
        )
        .expect("valid metric");
        let state = IntGaugeVec::new(
            Opts::new("ecat_outbound_breaker_state", "Circuit breaker state"),
            &["backend"],
        )
        .expect("valid metric");
        let registry = ecat_metrics::registry();
        // AlreadyReg 只可能是名字撞车，而这三个名字以 ecat_outbound_ 独占。
        let _ = registry.register(Box::new(timeouts.clone()));
        let _ = registry.register(Box::new(opened.clone()));
        let _ = registry.register(Box::new(state.clone()));
        Metrics {
            timeouts,
            opened,
            state,
            breakers: Mutex::new(Vec::new()),
        }
    });

    let mut breakers = metrics.breakers.lock().unwrap_or_else(|e| e.into_inner());
    breakers.retain(|(b, _)| *b != backend);
    breakers.push((backend, breaker));
}

fn state_code(s: BreakerState) -> i64 {
    match s {
        BreakerState::Closed => 0,
        BreakerState::Open => 1,
        BreakerState::HalfOpen => 2,
    }
}

/// 每次抓取前把两个快照型指标刷一遍。
///
/// `ecat-metrics` 的 `/metrics` 处理器直接调 `metrics_text()`，没有 hook 可以插；
/// 所以这里**在注册时**先刷一次、并**在抓取路径之外**由调用方定期调用。
/// 简化取舍：`ponytail:` 只在 `register_outbound_metrics` 时刷新一次是不够的 ——
/// 因此本模块导出一个 [`refresh_outbound_metrics`]，由使用方在抓取前调用。
/// （完整做法是自定义 `Collector`，比照 `ecat-data-sqlx/src/metrics.rs:44`。）
pub fn refresh_outbound_metrics() {
    let Some(metrics) = METRICS.get() else {
        return;
    };
    let breakers = metrics.breakers.lock().unwrap_or_else(|e| e.into_inner());
    for (backend, breaker) in breakers.iter() {
        let _ = metrics
            .timeouts
            .with_label_values(&[backend])
            .inc_by(0); // 占位，见下
        let _ = metrics.opened.with_label_values(&[backend]);
        metrics
            .state
            .with_label_values(&[backend])
            .set(state_code(breaker.state()));
    }
}
```

> ⚠️ **上面那段有占位，不要照抄。** 落地时按 `ecat-data-sqlx/src/metrics.rs` 的**手写 `Collector`** 写（那才是本仓已用的、能挂钩抓取的唯一方式）：`impl Collector for Registered`，在 `collect()` 里现读 `TIMEOUTS` 与各 `Breaker`。写完后 `refresh_outbound_metrics` 与 `IntCounterVec`/`IntGaugeVec` **都不需要**（counter 用 `MetricFamily` 现造）。

**这一处必须按 sqlx 那份实现，不要新造机制。** 具体照抄它 `:44-180` 的结构，把数据源换成：

| 指标 | 数据源 |
|---|---|
| `ecat_outbound_timeouts_total{backend}` | `TIMEOUTS[BackendKind::Cache as usize].load(Relaxed)` |
| `ecat_outbound_breaker_open_total{backend}` | `breaker.opened_total()` |
| `ecat_outbound_breaker_state{backend}` | `state_code(breaker.state())` |

⚠️ 三个指标各自的 `Desc` 要用**各自的常量标签**（`backend`），与 sqlx 那份里的 `labels(&[("backend", ..)])` 同款。

- [ ] **Step 13: `metrics` feature 的测试**

写在 `src/metrics.rs` 末尾的 `#[cfg(test)] mod tests`（**必须与 `metrics` feature 同门控**，否则默认构建会编译它而 `prometheus` 不在依赖树里）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ecat_circuit_breaker::BreakerConfig;

    #[test]
    fn outbound_metrics_appear_in_registry_text() {
        register_outbound_metrics("redis-test", Arc::new(Breaker::new(BreakerConfig::default())));
        let text = ecat_metrics::metrics_text();
        assert!(text.contains("ecat_outbound_timeouts_total"), "{text}");
        assert!(text.contains("ecat_outbound_breaker_open_total"), "{text}");
        assert!(
            text.contains("ecat_outbound_breaker_state{backend=\"redis-test\"}"),
            "{text}"
        );
    }
}
```

- [ ] **Step 14: 带 feature 跑 + 确认默认构建不含它**

```bash
cargo test -p ecat-data-redis 2>&1 | grep -c 'outbound_metrics'   # 期望 0（默认构建里没有）
cargo test -p ecat-data-redis --features metrics 2>&1 | grep -E '^test |^test result'
```

**第一条必须是 0** —— 否则新测试写在 feature 门控之外（空验收的变体）。第二条应看到 17 → 18。

- [ ] **Step 15: 行数复核 + 闸门 + 提交**

```bash
find ecat-data-redis/src -name '*.rs' -exec awk 'END{if(NR>500) print FILENAME": "NR}' {} \;
cargo fmt --all
cargo fmt --all -- --check; echo "fmt rc=$?"
cargo clippy -p ecat-data-redis --all-targets --features metrics -- -D warnings; echo "clippy rc=$?"
cargo test -p ecat-circuit-breaker 2>&1 | grep -E '^test result'   # Step 5 改过它，必须仍绿
git add ecat-data-redis ecat-circuit-breaker
git commit -m "feat(ecat-data-redis): 出站超时 + 熔断 + metrics feature，补多路复用能力边界文档"
```

### ⚠️ Task 3 必读

1. **`guarded` 一律「熔断在外、超时在内」**（出入 4）。若 lead 裁决维持 spec 顺序，这一节整体重写，且 `ecat-circuit-breaker` 要先修半开名额泄漏。
2. **`0` = 禁用**，不是零超时。
3. **`from_config` 的密码路径不能被绕过** —— 原实现分 `connect_with_password` / `connect` 两支，重构时保住这个分支（既有测试 `from_config_with_password_path_fails_on_unreachable` 会把守）。
4. **Redis 多路复用的能力边界要写进 rustdoc**（spec §6 明确要求，目前「完全没有文档」）：

```rust
//! # 能力边界
//!
//! 本 client 用 `MultiplexedConnection`（一条 TCP 服务所有并发），**不是连接池** ——
//! 对缓存负载这比池更优：连接数与往返都更低。代价是**有状态命令序列不能用它**：
//! `MULTI`/`EXEC` 事务、`WATCH`、`SUBSCRIBE`、`BLOCKING` 命令需要独占连接，
//! 多路复用下会与其它命令交错。需要时用 `redis::Client::get_async_connection()`
//! 另开一条专用连接。
```

---

## Task 4: `ecat-data-clickhouse` —— 最难 case

**Files:**
- Create: `ecat-data-clickhouse/src/tsdb.rs`（`TsdbClient` 实现从 lib.rs 抽出）
- Create: `ecat-data-clickhouse/src/tests/resilience.rs`
- Create: `ecat-data-clickhouse/src/metrics.rs`
- Modify: `ecat-data-clickhouse/src/lib.rs`
- Modify: `ecat-data-clickhouse/src/tests.rs`（加一行 `mod resilience;`）
- Modify: `ecat-data-clickhouse/Cargo.toml`

**为什么它最难**：同时实现 `SqlExecutor` 与 `TsdbClient`（两条 I/O 路径共用一个 `Breaker`）、两个 trait 各有一个 `query` 方法同名、`transaction()` 是常量错误（不包）、`_with` 落到默认实现（不包，但要测试把守）、还是 5a 里唯一的 HTTP 后端（要并发上限）。

- [ ] **Step 1: 基线**

```bash
cargo test -p ecat-data-clickhouse 2>&1 | grep -E '^test |^test result'
```

记下测试名（`lib.rs` 内 0 个 + `tests.rs` 24 个）。

- [ ] **Step 2: `Cargo.toml`**

```toml
[dependencies]
# ...（既有不动）
ecat-circuit-breaker.workspace = true
# 并发上限用 Semaphore。tokio 现在只在 dev-dependencies 里，必须提到这里。
tokio = { workspace = true, features = ["sync"] }
# 指标依赖 optional：默认不把 axum 拖进核心依赖树。
ecat-metrics = { workspace = true, optional = true }
prometheus = { version = "0.13", optional = true }

[features]
metrics = ["dep:ecat-metrics", "dep:prometheus"]
```

- [ ] **Step 3: 抽出 `src/tsdb.rs`**

把 `lib.rs:303-447`（`#[async_trait] impl TsdbClient for ClickhouseClient { ... }`）整块搬到新文件，文件头：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `TsdbClient` 实现。独立成文件是因为 `lib.rs` 紧贴 500 行硬上限
//! （项目约定，与批次 4 拆 `ecat-circuit-breaker` 同因）。
use crate::{ClickhouseClient, field_to_json, field_type, quote_ident};
use async_trait::async_trait;
use ecat_data::{BackendKind, DataPoint, Error, TsdbClient, run_with_timeout};
use ecat_errors::ErrorCode;
use std::time::Duration;
```

`lib.rs` 顶部模块表加 `mod tsdb;`，并把 `build_insert_body` 等被 `tsdb.rs` 用到的项改为 `pub(crate)`（本模块内 `use crate::...` 引用）。

（`use ecat_data::Error` 走的是 `ecat-data` 对 `ecat_errors::Error` 的重导出，`lib.rs:5` 已在用 `ecat_errors::{Error, ErrorCode}` —— 两处保持与各文件原样一致即可。）

- [ ] **Step 4: 加 `mod` 声明 + 确认搬运无损**

```bash
cargo test -p ecat-data-clickhouse 2>&1 | grep -E '^test |^test result'
```

**24 个名字逐条比对。** `mod tsdb;` 若漏写，`TsdbClient` 的实现会整体消失 —— 编译期就会报（`impl` 不存在则 trait 方法未实现），不会静默。

- [ ] **Step 5: 配置加三个字段**

```rust
    /// 单次调用超时秒数。`0` = 禁用；未配置 = 30 秒。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 并发上限。reqwest **只有** `pool_max_idle_per_host`（空闲保留数），
    /// 没有「最大总连接数」—— 默认无上限意味着并发无背压。
    /// 未配置 = 32。上限由本 crate 的信号量实现，不是 reqwest 的旋钮。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
```

- [ ] **Step 6: `ClickhouseClient` 加三个字段 + 三个构造器装配**

```rust
pub struct ClickhouseClient {
    client: reqwest::Client,
    base_url: String,
    database: String,
    username: Option<String>,
    password: Option<String>,
    created: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
    create_ttl: std::time::Duration,
    query_timeout: Option<Duration>,
    /// 两条 I/O 路径（`SqlExecutor` / `TsdbClient`）**共用**一个 ——
    /// 同一个服务器、同一个故障域，两条路径各判一次会把故障域切错。
    breaker: Arc<Breaker>,
    semaphore: Arc<Semaphore>,
}
```

三个构造器（`new` / `with_auth` / `from_config`）都补默认装配：

```rust
            query_timeout: Some(Duration::from_secs(30)),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Arc::new(Semaphore::new(32)),
```

`from_config` 用配置值：

```rust
        Ok(Self {
            client,
            base_url: cfg.base_url,
            database: cfg.database,
            username: cfg.username,
            password: cfg.password,
            created: std::sync::Mutex::new(std::collections::HashMap::new()),
            create_ttl: CREATE_TTL,
            query_timeout: match cfg.query_timeout_secs {
                None => Some(Duration::from_secs(30)),
                Some(0) => None,
                Some(s) => Some(Duration::from_secs(s)),
            },
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
            semaphore: Arc::new(Semaphore::new(cfg.max_concurrency.unwrap_or(32))),
        })
```

读取口：

```rust
    /// 本 client 的熔断器（`metrics` feature 用）。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }
```

- [ ] **Step 7: `guarded` 两个变体（错误类型不同）+ 许可**

```rust
impl ClickhouseClient {
    /// 取一个并发许可。信号量从不 `close()`，`AcquireError` 不可达。
    async fn permit(&self) -> SemaphorePermit<'_> {
        self.semaphore
            .acquire()
            .await
            .expect("semaphore is never closed")
    }

    /// 一次出站调用的外壳（`RdbmsError` 路径）。
    ///
    /// **顺序：许可 → 熔断 → 超时**（理由见批次 5a 计划的「与 spec 的出入 4」）：
    /// - 许可在最外：还在排队的请求**还没碰后端**，不该计入熔断失败、也不该被超时掐断
    /// - 熔断在超时外：超时是一次普通的 `Err`，会**如实计入**熔断窗口 ——
    ///   否则卡死的后端永远打不开熔断器
    async fn guarded<F, T>(&self, kind: BackendKind, fut: F) -> Result<T, RdbmsError>
    where
        F: std::future::Future<Output = Result<T, RdbmsError>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(kind, self.query_timeout, fut))
            .await
            .map_err(ecat_data::map_breaker_error)
    }

    /// 同上的 `ecat_errors::Error` 路径（`TsdbClient`）。
    async fn guarded_tsdb<F, T>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Tsdb, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "clickhouse"))
    }
}
```

> `pub(crate)` 可见性：`guarded` 在 `lib.rs`，`impl SqlExecutor` 也在 `lib.rs`，`impl TsdbClient` 在 `tsdb.rs`。所以两个都要 `pub(crate)`。

- [ ] **Step 8: 改造 `impl SqlExecutor`（**只动 `execute` / `query`**）**

```rust
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.guarded(BackendKind::Rdbms, async {
            let resp = self
                .post(sql, &[("send_progress_in_http_headers", "1".to_string())])
                .send()
                .await
                .map_err(|e| RdbmsError::Database(format!("ch: {e}")))?;
            // ...原体一行不改...
            Ok(affected)
        })
        .await
    }
```

`query` 同构（原体在 `lib.rs:258-286`，**逐字搬进闭包**）。

**`dialect()` 不包**（纯本地判断）。**`execute_with` / `query_with` / `query_write` 一个字都不写** —— 留 trait 默认实现（出入 7）。

- [ ] **Step 9: 改造 `impl TsdbClient`（`tsdb.rs`）**

`write` 原体较长，**整块搬进 `guarded_tsdb(async { ... })`**；其中 `create_table`、`table_needs_create`、`post`、`build_insert_body` 的调用**不加**任何包装（它们在 `write` 的许可/熔断/超时**之内**，再包一次会重复计数超时、重复取许可）。

⚠️ **`create_table` 里的 `?` 语义不变**：它现在会随 `guarded_tsdb` 的返回值一起被熔断器看到（一次 `write` 失败才记一次失败），这正是想要的。

`query` / `delete` 同构。

- [ ] **Step 10: import 更新**

```rust
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{BackendKind, breaker_error_to_backend_error, run_with_timeout};
use std::sync::Arc;
use tokio::sync::{Semaphore, SemaphorePermit};
```

- [ ] **Step 11: 把守测试 `tests/resilience.rs`**

`tests.rs` 顶部加（**假绿防线**：先建文件再加声明）：

```rust
mod resilience;
```

新文件头：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 并发上限）测试。独立成文件：`tests.rs` 已有 425 行。
use super::*;
use ecat_circuit_breaker::{BreakerConfig, BreakerState};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 一个「先拖住再回」的 mock：`delay` 之后回 200 空体。
/// 既有的 `spawn_mock` 第 4 个参数是响应头、**不是延迟**，故另起一个。
async fn spawn_slow_clickhouse(delay: Duration, in_flight: Arc<AtomicUsize>) -> String {
    let app = axum::Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
        let in_flight = Arc::clone(&in_flight);
        async move {
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            let _peak = now;
            tokio::time::sleep(delay).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
            axum::response::Response::new(axum::body::Body::from(""))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> ClickhouseClient {
    let cfg: ClickhouseConfig = serde_json::from_str(&format!(
        r#"{{"base_url": "{url}", "query_timeout_secs": {timeout_secs}}}"#
    ))
    .unwrap();
    let mut c = ClickhouseClient::from_config(cfg).unwrap();
    if let Some(n) = max_concurrency {
        c.semaphore = Arc::new(tokio::sync::Semaphore::new(n));
    }
    c
}
```

六条测试：

```rust
/// 超时真的开火（spec §8 判据 2）。
///
/// **必须用 1 秒超时 + 5 秒 mock**：`from_config` 建的 client 自带 reqwest 的
/// 30 秒总超时，mock 只拖 5 秒时它来不及开火；若我们的外层没接上，
/// 调用会**成功返回** ⇒ 断言失败。这条测试不是空验收。
#[tokio::test]
async fn query_times_out_with_timeout_error() {
    let url = spawn_slow_clickhouse(
        Duration::from_secs(5),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let c = client_at(&url, 1, None);
    let before = TIMEOUTS[BackendKind::Rdbms as usize].load(Ordering::SeqCst);
    let err = ecat_data::SqlExecutor::query(&c, "SELECT 1")
        .await
        .expect_err("必须超时");
    assert!(matches!(err, RdbmsError::Timeout(_)), "got: {err:?}");
    assert!(
        TIMEOUTS[BackendKind::Rdbms as usize].load(Ordering::SeqCst) > before,
        "应计入 Rdbms 维度"
    );
}

/// `TsdbClient` 路径独立计维度 —— 一个 client 两条路径，别串到一个槽里。
#[tokio::test]
async fn tsdb_path_counts_its_own_dimension() {
    let url = spawn_slow_clickhouse(
        Duration::from_secs(5),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let c = client_at(&url, 1, None);
    let rdbms_before = TIMEOUTS[BackendKind::Rdbms as usize].load(Ordering::SeqCst);
    let tsdb_before = TIMEOUTS[BackendKind::Tsdb as usize].load(Ordering::SeqCst);
    let err = ecat_data::TsdbClient::query(&c, "SELECT 1")
        .await
        .expect_err("必须超时");
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "got: {err}");
    assert!(TIMEOUTS[BackendKind::Tsdb as usize].load(Ordering::SeqCst) > tsdb_before);
    assert_eq!(
        TIMEOUTS[BackendKind::Rdbms as usize].load(Ordering::SeqCst),
        rdbms_before,
        "Tsdb 的超时不得落到 Rdbms 槽"
    );
}

/// 熔断真的打开（spec §8 判据 3）：连续超时后**快速失败**，不再等满超时。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let url = spawn_slow_clickhouse(
        Duration::from_secs(5),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let c = client_at(&url, 1, None);
    for _ in 0..5 {
        let _ = ecat_data::SqlExecutor::query(&c, "SELECT 1").await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);
    assert_eq!(c.breaker().opened_total(), 1, "超时失败必须真的打开过熔断器");

    let start = std::time::Instant::now();
    let err = ecat_data::SqlExecutor::query(&c, "SELECT 1")
        .await
        .expect_err("熔断已打开");
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "熔断打开后必须立即返回，实际 {:?}",
        start.elapsed()
    );
    assert!(matches!(err, RdbmsError::Connection(_)), "got: {err:?}");
}

/// **`_with` 落到 trait 默认实现，绝不触碰熔断器**（出入 7）。
///
/// 三个方法默认返回「不支持」—— 那是**调用方的用法错**，不是后端故障。
/// 若有人给 ClickHouse 补上 `_with` 的实现并"顺手"包进 `guarded`，
/// 5 次「不支持」就会把熔断器打开，之后**正常查询全被拒绝**。
#[tokio::test]
async fn unsupported_with_methods_do_not_trip_the_breaker() {
    let url = spawn_slow_clickhouse(
        Duration::from_millis(10),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let c = client_at(&url, 30, None);
    for _ in 0..8 {
        let _ = ecat_data::SqlExecutor::execute_with(&c, "UPDATE t SET x = ?", &[]).await;
        let _ = ecat_data::SqlExecutor::query_with(&c, "SELECT ?", &[]).await;
        let _ = ecat_data::SqlExecutor::query_write(&c, "INSERT INTO t VALUES (?)", &[]).await;
    }
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "「不支持」不是后端故障，不得计入熔断窗口"
    );
    assert_eq!(c.breaker().opened_total(), 0);
}

/// `transaction()` 是常量错误、不含 I/O —— 同样不得触碰熔断器（出入 6）。
#[tokio::test]
async fn transaction_error_does_not_trip_the_breaker() {
    let url = spawn_slow_clickhouse(
        Duration::from_millis(10),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let c = client_at(&url, 30, None);
    for _ in 0..8 {
        assert!(ecat_data::RdbmsClient::transaction(&c).await.is_err());
    }
    assert_eq!(c.breaker().state(), BreakerState::Closed);
    assert_eq!(c.breaker().opened_total(), 0);
}

/// 并发上限真的封顶（spec §8 判据 5）。
/// 起 N+1 个并发，断言**同时在线**的请求数不超过 N。
/// 每个请求 sleep 50ms，远小于 30 秒超时 —— 排队的那个不会因超时而失败。
#[tokio::test]
async fn concurrency_cap_limits_in_flight_requests() {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let url = spawn_slow_clickhouse(Duration::from_millis(50), Arc::clone(&in_flight)).await;
    let c = Arc::new(client_at(&url, 30, Some(2)));

    let mut handles = Vec::new();
    for _ in 0..3 {
        let c = Arc::clone(&c);
        let peak = Arc::clone(&peak);
        let in_flight = Arc::clone(&in_flight);
        handles.push(tokio::spawn(async move {
            // 采样：本请求在飞时看到的峰值
            let mut last = 0;
            let sampler = tokio::spawn(async move {
                for _ in 0..10 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    last = last.max(in_flight.load(Ordering::SeqCst));
                }
                last
            });
            let _ = ecat_data::SqlExecutor::query(&c, "SELECT 1").await;
            let seen = sampler.await.unwrap();
            peak.fetch_max(seen, Ordering::SeqCst);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert!(
        peak.load(Ordering::SeqCst) <= 2,
        "并发上限是 2，实测峰值 {}",
        peak.load(Ordering::SeqCst)
    );
}
```

> ⚠️ `spawn_slow_clickhouse` 的闭包里没有采样逻辑；上面的采样在**测试侧**用独立 task 轮询。这样 mock 保持简单，且采样不改变被观测的并发数。若轮询式采样不稳定（会 flaky），改用 `tokio::sync::Barrier` —— 但**不要**改成「不采样、只断言全部成功」（那对上限毫无约束，是空验收）。

- [ ] **Step 12: 确认新测试在跑**

```bash
cargo test -p ecat-data-clickhouse 2>&1 | grep -E '^test |^test result'
```

**24 → 30**（+6）。`mod resilience;` 漏写则**一个都不多**（静默 0 通过）—— 这就是假绿防线要盯的。

- [ ] **Step 13: 空验收自证（三条）**

1. 把 `guarded` 的 `.call(|| run_with_timeout(..))` 拆成只 `run_with_timeout`（去掉熔断）→ **`repeated_timeouts_open_the_breaker_and_fail_fast` 必须 FAILED**。
2. 把 `guarded` 里的 `run_with_timeout` 去掉、只留 `self.breaker.call(|| fut)` → **`query_times_out_with_timeout_error` 必须 FAILED**（超时不再发生，mock 5 秒后成功）。
3. 把 `permit()` 改成不做 `acquire`（返回一个假 permit 或直接不取）→ **`concurrency_cap_limits_in_flight_requests` 必须 FAILED**（峰值变 3）。

三条都要记录实际输出，逐条还原。

- [ ] **Step 14: `src/metrics.rs`**

与 Task 3 Step 12 **同构**（三个指标、手写 `Collector`、`OnceLock` 单例）。数据源换成：

| 指标 | 数据源 |
|---|---|
| `ecat_outbound_timeouts_total{backend}` | `TIMEOUTS[BackendKind::Rdbms as usize]` + `TIMEOUTS[BackendKind::Tsdb as usize]`（**两条路径各出一份样本**，标签值分别用 `backend` 与 `backend-tsdb`） |
| `ecat_outbound_breaker_open_total{backend}` | `breaker.opened_total()` |
| `ecat_outbound_breaker_state{backend}` | `state_code(breaker.state())` |

`lib.rs` 加：

```rust
#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;
```

测试同 Task 3 Step 13（`backend` 用 `"clickhouse-test"`）。

- [ ] **Step 15: 带 feature 跑**

```bash
cargo test -p ecat-data-clickhouse 2>&1 | grep -c 'outbound_metrics'   # 期望 0
cargo test -p ecat-data-clickhouse --features metrics 2>&1 | grep -E '^test result'
```

- [ ] **Step 16: 行数复核 + 闸门 + 提交**

```bash
find ecat-data-clickhouse/src -name '*.rs' -exec awk 'END{if(NR>500) print FILENAME": "NR}' {} \;
cargo fmt --all
cargo fmt --all -- --check; echo "fmt rc=$?"
cargo clippy -p ecat-data-clickhouse --all-targets --features metrics -- -D warnings; echo "clippy rc=$?"
git add ecat-data-clickhouse
git commit -m "feat(ecat-data-clickhouse): 出站超时 + 熔断 + 并发上限 + metrics feature"
```

### ⚠️ Task 4 必读

1. **两个 `query` 同名**：`SqlExecutor::query` 与 `TsdbClient::query` 都在 `ClickhouseClient` 上。测试里调用必须写全路径（`ecat_data::SqlExecutor::query(&c, ..)`），否则编译器选哪个不明确。
2. **`create_table` 不再单独包**（它在 `write` 的包装之内）。
3. **`_with` / `transaction()` 一个字都不写** —— 这是设计，不是漏做（出入 6/7）。
4. **并发上限不是 reqwest 的旋钮**，是我们自己的 `Semaphore`。要写进配置字段的 rustdoc（spec §8 判据 6 明确要求）。

---

## Task 5: 接入 checklist（**5b 的输入**）

**Files:**
- Create: `docs/superpowers/checklists/backend-resilience-onboarding.md`

**这是 spec §7.5 点名的 5a 交付物**。5b 要把同一套动作重复 11 次，checklist 就是防「10 次即兴发挥」的那一份。

- [ ] **Step 1: 建目录与文件，按下面六节写**

```bash
mkdir -p docs/superpowers/checklists
```

**必须包含以下六节**（标题照抄，节内内容用**本批 Redis / ClickHouse 的真实验证结果**填，不要写成泛泛的注意事项）：

1. `## 1. 加配置字段` —— `query_timeout_secs` / `breaker` / `max_concurrency` 三个字段的**逐字**声明 + `0 = 禁用` 转换函数 + 各自的行内 rustdoc。注明：`breaker` 字段依赖 `BreakerConfig: Deserialize`（Task 3 Step 5 加的）。
2. `## 2. 包 run_with_timeout` —— **熔断在外、超时在内**的顺序，附一段说明（为什么不能反过来：卡死后端会打不开熔断器 + 半开名额泄漏）。给出 `guarded` 外壳的两个变体（`RdbmsError` / `ecat_errors::Error`）。
3. `## 3. 加 Breaker 字段` —— 逐实例一个 `Arc<Breaker>`、`state()` 给路由用、`opened_total()` 给指标用、`guarded` 里闭包按需构造 future。
4. `## 4. 注册指标` —— 三个指标名 + 维度 + 数据源表；`[features] metrics = ["dep:ecat-metrics", "dep:prometheus"]`；手写 `Collector` 骨架指向 `ecat-data-sqlx/src/metrics.rs`；`backend` 标签值取该后端的**实例名**。
5. `## 5. 加一条超时测试` —— 判据三选一（按后端的可测性）：①有 HTTP 接口 → axum mock + 延迟（ClickHouse 模式）；②有原生连接 → 假 `TcpListener` 装死（mssql 模式）；③内层可替身 → 假 impl + `future::pending()`。**必须是端到端**（打真实方法），不能只测 `run_with_timeout` 本身。
6. `## 6. 加一条熔断测试` —— 连续失败后断言**两件事**：`state() == Open` **且** 下一次调用**不等满超时**就返回（时间断言）。只断言 `is_err()` 是空验收。

**外加两节**：

7. `## 7. 已知陷阱` —— 至少收录：`0` 是禁用不是零超时；默认实现的方法不要包（会把「用法错」记成后端故障）；常量错误返回的方法（如 ClickHouse 的 `transaction()`）不要包；`from_config` 已有的 reqwest 内层超时要写进 rustdoc；`--features metrics` 才能编译到指标代码；新建 `.rs` 必须在父模块加 `mod`。
8. `## 8. 验收命令` —— 每个 crate 的 `cargo test -p <crate>`、`--features metrics`、`cargo fmt --all -- --check`、`cargo clippy -p <crate> --all-targets --features metrics -- -D warnings`、行数复核 `awk` 一行命令。

- [ ] **Step 2: 自检 —— checklist 里的每一节，本批都有对应的落地文件**

用这条命令验证六节都能指到实证（不是空清单）：

```bash
grep -c 'ecat-data-redis\|ecat-data-clickhouse\|ecat-data-sqlx\|ecat-data-mssql' docs/superpowers/checklists/backend-resilience-onboarding.md
```

**期望 ≥ 8**。若某节举不出本批的实例，说明那一节是凭空写的，删掉或改写。

- [ ] **Step 3: 提交**

```bash
git add docs/superpowers/checklists/backend-resilience-onboarding.md
git commit -m "docs(checklist): 出站韧性接入 checklist（5b 逐 crate 照做）"
```

---

## Task 6: 文档（配置教程 + Redis 能力边界）

**Files:**
- Modify: `docs/database-config-tutorial.md`
- Modify: `docs/i18n/{ar,bn,de,en,es,fr,hi,id,ja,ko,pt,ru}/database-config-tutorial.md`（**12 份**）

**共 13 份手写镜像**（root 中文 + 12 语言）。每份要改**两处**：

- [ ] **Step 1: Redis 段（root 在 `:228-241`）**

YAML 示例补两项、表格补两行：

```yaml
redis:
  url: "redis://host:6379"
  # password: "auth_token"     # 可选
  # query_timeout_secs: 30     # 可选：单次命令超时，0 = 禁用
  # breaker: {}                # 可选：熔断配置，省略 = 保守默认（0.5 / 30s / 打开 10s）
```

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `query_timeout_secs` | `Option<u64>` | `30` | 单次命令超时秒数；**`0` = 禁用** |
| `breaker` | `Option<BreakerConfig>` | 保守默认 | 熔断阈值与窗口；`{"enabled": false}` 之类的整体关闭见下 |

并加一段 **Redis 多路复用的能力边界**（spec §6 明确要求，目前完全没有文档）：

> `RedisCache` 用 `MultiplexedConnection`（一条 TCP 服务所有并发），**不是连接池** —— 缓存负载下这比池更优。代价是**有状态命令序列不能用它**：`MULTI`/`EXEC`、`WATCH`、`SUBSCRIBE`、阻塞命令需要独占连接。

- [ ] **Step 2: ClickHouse 段（root 在 `:257-275`）**

YAML 与表格补三项 `query_timeout_secs` / `breaker` / `max_concurrency`（默认 30 / 保守默认 / 32）。**必须写明两层超时的关系**（出入 8）：

> 注意 `ecat-tls` 建的 `reqwest::Client` 自带 30 秒总超时（`ecat-tls/src/lib.rs:83-84,97-98`），`query_timeout_secs` 是**外层**；两层都在时**谁先到谁生效**，且内层那次的错误是 `RdbmsError::Database`、**不计入** `ecat_outbound_timeouts_total`。

- [ ] **Step 3: i18n ×12 同步**

**12 个语言目录**：`ar bn de en es fr hi id ja ko pt ru`。按各语言的既有译法翻译；**代码块内的字段名与 YAML 值不翻译**，表格里的 `Option<u64>` 等类型名不翻译。

- [ ] **Step 4: 校验 13 份都改了**

```bash
grep -lc 'query_timeout_secs' docs/database-config-tutorial.md docs/i18n/*/database-config-tutorial.md | wc -l
```

**期望 13。** 少于 13 就是漏翻（i18n 是 13 份手写镜像，没有自动同步）。

**不要**动 README 的后端表（出入 10：5b 统一改 14 份）。

- [ ] **Step 5: 提交**

```bash
git add docs/database-config-tutorial.md docs/i18n
git commit -m "docs: 配置教程补 Redis/ClickHouse 的超时与熔断字段，写明多路复用能力边界"
```

---

## Task 7: CHANGELOG + 版本 6.0.0

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `Cargo.toml`（workspace 版本）+ 37 个 crate 的内部 path 依赖引用
- Modify: `Cargo.lock`
- Modify: `README.md` / `README.en.md` + `docs/i18n/{12}/README.md`（各 2 处）

- [ ] **Step 1: CHANGELOG 加 6.0.0 段**

**破坏性变更**（这是进位主版本号的唯一理由，要点名）：
- `ecat_data::run_with_timeout` 签名变更：`(Option<Duration>, F)` → `(BackendKind, Option<Duration>, F)`
- **删除** `ecat_data::QUERY_TIMEOUTS`（由 `ecat_data::TIMEOUTS` + `BackendKind` 取代）

新增：`BackendKind` / `TimeoutError` / `TIMEOUTS`、`map_breaker_error` 与 `breaker_error_to_backend_error` 公开、`Breaker::opened_total()`、`BreakerConfig: Deserialize`、`RedisConfig` / `ClickhouseConfig` 三个新字段、`ecat-data-redis` 与 `ecat-data-clickhouse` 的 `metrics` feature 与 `ecat_outbound_*` 三个指标。

- [ ] **Step 2: bump 前先核**

```bash
git ls-files '*Cargo.toml' | xargs grep -h '5\.0\.0' | grep -v '^ecat' | sort -u
```

**期望只剩一行** `version = "5.0.0"`（workspace 版本行）。若还有别的，说明有第三方依赖恰好也是 5.0.0，**停下来报给 lead**。

- [ ] **Step 3: bump 5.0.0 → 6.0.0**

```bash
git ls-files '*Cargo.toml' | xargs sed -i 's/"5\.0\.0"/"6.0.0"/g'
sed -i 's/5\.0\.0/6.0.0/g' README.md README.en.md docs/i18n/*/README.md
cargo metadata --format-version=1 --offline > /dev/null; echo "rc=$?"
```

（实测：`Cargo.toml` 里 5.0.0 共 **87 处**、README 系 **14 份 × 2 处**。）

- [ ] **Step 4: `cargo metadata` 会顺带重生成 `Cargo.lock`**

```bash
git diff --stat Cargo.lock
```

**核一眼**：`Cargo.lock` 里若有**第三方** crate 恰好是 5.0.0（前例：`fsevent-sys` / `rusticata-macros`），`sed` 不该碰它们 —— 它们不在 `git ls-files '*Cargo.toml'` 里，所以不会被误改；但 `Cargo.lock` 的 diff 里若出现它们的版本变化，说明改错了，还原重来。

- [ ] **Step 5: 发布前闸门（**本批唯一的全量验证点**）**

```bash
cargo test --workspace; echo "rc=$?"
cargo test --workspace --doc 2>&1 | grep -E '^test result'; echo "rc=${PIPESTATUS[0]}"
cargo fmt --all -- --check; echo "rc=$?"
cargo clippy --workspace --all-targets -- -D warnings; echo "rc=$?"
cargo test -p ecat-data-redis -p ecat-data-clickhouse -p ecat-data-sqlx -p ecat-data-mssql --all-features 2>&1 | grep -E '^test result'
```

**最后一条不能省**：三个 `metrics` feature 默认关，`cargo test --workspace` 用的是 feature **并集**（批次 3 的教训），但 `--all-features` 才是把两个新 metrics 模块真正编进来的那一次。

- [ ] **Step 6: 提交**

```bash
git add CHANGELOG.md Cargo.toml Cargo.lock README.md README.en.md docs/i18n
git ls-files '*Cargo.toml' | xargs git add
git commit -m "chore: 版本 5.0.0 → 6.0.0（run_with_timeout 签名变更 + QUERY_TIMEOUTS 删除，破坏性）"
```

---

## 批次完成判据

| # | 判据 | 验证 |
|---|---|---|
| 1 | `cargo test --workspace` 全绿，测试数 ≥ 1096（批次 4 末值）+ 本批新增 | Task 7 Step 5 |
| 2 | **13 个 `run_with_timeout` 调用点全部迁移**（实测数，非 spec 的 16） | `rg -n 'run_with_timeout\(' --type rust \| wc -l` 与 Task 1 Step 1 基线比对 |
| 3 | `QUERY_TIMEOUTS` 在仓内**零残留**（除 CHANGELOG 的历史段） | `rg 'QUERY_TIMEOUTS' --type rust` 输出为空 |
| 4 | **两个 `metrics.rs` 消费者在 `--features metrics` 下编译并测试通过** | Task 1 Step 8 最后一条 |
| 5 | Redis 与 ClickHouse 各有**一条真超时**测试（断言 `DeadlineExceeded` / `Timeout`） | Task 3 Step 9、Task 4 Step 11 |
| 6 | Redis 与 ClickHouse 各有**一条真熔断**测试（断言 `Open` **且** 快速返回的时间判据） | 同上 |
| 7 | `TIMEOUTS` 分维度互不串（`Cache`/`Tsdb`/`Rdbms` 各一条断言） | Task 1 Step 2 + Task 4 Step 11 |
| 8 | ClickHouse 并发上限可验证（N+1 并发、峰值 ≤ N） | Task 4 Step 11 |
| 9 | `_with` 方法与 `transaction()` **不**触发熔断（两条把守测试） | Task 4 Step 11 |
| 10 | 每个源文件 < 500 行（含新建的 `tests.rs` / `tsdb.rs` / `metrics.rs` / `resilience.rs`） | 各 Task 的行数复核 |
| 11 | `cargo clippy --workspace --all-targets -- -D warnings` rc=0 | Task 7 Step 5 |
| 12 | checklist 落在 `docs/superpowers/checklists/backend-resilience-onboarding.md`，八节齐 | Task 5 Step 2 |
| 13 | `database-config-tutorial.md` ×13 都含 `query_timeout_secs` | Task 6 Step 4 |
| 14 | 既有用户代码零改动：所有新字段都有默认值，`new()` / `connect()` 也拿到全部能力 | Task 3/4 的构造器步骤 |

**本批不做**（留 5b）：其余 10 个 HTTP 后端 + MongoDB 池配置；README 后端表；按实例（而非后端类别）的指标维度；延迟直方图；重试。

**本批不做，且是判断不是遗漏**：Redis 多路复用换连接池（spec §6）、`RedisLock` 的 `DistributedLock` 路径（出入 9）、`ecat-middleware` 的 tower 层、Memcached。

---

## 执行记录（落码后填）

（执行者按批次 4 的格式补：五步闸门的 rc 与计数、判据逐条结论、任何与计划的偏离及证据。）

| 步骤 | 结果 |
|---|---|
| `cargo test --workspace` | |
| `cargo test --workspace --doc` | |
| `cargo fmt --all -- --check` | |
| `cargo clippy --workspace --all-targets -- -D warnings` | |
| `--all-features` 四 crate | |
