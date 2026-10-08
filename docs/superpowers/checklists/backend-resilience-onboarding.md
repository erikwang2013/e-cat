# 出站韧性接入 checklist（5b 逐 crate 照做）

> **这是 5a 的交付物**（spec §7.5 点名）。5b 在 11 个后端（10 个 HTTP + MongoDB，spec §7.5）
> 上把同一套动作重复 11 次 —— 本清单的价值是**防 10 次即兴发挥**：每一节给**逐字代码**
> + **为什么这么做（附 5a 实测证据）** + **验收命令与期望输出**。
>
> **5a 的参照实现**（全部已过闸门、可编译可跑，遇到疑问直接读这几份）：
>
> | 角色 | 文件 |
> |---|---|
> | 泛型助手（`BackendKind` / `run_with_timeout` / `TIMEOUTS` / `TimeoutError`） | `ecat-data/src/timeout.rs` |
> | 三个 `ecat_outbound_*` 家族的**唯一** collector | `ecat-metrics/src/outbound.rs` |
> | 熔断器（`Breaker` / `BreakerConfig` / `opened_total()` / `state().code()`） | `ecat-circuit-breaker/src/breaker.rs` |
> | 熔断错误映射（两个错误类型各一个函数） | `ecat-data/src/breaker.rs`（`:39-56`） |
> | **非 HTTP 客户端**路径（假 RESP 服务端） | `ecat-data-redis/src/{lib,metrics,tests}.rs` |
> | **最难 case**：一个 client 两条 I/O 路径、两个指标槽、并发信号量 | `ecat-data-clickhouse/src/{lib,tsdb,metrics,tests/resilience}.rs` |
>
> **不要重做**：`ecat_rdbms_*` 四个家族（`ecat-data-sqlx` / `ecat-data-mssql`）的
> 全进程唯一 collector 已在 5a Task 3 修好，那两个 crate 只挂数据源
> （见 `ecat-data-sqlx/src/metrics.rs:43-66`）。5b 若要再动它们，先读「出入 11」。
>
> **实施顺序**（照 `ecat-data-redis` 的提交顺序）：①加配置字段 → ②写 `guarded` 外壳
> → ③逐个出站方法包壳 → ④metrics feature 挂数据源 → ⑤超时测试 → ⑥熔断测试
> → ⑦挂 `mod` + 闸门 + 提交。每步的验收在对应小节里。

---

## 1. 加配置字段

前两个字段**逐字**（`ecat-data-redis/src/lib.rs:45-55`；`ecat-data-clickhouse/src/lib.rs:49-57`
同名同型，差异只在 rustdoc：它的 `query_timeout_secs` 多两句「外层预算」、`breaker` 只有一行
—— 下面引用的是**更完整**的 rustdoc 版本，5b 按完整版写）：

```rust
    /// 单次命令超时秒数。`0` = 禁用；未配置 = 30 秒。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    ///
    /// 熔断**默认开启** —— 保守阈值下只在持续失败时打开。**当前没有总开关**：
    /// `BreakerConfig` 只有阈值字段，没有 `enabled`（计划 Task 7 也点名不许写
    /// `{"enabled": false}`：那是反序列化错误，或被 `#[serde(default)]` 静默吞掉
    /// 后以为关掉了）。真要停用，只能把阈值调到不可能触发（如 `failure_ratio: 1.1`）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
```

HTTP 后端**再加**第三个（`ecat-data-clickhouse/src/lib.rs:58-62`）；非 HTTP 后端
（Redis 就是反例）**没有这个字段** —— 多路复用连接不需要它：

```rust
    /// 并发上限。reqwest **只有** `pool_max_idle_per_host`（空闲保留数），
    /// 没有「最大总连接数」—— 默认无上限意味着并发无背压。
    /// 未配置 = 32。上限由本 crate 的信号量实现，不是 reqwest 的旋钮。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
```

⚠️ **`max_concurrency: Some(0)` 必须是「不限并发」，不能落成 `Semaphore::new(0)`**：
`guarded` 的第一句是 `let _permit = self.permit().await;`，而**超时层在许可里层** —— 0 个许可 ⇒
这句永不返回 ⇒ **静默无限挂起，连超时都不触发**（比本批要防的超时缺口更糟）。
与 `query_timeout_secs: 0` = 禁用同构：用户读配置文档的预期就是「0 = 不限」。

```rust
    // from_config 里：Some(0) = 不限并发 ⇒ 根本不建信号量。
    let semaphore = match cfg.max_concurrency {
        Some(0) => None,                                  // 不限
        Some(n) => Some(Arc::new(Semaphore::new(n))),
        None => Some(Arc::new(Semaphore::new(32))),       // 默认 32
    };
```

配套：`permit()` 返回 `Option<SemaphorePermit<'_>>`（不限并发时返回 `None`）——
调用点 `let _permit = self.permit().await;` **不用改**。
验收：一条「`Some(0)` 下 N+1 个并发**全部放行、不排队**」的用例（对 mock 起 N+1 个并发，断言都返回而非挂起）。

`query_timeout_secs` 的 rustdoc 若你的后端跑在 `ecat_tls` 建的 reqwest client 上，要**多写两句**
（理由在「§7 陷阱 5」；`ecat-data-clickhouse/src/lib.rs:49-54` 逐字，含字段本体）：

```rust
    /// 单次调用超时秒数。`0` = 禁用；未配置 = 30 秒。
    ///
    /// 这是**外层**预算，与 reqwest 自带的总超时（`from_config` 建的 client 有
    /// 30 秒、`new` / `with_auth` 没有）取先到者。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
```

（Redis 没有内层超时，所以它的 rustdoc 只有一行 `单次命令超时秒数。`0` = 禁用；未配置 = 30 秒。`。
没有内层超时的后端按 Redis 写，有的按上面 ClickHouse 写。）

`0 = 禁用`的转换函数逐字（两个 crate 各一份私有函数，`ecat-data-redis/src/lib.rs:58-65`、
`ecat-data-clickhouse/src/lib.rs:89-96`）：

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

**为什么**：

- `BreakerConfig` 的 `Deserialize` 是 5a Task 4 Step 5 加的（`e9b1246`）。`breaker` 字段的
  类型就是它，**不需要**你自己给每个后端再造一个熔断配置结构。四个字段各自带
  `#[serde(default)]`（`ecat-circuit-breaker/src/breaker.rs:36-46`），所以 `breaker: {}`
  也能反序列化；`window` / `open_duration` 按**整数秒**反序列化（同文件 `duration_secs` 模块）。
- 默认值住在转换函数里（未配置 = 30 秒），**不是**住在字段类型里 ——
  `Option<u64>` 的 `None` 不能区分「没配」与「禁用」。
- `from_config` 里装配：`query_timeout: query_timeout(cfg.query_timeout_secs)`、
  `breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default()))`、
  `semaphore: Arc::new(Semaphore::new(cfg.max_concurrency.unwrap_or(32)))`
  （`ecat-data-clickhouse/src/lib.rs:145-147`；Redis 的对应三行在 `ecat-data-redis/src/lib.rs:126-130`，没有 semaphore）。

**验收**（`ecat-data-redis/src/tests.rs:334-343` 的 `zero_timeout_means_disabled` 是模板）：

```bash
cargo test -p <crate> zero_timeout        # ① 转换函数：Some(0)==None、None==30s
cargo test -p <crate> config_             # ② 从配置装配
```

期望：两条都绿。②必须**经 `from_config` 走一遍**，不能只断言转换函数 ——
只测 `query_timeout(Some(0))` 的话，把 `from_config` 里那一行删掉照样绿（空验收）。

---

## 2. 包 `run_with_timeout`

**顺序：熔断在外、超时在内。**（HTTP 后端还要在最外面套并发许可：**许可 → 熔断 → 超时**。）

**为什么不能反过来**（「与 spec 的出入 4」的实测依据，源码在 `ecat-circuit-breaker/src/breaker.rs`）：

1. **超时在外 ⇒ 熔断器对「卡死的后端」永久失明。** 超时触发时 `tokio::time::timeout`
   会 drop 掉内层 future；此时 `Breaker::call` 正停在 `let result = f().await;`（`:175`），
   它后面的 `inner.window.record(false)`（`:178-192` 的记录块）**永远不会执行** —— 每次「后端没在
   预算内作答」都是一次 drop，窗口里什么都不记。一个每次都挂满超时的后端**永远打不开
   熔断器**，而「卡死的后端」正是本设计要防的头号场景。
2. **半开探测名额有借无还。** 名额在 `f().await` **之前**借出（`:161-170`），归还只在记录
   逻辑里。future 被 drop ⇒ 漏名额；`half_open_probes` 默认 3，三次被掐断的半开探测之后
   每次调用直接 `ProbesExhausted`，**永久停在 `HalfOpen`**。5a 已给 `Breaker` 补了 RAII
   归还（`18c6bd0`，有红→绿证据），但顺序仍按「熔断在外」—— 问题 1 无解，且「熔断在外」
   的语义一句话说得清：**熔断器数的是「我们是否在预算内拿到一个可接受的答复」，超时不
   算数就是不算数。**
3. **许可在最外**（仅 HTTP 后端）：还在排队的请求**还没碰后端**，不该计入熔断失败、
   也不该被超时掐断（`ecat-data-clickhouse/src/lib.rs:166-170` 的注释）。

**变体 A：`ecat_errors::Error` 路径**（`ecat-data-redis/src/lib.rs:146-166` 逐字）——
六个非 RDBMS trait（`Cache` / `SearchClient` / `GraphClient` / `DocumentClient` /
`StorageClient` / `TsdbClient`）用这个：

```rust
    /// 一次出站调用的公共外壳：**熔断在外、超时在内**。
    ///
    /// 顺序与 spec §3 相反（理由见批次 5a 计划的「与 spec 的出入 4」）：
    /// 超时若在外层，`tokio::time::timeout` 会把熔断器的 future 直接 drop 掉，
    /// 于是每次「后端没在预算内作答」都**什么都不记** —— 卡死的后端永远打不开熔断器，
    /// 而卡死正是本设计要防的头号场景。熔断在外时超时是一次普通的 `Err`，如实计入失败。
    ///
    /// **只包真正发 I/O 的路径。** 纯本地分支（如 `multi_get` 的空 keys 提前返回）
    /// 必须留在外面：半开态下每次 `call` 都要借走一个探测名额，而「没发请求就返回」
    /// 会以 `Ok` 记账 —— 探测名额被白白消耗、熔断器还可能被**误关回 `Closed`**，
    /// 于是新窗口的流量全部冲向一个根本没被碰过的后端。记账口径是「后端的表现」，
    /// 不是「函数的返回值」。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        self.breaker
            .call(|| run_with_timeout(BackendKind::Cache, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "redis"))
    }
```

**变体 B：`RdbmsError` 路径 + 信号量**（`ecat-data-clickhouse/src/lib.rs:164-195` 逐字）——
`SqlExecutor` 家族用这个（**`kind` 参数只在多 I/O 路径的 crate 上保留**，单路径写死，见下）；
同一文件还有 `Error` 路径的孪生 `guarded_tsdb`：

```rust
    /// 一次出站调用的外壳（`RdbmsError` 路径）。
    ///
    /// **顺序：许可 → 熔断 → 超时**（理由见批次 5a 计划的「与 spec 的出入 4」）：
    /// - 许可在最外：还在排队的请求**还没碰后端**，不该计入熔断失败、也不该被超时掐断
    /// - 熔断在超时外：超时是一次普通的 `Err`，会**如实计入**熔断窗口 ——
    ///   否则卡死的后端永远打不开熔断器
    pub(crate) async fn guarded<F, T: 'static>(
        &self,
        kind: BackendKind,
        fut: F,
    ) -> Result<T, RdbmsError>
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
    pub(crate) async fn guarded_tsdb<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Tsdb, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "clickhouse"))
    }
```

**`kind` 写死，除非你的 crate 有多条 I/O 路径**（5b 起，口径统一）：

`kind` 是**维度选择器**，它的值由「调用点包着哪个 trait」唯一决定。只有当一个 crate
**同时实现多个 I/O trait、必须按调用点区分维度**时（`ecat-data-clickhouse`：
`SqlExecutor` → `Rdbms` 与 `TsdbClient` → `Tsdb` 共用同一个 `Breaker`），才把 `kind`
做成函数参数（上面的变体 B 就是从它逐字抄的）。**单路径 crate 一律写死**：

```rust
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await; // HTTP 后端才有（见上面的「许可在最外」）
        self.breaker
            .call(|| run_with_timeout(BackendKind::Graph, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "arangodb"))
    }
```

理由：写成参数意味着**调用点可以传任意维度**，把「维度由 trait 家族决定」这条约束
降级成「调用者说了算」；而填错只会**静默少数**（见下一条），没有编译期保护。
单路径 crate 带这个参数 = 白送一个能悄悄写错的旋钮，没有任何收益。
（批次 5b 的 10 个 HTTP 后端全部是单路径，因此全部写死；`ecat-data-clickhouse` 是
双路径的例外，保留参数形式。`ecat-data-questdb` 虽是 `RdbmsError` 路径，但只有
`SqlExecutor` 一条路径 ⇒ 也写死 `BackendKind::Rdbms`。）

**包哪些方法**：

- `kind` 按**调用点包着哪个 trait** 选，不按产品品类选（`ecat-data/src/timeout.rs:15-35`
  的判据表）：`SqlExecutor`/`RdbmsClient` → `Rdbms`、`Cache` → `Cache`、`TsdbClient` → `Tsdb`……
  带 `--features metrics` 时 `Rdbms` 槽会被 `ecat_rdbms_query_timeout_total` 读走 ——
  错填会**静默少数**，且没有编译期保护。
- 包的是**发 I/O 的方法**，不是「trait 的所有方法」：`dialect()`（纯本地判断）不包；
  `multi_get` 的空 keys 提前返回不包（理由见上面 `guarded` 的 rustdoc）。
- **默认实现不要包**、**常量错误返回的方法不要包** —— 见「§7 陷阱 2/3」，
  并各配一条把守测试（模板见 §3 验收）。
- 若你的后端 `_with` 真做参数化 I/O（而不是落默认实现），那就正常包 —— ClickHouse 的
  「一个字都不写」只因为它的 `_with` 落到 trait 默认实现（出入 7）。

**错误映射**（`ecat-data/src/breaker.rs:39-56`，两个错误类型各一个函数，孤儿规则写不出统一 `From`）：

- `RdbmsError` 侧：`map_breaker_error` —— 内层错误原样透出；`Open` / `ProbesExhausted`
  → `RdbmsError::Connection("circuit breaker is open")` / `"circuit breaker: too many probes"`。
- `Error` 侧：`breaker_error_to_backend_error(e, "<产品名>")` —— 熔断拒绝
  → `ErrorCode::Unavailable` + `reason`。`reason` 传**产品名**（redis 传 `"redis"`、
  clickhouse 传 `"clickhouse"`；注意 ClickHouse 的 tsdb 路径也传 `"clickhouse"`，
  不是 `"clickhouse-tsdb"` —— 路径后缀只属于指标标签）。
  这与**超时**路径的 reason 不同：超时的 reason 是 `BackendKind::slug()`（`"cache"` /
  `"tsdb"`，`ecat-data/src/timeout.rs:52-62`），按家族粒度。两者粒度不同是有意的
  （「§4」也点了一遍），别“统一”成一种。

**验收**：写完立刻做过一次**红探针** —— 随便拆掉一个方法的壳（直接 `fut.await`），
§5 的超时用例必须变红。实测例（`d2aea42` 提交信息）：拆掉 Redis `set` 的 `guarded` ⇒
用例 5.00s 后 FAILED，信息 `set: 内层超时没开火（漏包 guarded？）`；拆掉前 0.11s 绿。
**探针输出记进提交信息** —— 这是「包装真的生效」的证据链。

---

## 3. 加 `Breaker` 字段

字段与构造（`ecat-data-redis/src/lib.rs:75-80, 126-131` 逐字）：

```rust
pub struct RedisCache {
    conn: MultiplexedConnection,
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
}
```

```rust
        Ok(Self {
            conn: client.conn,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
        })
```

其余构造器（`connect` / `connect_with_password` / `from_connection` / `new` / `with_auth`）
一律 `Arc::new(Breaker::new(BreakerConfig::default()))` —— 保守默认。

`metrics` feature 要读它的两个数据源，所以给一个访问器（`ecat-data-redis/src/lib.rs:141-144` 逐字；
ClickHouse 的同款在 `ecat-data-clickhouse/src/lib.rs:151-154`，只有 rustdoc 短一些）：

```rust
    /// 本 client 的熔断器。`metrics` feature 注册指标时要读它的状态与打开次数。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }
```

**为什么**：

- **逐实例一个**：熔断器数的是「这个后端实例答不答」。挂进程上 = 一个从库故障熔断整条链路。
- **`Arc` 而不是裸 `Breaker`**：注册指标时三个取数闭包要 `'static`（`Box<dyn Fn() -> u64 + Send + Sync>`），
  且 client 自己也要持有一份 —— `Arc` 两处共享。`register_outbound_metrics(breaker: Arc<Breaker>)`
  收走一个 clone，client 保留自己的。
- **`state()` 给路由用、`opened_total()` 给指标用**：`state()` 在冷却期已过时**报告**
  `HalfOpen` 但不改内部状态（`ecat-circuit-breaker/src/breaker.rs:233-253`），拿它当 gauge
  是安全的；「开了几次」**只能**用 `opened_total()` —— 出入 5：拿 `state()` 轮询猜是错的
  （轮询间隔决定准确性，开又关抓不到）。
- **两条 I/O 路径共用一个 `Breaker`**（ClickHouse）：同一个服务器、同一个故障域。
  若拆成两个字段，故障域就切错了。这条有把守测试
  （`ecat-data-clickhouse/src/tests/resilience.rs:122-145`：`SqlExecutor` 打到 `Open` 后
  `TsdbClient` 必须被同一个熔断器拒绝），且有两条红探针实测（`2bbe1af`：绕过 breaker →
  红；改用独立 `Breaker::new` → 红）。
- **闭包按需构造 future**：`Breaker::call` 收的是 `FnOnce() -> Fut`（`:138-144`），
  `Open` 分支**直接 `return Err`，`f` 根本不被调用** —— 所以写成
  `call(|| run_with_timeout(kind, self.query_timeout, fut))`，熔断打开时内层一次都不碰
  （`ecat-data/src/breaker.rs:61` 同款注释）。

**验收**：

```bash
grep -n 'Arc<Breaker>' <crate>/src/lib.rs          # 字段 + 访问器都在
cargo test -p <crate> -- --list | grep -E 'trip_the_breaker|open_breaker'
```

把守测试模板（写你 crate 的对应版本，逐字参考）：

- `unsupported_with_methods_do_not_trip_the_breaker`（`ecat-data-clickhouse/src/tests/resilience.rs:153-167`）
  —— 连打 8 次「不支持」的方法，`state()` 必须还是 `Closed`、`opened_total() == 0`。
- `transaction_error_does_not_trip_the_breaker`（同文件 `:171-179`）—— 常量错误同理。
- 两条路径的后端补一条「路径 A 打开后路径 B 也被拒」（同文件 `:122-145`）。

---

## 4. 注册指标

**三个指标名、类型、维度**（`ecat-metrics/src/outbound.rs` 模块文档逐字）：

| 指标 | 类型 | 维度 |
|---|---|---|
| `ecat_outbound_timeouts_total` | counter | `backend` |
| `ecat_outbound_breaker_open_total` | counter | `backend` |
| `ecat_outbound_breaker_state` | gauge | `backend`（0=closed 1=open 2=half-open）|

**数据源**：超时数 ← `timeout_counter(kind).load(Ordering::Relaxed)`（进程级静态量，
本 crate 的 `metrics` feature 只做读取）；打开次数 ← `breaker.opened_total()`；
状态 ← `breaker.state().code()`（`0/1/2` 的编码由 `BreakerState::code()` 提供，
`ecat-circuit-breaker/src/breaker.rs:22-28`，**不要改这个映射**）。

**你要写的全部代码就这么点**（`ecat-data-redis/src/metrics.rs:20-28` 逐字；生产代码 10 行）：

```rust
pub fn register_outbound_metrics(breaker: Arc<Breaker>) {
    let opened = Arc::clone(&breaker);
    ecat_metrics::register_outbound_metrics(
        "redis",
        Box::new(|| timeout_counter(BackendKind::Cache).load(Ordering::Relaxed)),
        Box::new(move || opened.opened_total()),
        Box::new(move || breaker.state().code()),
    );
}
```

一个后端有**两条 I/O 路径**时按路径各挂一份（`ecat-data-clickhouse/src/metrics.rs:17-30` 逐字）：
`backend="clickhouse"`（Rdbms 槽）与 `backend="clickhouse-tsdb"`（Tsdb 槽）；
熔断两项是**同一个** `Breaker`，两份样本值相同 —— 这是有意的，按 `backend` 分组时两条都看得到。

```rust
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
```

注意**第四个**参数（`breaker_state`）的类型是 `OutboundStateFn = Box<dyn Fn() -> u8 + Send + Sync>`
—— 返回 **u8**（`BreakerState::code()`），别把 `opened_total()` 的 u64 塞给它；第三个参数
`breaker_opened` 才是 `Box<dyn Fn() -> u64>`。两个 `Arc::clone` 是承重的：三个闭包要 `'static`，
`move` 走的是 `Arc` 的所有权。

**Cargo.toml 与 lib.rs 的接线**（两个 crate 同款，`ecat-data-redis/Cargo.toml` 逐字）：

```toml
# 指标依赖 optional：默认不把 axum（ecat-metrics 的依赖）拖进核心依赖树。
ecat-metrics = { workspace = true, optional = true }

[features]
# 只有 ecat-metrics：三个出站指标家族的唯一 collector 在那边（批次 5a「出入 11」），
# 本 crate 不需要直接依赖 prometheus。
metrics = ["dep:ecat-metrics"]
```

```rust
#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;
```

**为什么不能照抄批次 4 的「每 crate 一份 collector」**（出入 11）：`ecat-metrics` 的
`REGISTRY` 是全进程一个（`ecat-metrics/src/lib.rs:16`），`Registry` 按「指标名 + 常量标签」
去重 —— 同名 `Collector` 注册第二次直接 `AlreadyReg`，**后注册者的样本一个都不输出**
（无报错、无日志）。Redis 与 ClickHouse 同时开 `metrics` 就是「谁先注册谁独活」。
5b 会把这个坑复制 11 倍。**实测证据**（Task 3）：`ecat-data-sqlx` 与 `ecat-data-mssql`
曾各建一份 `ecat_rdbms_*` collector，跨 crate 复现测试里 mssql 那一族**连 `# HELP`/`# TYPE`
行都没有**；修法就是本节的模式：collector 只建一份（在 `ecat-metrics`），各 crate 只挂闭包。

**标签值取产品名，不取类别名**。⚠️ **与 spec §4 的差异**：spec 写的是类别名
（`rdbms`/`cache`/…），**实际约定是产品名**（`"redis"`、`"clickhouse"`）。告警规则**必须按
产品名写**（`backend="redis"`）—— 照 spec 原文写 `backend="cache"` 永远匹配不到任何样本。
这与 `BackendKind::slug()`（错误里的 `reason`，家族粒度）不同，两者粒度不同是有意的。
另注意注册函数的第一个参数是 `&'static str` —— 标签必须是静态字符串字面量，
不能是运行期拼出来的 `String`（想做 per-instance 标签得用 `Box::leak` 之类，5a 没做）。

**幂等与覆盖**：collector 只注册一次；同一个 `backend` 重复注册**覆盖**它的三个闭包
（不追加 —— 同一标签两份样本会让 Prometheus 报错）。注册发生在**使用方的初始化**里，
5a 结束时全仓没有生产调用点（只有定义与测试用），5b 同样不必自己接。

**验收**：

```bash
cargo test -p <crate> --features metrics      # 你的 metrics 用例
cargo test -p <crate> 2>&1 | grep -c 'outbound_metrics'   # 期望 0：指标代码在 feature 之外
```

用例模板：`ecat-data-redis/src/metrics.rs:47-82`（三个样本都在 + 熔断器推到 `Open` 后
是**抓取时现读**的：注册时快照的实现只会给出 0）。本 crate 内的测试只能证明「自己挂上了」；
「多个后端共存」的真验收在 `ecat-metrics/src/outbound.rs:292-305`
（`multiple_backends_coexist_in_one_registry`）与 Task 3 的跨 crate 回归测试。
测试的 `backend` 标签**用例之间不要重复**（registry 与静态量都是进程级的）。

---

## 5. 加一条超时测试

**必须是端到端的**（打真实方法）。只测 `run_with_timeout` 本身不覆盖「你的方法包没包壳」——
漏包的后果是**行为原样**（挂着不返回）或**根本没有超时**，只有端到端能红。

**判据三选一，按后端的可测性**：

### 模式 ①：有 HTTP 接口 → 进程内 axum mock + 延迟（ClickHouse 模式）

`ecat-data-clickhouse/src/tests/resilience.rs:12-43`（mock + 经 `from_config` 装配的 helper 逐字）：

```rust
/// 一个「先拖住再回」的 mock：`delay` 之后回 200 空体。
/// 既有的 `spawn_mock` 第 4 个参数是响应头、**不是延迟**，故另起一个。
async fn spawn_slow_clickhouse(delay: Duration, in_flight: Arc<AtomicUsize>) -> String {
    let app = axum::Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
        let in_flight = Arc::clone(&in_flight);
        async move {
            in_flight.fetch_add(1, Ordering::SeqCst);
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

/// `max_concurrency` 走**配置**（`Some` 显式给值、`None` 走 `from_config` 的默认
/// 32）：直接改私有字段会让 `cfg.max_concurrency` 的装配路径失去覆盖 —— 那样
/// `from_config` 里删掉那行，本文件的并发测试照样绿。
fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> ClickhouseClient {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    let cfg: ClickhouseConfig = serde_json::from_str(&format!(
        r#"{{"base_url": "{url}", "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
    .unwrap();
    ClickhouseClient::from_config(cfg).unwrap()
}
```

用例本体（`:45-63`，含**为什么这参数是配的**）：

```rust
/// 超时真的开火（spec §8 判据 2）。
///
/// **必须用 1 秒超时 + 5 秒 mock**：`from_config` 建的 client 自带 reqwest 的
/// 30 秒总超时，mock 只拖 5 秒时它来不及开火；若我们的外层没接上，
/// 调用会**成功返回** ⇒ 断言失败。这条测试不是空验收。
```

断言：`matches!(err, RdbmsError::Timeout(_))` + 计数方向断言（见下面的证人槽规则）。

### 模式 ②：有原生连接 → 假 `TcpListener` 装死（mssql 模式）

`ecat-data-mssql/src/tests.rs:439-467`（逐字，要点：只 `accept` 不回应 + `pending` 挂住）：

```rust
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // 收下连接后挂住不放：握手等不到响应，也断不了。
    let stall = tokio::spawn(async move {
        let _sock = listener.accept().await.unwrap().0;
        std::future::pending::<()>().await;
    });

    let c: MssqlConfig = serde_json::from_str(&format!(
        r#"{{"url": "mssql://sa:pw@127.0.0.1:{port}/app", "query_timeout_secs": 1}}"#
    ))
    .unwrap();
    let client = MssqlClient::from_config(c).await.unwrap();
```

（末尾 `stall.abort();` 收尾。）Redis 的假 RESP 服务端（应答握手、对数据命令装死，
`ecat-data-redis/src/tests.rs:170-206`）是同一模式的变体 —— 注意**握手必须应答**，
否则测到的是建连超时而不是命令超时。

### 模式 ③：内层可替身 → 假 impl + `future::pending()`

照 `ecat-data/src/breaker.rs:153-227` 的 `FakeExecutor` 写法（实现 trait、`calls` 计数），
慢方法体里 `std::future::pending::<Result<(), _>>().await`（同款闭包见
`ecat-circuit-breaker/src/breaker.rs:929`）。适合内层是纯 Rust 类型的后端。

### 三个硬要求（三个模式都适用）

**(a) 错误码断言**：`Error` 路径断言 `err.code == ErrorCode::DeadlineExceeded`、
`reason == kind.slug()`；`RdbmsError` 路径断言 `RdbmsError::Timeout(_)`。
超时与熔断必须是**不同**的码（`ecat-data-redis/src/tests.rs:316-328`
`timeout_and_breaker_open_have_distinct_codes`）—— 混成一个码，调用方就分不清
「这次慢」和「后端已经放弃了」。

**(b) 槽计数：先决定「方向断言还是严格断言」，取决于**那个槽有几个写者**。**
`TIMEOUTS` 是进程级静态量，libtest 并行跑用例时别的用例的 `+1` 会插进你的观测窗口。

- **槽有多个写者时**：用**方向断言** `> before`（它只要求「我的这次计进去了」，对杂散写者免疫）。
  `ecat-data-redis/src/tests.rs:370-371` 是仓内范例（`Cache` 槽有两个写者）。
- **槽只有你一个写者时**：用**严格** `== before + 1` —— 它还能抓住「两份样本同源」这类错误
  （值相等 = 读了同一个槽），方向断言抓不到。
- **找不到自由槽时不要用比较符凑**：ClickHouse 的最终形态是**消除并发写者**
  （`metrics.rs` 的用例不再推进 `Tsdb` 槽，改为只断言样本存在）**然后**上严格断言
  （`e9b54e3`）。**不要用加锁串行化**——那会被 11 份拷贝各带一把，且将来加一条写同一槽的
  用例时 flake 会原样回来。
- ⚠️ **不要用「跑了很多次都绿」当单写者的证明**：调度顺序会让早期写者系统性地落在窗口外
  —— ClickHouse 的对照实验里，`== +1` 在自然调度下 **20/20 绿**，但注入一个「迟到 150ms 的
  写者」后误路由探针 **5/5 假绿**。判断单写者要**枚举写者**（`grep` 全 crate 的
  `BackendKind::<那个槽>`，**含 `--features metrics` 那个编译单元**），不是靠跑。

**(c1) 多方法 crate：每条 I/O 路径都要有自己的一条红灯源。**
单方法 crate 的两个探针（删 `mod resilience;` / 拆 `run_with_timeout`）在多方法 crate 上**只证明「至少一条路径包了」** —— 拆 `run_with_timeout` 会同时打红所有超时用例，却分不出是哪条路径漏包。要证明「每条路径都包」必须**逐条拆方法**（每个被包方法各拆一次，每次应打红**一条不同**的用例）。样例：`ecat-data-elasticsearch/src/tests/resilience.rs` 的 `every_io_method_times_out_when_the_backend_stalls`，它的 `label` 参数就是漏包定位器。

**(c2) 把守测试的空验收是「反向」的。**
「删掉本任务新增物、看它是否还绿」这个口径对**把守测试**（如 `unsupported_ops_do_not_trip_the_breaker`）**不适用** —— 那类测试守的是「trait 默认实现不被我们包上」，本任务**根本没有实现代码可删**。它的红探针只能是**临时加**一个包了 `guarded` 的覆写，看它变红（ES 实测：`left: Open, right: Closed`）。

**(c3) 多方法 crate 的形态（ES 实测，Task 5-8/10 照抄）**：
- **`guarded` 仍只有一份**，不随方法复制；每个 I/O 方法的改写是机械的：**本地构造请求留在壳外**，方法体整体塞进 `self.guarded(async { … }).await` —— 净增只有 2-3 行/方法。
- **`kind` 仍写死**，不要因为方法多就改成参数：同一 trait 的方法同属一个**家族**维度。ClickHouse 收参数是因为它跨**家族**（`SqlExecutor`→Rdbms 与 `TsdbClient`→Tsdb），本批 10 个 HTTP 后端都不是。
- **不包的方法不留代码、只留注释**（说明「默认返回不支持 = 调用方用法错，不进熔断窗口」）；**不要**为了「显式」去覆写一份再包 —— 那正是 (c2) 要抓的错。
- **壳的边界 = 公开方法**：内部**会发 I/O 的 helper 绝不能在外面、也不能再包一层**。ES 恰好是「一方法一次 send」没有内部 helper；而 `ecat-data-tdengine` 的 `exec`（`src/lib.rs:61-76` 就是 POST+解析）、iotdb 的「每点一次 POST」、s3 `list` 的翻页**都长这样**。留在壳外 = 漏包；单独再包一层 = **一次调用 N 个预算**（`whole_call_budget_*` 用例要抓的正是它）。**判据：问「这一步发 HTTP 吗？」—— 发，就在壳内。**
- **reason 是双轨的**：超时用 `kind.slug()`（家族粒度，如 `"search"`），熔断拒绝用**产品名**（如 `"elasticsearch"`）。两条断言会并存在同一个用例里，别「统一」成一种。
- 泛型约束 `F: Future<Output = Result<T, Error>> + Send` 且 `T: 'static`：本批 10 个 crate 的返回类型全是 owned ⇒ 不会撞；将来若出现借用返回会**编译期**红（可见，非静默）。
- 参考实现：`ecat-data-elasticsearch/

**(c0) 探针的作用域要精确到「那一行」**：变异探针（把某行改坏、看测试是否红）**只改被测的那一处**。
本批实测踩到：Task 3 的探针写「把 `metrics.rs` 里的 `timeout_counter(BackendKind::Graph)` 换成别的槽」，
而该串在文件里出现 **3 次**（注册行 + 测试自己的 `fetch_add` + rustdoc）—— 全局替换把**写槽**也改了，
读槽与写槽一起移动 ⇒ **探针静默变绿**，人却以为证过了。**写探针时先 `grep -c` 数一下这个串出现几次**；
多于一次就把探针措辞写成「只改 `<函数名>` 里的那一行」。
**替换类操作（改标签、换 kind）同理：先 `grep -c` 数准再改。** Task 5 正文曾写「标签替换 3 处」，ES 实测是 **8 处**（含 4 条断言字符串与 2 处文档）—— 计数本身就会错；**数出来的和文档写的不一致时先停下**，别按文档的数改。

**(c) 证人槽约束 —— 本批踩过两次的必红陷阱**：若你要断言「计数**落到了正确的槽**」
（例：Tsdb 的超时不得落到别的槽），那个证人槽**必须是本测试二进制内没有任何其它用例
会写的槽**。实测证据：ClickHouse 的同一用例，无锁 + 证人槽 `Rdbms` 时**并行 20 次 17 红**
（红的是证人槽断言，产品行为正确 —— 另两条用例也在写 `Rdbms` 槽）；
`ecat-data/src/timeout.rs` 的同类用例原写法实测 **12/15 误红**。改成无人写的槽
（ClickHouse/ecat-data 都改用 `Storage`）后：**并行 20/20 绿、串行 30/30 绿、
`--features metrics` 并行 20/20 绿**。

```bash
# 加用例前先确认证人槽没有别的写者（含 --features metrics 的编译单元！）
grep -rn 'BackendKind::Storage' <crate>/
grep -rn 'TIMEOUTS\[' <crate>/src          # 直接按下标写的也要数上
```

**不要用加锁串行化来绕**：加锁实测 40 次 0 红，但那是把一把不该有的串行锁写进会被
5b 抄 12 遍的模板里，且将来任何人加一条写同一槽的用例，flake 原样回来（`b2bae3f` 的
裁决记录）。找不到自由槽就换断言口径（方向断言 + 只断言存在性）。

**(d) 外层保险丝**：用例外套一层 `tokio::time::timeout`，把「漏包 ⇒ 挂死」变成红灯
而不是卡住整个测试二进制（`ecat-data-redis/src/tests.rs:345-364` 逐字）：

```rust
/// 一次调用必须在**外层 5 秒内**返回 `DeadlineExceeded`，且推进 Cache 槽。
///
/// 外层 5 秒是**把挂死变成红灯**：漏包 `guarded` 的后果不是报错而是永远不返回
/// （假服务端对数据命令装死，配置的超时是唯一能结束它的东西），
/// 没有这层的话整个测试二进制会卡住而不是 FAILED。
///
/// **泛型 `T`（5b Task 1 起；与计划 T0-F 口径一致）**：5b 各后端被测方法的返回值
/// 多元（`Value` / `Vec<u8>` / `Vec<Row>` / `u64` / `()`），调用点直接透传；
/// `T = ()` 的老调用照样编译 —— 这是 redis 先例（调用点写 `.map(|_| ())`，
/// `ecat-data-redis/src/tests.rs:381`）的推广。写死 `Output = Result<(), Error>`
/// 与 arangodb 的 `execute`（`Value`）冲突，Task 1 实测 E0271。
/// `T: Debug` 是 `unwrap_err()` 的需要。（redis 现役代码是 `T = ()` 的形态，
/// 仍然兼容；它的 `.map(|_| ())` 调用点不必改。）
async fn assert_times_out<T, F>(label: &str, fut: F) -> Error
where
    T: std::fmt::Debug,
    F: std::future::Future<Output = Result<T, Error>>,
{
    let before = timeout_counter(BackendKind::Cache).load(Ordering::SeqCst);
    let err = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("{label}: 内层超时没开火（漏包 guarded？）"))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "{label}: {err}");
    assert!(
        timeout_counter(BackendKind::Cache).load(Ordering::SeqCst) > before,
        "{label}: 超时必须计入 Cache 维度"
    );
    err
}
```

**(e) 每个出站方法都要覆盖**：只测一个方法的话，谁漏包另一个（比如 `set` 直接直连），
现有测试不会红。模板：`ecat-data-redis/src/tests.rs:366-398`
（`every_cache_method_times_out_when_the_backend_stalls`，五个方法逐条打）。
注意 Redis 那条的设计细节：五条调用正好打满熔断窗口下限（5 条失败 ⇒ 第 5 条之后才
`Open`），所以五条都还能落到后端拿到 `DeadlineExceeded`；再多一条就是 `Unavailable`。

**验收**：

```bash
cargo test -p <crate> <你的超时用例名>            # 期望绿
cargo test -p <crate> <你的超时用例名> -- --test-threads=1   # 串行也绿
```

再做过一次红探针（见 §2 验收），两个配置（默认 / `--features metrics`）各跑一遍。

---

### 模式 ④（**并发上限类后端必须做**）：超时后并发许可必须归还

若你的后端有并发上限（信号量 / 连接池），**必须**补一条：**第一次调用超时后，许可要能立刻被第二次调用拿到**。

为什么单列：超时时 `run_with_timeout` 是**返回 Err**（不是取消 future），许可随 future 的 drop 释放 —— 这条看着显然，
但**仓内原本没有任何用例覆盖它**（Task 5 复核在副本里加临时用例才验出来：`max_concurrency=1`、
第一次 1s 超时打 3s mock、第二次必须能拿到许可 → 正常 1.00s 绿；注入 `std::mem::forget(permit)` 后 3.01s 红）。

```rust
// 上限 1：第一次超时（1s 预算 vs 3s mock），第二次必须仍能拿到许可并正常完成。
let c = client_at(&url, 1, Some(1));
let _ = ecat_data::SqlExecutor::query(&c, "SELECT 1").await;  // 超时；许可应随 future drop 归还
let t = std::time::Instant::now();
let ok = ecat_data::SqlExecutor::query(&c, "SELECT 1").await; // 这次用短 mock，不再超时
assert!(ok.is_ok() && t.elapsed() < std::time::Duration::from_secs(2),
        "超时后许可没归还 —— 后续调用会永远排队");
```

**探针自证**：把 `permit` 换成 `std::mem::forget(...)`（或让它逃逸出函数作用域）→ 第二次调用必须**拿不到许可**（挂到超时）。

---
## 6. 加一条熔断测试

**两件事都要断言**：`state() == Open` **且** 下一次调用**快速失败**。只断言 `is_err()`
是空验收（后端本来就报错也是 `is_err()`）。

用例模板（`ecat-data-redis/src/tests.rs:289-311` 逐字）：

```rust
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
```

**判据要选准**（Task 4 复核用 A/B 探针实测的结论）：

- **严格判据 = 错误语义**：`Error` 路径断言 `err.code == ErrorCode::Unavailable`；
  `RdbmsError` 路径断言 `matches!(err, RdbmsError::Connection(_))` **且**
  `err.message == "circuit breaker is open"`（`RdbmsError` 没有 code 字段，
  等价的严格判据就是变体 + 消息）。这类错误**只能由熔断器在调内层之前拒绝产生** ——
  本 crate 内没有其它生产者，所以它**严格强于墙钟**。红探针实测（`2bbe1af`）：
  让 `guarded_tsdb` 绕过 breaker ⇒ 红（拿到 `DeadlineExceeded` 而非 `Unavailable`）；
  改用独立 `Breaker::new`（模拟双 Breaker 重构）⇒ 同样红。
- **墙钟断言是冗余佐证**：把它删掉测试仍绿。可留作直观信号，**但别当唯一依据** ——
  它是全套里唯一对调度抖动敏感的断言。
- 消息相等断言只在**状态确实是 `Open`** 时成立；冷却期（默认 10 秒）过后会经
  `HalfOpen` 给出 `ProbesExhausted`（消息是 `"circuit breaker: too many probes"`）。
  用例在打满 5 次失败后**立刻**调用，所以安全 —— 别在中间插 sleep。
- 前置断言（`state()` + `opened_total()`）要留下：失败时能把「熔断没打开」与
  「错误码不对」分开报。
- 5 次失败足以打开：窗口下限是 5 条样本且失败率 ≥ 阈值（默认 0.5），
  `ecat-circuit-breaker/src/breaker.rs:194-207`。
- **两条 I/O 路径共用一个熔断器**的后端，补一条跨路径用例
  （`ecat-data-clickhouse/src/tests/resilience.rs:114-145`：Rdbms 路径打到 `Open` 后
  Tsdb 路径必须同步被拒 —— 判据同样用错误语义为准）。
- 被测方法连打 5 次时，若接口本身可能提前触发别的失败（如连接错误），
  断言只看熔断器的 `state()`/`opened_total()`，不要互相污染。

**验收**：同 §5（默认 + `--features metrics`、串行各跑一遍），外加两条红探针
（绕过 breaker / 独立 breaker）。

---

## 7. 已知陷阱

1. **`0` 是禁用不是零超时。** `query_timeout_secs: 0` ⇒ `None`（透传，不套 `timeout`）。
   `Some(Duration::ZERO)` 才是「立刻超时」（`ecat-data/src/timeout.rs:120-131` 写明了
   本仓别处的惯例在这里不适用）。禁用超时下**不要**拿卡死的假后端做用例 —— 那会永久挂住。
2. **默认实现的方法不要包。** 落到 trait 默认实现的「不支持」是**调用方的用法错**，
   不是后端故障；包进 `guarded` 后 5 次「不支持」就会打开熔断器，之后正常查询全被拒。
   ClickHouse 的 `_with` 就是这种（出入 7），把守测试：
   `unsupported_with_methods_do_not_trip_the_breaker`（`ecat-data-clickhouse/src/tests/resilience.rs:153-167`）。
3. **常量错误返回的方法不要包。** `transaction()` 不含 I/O，包了会让「本就不支持的调用」
   被记成后端失败（出入 6）。把守测试：`transaction_error_does_not_trip_the_breaker`（同文件 `:171-179`）。
4. **纯本地分支不要包。** 没发 I/O 就返回的路径（`multi_get` 空 keys、`dialect()`）
   走 `guarded` 会借走半开探测名额再以 `Ok` 记账 —— 熔断器可能被**误关回 `Closed`**，
   新窗口流量全部冲向根本没被碰过的后端（`ecat-data-redis/src/lib.rs:146-157`）。
5. **`from_config` 已有的内层超时要写进 rustdoc。** `ecat-tls::build_reqwest_client`
   建的 client 自带 5 秒连接 + 30 秒总超时（`ecat-tls/src/lib.rs:83-84,97-98`），
   `from_config` 走它、`new`/`with_auth` 用裸 `reqwest::Client::new()`（没有内层）。
   两层共存时**谁先到谁生效**；内层那次的错误是 `RdbmsError::Database`、
   **不计入** `ecat_outbound_timeouts_total`。测试要让两层可区分（外层 1 秒 + mock 拖 5 秒）。
6. **`--features metrics` 才能编译到指标代码。** 默认 `cargo test -p <crate>` 碰不到
   `metrics.rs`，改坏了也是绿的（出入 3 的 sqlx/mssql 教训：那两处消费者只在 feature 下编译）。
   每个 crate 的闸门都要带 feature 跑一次。
7. **新建 `.rs` 必须在父模块加 `mod`；且别把「加了 `mod` 未建文件」的中间态留到提交那一刻**
   —— 别人此刻跑 `cargo fmt --all` 会报 `failed to resolve mod`（污染别人的工作区）。
   测试文件同理：`ecat-data-clickhouse/src/tests.rs:8` 的 `mod resilience;`。
8. **假服务端/假连接的解析器要写清能力边界。** Redis 的 `drain_commands`
   （`ecat-data-redis/src/tests.rs:208-262`）靠扫 buffer 里**首个 `*`** 切命令 ——
   遇到**含 `*` 字节的二进制 bulk value 会错位**。当前输入固定所以无影响；
   5b 抄它时：要么跳过 bulk 内容、要么在注释里写明限制 —— 别让一个只在特定 payload
   下失效的解析器看起来是通用的。握手必须应答（否则测到的是建连超时）。
9. **别写 `{"enabled": false}`。** `BreakerConfig` **没有** `enabled` 字段
   （`ecat-circuit-breaker/src/breaker.rs:36-46` 只有 `failure_ratio` / `window` /
   `half_open_probes` / `open_duration`）。写不存在的字段比不写更糟：反序列化错误，
   或被 `#[serde(default)]` 静默吞掉后以为关掉了。关不了就是关不了，如实写。
10. **指标 collector 全进程只有一份**（出入 11）：本 crate 只挂数据源，**不要**再建
    collector；`backend` 标签用产品名（spec §4 写的是类别名，照抄永远匹配不到）。
11. **注册函数的标签是 `&'static str`**；同二进制内测试的 `backend` 标签要**用例互异**
    （registry 是进程级的）。
12. **500 行硬上限对 `lib.rs` 与测试文件都适用** —— 超了就拆（先例：
    `ecat-data-clickhouse/src/tsdb.rs`、`ecat-data-clickhouse/src/tests/resilience.rs`）。

---

## 8. 验收命令

**一律作用域化到本 crate**（`-p <crate>`）。⚠️ **不要用 `--all`**：多 agent 并行时
`cargo fmt --all` 会排版别人正在飞的半成品、`clippy --workspace` 会把别人的编译错误
算到你头上。只有 5b 的**最终**闸门才用 `--all`，且那一刻先确认没有别人在同一工作区。

```bash
# 1) 默认特性
cargo test -p <crate>

# 2) 指标 feature —— 指标代码只在它下面编译
cargo test -p <crate> --features metrics

# 3) 格式（只查本 crate）
cargo fmt -p <crate> -- --check; echo "fmt rc=$?"

# 4) clippy（指标代码也要进编译）
cargo clippy -p <crate> --all-targets --features metrics -- -D warnings; echo "clippy rc=$?"

# 5) 行数复核（500 行硬上限）
find <crate>/src -name '*.rs' -exec awk 'END{if(NR>500) print FILENAME": "NR}' {} \;

# 6) feature 门控自证：默认构建里出现指标测试名 ⇒ 写漏了 #[cfg(feature = "metrics")]
cargo test -p <crate> 2>&1 | grep -c 'outbound_metrics'     # 期望 0
```

**期望输出**：

- 1/2 的 `test result:` 行都要 `ok.`、0 failed。**先记基线数再记新数**（新增用例后
  数量必须增加；没增加就是 `mod` 没生效或者写进了 feature 门控之外）。
  5a 的参照：`ecat-data-redis` **18 → 19**（默认 / metrics）、`ecat-data-clickhouse`
  **31 → 32**（含拆出的 `tests/resilience.rs`）；`ecat-data` 与 `ecat-metrics` 各自的
  用例也全绿。
- 3/4 的 `rc=0`（`echo` 出 rc 是为了在一眼扫过的输出里保留退出码 —— CI 吞掉中间命令
  的 rc 是常见事故）。
- 5 无输出（没有超过 500 行的文件）。
- 6 输出 `0`。**若第 6 条不是 0**，说明指标代码写在了 feature 门控之外 —— 空验收的变体。

每次提交信息里带上：两个配置的用例数、§2/§5/§6 的红探针输出、与计划片段的任何偏离
及其实测证据（5a 的每个 crate 提交都是这个格式，照抄）。
