# 批次 5a 实施计划：出站韧性试点 —— 泛型超时助手 + Redis + ClickHouse

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `run_with_timeout` 泛型化（多一个「后端类别」维度、支持 `ecat_errors::Error`），并在 **Redis**（非 HTTP 客户端路径）与 **ClickHouse**（同时实现 `RdbmsClient` 与 `TsdbClient` 的最难 case）两个后端上试点「超时 + 熔断 + 指标」的完整接法；产出一份供 5b 逐 crate 照做的接入 checklist。

**Architecture:** 三个非 RDBMS trait 里，Redis 走 `Cache`、ClickHouse 走 `SqlExecutor` + `TsdbClient`，都是 `ecat-data` 已有 trait 的**实现体内**包装（不是 opt-in 包装器），用户代码零改动。`ecat-data` 只出**泛型助手**（`BackendKind` / `TimeoutError` / `run_with_timeout` / `TIMEOUTS`）与**熔断器错误映射**；`ecat-circuit-breaker::Breaker` 逐 client 持有一个（批次 4 已公开，直接复用）。三个出站指标的超时维度按 `BackendKind` 分，熔断两项按 client 的 `Breaker` 现读；**三个指标家族全进程只注册一个 collector**（在 `ecat-metrics`，各后端只挂数据源 —— 见「出入 11」），`metrics` feature opt-in。

**Tech Stack:** `ecat-data`（`SqlExecutor` / `RdbmsError` / `Cache` / `TsdbClient`）· `ecat-errors`（`Error` / `ErrorCode`）· `ecat-circuit-breaker`（`Breaker` / `BreakerConfig` / `BreakerState` / `BreakerError`）· `ecat-metrics` + `prometheus`（opt-in）· `tokio`（`time::timeout`、`sync::Semaphore`）· `redis` 0.27 · `reqwest` 0.12。

**设计依据:** `docs/superpowers/specs/2026-10-06-outbound-resilience-design.md` §1–§4、§6、§7.5、§8；写法与严格度参照 `docs/superpowers/plans/2026-10-07-batch4-pool-and-observability.md`。

**本计划含 lead 落码前的两条追加裁决**（写在对应 Task 里，不另起章节）：

- **追加 1（必须做）** → Task 2a Step 9-11：`Breaker` 的半开探测名额 **RAII 归还**。这是 **5.0.0 已发布到 crates.io 的真实缺陷**（任何外部取消都漏一个名额，`half_open_probes` 次之后永久锁死），与 5a 选哪个嵌套顺序无关。
- **追加 2（可延后，有停下报 lead 的条件）** → Task 3：`ecat-data-sqlx` / `ecat-data-mssql` 的 `ecat_rdbms_*` 四个同名家族的撞名 —— 用「出入 11」那套共用 collector 修掉。

---

## ⚠️ 与 spec 的实测出入（**先读这节，不要按 spec 原文照做**）

写本计划时逐条实测。**数量对不上的一律以实测为准**，并在下文据此展开。

### 出入 1：版本 —— spec 说的「4.0.0 窗口」已过期

| | |
|---|---|
| spec §1 原文 | 「与批次 1 的 4.0.0 一同发布即可（`ecat-data` 尚未进入 4.0.0 发布窗口）」 |
| 实测（2026-10-07） | `Cargo.toml:70` `version = "5.0.0"`；`git log` 显示 `4.1.0 → 5.0.0` 的 bump 已提交（`2ed667a`）。**4.0.0 与 5.0.0 都已发布** |

**结论**：本次签名变更进**下一个版本 = 6.0.0**（`run_with_timeout` 签名变更 + `QUERY_TIMEOUTS` 删除都是破坏性的）。本批执行 bump（Task 8）—— 与批次 4 的先例一致（破坏性变更与 bump 同批），且 `main` 不能长期挂着「标着 5.0.0 的破坏性变更」。

bump 的**逐步做法与实测数**在 Task 8 Step 2-4：**38 个 `Cargo.toml`**（`[workspace.package]` 1 处 + 86 处内部 path 引用）+ `Cargo.lock` + **14 份 README**（根 `README.md`、`README.en.md` + `docs/i18n/{ar,bn,de,en,es,fr,hi,id,ja,ko,pt,ru}/README.md`，每份 2 处 = 28 处）。⚠️ **i18n 那 12 份是本仓库踩过的坑**（批次 4 漏过），Task 3 还会给 `ecat-metrics/Cargo.toml` 加 2 处，届时以 Step 2 的重新实测为准。

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

> **已裁决（lead 逐条复核后）：按本计划的「熔断在外」展开。** lead 按源码逐条复核了上面三条依据（`:104-107` 名额先借、`:113` 的 await 是 drop 点、`:124-130` 的 record 在 await 之后、半开分支在 `count >= probes` 时提前 return），**三条全成立** —— spec §3 给的理由与它自己的代码相反，「超时在外」的固有语义（问题 1）无解。
>
> **问题 2 要单独修，这与选哪个顺序无关**：它不是「因为熔断在外才需要」。任何外部取消（`tokio::select!`、请求处理被 drop、调用方自己的超时层）都会漏掉一个半开探测名额，`half_open_probes` 次之后熔断器**永久停在 `HalfOpen`**（每次调用直接 `ProbesExhausted`，无路可退）。5.0.0 **已发布到 crates.io**，所以这条一并修 —— 见 **Task 2a Step 9-11「追加 1」**（先红后绿的证据链），5b 的 11 个后端因此不必各踩一次。
>
> **spec 原文与实测对照留在这里**（上面那三行 spec §3 的原文与 `spec 的理由`），将来翻 spec 的人需要看到它：**spec 说「若熔断在内，被超时掐断的调用仍会被记为一次待定/失败」，实测正好相反** —— 熔断在内时被掐断的调用**一次都不记**（`window.record` 在 await 之后）。别照着 spec 那句推断代码行为。

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

**5b 的硬约定（lead 点名，别漏）**：README 族 = 根 `README.md` + `README.en.md` + `docs/i18n/` 下的 **12** 个语言目录（`ar` `bn` `de` `en` `es` `fr` `hi` `id` `ja` `ko` `pt` `ru`）各一份 —— **共 14 份手写镜像**（不是生成的）。任何一处改动（版本号、crate 数、目录树、后端能力表）都要**同步全部 14 份**，漏一份就造出不一致。

⚠️ **别与「数据库配置教程族 13 份」混**：那是根 1 + i18n **12**。i18n 侧一律 **12** 个语言目录 ——「14」只属于 README 族总数（`docs/i18n` 下**没有** `zh` 目录，简体中文就是根 `README.md`）。

### 出入 11：三个指标**必须**是全进程一份 collector，不能照抄批次 4 的「每 crate 一份」

spec §4 只给了三个指标名与维度，没说 collector 放哪。批次 4 的先例是「collector 放在后端 crate 里、`metrics` feature 门控」（`ecat-data-sqlx/src/metrics.rs:1-17` 有完整说明）。**5a 不能照抄这一条**：

`ecat-metrics/src/lib.rs:12` 是 `static REGISTRY: OnceLock<Registry>` —— **全进程一个 registry**。而 `prometheus::Registry` 按「指标名 + 常量标签」去重，同名 `Collector` 注册第二次直接返回 `AlreadyReg`；`ecat-data-sqlx/src/metrics.rs:43-49` 的注释自己写明了后果：**「那时四个指标都不会输出」**。

于是「每 crate 一份 collector」在 5a 就会坏：Redis 与 ClickHouse 的 metrics feature 同时打开时，**谁先注册谁独活，另一个的三个指标静默消失** —— 没有报错、没有日志，`cargo test -p ecat-data-redis` 还是全绿（单 crate 测试看不到另一个 crate 的注册）。5b 的 11 个后端会把这个坑复制 11 倍。

**结论**：三个 `ecat_outbound_*` 指标家族在 `ecat-metrics` 里**只建一份**（`src/outbound.rs`），各后端只往里挂自己的数据源（三个取值闭包）。依赖不加：`ecat-metrics` 现有 `prometheus` 已够，数据源用 `Box<dyn Fn>`，因此**不必**依赖 `ecat-circuit-breaker`（拖 tower）或 `ecat-data`（拖 tokio + async-trait）。每个后端只需 ~10 行注册代码，5b 复用 11 次。

> **实测附注 → 已升级为本批的「追加 2」（Task 3）**：批次 4 的 `ecat-data-sqlx` 与 `ecat-data-mssql` 有**同一个已发布的真实 bug** —— 两者的 `register_pool_metrics` 都建 `ecat_rdbms_*` 四个同名家族（`sqlx/src/metrics.rs:88-109`、`mssql/src/metrics.rs:93-140` 同构），而 `sqlx/src/metrics.rs:47-48` 那句注释「**这里的名字是本 crate 独占的**」**是错的** —— 它是那句 `let _ =` 的唯一理由，也正是这个 bug 活到今天的原因。同时打开两个 metrics feature 时，后注册者的 `register()` 拿到 `AlreadyReg` 被吞掉，**它的四个指标一条样本都不输出**（无报错、无日志；单 crate 测试全绿）。
>
> lead 裁决：**本批修，可延后**，用本节这套「collector 在 `ecat-metrics`、各 crate 只挂数据源」的同一模式 —— 做法与停下报 lead 的条件见 **Task 3**。

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
  breaker.rs        已有（9744 B）；加 opened_total()、BreakerState::code()、BreakerConfig 的 Deserialize

ecat-metrics/src/
  outbound.rs       新建：三个 ecat_outbound_* 指标的**全进程唯一** collector
  rdbms.rs          新建（Task 3）：四个 ecat_rdbms_* 指标的**全进程唯一** collector
                    —— 修批次 4 留下的撞名（同名家族被 AlreadyReg 顶掉）
  lib.rs            加 mod outbound; / mod rdbms; + 重导出

ecat-metrics/tests/
  rdbms_shared_families.rs  新建（Task 3）：**跨 crate** 回归 —— 两个后端同进程时样本都不丢

ecat-data-sqlx/src/
  metrics.rs        改（Task 3）：不再自建 collector，改为往 ecat-metrics 挂数据源

ecat-data-mssql/src/
  metrics.rs        改（Task 3）：同上

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
  backend-resilience-onboarding.md   新建：5b 的接入 checklist（Task 6 产出）
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
3. **`QUERY_TIMEOUTS` 的删除是破坏性变更**，Task 8 的 CHANGELOG 要点名。
4. `ecat-data/Cargo.toml` **不需要**改 —— `ecat-errors` 与 `ecat-circuit-breaker` 都已是依赖。

---

## Task 2: 基础设施 —— `opened_total()` / `BreakerState::code()` / `BreakerConfig` 反序列化 / **半开名额 RAII** / 全进程共用的出站 collector

**Files:**
- Modify: `ecat-circuit-breaker/src/breaker.rs`
- Modify: `ecat-circuit-breaker/Cargo.toml`（加 serde）
- Create: `ecat-metrics/src/outbound.rs`
- Modify: `ecat-metrics/src/lib.rs`

**两个 crate、三次提交**（Step 1-8 = 2a 的只读计数与映射；Step 9-11 = 2a 的**半开名额 RAII 归还**（lead 追加 1）；Step 12-17 = 2b 的全进程共用出站 collector）。都是「两个试点后端都要用的公共件」，与 Redis / ClickHouse 无关，所以先做。理由见「出入 5」（`ecat_outbound_breaker_open_total` 没有数据源）、「出入 4 裁决段」（半开名额是真缺陷，5.0.0 已发布）与「出入 11」（三个指标必须全进程一份 collector）。

`ecat-circuit-breaker` 部分**只加只读计数器、一个数值映射，以及 Step 9-11 的一处取消安全修复（半开名额 RAII 归还）** —— 不改状态迁移逻辑本身。批次 4 的既有测试必须逐条保持全绿。

### 2a：`ecat-circuit-breaker`

- [ ] **Step 1: 基线**

```bash
cargo test -p ecat-circuit-breaker 2>&1 | grep -E '^test |^test result'
```

记下**测试名**（不是只记总数）。

- [ ] **Step 2: `Cargo.toml` 加 serde**

```toml
[dependencies]
serde.workspace = true
thiserror.workspace = true
tower = { workspace = true, features = ["util"] }
tracing.workspace = true
```

（`serde` 在 workspace 表里已经是 `{ version = "1", features = ["derive"] }`（`Cargo.toml:96`），按全仓惯例写 `serde.workspace = true` 即可。）

- [ ] **Step 3: `Inner` 加字段 + `Breaker::new` 初始化**

`:41-46` 的 `Inner` 加最后一项：

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

`:59-64` 的 `Inner { .. }` 字面量加 `opened_total: 0,`。

- [ ] **Step 4: 两处打开点各 +1**

`call` 里 `inner.state = BreakerState::Open;` 出现在**两处**，都在 `opened_at = Some(Instant::now());` 上一行：

| 位置 | 分支 |
|---|---|
| `:141-142` | Closed 分支，按失败率打开 |
| `:154-155` | HalfOpen 分支，探测失败重新打开 |

**两处都在 `opened_at = Some(Instant::now());` 之后**加同一行：

```rust
                    inner.opened_total += 1;
```

- [ ] **Step 5: `BreakerState::code()`**

`:13` 之后（`BreakerState` 的枚举定义紧后面）：

```rust
impl BreakerState {
    /// 指标的数值编码：`0` = closed、`1` = open、`2` = half-open。
    ///
    /// `ecat_outbound_breaker_state` 的取值即此。**不要改这个映射** ——
    /// 告警规则与仪表盘会写死这三个数；将来加状态只能往后再追加数字。
    ///
    /// 放在本 crate 而不是指标侧：编码器与枚举定义放一起才不会各改各的。
    pub fn code(self) -> u8 {
        match self {
            BreakerState::Closed => 0,
            BreakerState::Open => 1,
            BreakerState::HalfOpen => 2,
        }
    }
}
```

- [ ] **Step 6: `Breaker::opened_total()`**

放在 `state()`（`:174-184`）之后、`lock()`（`:186`）之前：

```rust
    /// `Closed → Open` 的累计次数（半开探测失败重新打开也计入）。
    ///
    /// 供指标 `ecat_outbound_breaker_open_total`。用 `state()` 轮询猜测
    /// 「开了几次」是错的 —— 轮询间隔决定准确性，而且熔断器可能开又关，
    /// 两次探测之间发生的事抓不到。
    pub fn opened_total(&self) -> u64 {
        self.lock().opened_total
    }
```

- [ ] **Step 7: `BreakerConfig` 可反序列化**

`:15-34` 整段替换为（**默认值的数字一字不改**，但改成一处定义）：

```rust
/// 熔断阈值。字段与 `CircuitBreakerLayer` 的 builder 一一对应，
/// 默认值与 `CircuitBreakerLayer::new()` 相同（0.5 / 30s / 3 / 10s）。
///
/// 每个字段都有 `#[serde(default)]`：配置文件里写 `breaker: {}`
/// 与 `breaker: {"failure_ratio": 0.5}` 都必须能反序列化（前者是常见写法）。
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

fn default_failure_ratio() -> f64 {
    0.5
}

fn default_window() -> Duration {
    Duration::from_secs(30)
}

fn default_half_open_probes() -> u32 {
    3
}

fn default_open_duration() -> Duration {
    Duration::from_secs(10)
}

/// `Duration` ↔ 秒的 serde 适配：配置里写 `window: 30`，
/// 而不是 serde 默认给的 `{secs, nanos}` 结构体。
mod duration_secs {
    use serde::{Deserialize, Deserializer};
    use std::time::Duration;

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_secs(u64::deserialize(d)?))
    }
}

impl Default for BreakerConfig {
    /// 走同一组 `default_*` 函数，而不是把四个数字再抄一遍 ——
    /// 抄两遍就会分叉：serde 侧省略字段拿到 30 秒、`Default` 侧拿到别的。
    fn default() -> Self {
        Self {
            failure_ratio: default_failure_ratio(),
            window: default_window(),
            half_open_probes: default_half_open_probes(),
            open_duration: default_open_duration(),
        }
    }
}
```

- [ ] **Step 8: 三条测试 + 逐条比对 + 提交**

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
        // 窗口的样本下限是 5（`breaker.rs:134` 的 `window.total() >= 5`），
        // 恰好打满才触发打开。
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

    /// 三个状态的编码是**对外契约**（告警规则里写死了这些数），
    /// 逐个钉住而不是只钉一个 —— 只钉一个的话，改另一个的映射不会被发现。
    #[test]
    fn state_codes_are_the_metric_contract() {
        assert_eq!(BreakerState::Closed.code(), 0);
        assert_eq!(BreakerState::Open.code(), 1);
        assert_eq!(BreakerState::HalfOpen.code(), 2);
    }

    /// serde 默认值必须与 `Default` **逐字段相同** ——
    /// 配置文件省略字段与代码里 `BreakerConfig::default()` 是两条路径，
    /// 分叉了就是「同一份配置在两种构造方式下行为不同」。
    #[test]
    fn deserialized_defaults_match_default_impl() {
        let from_json: BreakerConfig = serde_json::from_str("{}").unwrap();
        let d = BreakerConfig::default();
        assert_eq!(from_json.failure_ratio, d.failure_ratio);
        assert_eq!(from_json.window, d.window);
        assert_eq!(from_json.half_open_probes, d.half_open_probes);
        assert_eq!(from_json.open_duration, d.open_duration);

        // 显式给值也要生效（只测 `{}` 的话，一个忽略输入的实现也能过）。
        let given: BreakerConfig =
            serde_json::from_str(r#"{"failure_ratio": 0.9, "window": 5}"#).unwrap();
        assert_eq!(given.failure_ratio, 0.9);
        assert_eq!(given.window, Duration::from_secs(5));
    }
```

（`mod tests` 顶部若没有 `use super::*;` 就补上；`serde_json` 要加进这个 crate 的 `[dev-dependencies]`（`serde_json.workspace = true`）。）

```bash
cargo test -p ecat-circuit-breaker 2>&1 | grep -E '^test |^test result'
```

**与 Step 1 逐条比对**：既有测试名一个不少、全绿；总数 **+3**。

空验收自证：把 Step 4 加的两行 `inner.opened_total += 1;` 全部注释掉重跑 —— **`opened_total_counts_each_open_not_the_state` 必须 FAILED**。记录输出，还原。

```bash
cargo fmt --all
cargo fmt --all -- --check; echo "fmt rc=$?"
cargo clippy -p ecat-circuit-breaker --all-targets -- -D warnings; echo "clippy rc=$?"
git add ecat-circuit-breaker
git commit -m "feat(ecat-circuit-breaker): opened_total 计数、BreakerState::code 编码、BreakerConfig 可反序列化"
```

- [ ] **Step 9: 半开探测名额的归还 —— 先写会红的测试（lead 追加 1）**

**背景（源码级）**：`Breaker::call` 在 `HalfOpen` 分支**先借名额再 await** —— `breaker.rs:104-107` 的 `inner.half_open_count += 1` 在 `:113` 的 `let result = f().await;` **之前**；而名额的「用掉」只发生在 `:116-159` 的记录逻辑里，那在 await **之后**。于是**任何**外部取消都会让 future 在 `:113` 处被 drop，名额**有借无还**。

**后果**：`half_open_probes`（默认 3）次被取消的探测之后，`:104` 的 `if inner.half_open_count >= self.config.half_open_probes` 恒成立 ⇒ 每次调用**立即**返回 `Err(BreakerError::ProbesExhausted)`，而 `state()` 永远停在 `HalfOpen`（没有任何路径能从 `HalfOpen` 退出而不经过一次成功/失败的记录）。**永久性故障，只能重启进程。**

**这不是理论**：`tokio::time::timeout` / `select!` / 请求处理被 drop 全都是取消源，而 5a / 5b 正是要给每个后端套一层超时。

先加测试，**确认它在修复前失败**：

```rust
    /// 被取消的探测**必须**归还名额。
    ///
    /// 借出名额（`breaker.rs:107`）在 `f().await`（`:113`）之前，而名额的用掉
    /// （`:116-159` 的记录逻辑）在 await 之后 —— 中间的 future 一旦被 drop，
    /// 名额就丢了。`half_open_probes` 次之后熔断器永久停在 `HalfOpen`
    /// （每次调用直接 `ProbesExhausted`），**没有任何路径能恢复**。
    ///
    /// 这里用 `tokio::time::timeout` 制造 drop —— 它正是 5a / 5b 给每个后端
    /// 套的那一层，也是线上最常见的取消源。
    #[tokio::test]
    async fn cancelled_half_open_probes_return_their_permit() {
        let cfg = BreakerConfig {
            open_duration: Duration::from_millis(20),
            ..BreakerConfig::default()
        };
        let probes = cfg.half_open_probes;
        let b = Breaker::new(cfg);

        // 打满窗口的样本下限（5，见 `breaker.rs:134`）把熔断器打开。
        let fail = || async { Err::<(), &str>("backend down") };
        for _ in 0..5 {
            let _ = b.call(fail).await;
        }
        assert_eq!(b.state(), BreakerState::Open);
        tokio::time::sleep(Duration::from_millis(30)).await;

        // 借出名额后立刻取消，重复 `probes + 1` 次 —— 要比 `probes` 多一次：
        // 冷却后的**第一次**调用只做 `Open → HalfOpen` 迁移（`breaker.rs:92-102`），
        // 它**不借名额**，所以 `probes` 次取消只借走 `probes - 1` 个。
        for _ in 0..(probes + 1) {
            let hang = || async { std::future::pending::<Result<(), &str>>().await };
            let _ = tokio::time::timeout(Duration::from_millis(5), b.call(hang)).await;
        }

        // 名额都还回来了 ⇒ 还能放行探测，且这次成功探测把熔断器关回去。
        let ok = || async { Ok::<(), &str>(()) };
        assert!(
            b.call(ok).await.is_ok(),
            "被取消的探测没归还名额 ⇒ 永久 ProbesExhausted"
        );
        assert_eq!(b.state(), BreakerState::Closed);
    }
```

```bash
cargo test -p ecat-circuit-breaker cancelled_half_open_probes 2>&1 | grep -E '^test |^test result|panicked|永久'
```

**这一步必须 FAILED**（`ProbesExhausted` 不是 `Ok`）。**记录输出** —— 那是这条修复存在的证据。⚠️ 若这里就绿了，说明取消路径没被真正覆盖（例如 `timeout` 没 drop 到内层 future），**停下来查，别往下走**。

- [ ] **Step 10: 实现 `ProbePermit`（RAII 归还）**

在 `BreakerError` 定义附近加：

```rust
/// 半开探测名额的 RAII 归还。
///
/// 名额在 `f().await` **之前**借出（`call` 的 `HalfOpen` 分支），用掉却在那之后的
/// 记录逻辑里 —— 中间的 future 被 drop 就跑不到记录逻辑。本类型把「归还」挂到
/// `Drop` 上，让取消路径自动还名额。
///
/// **不归还的后果**：`half_open_probes` 次被取消的探测之后，熔断器永久停在
/// `HalfOpen`（每次调用立即 `ProbesExhausted`），只能靠重启进程恢复。
struct ProbePermit<'a> {
    breaker: &'a Breaker,
    /// `false` = 探测已记入滑动窗口，名额算用掉了，`Drop` 不再归还。
    armed: bool,
}

impl ProbePermit<'_> {
    /// 探测已按成功/失败记入窗口 —— 撤销归还。
    ///
    /// **必须在记录之后调用**：提前 disarm 等于回到「有借无还」。
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for ProbePermit<'_> {
    fn drop(&mut self) {
        if self.armed {
            let mut inner = self.breaker.lock();
            // `saturating_sub`：`armed` 与「借出过」同源，理论上不会到 0；
            // 真到了也宁可少还一个也不要在抓取/调用路径上 panic。
            inner.half_open_count = inner.half_open_count.saturating_sub(1);
        }
    }
}
```

`call` 的头部改成「借名额时一并取出 permit」：

```rust
        let permit = {
            let mut inner = self.lock();
            match inner.state {
                BreakerState::Open => {
                    if let Some(opened_at) = inner.opened_at {
                        if opened_at.elapsed() >= self.config.open_duration {
                            tracing::info!("circuit breaker: open → half-open");
                            inner.state = BreakerState::HalfOpen;
                            inner.half_open_count = 0;
                        } else {
                            return Err(BreakerError::Open);
                        }
                    }
                    // 迁移这一跳不借名额（与原实现一致）。
                    None
                }
                BreakerState::HalfOpen => {
                    if inner.half_open_count >= self.config.half_open_probes {
                        return Err(BreakerError::ProbesExhausted);
                    }
                    inner.half_open_count += 1;
                    Some(ProbePermit {
                        breaker: self,
                        armed: true,
                    })
                }
                BreakerState::Closed => None,
            }
        };
```

并在 `result.map_err(BreakerError::Inner)` **之前**（即 `:132-159` 的状态迁移 match 之后）加一句：

```rust
        // 探测已记录 ⇒ 名额归还的责任已由状态机承担，撤销 Drop 里的归还。
        if let Some(permit) = permit {
            permit.disarm();
        }
```

⚠️ **别改状态迁移本身**：permit 只多归还一次名额，`state` / `window` / `opened_at` 的语义一个都不动。`disarm()` 的位置必须在记录之后（提前 disarm 会让**同一次探测**既被记录又被归还，名额多还一个）。⚠️ **但这与「取消修复」无关**（2026-10-08 复核纠正）：取消路径**根本不经过 `disarm`** —— future 在 `f().await` 处被 drop，走的是 `Drop` 归还，所以 `disarm` 放前放后都不影响取消语义。原计划此处写「放前面就等于没修」，不准确。

```bash
cargo test -p ecat-circuit-breaker cancelled_half_open_probes 2>&1 | grep -E '^test |^test result'
```

**必须 PASSED。** 然后把 Step 9 记下的 RED 输出与这里的 GREEN 一起贴进「执行记录」。

- [ ] **Step 11: 全量复核 + 空验收自证 + 提交**

```bash
cargo test -p ecat-circuit-breaker 2>&1 | grep -E '^test |^test result'
```

**与 Step 1 基线逐条比对**：既有测试名一个不少、全绿；总数 = Step 8 之后的值 **+1**。

空验收自证：把 `ProbePermit::drop` 里的 `if self.armed { .. }` **整块注释掉**重跑 —— `cancelled_half_open_probes_return_their_permit` 必须 FAILED（这同时就是 5.0.0 线上缺陷的复现）。记录输出，还原。

```bash
cargo fmt --all
cargo fmt --all -- --check; echo "fmt rc=$?"
cargo clippy -p ecat-circuit-breaker --all-targets -- -D warnings; echo "clippy rc=$?"
git add ecat-circuit-breaker
git commit -m "fix(ecat-circuit-breaker): 半开探测名额 RAII 归还（future 被取消时不再永久锁死）"
```

### 2b：`ecat-metrics` 全进程共用的出站 collector

- [ ] **Step 12: 建 `ecat-metrics/src/outbound.rs`**

**⚠️ 先建文件、再在 `lib.rs` 加 `mod outbound;`**（假绿防线）。

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! 出站调用的三个指标（spec §4）：
//!
//! | 指标 | 类型 | 维度 |
//! |---|---|---|
//! | `ecat_outbound_timeouts_total` | counter | `backend` |
//! | `ecat_outbound_breaker_open_total` | counter | `backend` |
//! | `ecat_outbound_breaker_state` | gauge | `backend`（0=closed 1=open 2=half-open）|
//!
//! **为什么这三个指标家族在本模块，而不是像批次 4 那样放在各后端 crate 里**：
//! [`crate::registry()`] 是全进程一个 `Registry`，而它按「指标名 + 常量标签」去重
//! —— 同名 `Collector` 注册第二次直接 `AlreadyReg`，之后那个 collector 的样本
//! **一个都不输出**（`ecat-data-sqlx/src/metrics.rs:43-49` 写明了这个后果）。
//! 这三个名字是全进程共享的命名空间，而 5b 之后会有 14 个后端同时注册它们：
//! 「每 crate 一份 collector」会变成「谁先注册谁独活，其余静默消失」，
//! 而且单 crate 跑测试还看不见。所以指标家族在这里**只建一份**，各后端只挂数据源。
//!
//! 三项都**抓取时现读**：注册时快照一次没有意义，指标的价值就在随状态变。
//!
//! 数据源用 `Box<dyn Fn>` 而不直接收 `Breaker`：本 crate 因此不必依赖
//! `ecat-circuit-breaker`（拖 tower）与 `ecat-data`（拖 tokio + async-trait），
//! 保住现有「只有 prometheus + axum」的窄依赖树。

use crate::registry;
use prometheus::core::{Collector, Desc};
use prometheus::proto::{Counter, Gauge, LabelPair, Metric, MetricFamily, MetricType};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// 抓取时取一个 counter 值的闭包。
pub type OutboundCounterFn = Box<dyn Fn() -> u64 + Send + Sync>;

/// 抓取时取熔断状态的闭包：`0` = closed、`1` = open、`2` = half-open。
///
/// 编码由 `ecat_circuit_breaker::BreakerState::code()` 提供，本 crate 不知道
/// `BreakerState` 的存在。
pub type OutboundStateFn = Box<dyn Fn() -> u8 + Send + Sync>;

/// 挂一个后端的出站数据源。`backend` 是标签值（`"redis"` / `"clickhouse"` 之类）。
///
/// 幂等：collector 只注册一次；同一个 `backend` 重复注册则**覆盖**它的三个闭包
/// （避免同一标签出现两份样本 —— 那样 Prometheus 会因重复样本报错）。
///
/// 唯一会失败的情形是 registry 里已有同名指标 —— 那时三个指标都不会输出。
pub fn register_outbound_metrics(
    backend: &'static str,
    timeouts: OutboundCounterFn,
    breaker_opened: OutboundCounterFn,
    breaker_state: OutboundStateFn,
) {
    let outbound = OUTBOUND.get_or_init(|| {
        let outbound = Arc::new(Outbound::new());
        // 与 ecat-metrics 自己的注册同款：AlreadyReg 只可能是名字撞车，
        // 而这三个名字以 ecat_outbound_ 独占。
        let _ = registry().register(Box::new(Registered(Arc::clone(&outbound))));
        outbound
    });

    let entry = Entry {
        backend,
        timeouts,
        breaker_opened,
        breaker_state,
    };
    let mut entries = outbound.entries.lock().unwrap();
    match entries.iter_mut().find(|e| e.backend == backend) {
        Some(slot) => *slot = entry,
        None => entries.push(entry),
    }
}

/// 真正注册进 registry 的那层壳。
///
/// 不能直接 `impl Collector for Arc<Outbound>`：`Arc` 是外部类型，孤儿规则不允许
/// （E0117）。包一层自有类型即可，内部仍是同一个 [`Outbound`]。
struct Registered(Arc<Outbound>);

impl Collector for Registered {
    fn desc(&self) -> Vec<&Desc> {
        self.0.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        self.0.collect()
    }
}

/// 全局 collector：一个进程一个，三个指标家族都在它身上。
static OUTBOUND: OnceLock<Arc<Outbound>> = OnceLock::new();

/// 一个后端挂上来的三个数据源。
struct Entry {
    backend: &'static str,
    timeouts: OutboundCounterFn,
    breaker_opened: OutboundCounterFn,
    breaker_state: OutboundStateFn,
}

struct Outbound {
    entries: Mutex<Vec<Entry>>,
    descs: [Desc; 3],
}

impl Outbound {
    fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            descs: [
                desc(
                    "ecat_outbound_timeouts_total",
                    "Total outbound call timeouts",
                    &["backend"],
                ),
                desc(
                    "ecat_outbound_breaker_open_total",
                    "Total circuit breaker openings",
                    &["backend"],
                ),
                desc(
                    "ecat_outbound_breaker_state",
                    "Circuit breaker state (0=closed 1=open 2=half-open)",
                    &["backend"],
                ),
            ],
        }
    }
}

impl Collector for Outbound {
    fn desc(&self) -> Vec<&Desc> {
        self.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        let entries = self.entries.lock().unwrap();
        let mut timeouts = Vec::with_capacity(entries.len());
        let mut opened = Vec::with_capacity(entries.len());
        let mut states = Vec::with_capacity(entries.len());

        for e in entries.iter() {
            let one = labels(&[("backend", e.backend)]);
            timeouts.push(counter(one.clone(), count((e.timeouts)())));
            opened.push(counter(one.clone(), count((e.breaker_opened)())));
            states.push(gauge(one, f64::from((e.breaker_state)())));
        }

        vec![
            family(
                "ecat_outbound_timeouts_total",
                "Total outbound call timeouts",
                MetricType::COUNTER,
                timeouts,
            ),
            family(
                "ecat_outbound_breaker_open_total",
                "Total circuit breaker openings",
                MetricType::COUNTER,
                opened,
            ),
            family(
                "ecat_outbound_breaker_state",
                "Circuit breaker state (0=closed 1=open 2=half-open)",
                MetricType::GAUGE,
                states,
            ),
        ]
    }
}

fn count(v: u64) -> f64 {
    v as f64
}

/// 下面五个函数与 `ecat-data-sqlx/src/metrics.rs:190-237` 逐字相同。
///
/// 是**故意重复**而不是提到本 crate 里共用：那边是 feature 门控下的私有辅助函数，
/// 共用要把它们变成 `ecat-metrics` 的公开 API，而它们只是 prometheus 的结构体
/// 拼装，公开没有价值。
fn desc(name: &str, help: &str, labels: &[&str]) -> Desc {
    Desc::new(
        name.to_string(),
        help.to_string(),
        labels.iter().map(|l| (*l).to_string()).collect(),
        HashMap::new(),
    )
    .expect("指标名与标签名合法")
}

fn labels(pairs: &[(&str, &str)]) -> Vec<LabelPair> {
    pairs
        .iter()
        .map(|(name, value)| {
            let mut l = LabelPair::default();
            l.set_name((*name).to_string());
            l.set_value((*value).to_string());
            l
        })
        .collect()
}

fn gauge(labels: Vec<LabelPair>, value: f64) -> Metric {
    let mut m = Metric::default();
    m.set_label(labels.into());
    let mut g = Gauge::default();
    g.set_value(value);
    m.set_gauge(g);
    m
}

fn counter(labels: Vec<LabelPair>, value: f64) -> Metric {
    let mut m = Metric::default();
    m.set_label(labels.into());
    let mut c = Counter::default();
    c.set_value(value);
    m.set_counter(c);
    m
}

fn family(name: &str, help: &str, kind: MetricType, metrics: Vec<Metric>) -> MetricFamily {
    let mut f = MetricFamily::default();
    f.set_name(name.to_string());
    f.set_help(help.to_string());
    f.set_field_type(kind);
    f.set_metric(metrics.into());
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 抓取后按 `指标名{backend="..."}` 找样本值。找不到就是 None —— 断言
    /// 「指标压根没出现」与「值不对」是两码事，测试要能分开报。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// 三个指标都要出现，且值**直读数据源**：数据源在**注册之后**才推进，
    /// 快照型实现只能给出 0。
    #[test]
    fn metrics_are_read_live_at_scrape_time() {
        static TIMEOUTS: AtomicU64 = AtomicU64::new(0);
        let opened = Arc::new(AtomicU64::new(0));
        let state = Arc::new(AtomicU64::new(0));

        let o = Arc::clone(&opened);
        let s = Arc::clone(&state);
        register_outbound_metrics(
            "live-test",
            Box::new(|| TIMEOUTS.load(Ordering::Relaxed)),
            Box::new(move || o.load(Ordering::Relaxed)),
            Box::new(move || s.load(Ordering::Relaxed) as u8),
        );

        // 注册之后才推进。
        TIMEOUTS.store(7, Ordering::Relaxed);
        opened.store(3, Ordering::Relaxed);
        state.store(2, Ordering::Relaxed);

        let text = crate::metrics_text();
        assert_eq!(
            sample(&text, "ecat_outbound_timeouts_total{backend=\"live-test\"}"),
            Some(7.0),
            "{text}"
        );
        assert_eq!(
            sample(&text, "ecat_outbound_breaker_open_total{backend=\"live-test\"}"),
            Some(3.0),
            "{text}"
        );
        assert_eq!(
            sample(&text, "ecat_outbound_breaker_state{backend=\"live-test\"}"),
            Some(2.0),
            "{text}"
        );
    }

    /// **多个后端同时注册，样本必须都在。**
    ///
    /// 这条是「collector 必须全进程一份」的验收（出入 11）：照批次 4 的
    /// 「每 crate 一份 collector」写法，第二个注册者会拿到 `AlreadyReg`，
    /// 它的样本一个都不出现 —— 这条会红。
    #[test]
    fn multiple_backends_coexist_in_one_registry() {
        register_outbound_metrics("multi-a", Box::new(|| 1), Box::new(|| 2), Box::new(|| 0));
        register_outbound_metrics("multi-b", Box::new(|| 11), Box::new(|| 22), Box::new(|| 1));

        let text = crate::metrics_text();
        for (backend, expect) in [("multi-a", 1.0), ("multi-b", 11.0)] {
            let prefix = format!("ecat_outbound_timeouts_total{{backend=\"{backend}\"}}");
            assert_eq!(
                sample(&text, &prefix),
                Some(expect),
                "缺 {prefix}，实际输出:\n{text}"
            );
        }
    }

    /// 同一个 backend 注册两次只留一份样本（否则 Prometheus 会因重复样本报错），
    /// 且留下的是**新的闭包**。
    #[test]
    fn same_backend_registration_replaces_instead_of_duplicating() {
        register_outbound_metrics("dup-test", Box::new(|| 1), Box::new(|| 0), Box::new(|| 0));
        register_outbound_metrics("dup-test", Box::new(|| 9), Box::new(|| 0), Box::new(|| 0));

        let text = crate::metrics_text();
        let hits = text
            .lines()
            .filter(|l| l.starts_with("ecat_outbound_timeouts_total{backend=\"dup-test\"}"))
            .count();
        assert_eq!(hits, 1, "重复注册应覆盖而不是追加，实际输出:\n{text}");
        assert_eq!(
            sample(&text, "ecat_outbound_timeouts_total{backend=\"dup-test\"}"),
            Some(9.0),
            "留下的必须是新闭包"
        );
    }
}
```

- [ ] **Step 13: `ecat-metrics/src/lib.rs` 加声明与重导出**

```rust
mod outbound;
pub use outbound::{OutboundCounterFn, OutboundStateFn, register_outbound_metrics};
```

⚠️ `mod outbound;` **必须有**（假绿防线）。漏了它 → 三条测试一条都不跑、`cargo test -p ecat-metrics` 照样绿。

- [ ] **Step 14: 确认测试在跑 + 逐条比对**

```bash
cargo test -p ecat-metrics 2>&1 | grep -E '^test |^test result'
```

**测试数必须 +3**（本 crate 之前没有 `outbound` 的测试）。**没多就是 `mod outbound;` 没加。**

- [ ] **Step 15: 空验收自证（两条）**

1. 把 `collect()` 里的 `(e.timeouts)()` 改成注册时算好的常量（如 `0.0`）→ **`metrics_are_read_live_at_scrape_time` 必须 FAILED**（快照型实现）。
2. 把 `OUTBOUND.get_or_init(..)` 改成每次调用都 `registry().register(..)`（模拟「每 crate 一份」）→ **`multiple_backends_coexist_in_one_registry` 必须 FAILED**（第二个注册者拿 `AlreadyReg`）。记录输出，还原。

- [ ] **Step 16: 依赖检查 —— 本任务不该新增任何依赖**

```bash
git diff ecat-metrics/Cargo.toml
```

**期望：无输出。** 数据源是 `Box<dyn Fn>`，所以 `ecat-metrics` 不需要 `ecat-circuit-breaker` 也不需要 `ecat-data`。有 diff 说明设计跑偏了。

- [ ] **Step 17: 行数复核 + 闸门 + 提交**

```bash
find ecat-metrics/src -name '*.rs' -exec awk 'END{if(NR>500) print FILENAME": "NR}' {} \;
cargo fmt --all
cargo fmt --all -- --check; echo "fmt rc=$?"
cargo clippy -p ecat-metrics --all-targets -- -D warnings; echo "clippy rc=$?"
git add ecat-metrics
git commit -m "feat(ecat-metrics): 出站三指标的全进程唯一 collector（避免多后端注册撞名）"
```

> `ecat-metrics/src/outbound.rs` 大约 300 行（含 3 条测试），`lib.rs` 148 行 —— 都在 500 行以内。

### ⚠️ Task 2 必读

1. **`opened_total` 的两处 +1 都不能漏。** 只加 Closed 分支那一处，半开重新打开就不计数 —— 而那正是「后端恢复失败」的信号。
2. **`BreakerConfig` 的 `Default` 与 serde `default_*` 必须同源**，否则配置省略字段与代码默认值会分叉。Step 8 的测试把守这一点。
3. **三个指标家族只能有一份 collector。** 这是本任务存在的理由（出入 11）；后续任何「给某个后端单独加一个 collector」的想法都是在重建 `AlreadyReg` 那个坑。
4. **`ecat-metrics` 不新增依赖**（Step 16 会把关）。一旦有人为了方便收 `Arc<Breaker>`，就会把 tower 与 tokio 拖进这个基础 crate。
5. **半开名额的归还只能挂在 `Drop` 上**（Step 9-11）。不要试图在 `call` 的返回路径上写 `half_open_count -= 1` —— future 被 drop 时**根本没有返回路径**，那正是这个缺陷的成因。`disarm()` 必须在记录之后调用。

---

## Task 3: 修 `ecat_rdbms_*` 同名家族顶掉（**lead 追加 2**，可延后）

**Files:**
- Create: `ecat-metrics/src/rdbms.rs`（四个家族的全进程唯一 collector）
- Modify: `ecat-metrics/src/lib.rs`（`mod rdbms;` + 重导出）
- Modify: `ecat-metrics/Cargo.toml`（**只加 `[dev-dependencies]`**，不动 `[dependencies]`）
- Create: `ecat-metrics/tests/rdbms_shared_families.rs`（**跨 crate** 回归测试）
- Modify: `ecat-data-sqlx/src/metrics.rs`（不再自建 collector，改挂数据源）
- Modify: `ecat-data-mssql/src/metrics.rs`（同上）

**这是已发布代码里的真实缺陷**（与 Redis / ClickHouse 无关）：两个后端 crate 各建一份 `ecat_rdbms_*` 四个同名家族的 collector，而 `ecat-metrics/src/lib.rs:12` 的 `REGISTRY` 是**全进程一个**；`prometheus::Registry` 按「指标名 + 常量标签」去重 ⇒ 两个 `metrics` feature 同时打开时，**后注册者的 `register()` 拿到 `AlreadyReg`，被 `let _ =` 吞掉，它的四个指标一条样本都不输出**（无报错、无日志）。`ecat-data-sqlx/src/metrics.rs:47-48` 那句「这里的名字是本 crate 独占的」**是错的** —— 它是那句 `let _ =` 的唯一理由，也是这个 bug 活到今天的原因。

**修法 = 「出入 11」给 `ecat_outbound_*` 定的同一套**：collector 在 `ecat-metrics` 里只建一份，各 crate 只挂数据源（`Box<dyn Fn>`）。

**明确不动**（已发布的遥测契约）：`register_pool_metrics(backend, pool)` 的签名、四个指标名、HELP 文本、标签名与语义。两个后端 crate 的 `[dependencies]` / `[features]` 也**不动** —— 改完 `prometheus` 会变成未使用的直接依赖，但删它属于「换注册方式」之外（见文末「本批发现但不修」）。

> ### ⚠️ 停下报 lead 的条件（lead 裁决原文：「不要硬推」）
>
> 本任务**可延后**。实施中若发现要动**公开 API**、或超出「换注册方式」的范围 —— 例如：
> 1. Step 2 的 dev-dependency 环解析不了；
> 2. 需要改 `Pool` / `MssqlManager` 的可见性才能写出取数闭包；
> 3. 需要新增**非 dev** 依赖；
> 4. 需要动那四个指标的名字 / 标签语义才能修；
>
> —— **停下来报 lead**，把发现写清楚，整体推迟到 5b。**不修也比改坏已验收的 crate 强。**

- [ ] **Step 1: 基线（两个 crate 的**测试名**，不是只记总数）**

```bash
cargo test -p ecat-data-sqlx --features metrics 2>&1 | grep -E '^test |^test result'
cargo test -p ecat-data-mssql --features metrics 2>&1 | grep -E '^test |^test result'
```

- [ ] **Step 2: 回归测试的落点 —— 先确认 dev-dep 环能解析**

回归测试**必须**跑在「两个 crate 链进**同一个测试二进制**」的地方 —— 单 crate 的 `cargo test -p ecat-data-sqlx --features metrics` 永远看不到另一个 crate 的注册，**它在两边都是绿的**（这就是缺陷活到今天的原因）。

**主方案**：落在 `ecat-metrics/tests/rdbms_shared_families.rs`（不变量属于 registry，`ecat-metrics` 是 registry 的 owner）。代价是引入一个 **dev-dependency 环**（`ecat-metrics` →dev→ `ecat-data-sqlx` →dep(metrics)→ `ecat-metrics`）：

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
# 跨 crate 回归：两个后端必须在**同一个测试二进制**里才有意义
# （见 tests/rdbms_shared_families.rs 的模块文档）。dev-dependencies 不参与
# 正常构建图，所以这个环是合法的；但本仓库没有先例 —— 先验证一次。
ecat-data-sqlx = { workspace = true, features = ["metrics"] }
# ecat-data-mssql 不在 workspace.dependencies 表里，直写 path + version
# （与 ecat/Cargo.toml:22 同款）。
ecat-data-mssql = { version = "5.0.0", path = "../ecat-data-mssql", features = ["metrics"] }
```

```bash
cargo metadata --format-version=1 --offline > /dev/null; echo "rc=$?"
```

**rc=0 才继续。** 非 0（环被拒 / 解析失败）⇒ 走**回退方案**：

> **回退**：同一个测试文件原样搬到 `ecat-data-mssql/tests/rdbms_shared_families.rs`，`ecat-data-mssql/Cargo.toml` 只加一行 dev-dep（**无环**：mssql 不依赖 sqlx）：
>
> ```toml
> [dev-dependencies]
> ecat-data-sqlx = { workspace = true, features = ["metrics"] }
> ```
>
> 测试正文**一字不改**（它只用两个 crate 的公开 API）。把这次偏离记进「执行记录」。

⚠️ 那个 `version = "5.0.0"` 会被 Task 8 的 bump 一起改（`sed` 覆盖所有 `*Cargo.toml`）—— 但它是**新增的一处**，所以 Task 8 Step 2 的实测数会变成 **39 个文件 / 88 处**（本计划写作时是 38 / 87）。以 Step 2 的实测为准。

- [ ] **Step 3: 写回归测试（**先写，必须 FAILED**）**

`ecat-metrics/tests/rdbms_shared_families.rs`（回退方案就放到 mssql 的 `tests/` 下）：

```rust
//! 跨 crate 回归：**同一个进程里**两个 RDBMS 后端各自注册后，四个
//! `ecat_rdbms_*` 家族必须都在，且**两组 backend 标签的样本都能取到**。
//!
//! 为什么必须放在一个测试二进制里：这正是缺陷的触发条件。单 crate 的
//! `cargo test -p ecat-data-sqlx --features metrics` 永远看不到另一个 crate
//! 的注册，所以它在两边都是绿的 —— 只有两个都链进来才复现。
//!
//! 两个池都不需要真库：sqlite 走 `sqlite::memory:`（进程内），MSSQL 走 deadpool
//! 的惰性建连（建池不连库）。

use ecat_data_mssql::MssqlClient;
use ecat_data_sqlx::SqlxClient;

const FAMILIES: [&str; 4] = [
    "ecat_rdbms_pool_connections",
    "ecat_rdbms_pool_timeouts_total",
    "ecat_rdbms_query_timeout_total",
    "ecat_rdbms_transactions_leaked_total",
];

#[tokio::test]
async fn both_backends_keep_their_samples_in_one_process() {
    let sqlx = SqlxClient::connect("sqlite::memory:").await.unwrap();
    let mssql = MssqlClient::connect("mssql://sa:pw@127.0.0.1:1433/db")
        .await
        .unwrap();

    ecat_data_sqlx::register_pool_metrics("regression-sqlx", sqlx.pool());
    ecat_data_mssql::register_pool_metrics("regression-mssql", mssql.pool());

    let text = ecat_metrics::metrics_text();

    for family in FAMILIES {
        for backend in ["regression-sqlx", "regression-mssql"] {
            assert!(
                text.contains(&format!("{family}{{backend=\"{backend}\"")),
                "缺 {family}{{backend=\"{backend}\"}} —— 后注册者的整份 collector                  被 AlreadyReg 顶掉了。实际输出:\n{text}"
            );
        }
    }
}
```

（`ecat_rdbms_pool_connections` 的标签是 `backend` **+ `state`**，实际行是 `...{backend="x",state="idle"}` —— 前缀匹配对它同样成立。四个家族统一用「家族 + backend」前缀，是为了让断言只依赖这两层语义。）

```bash
cargo test -p ecat-metrics --test rdbms_shared_families 2>&1 | tail -40
```

**这一步必须 FAILED。** 现在两个 crate 各有一份同名 collector，后注册的那份整份拿 `AlreadyReg`，**它的两个 backend 断言必挂**。**记录输出** —— 记下**挂的是哪一组 backend 标签**（它同时告诉你谁先注册）。这是本任务存在的证据。

- [ ] **Step 4: `ecat-metrics/src/rdbms.rs`（四个家族的全进程唯一 collector）**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

//! 四个 `ecat_rdbms_*` 家族的**全进程唯一** collector（与 feature 无关）。
//!
//! 与 [`crate::outbound`] 同一个理由（见 `outbound.rs` 的模块文档与「出入 11」）：
//! `crate::registry()` 是全进程一个 `Registry`，而 `Registry` 按「指标名 + 常量
//! 标签」去重 —— **每个后端 crate 各建一份同名 collector，只会剩先注册的那份**，
//! 后注册者的四个指标一条样本都不输出（`register()` 返回 `AlreadyReg`，调用方
//! 通常 `let _ =` 掉）。批次 4 的 `ecat-data-sqlx` / `ecat-data-mssql` 正是这样
//! 互相顶掉的（Task 3 修的就是它）。
//!
//! 所以这里只建一份 collector，各后端把**取数闭包**挂进来。指标名、HELP、
//! 标签名与语义与批次 4 完全一致 —— 那是**已发布的遥测契约**。
//!
//! 为什么不直接收 `&Pool`：那会让 `ecat-metrics` 依赖 sqlx / deadpool，把两个
//! 重型驱动拖进这个基础 crate。`Box<dyn Fn>` 只要求调用方在自己的闭包里捕获池。

use prometheus::core::{Collector, Desc};
use prometheus::proto::{Counter, Gauge, LabelPair, Metric, MetricFamily, MetricType};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// 取连接池的 `(idle, active)`。
///
/// 两个数**一起**返回（而不是两个闭包）：它们是同一次池状态读取的两个投影，
/// 分开取会在并发下读到不同瞬间的池，产出 `idle + active > size` 的假样本。
pub type RdbmsConnectionsFn = Box<dyn Fn() -> (u64, u64) + Send + Sync>;

/// 读一个进程级计数器的当前值。
pub type RdbmsCounterFn = Box<dyn Fn() -> u64 + Send + Sync>;

/// 把一个后端的数据源挂到全进程唯一的 collector 上。
///
/// `backend` 是标签值（`"primary"` / `"replica-1"` 之类）。同一个 `backend` 重复
/// 注册**替换**（不叠加）：同一标签出现两份样本会被抓取端判为重复。
pub fn register_rdbms_metrics(
    backend: &'static str,
    connections: RdbmsConnectionsFn,
    pool_timeouts: RdbmsCounterFn,
    query_timeouts: RdbmsCounterFn,
    transactions_leaked: RdbmsCounterFn,
) {
    let rdbms = RDBMS.get_or_init(|| {
        let rdbms = Arc::new(Rdbms::new());
        // 与 ecat-metrics 自己的注册同款：AlreadyReg 只可能是名字撞车，而这里的
        // 名字是全仓独占的（本函数是这四个家族唯一的写入口）。
        let _ = crate::registry().register(Box::new(Registered(Arc::clone(&rdbms))));
        rdbms
    });
    rdbms.add(Entry {
        backend,
        connections,
        pool_timeouts,
        query_timeouts,
        transactions_leaked,
    });
}

/// 真正注册进 registry 的那层壳。
///
/// 不能直接 `impl Collector for Arc<Rdbms>`：`Arc` 是外部类型，孤儿规则不允许
/// （E0117）。包一层自有类型即可，内部仍是同一个 [`Rdbms`]。
struct Registered(Arc<Rdbms>);

impl Collector for Registered {
    fn desc(&self) -> Vec<&Desc> {
        self.0.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        self.0.collect()
    }
}

static RDBMS: OnceLock<Arc<Rdbms>> = OnceLock::new();

/// 一个 backend 的数据源。
struct Entry {
    backend: &'static str,
    connections: RdbmsConnectionsFn,
    pool_timeouts: RdbmsCounterFn,
    query_timeouts: RdbmsCounterFn,
    transactions_leaked: RdbmsCounterFn,
}

struct Rdbms {
    entries: Mutex<Vec<Entry>>,
    descs: [Desc; 4],
}

impl Rdbms {
    fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            descs: [
                desc(
                    "ecat_rdbms_pool_connections",
                    "Connections in the RDBMS connection pool",
                    &["backend", "state"],
                ),
                desc(
                    "ecat_rdbms_pool_timeouts_total",
                    "Total RDBMS pool acquire timeouts",
                    &["backend"],
                ),
                desc(
                    "ecat_rdbms_query_timeout_total",
                    "Total RDBMS query timeouts",
                    &["backend"],
                ),
                desc(
                    "ecat_rdbms_transactions_leaked_total",
                    "Total RDBMS transactions dropped without commit or rollback",
                    &["backend"],
                ),
            ],
        }
    }

    fn add(&self, entry: Entry) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match entries.iter_mut().find(|e| e.backend == entry.backend) {
            Some(slot) => *slot = entry,
            None => entries.push(entry),
        }
    }
}

impl Collector for Rdbms {
    fn desc(&self) -> Vec<&Desc> {
        self.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut connections = Vec::with_capacity(entries.len() * 2);
        let mut pool_timeouts = Vec::with_capacity(entries.len());
        let mut query_timeouts = Vec::with_capacity(entries.len());
        let mut leaked = Vec::with_capacity(entries.len());

        // 闭包在这里被调 —— 取数发生在**抓取时**，不是注册时。
        for e in entries.iter() {
            let backend = e.backend;
            let (idle, active) = (e.connections)();
            connections.push(gauge(
                labels(&[("backend", backend), ("state", "idle")]),
                count(idle),
            ));
            connections.push(gauge(
                labels(&[("backend", backend), ("state", "active")]),
                count(active),
            ));
            pool_timeouts.push(counter(labels(&[("backend", backend)]), count((e.pool_timeouts)())));
            query_timeouts.push(counter(
                labels(&[("backend", backend)]),
                count((e.query_timeouts)()),
            ));
            leaked.push(counter(
                labels(&[("backend", backend)]),
                count((e.transactions_leaked)()),
            ));
        }

        vec![
            family(
                "ecat_rdbms_pool_connections",
                "Connections in the RDBMS connection pool",
                MetricType::GAUGE,
                connections,
            ),
            family(
                "ecat_rdbms_pool_timeouts_total",
                "Total RDBMS pool acquire timeouts",
                MetricType::COUNTER,
                pool_timeouts,
            ),
            family(
                "ecat_rdbms_query_timeout_total",
                "Total RDBMS query timeouts",
                MetricType::COUNTER,
                query_timeouts,
            ),
            family(
                "ecat_rdbms_transactions_leaked_total",
                "Total RDBMS transactions dropped without commit or rollback",
                MetricType::COUNTER,
                leaked,
            ),
        ]
    }
}

fn count(v: u64) -> f64 {
    v as f64
}

fn desc(name: &str, help: &str, labels: &[&str]) -> Desc {
    Desc::new(
        name.to_string(),
        help.to_string(),
        labels.iter().map(|l| (*l).to_string()).collect(),
        HashMap::new(),
    )
    .expect("指标名与标签名合法")
}

fn labels(pairs: &[(&str, &str)]) -> Vec<LabelPair> {
    pairs
        .iter()
        .map(|(name, value)| {
            let mut l = LabelPair::default();
            l.set_name((*name).to_string());
            l.set_value((*value).to_string());
            l
        })
        .collect()
}

fn gauge(labels: Vec<LabelPair>, value: f64) -> Metric {
    let mut m = Metric::default();
    m.set_label(labels.into());
    let mut g = Gauge::default();
    g.set_value(value);
    m.set_gauge(g);
    m
}

fn counter(labels: Vec<LabelPair>, value: f64) -> Metric {
    let mut m = Metric::default();
    m.set_label(labels.into());
    let mut c = Counter::default();
    c.set_value(value);
    m.set_counter(c);
    m
}

fn family(name: &str, help: &str, kind: MetricType, metrics: Vec<Metric>) -> MetricFamily {
    let mut f = MetricFamily::default();
    f.set_name(name.to_string());
    f.set_help(help.to_string());
    f.set_field_type(kind);
    f.set_metric(metrics.into());
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 抓取后按 `指标名{backend="..."` 找样本值。找不到是 None —— 断言「压根没
    /// 出现」与「值不对」是两码事，测试要能分开报。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// 取数闭包是**抓取时**调用的 —— 注册时快照一次的实现会停在旧值上。
    /// 各用例用**互不相同**的 backend 标签：本模块的静态量与 registry 都是进程级的。
    #[test]
    fn metrics_are_read_live_at_scrape_time() {
        static IDLE: AtomicU64 = AtomicU64::new(7);
        register_rdbms_metrics(
            "rdbms-live-test",
            Box::new(move || (IDLE.load(Ordering::Relaxed), 0u64)),
            Box::new(|| 0u64),
            Box::new(|| 0u64),
            Box::new(|| 0u64),
        );

        let read = || {
            sample(
                &crate::metrics_text(),
                "ecat_rdbms_pool_connections{backend=\"rdbms-live-test\",state=\"idle\"}",
            )
        };
        assert_eq!(read(), Some(7.0));
        IDLE.store(9, Ordering::Relaxed);
        assert_eq!(read(), Some(9.0), "注册时快照的实现会停在上一次的值");
    }

    /// 多个 backend 共存时**谁都不丢** —— 这是本模块存在的唯一理由
    /// （「每 crate 一份 collector」的老做法在这里必红）。
    #[test]
    fn multiple_backends_coexist_in_one_registry() {
        for (backend, base) in [("rdbms-multi-a", 1u64), ("rdbms-multi-b", 4u64)] {
            register_rdbms_metrics(
                backend,
                Box::new(move || (base, 0u64)),
                Box::new(move || base),
                Box::new(move || base),
                Box::new(move || base),
            );
        }

        let text = crate::metrics_text();
        for backend in ["rdbms-multi-a", "rdbms-multi-b"] {
            for family in [
                "ecat_rdbms_pool_connections",
                "ecat_rdbms_pool_timeouts_total",
                "ecat_rdbms_query_timeout_total",
                "ecat_rdbms_transactions_leaked_total",
            ] {
                assert!(
                    text.contains(&format!("{family}{{backend=\"{backend}\"")),
                    "缺 {family}{{backend=\"{backend}\"}}，实际输出:\n{text}"
                );
            }
        }
    }

    /// 同一个 backend 重复注册**替换**而不是叠加：同一标签出现两份样本会被抓取端判为重复。
    #[test]
    fn same_backend_registration_replaces_instead_of_duplicating() {
        for value in [1u64, 2u64] {
            register_rdbms_metrics(
                "rdbms-dup-test",
                Box::new(move || (value, 0u64)),
                Box::new(move || value),
                Box::new(move || value),
                Box::new(move || value),
            );
        }

        let text = crate::metrics_text();
        let hits = text
            .lines()
            .filter(|l| l.contains("backend=\"rdbms-dup-test\""))
            .count();
        assert_eq!(hits, 5, "2 条 gauge（idle/active）+ 3 条 counter，一份不少一份不多:\n{text}");
    }
}
```

- [ ] **Step 5: `ecat-metrics/src/lib.rs` 挂 `mod` + 重导出 + **假绿防线**

```rust
mod rdbms;
pub use rdbms::{RdbmsConnectionsFn, RdbmsCounterFn, register_rdbms_metrics};
```

⚠️ `mod rdbms;` **必须有** —— 漏了它 → 三条测试一条都不跑、`cargo test -p ecat-metrics` 照样绿。

```bash
cargo test -p ecat-metrics 2>&1 | grep -E '^test |^test result'
```

**单元测试数必须 +3。**

- [ ] **Step 6: `ecat-data-sqlx/src/metrics.rs` 改挂数据源**

把 `:19-237`（`use crate::pool::Pool;` 到 `fn family` 结束）整体替换为下面这段 —— **`POOL_TIMEOUTS` 与 `count_pool_timeout` 原样保留**（`db_err` 还在调 `count_pool_timeout`，测试也在读 `POOL_TIMEOUTS`），`Registered` / `Pools` / 六个私有 helper 全删：

```rust
use crate::pool::Pool;
use ecat_data::{BackendKind, TRANSACTIONS_LEAKED, timeout_counter};
use std::sync::atomic::{AtomicU64, Ordering};

/// 取连接的累计超时次数（[`count_pool_timeout`] 递增）。
///
/// 不复用 [`timeout_counter`]`(BackendKind::Rdbms)` 那个槽：那个数的是**查询**超时（连接已经拿到手），这里数的是
/// **等连接**超时 —— 前者查慢查询、后者查池容量，混成一个指标会同时丢掉两条线索。
static POOL_TIMEOUTS: AtomicU64 = AtomicU64::new(0);

/// 记一次「等连接超时」。由 `db_err` 在见到 `sqlx::Error::PoolTimedOut` 时调用。
pub(crate) fn count_pool_timeout() {
    POOL_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
}

/// 注册池指标。`backend` 是标签值（`"primary"` / `"replica-1"` 之类）。
///
/// 幂等：重复调用只是多挂一个被观测的池；同一个 `backend` 重复注册则覆盖
/// （避免同一标签出现两份样本）。
///
/// **四个家族的全进程唯一 collector 在 `ecat_metrics`**（`ecat-metrics/src/rdbms.rs`）：
/// 本函数只把本 crate 的取数闭包挂上去。早期版本在这里自建 collector —— 那会与
/// `ecat-data-mssql` 的同名家族互相顶掉（`Registry` 只留先注册的那一份，
/// 后者的四个指标静默消失）。
pub fn register_pool_metrics(backend: &'static str, pool: &Pool) {
    // `Pool` 内部是 `Arc`，克隆只是多一个句柄；闭包要 `'static`，所以按值捕获。
    let pool = pool.clone();
    ecat_metrics::register_rdbms_metrics(
        backend,
        // 一次读取同时给出两个投影：分两次读会在并发下出现 idle + active > size。
        Box::new(move || {
            let idle = u64::from(pool.idle());
            let size = u64::from(pool.size());
            (idle, size.saturating_sub(idle))
        }),
        Box::new(|| POOL_TIMEOUTS.load(Ordering::Relaxed)),
        // 三个 counter 都是**进程级**的，与具体池无关 —— 每个 backend 各出一份
        // 同样的值（spec:726 的既定接法，不是缺陷）。
        Box::new(|| timeout_counter(BackendKind::Rdbms).load(Ordering::Relaxed)),
        Box::new(|| TRANSACTIONS_LEAKED.load(Ordering::Relaxed)),
    );
}
```

模块文档（`:3-17`）末段改一句：把「手写 [`Collector`]」换成「取数闭包挂在 `ecat_metrics` 的唯一 collector 上」。

```bash
cargo test -p ecat-data-sqlx --features metrics 2>&1 | grep -E '^test |^test result'
```

**与 Step 1 基线逐条比对**：测试名一个不少、全绿。⚠️ **下面那个 `#[cfg(test)] mod tests` 一个字都不用改**（它只用 `register_pool_metrics` / `POOL_TIMEOUTS` / `sample` / `assert_bracketed`）—— 需要改它才通过的话，说明公开行为变了，**停下报 lead**。

- [ ] **Step 7: `ecat-data-mssql/src/metrics.rs` 改挂数据源**

同样的删法：把 `:3-20`（模块文档）、`:22-29`（`use`）、`:43-58`（`register_pool_metrics`）、`:60-74`（`Registered`）、`:86-254`（`POOLS` / `Pools` / `impl Pools` / `Collector for Pools` / 六个私有 helper，到 `fn family` 结束 —— 后面 `:255` 就是 `#[cfg(test)]`）整体替换为下面这段，**保留** `POOL_TIMEOUTS` / `count_pool_timeout`（`:31-41`）与 `idle_active`（`:76-84`，含文档注释 —— 它的单元测试还要用）：

```rust
use crate::pool::MssqlManager;
use deadpool::managed::Pool;
use ecat_data::{BackendKind, TRANSACTIONS_LEAKED, timeout_counter};
use std::sync::atomic::{AtomicU64, Ordering};

// ...（这里放回原有的 POOL_TIMEOUTS 静态量、count_pool_timeout、idle_active，一字不改）

/// 注册池指标。`backend` 是标签值（`"primary"` / `"replica-1"` 之类）。
///
/// 幂等：重复调用只是多挂一个被观测的池；同一个 `backend` 重复注册则覆盖
/// （避免同一标签出现两份样本）。
///
/// **四个家族的全进程唯一 collector 在 `ecat_metrics`**（`ecat-metrics/src/rdbms.rs`）：
/// 本函数只把本 crate 的取数闭包挂上去（同 `ecat-data-sqlx`）。早期版本在这里自建
/// collector —— 与 sqlx 的同名家族互相顶掉，是 5.0.0 的已发布缺陷。
pub fn register_pool_metrics(backend: &'static str, pool: &Pool<MssqlManager>) {
    // deadpool 的 `Pool` 内部是 `Arc`，克隆很轻；闭包要 `'static`，按值捕获。
    let pool = pool.clone();
    ecat_metrics::register_rdbms_metrics(
        backend,
        // 一次 `status()` 同时给出两个投影：分两次读会在并发下出现
        // idle + active > size 的假样本。
        Box::new(move || idle_active(&pool.status())),
        Box::new(|| POOL_TIMEOUTS.load(Ordering::Relaxed)),
        Box::new(|| timeout_counter(BackendKind::Rdbms).load(Ordering::Relaxed)),
        Box::new(|| TRANSACTIONS_LEAKED.load(Ordering::Relaxed)),
    );
}
```

```bash
cargo test -p ecat-data-mssql --features metrics 2>&1 | grep -E '^test |^test result'
```

**与 Step 1 基线逐条比对**：测试名一个不少、全绿（`idle_active_maps_deadpool_status` 也在里面）。

- [ ] **Step 8: 跨 crate 回归测试转绿 + 空验收自证**

```bash
cargo test -p ecat-metrics --test rdbms_shared_families 2>&1 | grep -E '^test |^test result'
```

**必须 PASSED**（Step 3 是红的，这里是绿的 —— 这条红→绿就是本任务的验收）。

空验收自证（**两条**）：

1. 把 `register_rdbms_metrics` 里的 `RDBMS.get_or_init(..)` 改回「每次调用都 `crate::registry().register(..)`」（即每 crate 一份的老做法）→ `rdbms_shared_families` 必须重新 **FAILED**，且 `multiple_backends_coexist_in_one_registry` 也 FAILED。
2. 把 `Rdbms::add` 的「找到同 backend 就替换」改成无条件 `push` → `same_backend_registration_replaces_instead_of_duplicating` 必须 FAILED。

记录输出，还原。

- [ ] **Step 9: 行数复核 + 闸门 + 提交**

```bash
find ecat-metrics/src ecat-metrics/tests ecat-data-sqlx/src ecat-data-mssql/src -name '*.rs' -exec awk 'END{if(NR>500) print FILENAME": "NR}' {} \;
cargo fmt --all
cargo fmt --all -- --check; echo "fmt rc=$?"
cargo clippy -p ecat-metrics -p ecat-data-sqlx -p ecat-data-mssql --all-targets --all-features -- -D warnings; echo "clippy rc=$?"
git add ecat-metrics ecat-data-sqlx/src/metrics.rs ecat-data-mssql/src/metrics.rs
git commit -m "fix(ecat-metrics): ecat_rdbms_* 四指标全进程唯一 collector（修 sqlx/mssql 同名家族互相顶掉）"
```

> 交付物规模：`ecat-metrics/src/rdbms.rs` 约 330 行（含 3 条测试）、`tests/rdbms_shared_families.rs` 约 45 行；两个后端 crate 的 `metrics.rs` **各减约 120 行**（collector 搬走了，测试原样留着）。

---

## Task 4: `ecat-data-redis` —— 超时 + 熔断 + 指标

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

改后的 `[dependencies]`（既有 9 行**一行不动**，只在 `ecat-tls` 后加 2 行）：

```toml
[dependencies]
ecat-data = { version = "5.0.0", path = "../ecat-data" }
ecat-errors.workspace = true
ecat-lock = { version = "5.0.0", path = "../ecat-lock" }
async-trait.workspace = true
serde.workspace = true
serde_json.workspace = true
redis = { version = "0.27", features = ["tokio-comp", "aio"] }
uuid = { version = "1", features = ["v4"] }
ecat-tls = { version = "5.0.0", path = "../ecat-tls" }
# 出站熔断：批次 4 已抽取的 Breaker，逐 client 一个。
ecat-circuit-breaker.workspace = true
# 指标依赖 optional：默认不把 axum（ecat-metrics 的依赖）拖进核心依赖树。
ecat-metrics = { workspace = true, optional = true }
```

`[dev-dependencies]` 与新增的 `[features]`：

```toml
[dev-dependencies]
# net 给假 RESP 服务端的 TcpListener，time 给超时测试的 sleep。
tokio = { workspace = true, features = ["macros", "rt", "net", "time"] }

[features]
# 只有 ecat-metrics：三个指标家族的唯一 collector 在那边（「出入 11」），
# 本 crate 不需要直接依赖 prometheus。
metrics = ["dep:ecat-metrics"]
```

（`ecat-circuit-breaker` 与 `ecat-metrics` **都在 workspace 依赖表里**（`Cargo.toml:120`、`:123`），所以写 `.workspace = true`；`serde_json` 已在 `[dependencies]`，测试直接用。）

- [ ] **Step 3: 建 `src/tests.rs`，把 `mod tests` 整块搬过去**

在 `lib.rs` 里把 `#[cfg(test)] mod tests { ... }`（`:244-395`）整块剪出，粘到新文件，去掉外面那层 `mod tests { }` 与它的缩进，文件头如下：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 测试独立成文件：lib.rs 有 500 行硬上限（项目约定，见批次 4 的同类拆分）。
use super::*;
use ecat_circuit_breaker::BreakerState;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
```

（`use super::*` 能拿到 `lib.rs` 里 `use` 进来的名字 —— 私有 `use` 对子模块可见，这是本仓测试文件的既有写法。`Duration` / `ErrorCode` / `TIMEOUTS` / `BackendKind` / `Arc` / `RedisConfig` / `query_timeout` 都从那里来；只有 `BreakerState` 是新增的，因为 `lib.rs` 自己不直接用它（用了会触发 unused import 警告 → clippy 报错），所以写在测试文件里。）

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

⚠️ `BreakerConfig` 的 `Deserialize` **已在 Task 2 Step 7 加好**，本步直接用即可。若 Task 2 尚未落地，这里会编译失败（`BreakerConfig: Deserialize` 是这两个字段的前提）。

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

- [ ] **Step 12: `src/metrics.rs`（feature 门控，薄）**

三个指标家族的唯一 collector 在 `ecat-metrics`（Task 2b /「出入 11」），本 crate 只负责**把 Redis 的数据源挂上去**，所以这里只有约 30 行。

**调用方式与批次 4 的 `register_pool_metrics` 相同**：函数是公开 API，由**应用**在启动时显式调一次 `register_outbound_metrics(cache.breaker())`（`ecat-metrics` 的 registry 是全局的，注册一次即可）。所以「无调用点」在这里不是缺陷 —— 它是 opt-in 指标的正常形状；本 crate 的职责到「函数存在且被测试覆盖」为止。

`lib.rs` 加（放在其他 `mod` 声明附近）：

```rust
#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;
```

`src/metrics.rs`（全文件）：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "metrics"：把 Redis 的出站数据源挂进 `ecat-metrics` 的共用 collector。
//!
//! 三个指标家族（`ecat_outbound_timeouts_total` / `ecat_outbound_breaker_open_total`
//! / `ecat_outbound_breaker_state`）的 collector **不在本 crate** —— 指标名是全进程
//! 共享的命名空间，每个后端各建一份会在 `Registry` 里撞名（`AlreadyReg`），让后
//! 注册者的样本静默消失。理由见批次 5a 的「出入 11」与 `ecat-metrics/src/outbound.rs`。

use ecat_circuit_breaker::Breaker;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// 挂上 Redis 的出站数据源。标签值固定为 `"redis"`。
///
/// 收的是**熔断器**而不是 `RedisCache`：三项数据源都与具体实例无关 ——
/// 超时数读的是按**后端类别**的进程级静态量（`TIMEOUTS` 的 `Cache` 槽），
/// 熔断两项读的是这个熔断器本身。所以不必持有整个 client，测试也就能
/// 不依赖真实连接地验证注册。
pub fn register_outbound_metrics(breaker: Arc<Breaker>) {
    let opened = Arc::clone(&breaker);
    ecat_metrics::register_outbound_metrics(
        "redis",
        Box::new(|| timeout_counter(BackendKind::Cache).load(Ordering::Relaxed)),
        Box::new(move || opened.opened_total()),
        Box::new(move || breaker.state().code()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecat_circuit_breaker::{Breaker, BreakerConfig, BreakerState};

    /// 抓取后按 `指标名{backend="redis"}` 找样本值。找不到就是 None ——
    /// 「指标压根没出现」与「值不对」必须能分开报（`contains("} 0")` 会把
    /// `} 0.5` 也算进去，所以不能只做字符串包含断言）。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// 三个指标都要出现，且**值是抓取时现读的**：熔断器在**注册之后**才被推到
    /// `Open`，注册时快照的实现只会给出 0。
    #[tokio::test]
    async fn outbound_metrics_appear_with_live_values() {
        let breaker = Arc::new(Breaker::new(BreakerConfig::default()));
        register_outbound_metrics(Arc::clone(&breaker));

        let text = ecat_metrics::metrics_text();
        assert_eq!(
            sample(&text, "ecat_outbound_breaker_state{backend=\"redis\"}"),
            Some(0.0),
            "未打开时状态应为 0，实际输出:\n{text}"
        );

        // 打满窗口的样本下限（5 条失败）→ 打开。
        let fail = || async { Err::<(), &str>("backend down") };
        for _ in 0..5 {
            let _ = breaker.call(fail).await;
        }
        assert_eq!(breaker.state(), BreakerState::Open);

        let text = ecat_metrics::metrics_text();
        assert_eq!(
            sample(&text, "ecat_outbound_breaker_state{backend=\"redis\"}"),
            Some(1.0),
            "打开后状态应为 1，实际输出:\n{text}"
        );
        assert_eq!(
            sample(&text, "ecat_outbound_breaker_open_total{backend=\"redis\"}"),
            Some(1.0),
            "打开次数应为 1，实际输出:\n{text}"
        );
        // 超时数是进程级静态量，值会被别的用例推进 —— 只断言样本存在。
        assert!(
            sample(&text, "ecat_outbound_timeouts_total{backend=\"redis\"}").is_some(),
            "缺超时指标样本，实际输出:\n{text}"
        );
    }
}
```

- [ ] **Step 13: 带 feature 跑 + 确认默认构建不含它**

```bash
cargo test -p ecat-data-redis 2>&1 | grep -c 'outbound_metrics'   # 期望 0（默认构建里没有）
cargo test -p ecat-data-redis --features metrics 2>&1 | grep -E '^test |^test result'
```

**第一条必须是 0** —— 否则新测试写在 feature 门控之外（空验收的变体）。第二条应看到 **17 → 18**（17 是 Step 10 之后的数）。

- [ ] **Step 14: 行数复核 + 闸门 + 提交**

```bash
find ecat-data-redis/src -name '*.rs' -exec awk 'END{if(NR>500) print FILENAME": "NR}' {} \;
cargo fmt --all
cargo fmt --all -- --check; echo "fmt rc=$?"
cargo clippy -p ecat-data-redis --all-targets --features metrics -- -D warnings; echo "clippy rc=$?"
cargo test -p ecat-circuit-breaker 2>&1 | grep -E '^test result'   # Task 2 改过它，必须仍绿
git add ecat-data-redis
git commit -m "feat(ecat-data-redis): 出站超时 + 熔断 + metrics feature，补多路复用能力边界文档"
```

### ⚠️ Task 4 必读

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

## Task 5: `ecat-data-clickhouse` —— 最难 case

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

改后的 `[dependencies]`（既有 8 行**一行不动**，加 3 行）：

```toml
[dependencies]
ecat-data = { version = "5.0.0", path = "../ecat-data" }
ecat-errors.workspace = true
async-trait.workspace = true
serde.workspace = true
serde_json.workspace = true
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json"] }
ecat-tls = { version = "5.0.0", path = "../ecat-tls" }
# 出站熔断：批次 4 已抽取的 Breaker，逐 client 一个。
ecat-circuit-breaker.workspace = true
# 并发上限用 Semaphore。tokio 现在只在 dev-dependencies 里，必须提到这里。
tokio = { workspace = true, features = ["sync"] }
# 指标依赖 optional：默认不把 axum 拖进核心依赖树。
ecat-metrics = { workspace = true, optional = true }
```

`[dev-dependencies]` 与新增的 `[features]`：

```toml
[dev-dependencies]
axum.workspace = true
# 既有是 ["macros", "rt", "net"]：再加 time（超时/熔断测试要 sleep）。
tokio = { workspace = true, features = ["macros", "rt", "net", "time"] }

[features]
# 只有 ecat-metrics：三个指标家族的唯一 collector 在那边（「出入 11」），
# 本 crate 不需要直接依赖 prometheus。
metrics = ["dep:ecat-metrics"]
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
    // 证人槽必须是**本测试二进制内没有任何其它用例会写**的槽 —— `Rdbms` 不行
    // （另两条用例在写它，libtest 并行时它们的 +1 会插进本用例的观测窗口，实测 3/3 红）。
    // 加用例前先 `grep -rn 'BackendKind::Storage'`；要写它就先换一个自由槽。
    let witness_before = TIMEOUTS[BackendKind::Storage as usize].load(Ordering::SeqCst);
    let tsdb_before = TIMEOUTS[BackendKind::Tsdb as usize].load(Ordering::SeqCst);
    let err = ecat_data::TsdbClient::query(&c, "SELECT 1")
        .await
        .expect_err("必须超时");
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "got: {err}");
    assert!(TIMEOUTS[BackendKind::Tsdb as usize].load(Ordering::SeqCst) > tsdb_before);
    assert_eq!(
        TIMEOUTS[BackendKind::Storage as usize].load(Ordering::SeqCst),
        witness_before,
        "Tsdb 的超时不得落到别的槽（证人槽约束见上）"
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

- [ ] **Step 14: `src/metrics.rs`（feature 门控，薄）**

与 Task 4 Step 12 **同构**：collector 在 `ecat-metrics`，本 crate 只挂数据源。**唯一差别**是本 crate 有**两条路径、两个超时维度**，所以超时指标出两份样本（一条 I/O 路径的失败不该被另一条的计数稀释）。

`lib.rs` 加：

```rust
#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;
```

`src/metrics.rs`（全文件）：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "metrics"：把 ClickHouse 的出站数据源挂进 `ecat-metrics` 的共用 collector。
//!
//! 三个指标家族的 collector **不在本 crate**（理由见「出入 11」与
//! `ecat-metrics/src/outbound.rs`）：本 crate 有**两条 I/O 路径**
//! （`SqlExecutor` → `Rdbms` 槽、`TsdbClient` → `Tsdb` 槽），超时计数因此出
//! **两份样本**：`backend="clickhouse"` 与 `backend="clickhouse-tsdb"`。
//! 熔断两项是同一个 `Breaker`（两条路径共用，见 `ClickhouseClient` 的字段说明），
//! 所以两份样本的值相同 —— 这是有意的，方便按 `backend` 分组时两条都看得到。

use ecat_circuit_breaker::Breaker;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// 挂上 ClickHouse 的出站数据源。同 Task 4 Step 12，收熔断器而非整个 client。
pub fn register_outbound_metrics(breaker: Arc<Breaker>) {
    register_one("clickhouse", BackendKind::Rdbms, Arc::clone(&breaker));
    register_one("clickhouse-tsdb", BackendKind::Tsdb, breaker);
}

fn register_one(backend: &'static str, kind: BackendKind, breaker: Arc<Breaker>) {
    let opened = Arc::clone(&breaker);
    ecat_metrics::register_outbound_metrics(
        backend,
        Box::new(move || timeout_counter(kind).load(Ordering::Relaxed)),
        Box::new(move || opened.opened_total()),
        Box::new(move || breaker.state().code()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecat_circuit_breaker::{Breaker, BreakerConfig, BreakerState};

    /// 抓取后按 `指标名{backend="..."}` 找样本值（找不到 = None）。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// **两个超时维度各出一份样本** —— 只注册 `"clickhouse"` 一个标签的实现
    /// 在这里红。两条路径的超时不该合成一个数：合成后就分不清是 SQL 慢还是
    /// 时序写入慢。
    #[tokio::test]
    async fn both_paths_publish_their_own_timeout_sample() {
        let breaker = Arc::new(Breaker::new(BreakerConfig::default()));
        register_outbound_metrics(Arc::clone(&breaker));

        // 两条路径各推进一格（通过 ecat-data 的静态量，绕开真实网络）。
        TIMEOUTS[BackendKind::Rdbms as usize].fetch_add(1, Ordering::Relaxed);
        TIMEOUTS[BackendKind::Tsdb as usize].fetch_add(1, Ordering::Relaxed);

        let text = ecat_metrics::metrics_text();
        for backend in ["clickhouse", "clickhouse-tsdb"] {
            let times = sample(
                &text,
                &format!("ecat_outbound_timeouts_total{{backend=\"{backend}\"}}"),
            );
            assert!(times.is_some_and(|v| v >= 1.0), "缺 {backend} 的超时样本，实际输出:\n{text}");
            assert_eq!(
                sample(
                    &text,
                    &format!("ecat_outbound_breaker_state{{backend=\"{backend}\"}}")
                ),
                Some(0.0),
                "缺 {backend} 的状态样本"
            );
        }

        // 推到 Open：**两份**样本都要变成 1（同一个熔断器）。
        let fail = || async { Err::<(), &str>("backend down") };
        for _ in 0..5 {
            let _ = breaker.call(fail).await;
        }
        assert_eq!(breaker.state(), BreakerState::Open);
        let text = ecat_metrics::metrics_text();
        for backend in ["clickhouse", "clickhouse-tsdb"] {
            assert_eq!(
                sample(
                    &text,
                    &format!("ecat_outbound_breaker_state{{backend=\"{backend}\"}}")
                ),
                Some(1.0),
                "{backend} 的状态应随熔断器实时变化，实际输出:\n{text}"
            );
        }
    }
}
```

（`ecat_metrics::register_outbound_metrics` 的签名是 `(backend: &'static str, timeouts: OutboundCounterFn, breaker_opened: OutboundCounterFn, breaker_state: OutboundStateFn)` —— ⚠️ **前两个 counter 是 `Fn() -> u64`，最后一个是 `Fn() -> u8`**（`breaker.state().code()` 返回 u8，别放进 u64 的位置；这条括注原先只说了第三个参数，2026-10-08 由 Task 6 的 checklist 作者发现并补全）。`move` 走的是 `Arc<Breaker>` 的所有权，所以 `register_one` 里两次 `Arc::clone` 是必要的。）

- [ ] **Step 15: 带 feature 跑**

```bash
cargo test -p ecat-data-clickhouse 2>&1 | grep -c 'outbound_metrics'   # 期望 0
cargo test -p ecat-data-clickhouse --features metrics 2>&1 | grep -E '^test |^test result'
```

第二条应看到 **30 → 31**（30 是 Step 12 之后的数）。

- [ ] **Step 16: 行数复核 + 闸门 + 提交**

```bash
find ecat-data-clickhouse/src -name '*.rs' -exec awk 'END{if(NR>500) print FILENAME": "NR}' {} \;
cargo fmt --all
cargo fmt --all -- --check; echo "fmt rc=$?"
cargo clippy -p ecat-data-clickhouse --all-targets --features metrics -- -D warnings; echo "clippy rc=$?"
git add ecat-data-clickhouse
git commit -m "feat(ecat-data-clickhouse): 出站超时 + 熔断 + 并发上限 + metrics feature"
```

### ⚠️ Task 5 必读

1. **两个 `query` 同名**：`SqlExecutor::query` 与 `TsdbClient::query` 都在 `ClickhouseClient` 上。测试里调用必须写全路径（`ecat_data::SqlExecutor::query(&c, ..)`），否则编译器选哪个不明确。
2. **`create_table` 不再单独包**（它在 `write` 的包装之内）。
3. **`_with` / `transaction()` 一个字都不写** —— 这是设计，不是漏做（出入 6/7）。
4. **并发上限不是 reqwest 的旋钮**，是我们自己的 `Semaphore`。要写进配置字段的 rustdoc（spec §8 判据 6 明确要求）。

---

## Task 6: 接入 checklist（**5b 的输入**）

**Files:**
- Create: `docs/superpowers/checklists/backend-resilience-onboarding.md`

**这是 spec §7.5 点名的 5a 交付物**。5b 要把同一套动作重复 11 次，checklist 就是防「10 次即兴发挥」的那一份。

- [ ] **Step 1: 建目录与文件，按下面六节写**

```bash
mkdir -p docs/superpowers/checklists
```

**必须包含以下六节**（标题照抄，节内内容用**本批 Redis / ClickHouse 的真实验证结果**填，不要写成泛泛的注意事项）：

1. `## 1. 加配置字段` —— `query_timeout_secs` / `breaker` / `max_concurrency` 三个字段的**逐字**声明 + `0 = 禁用` 转换函数 + 各自的行内 rustdoc。注明：`breaker` 字段依赖 `BreakerConfig: Deserialize`（Task 4 Step 5 加的）。
2. `## 2. 包 run_with_timeout` —— **熔断在外、超时在内**的顺序，附一段说明（为什么不能反过来：卡死后端会打不开熔断器 + 半开名额泄漏）。给出 `guarded` 外壳的两个变体（`RdbmsError` / `ecat_errors::Error`）。
3. `## 3. 加 Breaker 字段` —— 逐实例一个 `Arc<Breaker>`、`state()` 给路由用、`opened_total()` 给指标用、`guarded` 里闭包按需构造 future。
4. `## 4. 注册指标` —— 三个指标名 + 维度 + 数据源表；**`collector` 在 `ecat-metrics`，本 crate 只写 ~15 行注册**（`[features] metrics = ["dep:ecat-metrics"]`，**不要**再各建 collector —— 会撞 `AlreadyReg`，见「出入 11」）；`backend` 标签值取**产品级名**（`"redis"` / `"clickhouse"`；一个后端有两条 I/O 路径时按路径各出一份，如 `"clickhouse-tsdb"`）。⚠️ **与 spec 的差异要写明**：spec §4 写的是类别名（`rdbms`/`cache`/…），**实际约定是产品名** —— 告警规则**必须按产品名写**（`backend="redis"`），照 spec 写永远匹配不到。另：指标标签是**产品名**、错误里的 `reason` 是**类别** slug（`"cache"`），两者粒度不同是有意的（见 `BackendKind::slug()`）。附一句「为什么不能照抄批次 4 的每 crate 一份」。
5. `## 5. 加一条超时测试` —— 判据三选一（按后端的可测性）：①有 HTTP 接口 → axum mock + 延迟（ClickHouse 模式）；②有原生连接 → 假 `TcpListener` 装死（mssql 模式）；③内层可替身 → 假 impl + `future::pending()`。**必须是端到端**（打真实方法），不能只测 `run_with_timeout` 本身。⚠️ **若该测试要断言「计数落到了正确的槽」，证人槽必须是本测试二进制内没有任何其它用例会写的槽** —— 否则 libtest 并行调度时别的用例的 `+1` 会插进你的观测窗口，**必现红**（本批在 Redis 与 ClickHouse 各踩一次：ClickHouse 实测 3/3 红、`--test-threads=1` 恒绿）。加用例前先 `grep -rn 'BackendKind::<槽>'` 确认；**不要**用加锁串行化来绕（那只护住当前这几条用例，将来加一条写同一槽的用例 flake 会原样回来，且会被 5b 抄 12 遍）。
6. `## 6. 加一条熔断测试` —— 连续失败后断言**两件事**：`state() == Open` **且** 下一次调用**不等满超时**就返回（时间断言）。只断言 `is_err()` 是空验收。**判据要选准**（Task 4 复核用 A/B 探针实测）：**严格判据是 `err.code == <熔断拒绝码>`**（如 `Unavailable`）—— 它只能由熔断器在**调内层之前**拒绝产生，crate 内无其它生产者，**严格强于墙钟**；而「不等满超时」的**墙钟断言是冗余佐证**（把它删掉测试仍绿），且是全套里唯一对调度抖动敏感的断言。语义判据必写；墙钟断言可留作直观信号，但别当唯一依据。

**外加两节**：

7. `## 7. 已知陷阱` —— 至少收录：`0` 是禁用不是零超时；默认实现的方法不要包（会把「用法错」记成后端故障）；常量错误返回的方法（如 ClickHouse 的 `transaction()`）不要包；`from_config` 已有的 reqwest 内层超时要写进 rustdoc；`--features metrics` 才能编译到指标代码；新建 `.rs` 必须在父模块加 `mod`（**且别把「加了 mod 未建文件」的中间态留到提交那一刻** —— 它会让别人的 `fmt --all` 报 `failed to resolve mod`）；**假服务端/假连接的解析器要写清能力边界**（本批 Redis 的 `drain_commands` 靠扫 buffer 首个 `*` 切命令，**遇到含 `*` 的二进制 bulk value 会错位** —— 当前输入固定故无影响，5b 抄它时须跳过 bulk 内容或在注释里写明限制，别让一个只在特定 payload 下失效的解析器看起来是通用的）。
8. `## 8. 验收命令` —— 每个 crate 的 `cargo test -p <crate>`、`--features metrics`、`cargo fmt -p <crate> -- --check`（⚠️ **任务级闸门一律用 `-p <crate>`，不要用 `--all`** —— 多 agent 并行时 `--all` 会排版别人正在飞的半成品；只有 Task 8 的**最终**闸门才用 `--all`，且那一刻应确认没有别人在同一工作区）、`cargo clippy -p <crate> --all-targets --features metrics -- -D warnings`、行数复核 `awk` 一行命令。

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

## Task 7: 文档（配置教程 + Redis 能力边界）

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
| `breaker` | `Option<BreakerConfig>` | 保守默认 | 熔断阈值与窗口，可省字段（`breaker: {}` 即全默认）；省略整个字段 = 用默认熔断配置，熔断**默认开启** |

⚠️ **不要写 `{"enabled": false}`** —— `BreakerConfig` **没有** `enabled` 字段（`ecat-circuit-breaker/src/breaker.rs:18-23` 只有 `failure_ratio` / `window` / `half_open_probes` / `open_duration`）。spec 也没要求一个总开关。文档里写一个不存在的字段比不写更糟：用户照抄会得到一个反序列化错误，或者更坏 —— 被 `#[serde(default)]` 静默吞掉后以为关掉了。要说明「怎么关」，就如实写**当前没有总开关**。

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

## Task 8: CHANGELOG + 版本 6.0.0

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `Cargo.toml`（`[workspace.package]` 的版本行）+ 内部 path 依赖引用（**实测 38 个文件 / 86 处**；Task 3 会给 `ecat-metrics/Cargo.toml` 再加 2 处 ⇒ 39 / 88）
- Modify: `Cargo.lock`（`cargo metadata` 重生成）
- Modify: **14 份 README**（根 `README.md`、`README.en.md` + `docs/i18n/{ar,bn,de,en,es,fr,hi,id,ja,ko,pt,ru}/README.md`），**各 2 处 = 28 处**

- [ ] **Step 1: CHANGELOG 加 6.0.0 段**

**破坏性变更**（这是进位主版本号的唯一理由，要点名）：
- `ecat_data::run_with_timeout` 签名变更：`(Option<Duration>, F)` → `(BackendKind, Option<Duration>, F)`
- **删除** `ecat_data::QUERY_TIMEOUTS`（由 `ecat_data::TIMEOUTS` + `BackendKind` 取代）

新增：`BackendKind` / `TimeoutError` / `TIMEOUTS`、`map_breaker_error` 与 `breaker_error_to_backend_error` 公开、`Breaker::opened_total()` 与 `BreakerState::code()`、`BreakerConfig: Deserialize`、`ecat_metrics::register_outbound_metrics`（全进程唯一的出站 collector）、`RedisConfig` / `ClickhouseConfig` 三个新字段、`ecat-data-redis` 与 `ecat-data-clickhouse` 的 `metrics` feature 与 `ecat_outbound_*` 三个指标。

- [ ] **Step 2: bump 前先核（**别信本计划的数，重新实测**）**

```bash
echo "--- 含 5.0.0 的 Cargo.toml 文件数（写作时 38，Task 3 后 39）---"
git ls-files '*Cargo.toml' | xargs grep -l '5\.0\.0' | wc -l
echo "--- 5.0.0 出现总次数（写作时 87，Task 3 后 88）---"
git ls-files '*Cargo.toml' | xargs grep -o '5\.0\.0' | wc -l
echo "--- 非 ecat 开头的 5.0.0 行：必须只剩 workspace 版本行 ---"
git ls-files '*Cargo.toml' | xargs grep -h '5\.0\.0' | grep -v '^ecat' | sort -u
echo "--- README 族：必须是 14 / 28 ---"
git ls-files 'README.md' 'README.en.md' 'docs/i18n/*/README.md' | wc -l
git ls-files 'README.md' 'README.en.md' 'docs/i18n/*/README.md' | xargs grep -o '5\.0\.0' | wc -l
```

**判读**（批次 4 的先例 + 本计划写作时的实测）：

- 第三个命令**必须只剩一行** `version = "5.0.0"`（`[workspace.package]` 那行）。多出别的 ⇒ 有**第三方依赖**恰好是 5.0.0，下一步的 `sed` 会把它一起改掉 —— **停下来报 lead**。
- 文件数 38 / 总次数 87 ⇒ **86 处是内部 path 依赖引用**（`ecat-data = { version = "5.0.0", path = "../ecat-data" }` 这种），加 `[workspace.package]` 1 处。
- README 族 **14 份 × 2 = 28**。**漏 i18n 那 12 份是本仓库踩过的坑**（批次 4 漏过）—— 第三个数字不等于 28 就别往下走。
- ⚠️ Task 3 会新增 2 处 `5.0.0`（`ecat-metrics/Cargo.toml` 的 dev-dep），所以执行时大概读到 **39 / 88**。**以你这次实测的数为准**。

- [ ] **Step 3: bump 5.0.0 → 6.0.0**

```bash
# Cargo.toml 里的 5.0.0 一律**带引号**（version = "5.0.0" / version = "5.0.0", path = ...），
# 所以 sed 带上引号 —— 既精确命中版本号，也不会碰注释或 URL 里的裸 5.0.0。
git ls-files '*Cargo.toml' | xargs sed -i 's/"5\.0\.0"/"6.0.0"/g'
# README 里是 `v5.0.0` / `5.0.0` 混排（表格与正文），不带引号。
sed -i 's/5\.0\.0/6.0.0/g' README.md README.en.md docs/i18n/*/README.md
```

复核（四条**都要**对，数字与 Step 2 对齐）：

```bash
git ls-files '*Cargo.toml' | xargs grep -o '5\.0\.0' | wc -l     # 期望 0
git ls-files '*Cargo.toml' | xargs grep -o '6\.0\.0' | wc -l     # 期望 == Step 2 的总次数
grep -rc '5\.0\.0' README.md README.en.md docs/i18n/*/README.md  # 期望每份都是 0（14 行）
git diff --stat | tail -1                                          # 期望 ~39 个 Cargo.toml + 14 份 README 的改动面
```

- [ ] **Step 4: `cargo metadata` 重生成 `Cargo.lock`（**必须在 bump 之后跑**）**

```bash
cargo metadata --format-version=1 --offline > /dev/null; echo "rc=$?"
git diff --stat Cargo.lock
```

**核一眼**：`Cargo.lock` 里若有**第三方** crate 恰好是 5.0.0（前例：`fsevent-sys` / `rusticata-macros`），`sed` 不该碰它们 —— 它们不在 `git ls-files '*Cargo.toml'` 里，所以不会被误改；但 `Cargo.lock` 的 diff 里若出现它们的版本变化，说明改错了，还原重来。

- [ ] **Step 5: 发布前闸门（**本批唯一的全量验证点**）**

```bash
cargo test --workspace; echo "rc=$?"
cargo test --workspace --doc 2>&1 | grep -E '^test result'; echo "rc=${PIPESTATUS[0]}"
cargo fmt --all -- --check; echo "rc=$?"
cargo clippy --workspace --all-targets -- -D warnings; echo "rc=$?"
cargo test -p ecat-data-redis -p ecat-data-clickhouse -p ecat-data-sqlx -p ecat-data-mssql -p ecat-metrics --all-features 2>&1 | grep -E '^test result'
```

**最后一条不能省**：三个 `metrics` feature 默认关，`cargo test --workspace` 用的是 feature **并集**（批次 3 的教训），但 `--all-features` 才是把两个新 metrics 模块真正编进来的那一次。`-p ecat-metrics` 也在里面 —— **Task 3 的跨 crate 回归测试（`tests/rdbms_shared_families.rs`）只有在两个后端的 metrics feature 同时开时才真正生效**，而 `--all-features` 正是那个条件。

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
| 1 | `cargo test --workspace` 全绿，测试数 ≥ 1096（批次 4 末值）+ 本批新增 | Task 8 Step 5 |
| 2 | **13 个 `run_with_timeout` 调用点全部迁移**（实测数，非 spec 的 16） | `rg -n 'run_with_timeout\(' --type rust \| wc -l` 会读到 **15**（13 个既有 + Task 1 新增的 2 处测试调用）→ 按**清单式核对**这 13 个文件:行，不比总数 |
| 3 | `QUERY_TIMEOUTS` 在仓内**零残留**（除 CHANGELOG 的历史段） | `rg 'QUERY_TIMEOUTS' --type rust` **零代码引用**（Task 1 Step 2 片段自带一句历史注释 `本量取代了 5.0.0 的 QUERY_TIMEOUTS`，故**输出非空是正常的**；只要求无 `use`/无读取点） |
| 4 | **两个 `metrics.rs` 消费者在 `--features metrics` 下编译并测试通过** | Task 1 Step 8 最后一条 |
| 5 | Redis 与 ClickHouse 各有**一条真超时**测试（断言 `DeadlineExceeded` / `Timeout`） | Task 4 Step 9、Task 5 Step 11 |
| 6 | Redis 与 ClickHouse 各有**一条真熔断**测试（断言 `Open` **且** 快速返回的时间判据） | 同上 |
| 7 | `TIMEOUTS` 分维度互不串（`Cache`/`Tsdb`/`Rdbms` 各一条断言） | Task 1 Step 2 + Task 5 Step 11 |
| 8 | ClickHouse 并发上限可验证（N+1 并发、峰值 ≤ N） | Task 5 Step 11 |
| 9 | `_with` 方法与 `transaction()` **不**触发熔断（两条把守测试） | Task 5 Step 11 |
| 10 | 每个源文件 < 500 行（含新建的 `tests.rs` / `tsdb.rs` / `metrics.rs` / `resilience.rs` / `outbound.rs` / `rdbms.rs` / `rdbms_shared_families.rs`） | 各 Task 的行数复核 |
| 11 | `cargo clippy --workspace --all-targets -- -D warnings` rc=0 | Task 8 Step 5 |
| 12 | checklist 落在 `docs/superpowers/checklists/backend-resilience-onboarding.md`，八节齐 | Task 6 Step 2 |
| 13 | `database-config-tutorial.md` ×13 都含 `query_timeout_secs`，且**没有** `enabled` 这个不存在的字段 | Task 7 Step 4 + `grep -c 'enabled' docs/database-config-tutorial.md` |
| 14 | 既有用户代码零改动：所有新字段都有默认值，`new()` / `connect()` 也拿到全部能力 | Task 4/5 的构造器步骤 |
| 15 | **三个 `ecat_outbound_*` 指标只注册一份 collector**，多后端共存时不丢样本 | Task 2 Step 15 的 `multiple_backends_coexist_in_one_registry` |
| 16 | 文档里写下的每个配置字段都真实存在于 `serde` 结构体上（无凭空字段） | Task 7 Step 1 的 ⚠️ + Task 8 Step 5 的 clippy |
| **17** | **（追加 1）半开探测名额在 future 被取消时归还** —— `ProbesExhausted` 不再永久锁死；5.0.0 的缺陷有红→绿证据 | Task 2a Step 9 的 RED + Step 10 的 GREEN + Step 11 的空验收自证 |
| **18** | **（追加 2）四个 `ecat_rdbms_*` 家族在两个 `metrics` feature 同时开启时样本都不丢**，且指标名/HELP/标签语义一字未改 | Task 3 Step 3 的 RED + Step 8 的 GREEN + `git diff` 里四个名字无改动 |

**本批不做**（留 5b）：其余 10 个 HTTP 后端 + MongoDB 池配置；README 后端表；按实例（而非后端类别）的指标维度；延迟直方图；重试。

**本批不做，且是判断不是遗漏**：Redis 多路复用换连接池（spec §6）、`RedisLock` 的 `DistributedLock` 路径（出入 9）、`ecat-middleware` 的 tower 层、Memcached。

**本批发现但不修（报给 lead）**：

- `ecat-data-sqlx` / `ecat-data-mssql` 的 `ecat_rdbms_*` 撞名 —— **已由 lead 裁决升级为 Task 3（追加 2），本批修**，不列在此处（原判断是「不在 5a 范围」，lead 推翻）。
- 两个后端 crate 的 `[features] metrics = ["dep:ecat-metrics", "dep:prometheus"]` 里，`dep:prometheus` 在 Task 3 之后**变成未使用的直接依赖**（collector 搬去 `ecat-metrics` 了）。**本批不删**：feature 列表是对外可见的，删它超出「换注册方式」的范围。留作 5b 或独立清理。
- 5b 的 10 个 HTTP 后端若各自 `metrics.rs` 自建 collector，会**原样复制**这个坑 —— 接入 checklist（Task 6）的第一节就是这条。

---

## 执行记录（落码后填）

（执行者按批次 4 的格式补：五步闸门的 rc 与计数、判据逐条结论、任何与计划的偏离及证据。）

| 步骤 | 结果 |
|---|---|
| 追加 1 的 RED 输出（Task 2a Step 9） | ✅ **复核者独立复现**（非仅实施者自述）：禁用 `ProbePermit` 的 `Drop` 归还体 → `cancelled_half_open_probes_return_their_permit` FAILED，`被取消的探测没归还名额 ⇒ 永久 ProbesExhausted`（即 5.0.0 线上缺陷）；复原后 19 passed / rc=0。另：把 `disarm` 改成不置 `armed=false` → 测试**挂死**（exit 124，同线程重入 Mutex）⇒ `armed = false` 是承重的一行。 |
| 追加 2 的 RED 输出（Task 3 Step 3） | ✅ 实测（跨 crate 测试 `both_backends_keep_their_samples_in_one_process`）：`FAILED … 缺 ecat_rdbms_pool_connections{backend="regression-mssql"}` —— 输出里 **mssql 那一族连 `# HELP`/`# TYPE` 行都没有**，证明是「整份 collector 被 `AlreadyReg` 顶掉」而非值算错；修后 GREEN（1 passed）。dev-dep 环闸门：`cargo metadata --offline` rc=0 **且测试目标真实编译+运行**（构建期环亦成立），`cargo tree --edges normal` 仍是 axum/prometheus/tower（零增重）。 |
| `cargo test --workspace` | |
| `cargo test --workspace --doc` | |
| `cargo fmt --all -- --check` | |
| `cargo clippy --workspace --all-targets -- -D warnings` | |
| `--all-features` 四 crate | |

**落码期发现的计划片段错误（均已由实施者实测复现后纠偏，已更新本文件的片段）**：
1. **Task 3 Step 6/7**：`Box::new(count_pool_timeout)` 编译不过（`E0271: expected … to return u64, but it returns ()`）——
   片段一边要求「原样保留 `count_pool_timeout`」（递增函数、返回 `()`），一边把它当 `RdbmsCounterFn = Box<dyn Fn() -> u64>` 传，自相矛盾。
   已改为 `Box::new(|| POOL_TIMEOUTS.load(Ordering::Relaxed))`（即原 collector 的写法，契约逐字未变）。
2. **Task 1 的新增测试** `timeout_counters_are_per_backend_kind` 原写法**是 flaky 的**（与既有的 `slow_future_times_out_and_counts` 并发争同一个 `Rdbms` 槽，实测 12/15 误红）——
   已改为用无人写的 `Storage` 作证人槽（20/20 绿）。计划已更新。
3. **Task 3 片段曾引用 Task 1 已删的 `QUERY_TIMEOUTS`**（会 E0432）—— 已在 `a97153b` 修正。
**共同教训**：本文件的代码片段由计划作者人工撰写、**未经编译**（当时的环境跑不动构建）。实施者遇到片段跑不通时，正确做法是「实测复现 → 停下报 lead → 经确认后偏离并写进提交信息」，**不要**默默改成自认为对的样子。
