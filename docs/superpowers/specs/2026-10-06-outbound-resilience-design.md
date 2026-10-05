# 出站数据调用的超时、熔断与指标 — 让 14 个后端具备 RDBMS 已有的韧性

日期：2026-10-06
状态：**设计中**（用户已确认方向，待审 spec；实施排在批次 1 之后）

## 背景

批次 1 给 RDBMS 加了三件事：查询超时（`run_with_timeout`）、事务泄漏计数、
熔断/读写分离包装器（批次 4）。但**这些只覆盖 16 个数据后端里的 2 个**。

实测（2026-10-06）当前各后端的连接与会话管理方式：

| 后端 | 现状 | 缺什么 |
|---|---|---|
| RDBMS（SQLite/PG/MySQL/TiDB） | ✅ 原生池 + 超时 + 计数 | — |
| SQL Server | 🔄 批次 2（deadpool） | — |
| MongoDB | 官方驱动**自带连接池**（默认 `max_pool_size=100`） | 池配置未暴露；无超时/熔断 |
| Redis | **不是池**：单个 `MultiplexedConnection` 多路复用 | 无超时/熔断；且有状态命令序列无处可去 |
| S3 / ClickHouse / OpenSearch / Elasticsearch / Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine（**11 个**） | 每客户端一个 `reqwest::Client`，**仅靠 keep-alive 复用 TCP** | 无超时/熔断；**并发无上限** |
| Memcached | 进程内 `HashMap`，不连服务器 | 不适用 |

**两个已验证的具体缺口**：

1. **`reqwest` 没有「最大总连接数」旋钮。** 实测 reqwest 0.12.28 默认值
   （`async_impl/client.rs:301-302`）：
   ```rust
   pool_idle_timeout: Some(Duration::from_secs(90)),
   pool_max_idle_per_host: usize::MAX,
   ```
   `pool_max_idle_per_host` 限的是**空闲保留数**，默认无上限。也就是说 1000 个并发请求
   会开 1000 条 TCP —— 有复用、**没有背压**。11 个后端全部用
   `reqwest::Client::new()` 默认值，一个都没配过。

2. **`ecat-middleware` 的熔断/超时是 tower `Layer`**，作用于**入站 HTTP**。
   出站到数据后端的调用**完全不经过它们** —— 这是个容易误判的地方：
   看配置以为有熔断，实际不在那条路径上。

**现状的后果**：一个卡死的 Redis GET 或 ES 查询会无限期挂着；后端整体故障时
所有请求继续排队打过去（雪崩）；且**没有任何指标**能看出是哪个后端在出事。

## 决策（用户 2026-10-06 确认）

| 项 | 决策 |
|---|---|
| 落地形态 | **改各后端 impl，自动生效**（非 opt-in 包装器）—— 用户代码零变化 |
| 与 sqlx 一致性 | 超时沿用 `run_with_timeout` 的**方法内调用**模式（已在该 crate 验证过） |
| 排序 | 独立一期（批次 5），本 spec 先出，实施排在批次 1 验收之后 |
| 范围 | 14 个后端 × 3 项（超时 / 熔断 / 指标），外加 HTTP 类的**并发上限** |

## 关键使能事实

**六个非 RDBMS trait 用的是同一个错误类型** —— `Error`（`ecat-data` 再导出的
`ecat_errors::Error`）。已核实：`Cache` / `SearchClient` / `GraphClient` /
`DocumentClient` / `StorageClient` / `TsdbClient` 的所有方法都返回 `Result<_, Error>`。

这让**一个泛型超时助手覆盖全部六个 trait**，不需要六套类型。

`ecat_errors::Error` 是**结构体**（非 enum）：`Error::new(code, reason, message)`，
`ErrorCode` 来自 `ecat_protos::errors::ErrorCode`，其中已有
**`DeadlineExceeded = 1009`** 可直接用。

## 1. 泛型化超时助手（`ecat-data/src/timeout.rs`）

```rust
/// 可被超时包装的错误类型。
pub trait TimeoutError: Sized {
    fn from_timeout(d: Duration) -> Self;
}

impl TimeoutError for RdbmsError {
    fn from_timeout(d: Duration) -> Self {
        RdbmsError::Timeout(format!("query exceeded {d:?}"))
    }
}

impl TimeoutError for ecat_errors::Error {
    fn from_timeout(d: Duration) -> Self {
        ecat_errors::Error::new(
            ecat_protos::errors::ErrorCode::DeadlineExceeded,
            "timeout",
            format!("call exceeded {d:?}"),
        )
    }
}
```

`run_with_timeout` 增加类型参数与后端维度：

```rust
/// 出站调用的后端类别。用于把超时/熔断计数分维度，否则只有一个总数看不出谁在出事。
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

pub static TIMEOUTS: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];

pub async fn run_with_timeout<F, T, E>(
    kind: BackendKind,
    timeout: Option<Duration>,
    fut: F,
) -> Result<T, E>
where
    F: Future<Output = Result<T, E>>,
    E: TimeoutError,
{
    match timeout {
        None => fut.await,
        Some(d) => match tokio::time::timeout(d, fut).await {
            Ok(r) => r,
            Err(_) => {
                TIMEOUTS[kind as usize].fetch_add(1, Ordering::Relaxed);
                Err(E::from_timeout(d))
            }
        },
    }
}
```

**破坏性**：`run_with_timeout` 的签名变了（多两个参数）。仓库内调用点是 batch 1 新加的
（`ecat-data-sqlx` 的 4 处 + 事务 wrapper），改动是机械的。外部实现者若调用过它也会
编译失败 —— 与批次 1 的 4.0.0 一同发布即可（`ecat-data` 尚未进入 4.0.0 发布窗口）。

## 2. 每个后端接入超时

以 Redis 为例（其余同构）：

```rust
pub struct RedisClient {
    conn: MultiplexedConnection,
    query_timeout: Option<Duration>,
}

#[async_trait]
impl Cache for RedisClient {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        run_with_timeout(BackendKind::Cache, self.query_timeout, async {
            // 原实现体，不改逻辑
        })
        .await
    }
    // set / delete / increment / ttl 同构
}
```

配置各后端 `XxxConfig` 增加：

```rust
/// 单次调用超时秒数。`0` = 禁用。
#[serde(default)]
pub query_timeout_secs: Option<u64>,   // 默认 30
```

**HTTP 类后端同时新增并发上限**（补上 reqwest 缺失的那个旋钮）：

```rust
/// 并发上限。reqwest 只有 pool_max_idle_per_host（空闲保留数），
/// 没有「最大总连接数」——默认 usize::MAX 意味着并发无背压。
#[serde(default)]
pub max_concurrency: Option<usize>,    // 默认 32
```

实现：客户端持一个 `tokio::sync::Semaphore`，每次调用先 `acquire()`；
信号量在超时释放（`SemaphorePermit` 随 future drop 释放，无需手工归还）。

## 3. 熔断

每个后端 client 增加 `breaker: Breaker` 字段（来自 `ecat-circuit-breaker`），
方法内包一层：

```rust
async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Error> {
    run_with_timeout(BackendKind::Cache, self.query_timeout, async {
        self.breaker
            .call(|| async { /* 原实现 */ })
            .await
            .map_err(|e| e.into_backend_error(/* reason */))
    })
    .await
}
```

超时与熔断的**嵌套顺序**：超时在外、熔断在内。理由：熔断器只应记录「后端答不答」，
而超时是我们主动放弃 —— 若熔断在内，被超时掐断的调用仍会被记为一次待定/失败，
语义混乱。

配置：

```rust
/// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30s、打开 10s）。
/// 设为 `{"enabled": false}` 可关闭。
#[serde(default)]
pub breaker: Option<BreakerConfig>,
```

**默认开启**（用户选择「自动生效」）。保守阈值下只在**持续失败**时打开，
不改变正常路径行为；需要关闭时显式配 `enabled: false`。

## 4. 指标（最小集）

`ecat-data` 提供按维度计数的静态量，各后端的 `metrics` feature 注册为 Prometheus 指标：

| 指标 | 类型 | 维度 |
|---|---|---|
| `ecat_outbound_timeouts_total` | counter | `backend`（rdbms/cache/search/graph/document/storage/tsdb） |
| `ecat_outbound_breaker_open_total` | counter | `backend` |
| `ecat_outbound_breaker_state` | gauge | `backend`（0=closed 1=open 2=half-open） |

**不做**（留给后续独立一期）：延迟直方图、按具体实例（而非后端类别）的维度。
理由：直方图需要引入分桶配置，是独立的设计决策；先补齐「能看到哪个后端在出事」。

## 5. MongoDB 的池配置暴露

`mongodb::Client` 自带池，但我们没暴露配置。补：

```rust
#[serde(default)] pub max_pool_size: Option<u32>,   // 驱动默认 100
#[serde(default)] pub min_pool_size: Option<u32>,   // 驱动默认 0
```

通过 `ClientOptions::max_pool_size()` 传给驱动，不自己实现池。

## 6. 非目标

- **不给 Redis 加连接池**。`MultiplexedConnection` 对缓存负载是**更优解**（一条 TCP
  服务所有并发，开销低于池）；换成池反而增加连接数与往返。但要在文档里写明它的
  **能力边界**：`MULTI/EXEC`、`WATCH`、`SUBSCRIBE` 等有状态命令序列不能用多路复用连接 ——
  需要时另开专用连接（`get_async_connection()`）。这条目前**完全没有文档**。
- 不改 `ecat-middleware` 的 tower 层（那是入站路径，本就该分开）。
- 不给 Memcached 加（它是内存实现，不连服务器）。
- 不做重试（超时+熔断已能防雪崩；重试需要幂等性保证，是独立决策）。

## 7. 影响面

| crate | 改动 |
|---|---|
| `ecat-data` | `timeout.rs`：`TimeoutError` trait、`BackendKind`、`TIMEOUTS`、泛型 `run_with_timeout` |
| `ecat-data-sqlx` | 4 处调用点适配新签名（机械） |
| `ecat-data-redis` | 超时 + 熔断 + 指标 feature + 文档写明多路复用的能力边界 |
| `ecat-data-mongodb` | 池配置暴露 + 超时 + 熔断 + 指标 |
| 11 个 HTTP 后端 | 超时 + 熔断 + **并发上限** + 指标（逐 crate 同构） |
| `ecat-data-clickhouse` / `ecat-data-questdb` | 它们同时实现 `RdbmsClient`，两条路径都要包 |
| 文档 | `database-config-tutorial.md` ×13 补三个新配置项；README 后端表补「超时/熔断」列 |

**前置依赖**：批次 4 的公开 `Breaker`（`ecat-circuit-breaker` 抽取）。
若希望更早实施，把那次抽取提前即可 —— 它本身是独立小改动。

## 7.5 实施分两阶段（范围决策）

**不一次铺 14 个后端。** 先验证模式，再铺开：

| 阶段 | 范围 | 为什么 |
|---|---|---|
| **5a** | `ecat-data` 泛型助手 + **Redis** + **ClickHouse** | Redis 验证「非 HTTP 客户端」路径；ClickHouse 是本设计里**最难**的 case —— 它同时实现 `RdbmsClient` 与 `TsdbClient`，两条路径都要包，且其 `_with` 参数化方法本就不支持（落到默认错误），超时/熔断要在这个前提下仍然语义正确 |
| **5b** | 其余 10 个 HTTP 后端 + MongoDB（池配置） | 模式已验证，纯同构铺开 |

**理由**：本会话在「结构相同的机械改动」上吃过教训 —— 批次 1 的原生池重写被判定为机械同构，
实际在真 MySQL/PG 上暴露了 2 个 Critical（`UNSIGNED` 主键、`smallint`）。11 个 HTTP 后端
结构相同，但**一个模式错误会被复制 11 份**。用 5a 两个 crate 换这个保险是划算的。

**5a 的交付物必须包含**：一份「接入 checklist」——把「加配置字段、包 `run_with_timeout`、
加 `Breaker` 字段、注册指标、加一条超时测试、加一条熔断测试」写成逐步清单，
供 5b 逐 crate 照做，避免 10 次即兴发挥。

## 8. 验收标准

1. `cargo test --workspace` 全绿，测试数不下降
2. 每个后端至少一条**超时真的生效**的测试（用一个永不返回的假 inner，断言在
   `query_timeout` 后返回 `DeadlineExceeded`）
3. 每个后端至少一条**熔断真的打开**的测试（连续失败后快速失败）
4. `TIMEOUTS` 的分维度计数可被断言（不同 `BackendKind` 互不串）
5. HTTP 后端的并发上限可验证（起 N+1 个并发，断言同时在飞的不超过 N）
6. `reqwest::Client` 的构造带上了并发上限 —— 但注意 reqwest 无此旋钮，
   上限由我们自己的信号量实现，需在文档里写明这层
7. 既有用户代码零改动即可获得全部能力（不新增必需配置项）

## 9. 风险

| 风险 | 缓解 |
|---|---|
| 14 个 crate 同构改动，机械重复易漏 | 抽一个 `ecat-data` 侧的宏或文档化的 checklist；逐个 crate 一条超时测试兜底 |
| 默认开启熔断可能改变现有行为 | 保守阈值（0.5 / 30s / 10s）；只在持续失败时打开；提供 `enabled: false` |
| `run_with_timeout` 签名变更影响外部调用者 | 与批次 1 的 4.0.0 同窗口发布 |
| 并发上限引入新的排队延迟 | 默认 32 足够宽；可通过配置调；超时会释放信号量 |
| Redis 多路复用的边界未文档化导致误用 | 本 spec 明确要求补进文档与 rustdoc |
