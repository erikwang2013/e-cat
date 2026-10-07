# 批次 4 实施计划：包装器、可观测性与收尾

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 RDBMS 后端补上熔断/读写分离包装器与可观测性（metrics / health / tracing 三 opt-in feature），并把 `ecat-circuit-breaker` 的熔断状态机抽成公开类型 —— 后者是**批次 5（14 个后端的出站韧性）的前置依赖**。

**Architecture:** 三个 `SqlExecutor` 包装器放 `ecat-data`（与 trait 同处，且不依赖 ORM）。**熔断逐端点包装**，不是包在路由外层 —— 否则单个从库故障会误熔断整条链路。`RdbmsRouting` 选端点时**读取各端点熔断状态并跳过 `Open` 的** —— 只做逐端点包熔断是不够的：从库挂掉后轮询仍会把 1/N 的读转过去靠熔断快速失败，那不是故障隔离，是**稳定的 1/N 失败率**（spec:235-236）。可观测性三 feature 默认关闭，避免把 axum 拖进核心依赖树。

**Tech Stack:** `ecat-data`（`SqlExecutor`/`RdbmsClient`/`RdbmsError`）· `ecat-circuit-breaker`（熔断状态机）· `ecat-metrics`（Prometheus registry）· `ecat-health`（`HealthCheck` trait）· `ecat-tracing`。

**设计依据:** `docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md` §2.5、§7.5；`docs/superpowers/plans/2026-10-05-orm-mssql-overview.md` 批次 4 段。

---

## 已核实的事实（**不要重新猜**）

均为写本计划时实测：

### `ecat-circuit-breaker`（批次 5 的前置）

```
ecat-circuit-breaker/src/lib.rs   546 行   ← ⚠️ 已超 500 行硬规则（既有违规）
```

- **公开面只有 tower 的两项**：`CircuitBreakerLayer`（:80，builder：`new`/`classify`/`failure_ratio`/`window`/`half_open_probes`/`open_duration`）、`CircuitBreakerService<S>`（:158）
- **状态机 `BreakerInner`（:68）是私有的** —— 这正是本批要抽出来的东西
- **没有独立的 `BreakerConfig`** —— 配置现在是内联在 Layer 的 builder 字段里，抽 `Breaker` 时必须**先把它提出来**
- 有 **12 个既有测试**（spec §11 要求它们**保持全绿** —— `Breaker` 抽取不得改行为）
- `SlidingWindow`（:20）是另一个内部类型

### 三个可观测性 crate

```rust
// ecat-metrics（148 行）
pub fn registry() -> &'static Registry;
pub fn metrics_text() -> String;
pub fn metrics_router() -> Router;

// ecat-health（231 行）
pub trait HealthCheck: Send + Sync { /* :13 */ }
pub struct HealthRegistry { /* :19 */ }
pub struct FnCheck<F> { /* :95 */ }        // 闭包版，省一个具名类型

// ecat-tracing（319 行）
pub fn init(service_name: &str);
pub struct TracingLayer { /* :30 */ }
pub struct TracingService<S> { /* :54 */ }
```

### `ecat-data` 现有的计数器

```rust
// ecat-data/src/timeout.rs:11,14
pub static QUERY_TIMEOUTS: AtomicU64;        // 进程级，批次 1 加的
pub static TRANSACTIONS_LEAKED: AtomicU64;
```

### 两个后端 crate 的 feature

`ecat-data-sqlx/Cargo.toml` 与 `ecat-data-mssql/Cargo.toml` **都还没有 `[features]` 段** —— 本批新建。

### 全批次的硬约束（沿用批次 3，**都实测踩过**）

这三条每条都有实测复现的实例，不是形式要求：

1. **假绿灯**：新建 `.rs` **先加 `mod` 声明再写内容**。「确认失败」必须是**编译错误**或 **FAILED 且失败数 > 0**。看到 `ok. 0 passed; 0 failed` 一律当成「测试没被编译」。
2. **空验收**：**把本任务新增的东西整行删掉，验收步骤还会通过吗？会 → 它是空验收。** 判据的六个已知变体：无测试覆盖 / 无调用点 / **注册表 fallback 共用** / **引用了不存在的东西** / **doctest 放在不执行的位置** / **只断言 `contains`**。
3. **计划片段不保证过 rustfmt**。落码后跑 `cargo fmt`，`fmt --check` 必须 rc=0。计划管语义，rustfmt 管排版。

**另加一条本批专属**：**每个源文件 < 500 行**，收尾必须复核 —— 批次 3 有个文件 534 行而三条闸门全绿（`fmt`/`clippy`/`test` 都抓不到行数）。

```bash
find ecat-orm/src ecat-data/src ecat-circuit-breaker/src ecat-data-sqlx/src ecat-data-mssql/src \
  -name '*.rs' -exec awk 'END{if(NR>500) print FILENAME": "NR}' {} \;
```

---

## 文件结构

```
ecat-circuit-breaker/src/
  lib.rs            公开面重导出（tower Layer/Service + 新的 Breaker/BreakerConfig）
  breaker.rs        Breaker + BreakerConfig（从 lib.rs 抽出，公开）
  window.rs         SlidingWindow（从 lib.rs 抽出，私有）
  tower.rs          CircuitBreakerLayer / CircuitBreakerService（改为调用 Breaker）

ecat-data/src/
  breaker.rs        CircuitBreakerExecutor<S>（泛型包装器）
  routing.rs        RdbmsRouting（读写分离 + 跳过已熔断端点）

ecat-data-sqlx/src/
  metrics.rs        feature = "metrics"：池指标注册
  health.rs         feature = "health"：RdbmsHealthCheck
  tracing.rs        feature = "tracing"：慢查询 warn

ecat-data-mssql/src/
  metrics.rs / health.rs / tracing.rs   同构
```

---

## Task 1: `ecat-circuit-breaker` 抽出公开 `Breaker`（**批次 5 的前置**）

**Files:**
- Create: `ecat-circuit-breaker/src/breaker.rs`
- Create: `ecat-circuit-breaker/src/window.rs`
- Create: `ecat-circuit-breaker/src/tower.rs`
- Modify: `ecat-circuit-breaker/src/lib.rs`（变薄，只做重导出）

**这是全批最关键的一步**：批次 5 的 14 个后端超时/熔断全部依赖这个公开 `Breaker`。**抽取不得改行为** —— 既有 12 个测试必须保持全绿（spec §11 明确要求）。

- [ ] **Step 1: 先跑基线，记下 12 个测试的实际输出**

```bash
cargo test -p ecat-circuit-breaker 2>&1 | grep -E '^test |^test result'
```

**把输出记下来** —— 抽取后要逐条对比，不是只看总数。

- [ ] **Step 2: 把 `SlidingWindow` 与 `BreakerInner` 逐字搬到新文件**

**逐字搬，不改一行逻辑。** 搬完 `<500` 行的约束靠这一步达成（546 行拆成 3~4 个文件）。

`window.rs`：`SlidingWindow`（原 `lib.rs:20` 起）+ 它的专属测试。
`breaker.rs`：`BreakerInner`（原 `:68`）→ 改名并公开化为 `Breaker`，加 `BreakerConfig`。

- [ ] **Step 3: 提出 `BreakerConfig`（**现在不存在，必须新建**）**

配置现在是**内联在 `CircuitBreakerLayer` 的 builder 字段**里的。先把它提成一个独立结构体：

```rust
/// 熔断阈值。字段与 `CircuitBreakerLayer` 的 builder 一一对应 ——
/// 抽取时**不要顺手改默认值**，那会让既有 12 个测试的语义漂移。
#[derive(Debug, Clone)]
pub struct BreakerConfig {
    pub failure_ratio: f64,
    pub window: Duration,
    pub half_open_probes: u32,
    pub open_duration: Duration,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        // 逐字段照抄 CircuitBreakerLayer::default() 现在用的值。
        // ⚠️ 实施时**去读 `ecat-circuit-breaker/src/lib.rs:134` 附近的 impl Default**，
        //    把实际值抄过来，不要用本计划猜测的数字。
        todo!("照抄 lib.rs 现有 Default 的实际值")
    }
}
```

- [ ] **Step 4: `Breaker` 的公开接口**

```rust
pub struct Breaker { /* 内部即原 BreakerInner 的全部字段 */ }

impl Breaker {
    pub fn new(cfg: BreakerConfig) -> Self;

    /// 执行一次受熔断保护的调用。
    ///
    /// - 熔断器 `Open` → **不调用 `f`**，直接返回 `Err(BreakerError::Open)`
    /// - 半开态 → 放行 `half_open_probes` 个探测
    /// - `Ok`/`Err` 按 `classify` 的判据记入滑动窗口
    pub async fn call<F, Fut, T, E>(&self, f: F) -> Result<T, BreakerError<E>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>> + Send;

    /// 供 `RdbmsRouting` 选端点时读取 —— **这是「跳过已熔断端点」的依据**。
    /// 没有它，路由只能盲选再靠熔断快速失败（= 稳定的 1/N 失败率）。
    pub fn state(&self) -> BreakerState;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState { Closed, Open, HalfOpen }

#[derive(Debug, thiserror::Error)]
pub enum BreakerError<E> {
    #[error("circuit breaker is open")]
    Open,
    #[error(transparent)]
    Inner(E),
}
```

**`state()` 是本批新增的公开方法**（原状态机里状态是内部字段）。实现就是读当前状态，**不加逻辑**。

- [ ] **Step 5: `tower.rs` 改为调用 `Breaker`**

`CircuitBreakerLayer` / `CircuitBreakerService<S>` 的**公开签名一个都不改** —— 它们改为持有 `Breaker` 并把调用委托过去。Layer 的 builder 方法（`failure_ratio` 等）改为构造 `BreakerConfig`。

- [ ] **Step 6: `lib.rs` 变薄**

只留 crate 文档 + `mod` 声明 + `pub use`。目标 < 100 行。

- [ ] **Step 7: 逐条对比测试输出**

```bash
cargo test -p ecat-circuit-breaker 2>&1 | grep -E '^test |^test result'
```

**与 Step 1 记下的输出逐条比对** —— 测试名与通过状态都要一致，不是只比总数。

- [ ] **Step 8: 行数复核 + 闸门 + 提交**

```bash
find ecat-circuit-breaker/src -name '*.rs' -exec awk 'END{if(NR>500) print FILENAME": "NR}' {} \;
cargo fmt --check; echo "fmt rc=$?"
cargo clippy -p ecat-circuit-breaker --all-targets -- -D warnings; echo "clippy rc=$?"
git add ecat-circuit-breaker/src
git commit -m "refactor(ecat-circuit-breaker): 抽出公开 Breaker + BreakerConfig，拆分为多文件"
```

### ⚠️ Task 1 必读：空验收的两个口子

1. **「12 个测试仍绿」只对了一半** —— 若把某个测试**改名**了，总数不变而覆盖变了。**逐条比对测试名**，不是只比 `12 passed`。
2. **`state()` 必须有测试** —— 它是本批新增的公开方法，且 `RdbmsRouting` 的「跳过已熔断端点」完全依赖它。**没有测试的话，路由那部分的正确性无从谈起**。至少一条：连续失败触发 `Open` 后 `state() == BreakerState::Open`。

---

## Task 2: `ecat-data::CircuitBreakerExecutor`

**Files:**
- Create: `ecat-data/src/breaker.rs`
- Modify: `ecat-data/src/lib.rs`（`pub mod breaker;` + 重导出）
- Modify: `ecat-data/Cargo.toml`（加 `ecat-circuit-breaker` 依赖）

- [ ] **Step 1: 写失败测试**

用假 executor 记录调用次数，断言：

```rust
/// 熔断打开后，内层**一次都不该被调用** —— 这才叫熔断，
/// 否则只是「快速失败的转发」。
#[tokio::test]
async fn open_breaker_never_reaches_the_inner_executor() { /* … */ }

/// 熔断只看「后端答不答」，`Ok` 与后端报错分别记入成功/失败。
#[tokio::test]
async fn inner_errors_count_as_failures() { /* … */ }

/// 熔断器状态可被外部读取（`RdbmsRouting` 靠它跳端点）。
#[tokio::test]
async fn breaker_state_is_readable() { /* … */ }
```

- [ ] **Step 2: 实现**

```rust
/// 给任意 `SqlExecutor` 包一层熔断。
///
/// **每个端点各包一个** —— 不是包在路由外层（那样单个从库故障会误熔断整条链路，spec:225）。
pub struct CircuitBreakerExecutor<S> {
    inner: S,
    breaker: Breaker,
}

impl<S> CircuitBreakerExecutor<S> {
    pub fn new(inner: S, cfg: BreakerConfig) -> Self;

    /// 供 `RdbmsRouting` 跳过已熔断的端点。
    pub fn state(&self) -> BreakerState;
    pub fn inner(&self) -> &S;
}

#[async_trait]
impl<S: SqlExecutor> SqlExecutor for CircuitBreakerExecutor<S> {
    // execute / query / execute_with / query_with / query_write —— 全部经 breaker.call
    // dialect() —— 直接委托（不是受保护的调用）
}
```

**`dialect()` 不走熔断** —— 它是纯本地判断，不会失败、也不该被「熔断」影响。

**实现要点**：`breaker.call(|| self.inner.query_with(sql, params))` —— 闭包**按需构造 future**，这样熔断打开时内层根本没被碰。

- [ ] **Step 3: 空验收自证**

把 `call` 换成直接调内层（绕过熔断），确认 `open_breaker_never_reaches_the_inner_executor` **FAILED**（内层调用次数从 0 变非 0）。记录实际输出，还原。

- [ ] **Step 4: 闸门 + 提交**

### ⚠️ Task 2 必读：`RdbmsClient` **必须**也实现（**订正 2026-10-07**）

> **本节初版写的是「本任务不实现 `RdbmsClient`」—— 错的，已于 `954289b` 后被实施者发现并订正。**
>
> 初版的推理混淆了两件**完全不同**的事：
>
> | | 复杂度 | 要不要做 |
> |---|---|---|
> | **把熔断套到事务上** | 要在 `TransactionInner` 层面再包一层，代价大于收益 | ❌ 不做 |
> | **把 `transaction()` 委托给内层** | **六行** | ✅ **必须做** |
>
> 而 spec:225-233 的组合方式要求后者：
>
> ```rust
> let primary  = CircuitBreakerExecutor::new(sqlx_primary, cfg.clone());
> let replicas = ...map(|c| CircuitBreakerExecutor::new(c, cfg.clone())).collect();
> let db = RdbmsRouting::new(primary, replicas);
> ```
>
> `RdbmsRouting` 的字段是 `Arc<dyn RdbmsClient>` —— **端点要装进去，所以 `CircuitBreakerExecutor` 必须实现 `RdbmsClient`**，哪怕只是委托。

```rust
#[async_trait]
impl<S: RdbmsClient> RdbmsClient for CircuitBreakerExecutor<S> {
    /// 事务**不经熔断**，直接委托内层。
    ///
    /// 与 `RdbmsRouting::transaction()` 的语义一致 —— 它永远走 primary，
    /// 失败由 SQL 层报错，不需要熔断器介入。
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        self.inner.transaction().await
    }
}
```

**必须补的测试**：内层是假 `RdbmsClient`，调 `transaction()` 断言 ①内层 `transaction` 计数 = 1 ②**熔断器未被触碰**（`state()` 未因这次调用改变）。**这是「委托 ≠ 熔断」的把守** —— 没有它，有人把 `transaction()` 改成走 `breaker.call` 也不会被发现。

> **教训**：这一节初版把「不做复杂的那件事」写成了「两件事都不做」。**否定一个方案时，要写清否定的是它的哪一部分** —— 否则读者（包括三个月后的我）会以为整个方向都不该做。

---

## Task 3: `ecat-data::RdbmsRouting`

**Files:**
- Create: `ecat-data/src/routing.rs`
- Modify: `ecat-data/src/lib.rs`
- Modify: `ecat-data/Cargo.toml`（若需要）

- [ ] **Step 1: 写失败测试**（用记录调用的假端点）

必须覆盖的**行为矩阵**：

| 测试 | 断言 |
|---|---|
| 写落主 | `execute` / `execute_with` → primary |
| 读落从 | `query` / `query_with` → replicas 轮询 |
| **`query_write` 落主** | 它是写路径的返回行查询，落从会读到陈旧数据 |
| 轮询 | 连续 3 次读落在 3 个副本上，顺序可预测 |
| **跳过已熔断的副本** | 副本 A 熔断后，读请求**不再选中 A**（断言 A 的调用次数不增） |
| **副本全熔断 + `fallback_to_primary=true`** | 读降级到主库，成功返回 |
| **副本全熔断 + `false`** | 报 `RdbmsError::NoAvailableReplica` |
| `transaction()` 落主 | 且**不经熔断** |
| `dialect()` | 取 primary 的 |

- [ ] **Step 2: 实现**

```rust
pub struct RdbmsRouting {
    primary: Arc<dyn RdbmsClient>,
    replicas: Vec<Arc<dyn RdbmsClient>>,
    next: AtomicUsize,                    // round-robin，无需锁
    fallback_to_primary: bool,            // 默认 true
}
```

**端点选择必须读熔断状态**：

```rust
fn pick_replica(&self) -> Option<&Arc<dyn RdbmsClient>> {
    // 从 next 开始轮询，**跳过状态为 Open 的**，找到第一个可用副本。
    // 全不可用 → None（由调用方按 fallback_to_primary 决定降级还是报错）
}
```

**这就是 spec:235-236 那条要求的落点**：只做逐端点包熔断是不够的，轮询仍会把 1/N 的读转过去靠熔断快速失败 —— 那不是故障隔离，是**稳定的 1/N 失败率**。

- [ ] **Step 3: 空验收自证（**本任务有三条，都要做**）**

1. 删掉 `pick_replica` 里的熔断状态过滤 → 「跳过已熔断的副本」那条 **FAILED**（A 的调用次数增加）
2. 把 `query_write` 改成走 `pick_replica` → 它对应用例 **FAILED**（落到了副本）
3. 把 `fallback_to_primary` 忽略、恒降级 → 「全熔断 + false 报错」那条 **FAILED**

三条都要记录实际输出，逐条还原。

- [ ] **Step 4: 闸门 + 提交**

### ⚠️ Task 3 必读：`NoAvailableReplica` 是新错误变体

`RdbmsError` 现在只有四个变体（`Database`/`Connection`/`Config`/`Timeout`）。本任务要加第五个：

```rust
#[error("no available replica")]
NoAvailableReplica,
```

**加变体是破坏性变更**（穷举 `match` 的下游会编译失败）。仓库内所有 `match RdbmsError` 的点要一起改 —— **实施时先 grep**：

```bash
grep -rn 'match .*RdbmsError\|RdbmsError::' --include='*.rs' ecat-data ecat-data-sqlx ecat-data-mssql ecat-orm | grep -v '^.*/tests' | head -20
```

若有穷举 match 且**不该**因新增变体而失败，那是设计信号，**报给我**再动。

---

## Task 4: 可观测性三 feature（`ecat-data-sqlx` / `ecat-data-mssql`）

**Files:**
- Create: `ecat-data-sqlx/src/{metrics,health,tracing}.rs`（各 feature 门控）
- Create: `ecat-data-mssql/src/{metrics,health,tracing}.rs`
- Modify: 两个 crate 的 `Cargo.toml`（新建 `[features]`）
- Modify: 两个 crate 的 `lib.rs`

**三个 feature 默认关闭** —— 理由（spec:717-718）：避免把 axum（`ecat-metrics`/`ecat-health` 的依赖）拖进核心依赖树。

- [ ] **Step 1: `Cargo.toml` 加 feature 段**

```toml
[features]
# 三个都是 opt-in —— 默认关闭，避免把 axum 拖进核心依赖树。
metrics = ["dep:ecat-metrics"]
health  = ["dep:ecat-health"]
tracing = ["dep:ecat-tracing"]
```

（`dep:` 语法要求 `ecat-metrics` 等在 `[dependencies]` 里是 **optional**。照该 crate 现有写法。）

- [ ] **Step 2: `metrics` feature**

指标名与维度（spec:722 规定）：

| 指标 | 类型 | 维度 |
|---|---|---|
| `ecat_rdbms_pool_connections` | gauge | `backend`、`state="idle"\|"active"` |
| `ecat_rdbms_pool_timeouts_total` | counter | `backend` |
| `ecat_rdbms_query_timeout_total` | counter | `backend` |
| `ecat_rdbms_transactions_leaked_total` | counter | `backend` |

```rust
/// 把池与超时指标注册进全局 registry（`/metrics` 端点自动出现）。
pub fn register_pool_metrics(backend: &'static str);
```

**后两个 counter 接的是既有的进程级静态量**：`ecat_data::QUERY_TIMEOUTS` / `TRANSACTIONS_LEAKED`（`ecat-data/src/timeout.rs:11,14`）—— **把日志变成可告警的指标**（spec:726-727），不要另建计数器。

- [ ] **Step 3: `health` feature**

```rust
/// 池连通性探针。接 `/health` 的 readyz。
pub struct RdbmsHealthCheck<C> { /* 持客户端，发 SELECT 1 */ }
#[async_trait] impl<C: SqlExecutor> ecat_health::HealthCheck for RdbmsHealthCheck<C> { /* … */ }
```

`ecat_health::HealthCheck` 的 trait 定义在 `ecat-health/src/lib.rs:13` —— **先读它的方法签名再写实现**。

- [ ] **Step 4: `tracing` feature**

超阈值 SQL 打 warn（耗时 + **截断后的 SQL**）。阈值 `slow_query_ms` 可配，加进各后端的 `SqlxConfig` / `MssqlConfig`。

**SQL 截断是必须的** —— 参数化的 SQL 可能很长，整条打日志会淹没有用信息。截断策略：保留前 N 字符 + 总长度。**N 用常量，写进 rustdoc**。

- [ ] **Step 5: 每个 feature 至少一条测试**

```bash
cargo test -p ecat-data-sqlx --features metrics,health,tracing
cargo test -p ecat-data-mssql --features metrics,health,tracing
```

⚠️ **`--features` 下的测试必须真的跑**。验证方式：**先确认不带 feature 时新测试不存在**（`cargo test -p ecat-data-sqlx 2>&1 | grep -c metrics` 应为 0），带 feature 时才出现。**否则新测试可能写在 feature 门控之外** —— 那是空验收的变体。

- [ ] **Step 6: 闸门 + 提交**

### ⚠️ Task 4 必读：`dep:` 语法与 feature 组合爆炸

若 `ecat-metrics` 等已在 `[dependencies]` 里（非 optional），加 feature 时**不要**用 `dep:` —— 那会报错。先读该 crate 的现有依赖声明再写。

另外 **`cargo test --workspace` 会用上 feature 并集**（批次 3 的教训：`ecat-mq-nats` 的 rustls provider panic **只在这一层暴露**）。所以三个 feature 的代码必须彼此兼容，且与默认构建兼容。

---

## Task 5: `ecat` 聚合 crate 的 feature

**Files:**
- Modify: `ecat/Cargo.toml`
- Modify: `ecat/src/lib.rs`（若需要重导出）

- [ ] **Step 1: 加两个 feature**

```toml
[features]
orm   = ["dep:ecat-orm"]
mssql = ["dep:ecat-data-mssql"]
```

- [ ] **Step 2: 验证组合**

```bash
cargo check -p ecat --features orm,mssql
cargo check -p ecat                                  # 默认仍能编
```

- [ ] **Step 3: 提交**

### ⚠️ Task 5 必读：不要顺手加别的 feature

`ecat` 是聚合入口 crate。**只加计划要求的两个** —— 顺手把别的后端也加进去会让依赖树膨胀，且不在本批范围内。

---

## Task 6: 文档

**Files:**
- Modify: `docs/ecosystem-plan-v3.md` + `docs/i18n/{12}/ecosystem-plan-v3.md`
- Modify: `docs/tls-certificate-tutorial.md` + `docs/i18n/{12}/tls-certificate-tutorial.md`
- Modify: `README.md` / `README.en.md` + `docs/i18n/{12}/README.md`（后端表补「超时/熔断」列）
- Modify: `docs/database-config-tutorial.md` + i18n ×12（补 `slow_query_ms` 与 feature 开关）

- [ ] **Step 1: 逐份更新**

**i18n 是 12 个语言目录**：`ar` `bn` `de` `en` `es` `fr` `hi` `id` `ja` `ko` `pt` `ru`。

**⚠️ 文档里的状态标记必须与实际一致** —— README 系带 `✅ 已实现` 这类状态列。**写之前确认那个功能真的落地了**。

- [ ] **Step 2: 提交**

### ⚠️ Task 6 必读

**不要为了「有改动」而改**。生态规划里的「尚未实现」标记要在**功能真的落地后**才去掉。

---

## Task 7: CHANGELOG + 版本 bump

**Files:**
- Modify: `CHANGELOG.md`
- Modify: 37 个 `Cargo.toml` + `Cargo.lock`

- [ ] **Step 1: CHANGELOG 加 4.2.0 段**

新增（`Breaker`/`BreakerConfig`/`BreakerState` 公开、`CircuitBreakerExecutor`、`RdbmsRouting`、三个 feature、`ecat` 两个 feature），变更（`RdbmsError` 新增 `NoAvailableReplica` 变体 —— **要在破坏性变更段点名**）。

- [ ] **Step 2: 版本 bump 4.1.0 → 4.2.0**

```bash
git ls-files '*Cargo.toml' | xargs sed -i 's/"4\.1\.0"/"4.2.0"/g'
cargo metadata --format-version=1 --offline > /dev/null; echo "rc=$?"
```

⚠️ **bump 前先核**：`git ls-files '*Cargo.toml' | xargs grep -h '4\.1\.0' | grep -v '^ecat'` 应只剩 workspace 版本行 —— 确认没有第三方依赖恰好也是 4.1.0。

- [ ] **Step 3: 发布前闸门**

```bash
cargo test --workspace; echo "rc=$?"
cargo test --workspace --doc 2>&1 | grep -E '^test result' ; echo "rc=${PIPESTATUS[0]}"
cargo fmt --check; echo "rc=$?"
cargo clippy --workspace --all-targets -- -D warnings; echo "rc=$?"
```

**这是本次唯一的全量验证点** —— 发布脚本没有闸门（批次 3 已验证）。

---

## 批次完成判据

| # | 判据 | 验证 |
|---|---|---|
| 1 | `cargo test --workspace` 全绿，测试数 ≥ 1076（批次 3 末值） | Task 7 Step 3 |
| 2 | **`ecat-circuit-breaker` 既有 12 个测试逐条仍绿**（不是只比总数） | Task 1 Step 7 |
| 3 | `cargo clippy --workspace --all-targets -- -D warnings` rc=0 | Task 7 Step 3 |
| 4 | 每个源文件 < 500 行（含 `ecat-circuit-breaker` 那个 546 行的既有违规） | 文末复核命令 |
| 5 | 三个 feature 各自可单独启用并编译 | Task 4 Step 6 |
| 6 | **`Breaker::state()` 有测试**，且 `RdbmsRouting` 的跳端点行为有测试 | Task 1/3 |
| 7 | **`RdbmsError::NoAvailableReplica` 的破坏性影响已 grep 排清** | Task 3 必读 |

**本批不做**（留批次 5）：14 个后端的超时/熔断/指标铺开、HTTP 类的并发上限、MongoDB 池配置暴露。
