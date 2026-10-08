# 批次 5b 实施计划：其余 10 个 HTTP 后端 + MongoDB 的池配置暴露

> 输入：spec `docs/superpowers/specs/2026-10-06-outbound-resilience-design.md`、
> checklist `docs/superpowers/checklists/backend-resilience-onboarding.md`（**738 行，逐 crate 照做**）、
> 5a 计划 `docs/superpowers/plans/2026-10-07-batch5a-outbound-resilience.md`（Redis / ClickHouse 两个试点已落地）。
>
> 本计划**只写计划，不写实现**。所有代码片段都来自对当前源码的实测抄录，但**片段仍可能跑不通** ——
> 遇到跑不通或与验收冲突时，正确做法是「实测复现 → 停下报 lead → 经确认后偏离并把偏离与证据写进提交信息」
> （5a 的教训，见其「落码期发现的计划片段错误」）。

## 0. 执行者须知（先读这 6 条）

1. **所有 cargo 命令都带 `export CARGO_TARGET_DIR=/var/tmp/ecat-target`** —— `/home`（sda1）写入会停摆，
   不带这行的 cargo 会卡在写 target 上。本计划的命令块**每条都带**，照抄即可。
2. **提交一律 `git commit --only <精确路径>`**（不带 `--only` 会把别人 staged 在共享索引里的文件一起带走）。
   消息用中文、**不写 `Co-Authored-By`**。撞 `.index.lock` 时**不要删锁**，等 15 秒重试。
3. **任务级闸门只作用域化到本 crate**：`cargo fmt -p <crate>`、`cargo clippy -p <crate>`。
   **绝不用 `--all` / `--workspace`** —— 多 agent 并行时会排版/归责到别人正在飞的半成品。全仓闸门只在 Task 13。
4. **单一文件 < 500 行**（`lib.rs` 与测试文件都算）。本批有 4 个 crate 必须先拆测试文件（见 §3 差异表）。
5. **假绿防御**：新建 `.rs` 必须在父模块加 `mod`（否则静默不编译）；每条任务的验收都要**核对测试数增加**
   （数字对不上就是 `mod` 没生效 / 写在了 feature 门控之外）。
6. **空验收防御**：每个验收自问「删掉本任务的新增物，这条验收还绿吗？」。绿 ⇒ 它不是验收，删掉或改严。

---

## 1. 与 spec / checklist 的实测出入（逐条附证据）

> 这些是**写作时实测**的结论。执行时若再次实测与这里不符，以实测为准并报 lead。

**出入 1：范围是 11 个后端，不是 12 个。** `ecat-data-memcached` 是**进程内实现**
（没有 reqwest / 网络依赖，见 `ecat-data-memcached/Cargo.toml` 与 `src/lib.rs`），没有出站调用可包。
spec §7.5 的枚举若含它，那部分不成立。5b = **10 个 HTTP 后端 + MongoDB**。

**出入 2：QuestDB 只有一条 I/O 路径。** spec §7 的影响表把 `ecat-data-questdb` 与 `ecat-data-clickhouse`
并列（都在 spec §7.5 里），实测 QuestDB **只实现 `SqlExecutor` + `RdbmsClient`**
（`ecat-data-questdb/src/lib.rs:70-159`），**没有 `TsdbClient` 实现** ⇒ 不需要 kind 参数、不需要孪生的
`guarded_tsdb`。ClickHouse 的「两条路径」是它的特殊形状，不要套到 QuestDB 上。

**出入 3：指标标签用产品名，不是类别名。** spec §4 写的是类别名（`rdbms`/`cache`/…），
checklist §4 `:383-386` 实测约定是**产品名**。本批 11 个标签一律取**配置节名**（`arangodb`、`neo4j`、
`nebulagraph`、`elasticsearch`、`opensearch`、`influxdb`、`iotdb`、`tdengine`、`questdb`、`s3`、`mongodb`）。
照 spec 原文写 `backend="search"` 的告警**永远匹配不到样本**。

**出入 4：顺序是「许可 → 熔断 → 超时」，与 spec §3 相反。** 5a「出入 4」已裁决（超时在外会让熔断器对卡死
的后端永久失明），checklist §2 `:115-131` 固化。5b 一律照做。

**出入 5：MongoDB 的驱动默认池大小是 10，不是 spec §5 说的 100。** 实测 mongodb 3.8.0：
`src/cmap.rs:50` `pub(crate) const DEFAULT_MAX_POOL_SIZE: u32 = 10;`，`src/cmap/worker.rs:171`
`.unwrap_or(DEFAULT_MAX_POOL_SIZE)`，`src/client/options.rs:577-582` 的 rustdoc 也写 "The default value is 10"。
（100 是 Node.js 驱动的默认值。）本批**不硬编码默认**：配置省略 ⇒ `None` ⇒ 驱动默认。文档里也要写 10，别抄 spec 的 100。

**出入 6：`ClientOptions` 的传法 spec 没说清，实测是「parse → 字段赋值 → `with_options`」。**
mongodb 3.8.0 的 `ClientOptions::parse` **不是 `async fn`**（`src/action/client_options.rs:68-79` 返回可 await 的
action builder，`Client::with_uri_str` 也是 `.await` 它，见 `src/client.rs:181-185`）；`max_pool_size` /
`min_pool_size` 是**公开字段**（`src/client/options.rs:582,589`）——`#[non_exhaustive]` 只挡结构体字面量，
**不挡对已有值的字段赋值**。所以：

```rust
let mut options = mongodb::options::ClientOptions::parse(&cfg.url).await?;
options.max_pool_size = cfg.max_pool_size;   // 新字段
options.min_pool_size = cfg.min_pool_size;   // 新字段
let client = mongodb::Client::with_options(options)?;
```

**出入 7：`ecat-data-mongodb` 的 `tls` 字段是死字段。** `MongoConfig.tls`
（`ecat-data-mongodb/src/lib.rs:15-16`）从未被使用（原 `from_config` 只把 `url` 交给 `with_uri_str`）。
本批**不修**（spec 未要求；mongodb 3.x 的 TLS 走 URI 选项或 `Tls` 选项结构，改动面超出 5b）。
**报 lead 裁决**：要么在 5b 顺手接上，要么单开一条技术债。

**出入 8：`max_concurrency` 只给 HTTP 后端。** checklist §1 `:48-57`。实测 10 个 HTTP 后端的
`from_config` 都跑在 `ecat_tls::build_reqwest_client` 上（各自一行）；9 个 crate 还有裸
`reqwest::Client::new()` 的 `new` / `with_auth`（没有内层超时），**S3 只有 `from_config`**（无裸构造器）。

**出入 9：教程与 example 有既存欠账（本条 2026-10-08 修正过，别照抄早前描述）。**
实测 `docs/database-config-tutorial.md:228-290`：5a **已经**更新了 Redis / ClickHouse 两节
（yaml 块三个新字段 + 四列表格三行 + 「两层超时」「熔断默认开启」两段）。真正缺的是：
**其余 8 节**（QuestDB / Elasticsearch / OpenSearch / InfluxDB / Neo4j / NebulaGraph / ArangoDB / IoTDB）
的新字段，以及 **TDengine / MongoDB / S3 三节整节**（`:113-419` 里没有这三节）。
`config/databases.example.yaml` 则**连 5a 都没动**（`:35-50` 的 `redis:` / `clickhouse:` 仍是老字段，
且没有 tdengine / mongodb / s3 三节，头部还写着 `v2.4.2`）。5b 一并补（Task 12）。

**出入 10：README 的「超时/熔断」列已过期。** `README.md:136-157`：Redis 行还是 `—`（5a 后应更新）、
ClickHouse / QuestDB 行写「✅ 熔断」（5b 后是「✅ 超时 + 熔断」）、脚注 `:159` 还写「目前仅 sqlx / mssql 可配」。
**14 份手写镜像**（root + `README.en.md` + 12 个 i18n），别只改 root。

**出入 11：11 个目标 crate 的测试是内联的，不是 5a 的 `src/tests.rs`。**
实测每个 crate 都是 `#[cfg(test)] mod tests { … }` 内联在 `lib.rs` 里（如 `ecat-data-arangodb/src/lib.rs:103`）。
加上本批约 80~90 行生产代码后，`influxdb`(426→~511)、`iotdb`(427→~512)、`s3`(419→~504)、`tdengine`(408→~493)
会顶破或贴近 500 行硬上限 ⇒ 这 4 个 crate **先把内联测试搬到 `src/tests.rs`**（Task 6/7/8/10 的 Step 3）。

**出入 12：S3 的错误类型就是 `ecat_errors::Error`。** `ecat-data-s3/src/lib.rs:22`
`use ecat_errors::{Error as StorageError, ErrorCode};` —— `StorageError` 只是别名 ⇒ 走**变体 A**
（`breaker_error_to_backend_error(e, "s3")`），不需要新的映射函数、不需要动 `ecat-data`
（`TimeoutError` 已为 `Error` 实现：`ecat-data/src/timeout.rs:108-118`）。

**出入 13：`iotdb` / `tdengine` 的公开方法内部会发多次 HTTP。**
`ecat-data-iotdb/src/lib.rs:52-122` 的 `write` **每个点一个 POST**；
`ecat-data-tdengine/src/lib.rs:157,161-171` 的 `write` 按 `BATCH_SIZE = 100` 分批、每批一次 `exec`。
包装**必须落在公开方法上**（一次调用一个预算），**不许**包到内部 `exec` / 循环里 ——
否则一次 `write` 会变成 N 个独立预算（有专项测试，见 Task 7/8 的 `whole_call_budget_*`）。

**出入 14（不是范围问题，是纪律问题）：** 5a 遗留的 `ecat-data-sqlx` / `ecat-data-mssql` 里
「声明了未使用的 `prometheus` optional 依赖」已在 CHANGELOG 里如实登记，**5b 不要顺手修它**。

**出入 15：README 的 QuestDB 行写着「✅ 熔断」，但 crate 里 0 处 `Breaker`。**
`grep -rn 'Breaker' ecat-data-questdb/` → **空**；把 13 个数据后端逐个计数，只有
`ecat-data-redis`(17 处) / `ecat-data-clickhouse`(15 处) 有，**11 个目标 crate 全是 0**。
⇒ **在 5b 落地之前，那一行是假的**（表格领先于代码）—— 读到它的人不要当成现状引用，
QuestDB 的熔断要等 Task 9 合并才成立。
处理方向已裁决：**让代码追上表格**（不是把表格改小、更不是把那行删掉）。Task 12 的尾列
✅ 计数验收（7 → 18，14 份镜像逐份查）同时管两件事：表格比代码先行、以及漏改某个镜像。

---

## 2. 实测后端清单（crate + trait + 是否 HTTP）

| # | crate | 实现的 I/O trait | HTTP？ | 出站方法数 | kind | 指标标签 |
|---|-------|-----------------|--------|-----------|------|---------|
| 1 | `ecat-data-arangodb` | `GraphClient` | 是（reqwest） | 1（execute） | Graph | arangodb |
| 2 | `ecat-data-neo4j` | `GraphClient` | 是 | 1 | Graph | neo4j |
| 3 | `ecat-data-nebulagraph` | `GraphClient` | 是 | 1 | Graph | nebulagraph |
| 4 | `ecat-data-elasticsearch` | `SearchClient` | 是 | 3（index/search/delete） | Search | elasticsearch |
| 5 | `ecat-data-opensearch` | `SearchClient` | 是 | 3 | Search | opensearch |
| 6 | `ecat-data-influxdb` | `TsdbClient` | 是 | 2（write/query） | Tsdb | influxdb |
| 7 | `ecat-data-iotdb` | `TsdbClient` | 是 | 2 | Tsdb | iotdb |
| 8 | `ecat-data-tdengine` | `TsdbClient` | 是 | 2 | Tsdb | tdengine |
| 9 | `ecat-data-questdb` | `SqlExecutor` + `RdbmsClient` | 是 | 2（execute/query） | Rdbms | questdb |
| 10 | `ecat-data-s3` | `StorageClient` | 是 | 4（put/get/delete/list） | Storage | s3 |
| 11 | `ecat-data-mongodb` | `DocumentClient` | **否**（原生驱动） | 4（insert/find/update/delete） | Document | mongodb |

**为何 MongoDB 单列**（spec §5）：它没有 reqwest，所以**没有信号量**、也**没有 `max_concurrency` 字段** ——
它的并发背压交给**驱动自己的连接池**，暴露的是 `max_pool_size` / `min_pool_size`。
因此模式 ④（许可归还）对它**不适用**（无许可），别硬造一条空验收。

已完成的参照（**5a 试点，本批的模板来源**）：`ecat-data-redis`（变体 A，`src/lib.rs:146-166`）、
`ecat-data-clickhouse`（变体 B + 许可，`src/lib.rs:164-195`）。

---

## 3. T0：10 个 HTTP 后端共用的模板

> **Task 1（arangodb）是模板的固化点**：它的每一步都写全了。Task 2~11 只列**与模板不同的部分**
> （标签 / kind / 方法清单 / 构造器 / 配置必填字段 / 测试用例），其余**逐字复制模板**，
> 并带一条 grep 自证复制到位（不写「同上」这种无判据的话）。

### T0-A `Cargo.toml`（十连抄，逐字）

以 `ecat-data-clickhouse/Cargo.toml` 为准，每个 HTTP crate 加三处：

```toml
# [dependencies] 末尾追加（**逐字**，含注释）
# 出站熔断：批次 4 已抽取的 Breaker，逐 client 一个。
ecat-circuit-breaker.workspace = true
# 并发上限用 Semaphore。tokio 现在只在 dev-dependencies 里，必须提到这里。
tokio = { workspace = true, features = ["sync"] }
# 指标依赖 optional：默认不把 axum（ecat-metrics 的依赖）拖进核心依赖树。
ecat-metrics = { workspace = true, optional = true }
```

```toml
# 新增 [features] 段（放在 [dependencies] 之后、[dev-dependencies] 之前）
[features]
# 只有 ecat-metrics：三个出站指标家族的唯一 collector 在那边（批次 5a「出入 11」），
# 本 crate 不需要直接依赖 prometheus。
metrics = ["dep:ecat-metrics"]
```

```toml
# [dev-dependencies] 里的 tokio 加 "time"（超时/熔断测试要 sleep）
tokio = { workspace = true, features = ["macros", "rt", "net", "time"] }
```

**每 crate 的例外**（其余 9 个逐字照上面）：

| crate | 例外 |
|-------|------|
| `ecat-data-s3` | dev-deps **没有 axum**，且 tokio 是 `["macros", "rt"]` ⇒ 改成 `axum.workspace = true` + `tokio = { workspace = true, features = ["macros", "rt", "net", "time"] }`（S3 走模式 ① 需要 axum；既有的裸 socket 测试**不动**） |
| `ecat-data-mongodb` | **非 HTTP**：不加 `tokio/sync`（无信号量），要加 `ecat-circuit-breaker.workspace = true` 与 `ecat-metrics` optional；dev tokio 从 `["macros","rt"]` 改成 `["macros","rt","net","time"]` |

自证：`grep -c 'ecat-metrics' ecat-data-<crate>/Cargo.toml` 期望 **4** —— 依赖行 + feature 行
**之外还有两行注释**也含该串（Task 1 实测：arangodb 4、5a 的 redis / clickhouse 也是 4；
原写「期望 2」只数了非注释两行）。

### T0-B 配置字段（逐字；两版 rustdoc，按有没有内层超时选）

HTTP 后端（跑在 `ecat_tls::build_reqwest_client` 上）用 **ClickHouse 版**（`ecat-data-clickhouse/src/lib.rs:49-62` 逐字）：

```rust
    /// 单次调用超时秒数。`0` = 禁用；未配置 = 30 秒。
    ///
    /// 这是**外层**预算，与 reqwest 自带的总超时（`from_config` 建的 client 有
    /// 30 秒、`new` / `with_auth` 没有）取先到者。
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
    /// 并发上限。reqwest **只有** `pool_max_idle_per_host`（空闲保留数），
    /// 没有「最大总连接数」—— 默认无上限意味着并发无背压。
    /// 未配置 = 32。上限由本 crate 的信号量实现，不是 reqwest 的旋钮。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
```

MongoDB 用 **Redis 版**（`ecat-data-redis/src/lib.rs:45-55` 逐字，**没有** `max_concurrency`，
`query_timeout_secs` 的 rustdoc 只有一行）：

```rust
    /// 单次命令超时秒数。`0` = 禁用；未配置 = 30 秒。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
```

（`breaker` 字段两版完全相同。）**MongoDB 额外两个**（出入 5/6）：

```rust
    /// 连接池上限。未配置 = 驱动默认（mongodb 3.8.0 实测 **10**，
    /// `src/cmap.rs:50`；不是 spec §5 写的 100 —— 那是 Node 驱动的默认值）。
    #[serde(default)]
    pub max_pool_size: Option<u32>,
    /// 连接池下限（后台保活连接数）。未配置 = 驱动默认 0。
    #[serde(default)]
    pub min_pool_size: Option<u32>,
```

转换函数**逐字**（每个 crate 各一份私有函数，`ecat-data-redis/src/lib.rs:58-65`）：

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

`from_config` 的三行装配**逐字**（`ecat-data-clickhouse/src/lib.rs:145-147`）：

```rust
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
            semaphore: Arc::new(Semaphore::new(cfg.max_concurrency.unwrap_or(32))),
```

（MongoDB 没有第三行；`max_pool_size` / `min_pool_size` 按 T0-C2 装配。）

### T0-C 结构体 / 构造器 / 访问器 / 许可 / `guarded`

#### C1 字段（HTTP 九连抄）

```rust
pub struct <XxxClient> {
    // …既有字段不动…
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
    semaphore: Arc<Semaphore>,
}
```

构造器装配：
- `from_config`：T0-B 的三行 + `max_concurrency` 那行**走配置**（不许写死 32）。
- `new` / `with_auth`（裸 `reqwest::Client::new()`，**没有内层超时**）：

```rust
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Arc::new(Semaphore::new(32)),
```

（S3 只有 `from_config`，跳过本条的裸构造器部分。）

#### C2 MongoDB 的装配（**与上面不同，逐字**）

```rust
    pub async fn from_config(cfg: MongoConfig) -> Result<Self, Error> {
        let mut options = mongodb::options::ClientOptions::parse(&cfg.url)
            .await
            .map_err(|e| {
                Error::new(ErrorCode::Internal, "mongodb", format!("mongodb connect: {e}"))
            })?;
        // 池大小走**驱动的旋钮**（本 crate 没有信号量）：`None` = 驱动默认。
        options.max_pool_size = cfg.max_pool_size;
        options.min_pool_size = cfg.min_pool_size;
        let client = mongodb::Client::with_options(options).map_err(|e| {
            Error::new(ErrorCode::Internal, "mongodb", format!("mongodb connect: {e}"))
        })?;
        Ok(Self {
            client,
            database: cfg.database,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
        })
    }
```

> 若 `ClientOptions::parse(&cfg.url)` 因泛型（`C: TryInto<ConnectionString>`）编译不过，
> 换成 `ClientOptions::parse(cfg.url.as_str())` —— 实测 `impl TryFrom<&String> for ConnectionString`
> 存在（`src/client/options.rs:1583-1587`），两种写法理论上都行，以编译结果为准（这属于可自行裁决的片段纠偏，
> 但要写进提交信息）。

#### C3 访问器（`metrics` feature 要用，逐字；`ecat-data-redis/src/lib.rs:141-144`）

```rust
    /// 本 client 的熔断器。`metrics` feature 注册指标时要读它的状态与打开次数。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }
```

#### C4 许可（HTTP 九连抄，`ecat-data-clickhouse/src/lib.rs:156-162` 逐字）

```rust
    /// 取一个并发许可。信号量从不 `close()`，`AcquireError` 不可达。
    async fn permit(&self) -> SemaphorePermit<'_> {
        self.semaphore
            .acquire()
            .await
            .expect("semaphore is never closed")
    }
```

#### C5 `guarded`（**5b 模板 A**：变体 A 的 body + 许可层；kind 写死，不收参数）

```rust
    /// 一次出站调用的公共外壳：**许可 → 熔断 → 超时**。
    ///
    /// 顺序与 spec §3 相反（理由见批次 5a 计划的「与 spec 的出入 4」）：
    /// 超时若在外层，`tokio::time::timeout` 会把熔断器的 future 直接 drop 掉，
    /// 于是每次「后端没在预算内作答」都**什么都不记** —— 卡死的后端永远打不开熔断器，
    /// 而卡死正是本设计要防的头号场景。熔断在外时超时是一次普通的 `Err`，如实计入失败。
    /// 许可在最外：还在排队的请求**还没碰后端**，不该计入熔断失败、也不该被超时掐断。
    ///
    /// **只包真正发 I/O 的路径。** 纯本地分支（如 NebulaGraph 的 `params` 早退）必须留在
    /// 外面：半开态下每次 `call` 都要借走一个探测名额，而「没发请求就返回」会以 `Ok` 记账 ——
    /// 探测名额被白白消耗、熔断器还可能被**误关回 `Closed`**。记账口径是「后端的表现」，
    /// 不是「函数的返回值」。
    ///
    /// `kind` **写死**不收参数：本 crate 只有一条 I/O 路径（ClickHouse 收参数是因为它有
    /// `SqlExecutor` / `TsdbClient` 两条、共用外壳）。多一个永不变化的入参，就多一个填错的
    /// 机会，而填错只是**静默少数**（`ecat-data/src/timeout.rs:15-35`），没有编译期保护。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::<KIND>, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "<LABEL>"))
    }
```

> **与 checklist 的一处有意偏离（已裁决 = 批准，并已回写 checklist）**：checklist §2 变体 B
> （`:171-184`）的签名带 `kind: BackendKind` —— 那是 ClickHouse 的形状（两条路径）。
> 11 个目标 crate 都只有一条路径 ⇒ 写死更安全（参数化会把「维度由 trait 家族决定」
> 降级成「调用者说了算」，而填错只静默少数）。checklist §2 已加上同口径的规则段
> （**Task 12 Step 0 是它的落地/核对步骤，开工前先做那一步**）。实施时**不要**「统一」回去。

`<KIND>`/`<LABEL>` 取值见 §2 表；QuestDB 是 `RdbmsError` 路径，见 Task 9；MongoDB 去掉 `let _permit` 行，见 Task 11。

### T0-D 包哪些方法（10 个 crate 同一张表）

| crate | 包（逐方法，`self.guarded(async { <原体原样> }).await`） | **不包**（写进注释说明理由） |
|-------|--------------------------------------------------------|--------------------------|
| arangodb | `execute` | `percent_encode_segment`（纯本地） |
| neo4j | `execute` | —（无本地分支） |
| nebulagraph | `execute` 的**发送段**（`params not null` 的 `return Err` 留在外面） | 同上早退 |
| elasticsearch | `index` / `search` / `delete` | `bulk_index` / `update`（trait 默认实现） |
| opensearch | `index` / `search` / `delete` | 同 ES |
| influxdb | `write` / `query` | `delete`（trait 默认实现）、`escape_*` |
| iotdb | `write`（整个循环一次预算）/ `query` | `delete`（默认实现） |
| tdengine | `write`（整个分块循环一次预算）/ `query` | `exec`（**内部** helper，包了两层就会把一次调用切成 N 个预算）、`delete`（默认实现）、`percent_encode_segment` |
| questdb | `execute` / `query` | `transaction`（常量错误）、`dialect`（纯本地）、`apply_auth` |
| s3 | `put` / `get` / `delete` / `list`（**含翻页循环**，一次调用一个预算） | `object_path` / `signed_request` / `check_status`（纯本地，不发 I/O） |
| mongodb | `insert`/`find`/`update`/`delete` 的**网络段**（bson 转换留在外面） | bson 转换（本地，且是**调用方**的输入错误） |

### T0-E `src/metrics.rs`（feature 门控，十连抄 + 每 crate 换两个词）

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "metrics"：把 <产品名> 的出站数据源挂进 `ecat-metrics` 的共用 collector。
//!
//! 三个指标家族（`ecat_outbound_timeouts_total` / `ecat_outbound_breaker_open_total`
//! / `ecat_outbound_breaker_state`）的 collector **不在本 crate** —— 指标名是全进程
//! 共享的命名空间，每个后端各建一份会在 `Registry` 里撞名（`AlreadyReg`），让后
//! 注册者的样本静默消失。理由见批次 5a 的「出入 11」与 `ecat-metrics/src/outbound.rs`。

use ecat_circuit_breaker::Breaker;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// 挂上 <产品名> 的出站数据源。标签值固定为 `"<LABEL>"`。
pub fn register_outbound_metrics(breaker: Arc<Breaker>) {
    let opened = Arc::clone(&breaker);
    ecat_metrics::register_outbound_metrics(
        "<LABEL>",
        Box::new(|| timeout_counter(BackendKind::<KIND>).load(Ordering::Relaxed)),
        Box::new(move || opened.opened_total()),
        Box::new(move || breaker.state().code()),
    );
}
```

`lib.rs` 顶部接线（逐字）：

```rust
#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;
```

测试（`metrics.rs` 内 `#[cfg(test)] mod tests`，**每 crate 一条，标签在二进制内唯一**）：

```rust
    /// 抓取后按 `指标名{backend="<LABEL>"}` 找样本值（找不到 = None）。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// 三个指标都要出现，且**值是抓取时现读的**：先 +1000 再抓，快照实现只会给出 0。
    ///
    /// `timeout_counter(BackendKind::<KIND>)` 的 kind 误接（写成别的槽）在这里红：
    /// 两份样本都会存在，但值差着 1000。
    #[tokio::test]
    async fn outbound_metrics_appear_with_live_values() {
        let breaker = Arc::new(Breaker::new(BreakerConfig::default()));
        register_outbound_metrics(Arc::clone(&breaker));

        // 推进**本 crate 的 kind 槽**一大格。本文件**不许**动证人槽（见 §5(c)）。
        timeout_counter(BackendKind::<KIND>).fetch_add(1000, Ordering::Relaxed);

        let text = ecat_metrics::metrics_text();
        assert!(
            sample(&text, r#"ecat_outbound_timeouts_total{backend="<LABEL>"}"#)
                .is_some_and(|v| v >= 1000.0),
            "超时样本应现读静态量（先 +1000 再抓取），实际输出:\n{text}"
        );
        assert_eq!(
            sample(&text, r#"ecat_outbound_breaker_state{backend="<LABEL>"}"#),
            Some(0.0),
            "未打开时状态应为 0，实际输出:\n{text}"
        );

        let fail = || async { Err::<(), &str>("backend down") };
        for _ in 0..5 {
            let _ = breaker.call(fail).await;
        }
        assert_eq!(breaker.state(), BreakerState::Open);
        let text = ecat_metrics::metrics_text();
        assert_eq!(
            sample(&text, r#"ecat_outbound_breaker_open_total{backend="<LABEL>"}"#),
            Some(1.0),
            "打开次数应为 1，实际输出:\n{text}"
        );
        assert_eq!(
            sample(&text, r#"ecat_outbound_breaker_state{backend="<LABEL>"}"#),
            Some(1.0),
            "打开后状态应为 1（现读），实际输出:\n{text}"
        );
    }
```

需要 `use ecat_circuit_breaker::{Breaker, BreakerConfig, BreakerState};` 与 `use ecat_data::timeout_counter;`
（若外层已有 `use super::*` 则按 ClickHouse `metrics.rs:33-36` 的写法补）。

### T0-F 测试文件骨架（`src/tests/resilience.rs`）

`lib.rs`（或 `src/tests.rs`，见差异表）里的测试模块**第一行**加声明（checklist §7 陷阱 7）：

```rust
mod resilience;
```

`src/tests/resilience.rs` 的头与公共 helper（**逐字**，只改 crate 名与 kind/label）：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 并发上限）测试。
use super::*;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 一个「先拖住再回」的 mock（模式 ①）：`delay` 之后回 200 空体。
/// 既有的 `spawn_mock` 系列不拖时间，故另起一个。
async fn spawn_slow_<crate>(delay: Duration, in_flight: Arc<AtomicUsize>) -> String {
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

/// 「第一次拖住、之后立刻回 200 + `{}`」的 mock：模式 ④ 用。
///
/// 模式 ④ 只挑**下面这个方法**（第二次必须真的成功，所以体得能被该方法解析）：
/// 图三兄弟 / QuestDB 用 `execute`（`{}` 就是合法响应体）、ES/OS 用 `index`、
/// influxdb/iotdb 用 `write`、tdengine 用 `query`、S3 用 `put`（都只看状态码或
/// `{}` 即可解析）。**S3 的 `list` 不参与模式 ④** —— 它要解析 XML。
async fn spawn_slow_once(delay: Duration) -> String {
    let seen = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
        let seen = Arc::clone(&seen);
        async move {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(delay).await;
            }
            axum::response::Response::new(axum::body::Body::from("{}"))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// 一次调用必须在**外层 5 秒内**返回 `DeadlineExceeded`，且推进本维度。
///
/// 外层 5 秒是**把挂死变成红灯**：漏包 `guarded` 的后果不是报错而是永远不返回，
/// 没有这层的话整个测试二进制会卡住而不是 FAILED（`ecat-data-redis/src/tests.rs:345-364`）。
///
/// **泛型 `T`（2026-10-08 lead 裁决 B，Task 1 实测）**：被测方法的返回值各异
/// （图三兄弟的 `execute` 是 `Value`、S3 的 `get` 是 `Vec<u8>`……），调用点直接
/// 透传即可；`T = ()` 的老调用照样编译。这是 redis 先例（`assert_times_out("set",
/// … .map(|_| ())`，`ecat-data-redis/src/tests.rs:381`）的**推广**而非推翻。
/// 原片段写死 `Output = Result<(), Error>` 与 arangodb 的 `execute`（`Value`）
/// 冲突，Task 1 实测 E0271（三处）；`T: Debug` 是 `unwrap_err()` 的需要。
async fn assert_times_out<T, F>(label: &str, fut: F) -> Error
where
    T: std::fmt::Debug,
    F: std::future::Future<Output = Result<T, Error>>,
{
    let before = timeout_counter(BackendKind::<KIND>).load(Ordering::SeqCst);
    let err = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("{label}: 内层超时没开火（漏包 guarded？）"))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "{label}: {err}");
    // `reason` 是 kind 的 slug（`ecat-data/src/timeout.rs:108-118`）：
    // `guarded` 里 kind 传错（如 es 传成 Rdbms）在这里红，而不是等到告警面板。
    assert_eq!(err.reason, "<SLUG>", "{label}: 超时 reason 应是 kind.slug()");
    assert!(
        timeout_counter(BackendKind::<KIND>).load(Ordering::SeqCst) > before,
        "{label}: 超时必须计入 <KIND> 维度"
    );
    err
}
```

**槽断言口径（§5(b)(c)，本批的硬规矩）**：

- **本 crate 的 kind 槽**（有多写者：本文件多条用例 + `metrics.rs`）⇒ **只用方向断言 `> before`**。
- **证人槽**：每个 crate 选**本测试二进制内无人写**的槽，断言 `== before`。
  - 10 个 crate 用 **`Storage`**；`ecat-data-s3` 自己就是 Storage ⇒ 用它 **`Cache`**。
  - 落笔前必须实测：`grep -rn 'BackendKind::Storage' ecat-data-<crate>/`（s3 换 `Cache`）
    必须**只**出现在 witness 断言与大意的 rustdoc 里，**不能**有 `fetch_add` / `store` / `.call(...)` 的写者。
  - **不许加锁串行化**（5a 裁决：那把锁会被 12 份模板抄走，且 flake 会在下一个人加用例时回来）。

### T0-G 每个 crate 的标准测试集（7 条 + feature 下 1 条）

| # | 用例名 | 判据 | 模板 |
|---|--------|------|------|
| 1 | `config_wires_timeout_concurrency_and_breaker` | 经 `from_config` 装配：`c.query_timeout == Some(1s)`、`c.semaphore.available_permits() == 3`、`c.breaker().state() == Closed` | checklist §1 验收② |
| 2 | `zero_timeout_means_disabled` | `query_timeout(Some(0)) == None`、`query_timeout(None) == Some(30s)`，且 `from_config` 里 `query_timeout_secs: 0` ⇒ `c.query_timeout == None` | `ecat-data-redis/src/tests.rs:334-343` |
| 3 | `<主方法>_times_out_with_deadline_exceeded` | 1s 预算 vs 5s mock ⇒ `DeadlineExceeded` + `reason == "<slug>"` + 本 kind 槽 `> before` + 证人槽 `== before` | checklist §5 模式 ① |
| 4 | `every_<x>_method_times_out_when_the_backend_stalls` | 每个 I/O 方法各打一次（**≤4 次调用** ⇒ 打不满 5 条窗口，熔断不会在途中打开） | `ecat-data-redis/src/tests.rs:366-398` |
| 5 | `repeated_timeouts_open_the_breaker_and_fail_fast` | 5 次超时 ⇒ `state() == Open`、`opened_total() == 1`；下一次 `Unavailable` + `message == "circuit breaker is open"` + `reason == "<label>"` + 快速返回 | checklist §6 |
| 6 | `concurrency_cap_limits_in_flight_requests` | `max_concurrency = 2`、3 并发、mock 延迟 50ms ⇒ 在飞峰值 `<= 2` | `ecat-data-clickhouse/src/tests/resilience.rs:189-227` |
| 7 | `timed_out_request_returns_its_permit` | 上限 1；第一次 1s 超时；**第二次必须成功且 < 2s**（被排到 5s 保险丝 ⇒ 报「许可没归还」） | checklist §5 模式 ④（**必做**） |
| 8 | 把守测试（**按 crate 才有**，见差异表） | 默认实现 / 常量错误 / 本地早退 **不得**触碰熔断器 | checklist §3 / §7 陷阱 2/3/4 |
| 9 | `metrics.rs` 里的 `outbound_metrics_appear_with_live_values` | `--features metrics` 下 3 个样本 + 现读 + 开态翻转 | T0-E |

### T0-H 逐 crate 差异表

| crate | 任务 | `KIND` | `LABEL` | 构造器 | `client_at` 必填字段 | 方法数 | 证人槽 | 把守测试 | 拆测试文件 | 基线测试数 | 新增（默认） |
|-------|------|--------|---------|--------|---------------------|--------|--------|---------|-----------|-----------|-------------|
| arangodb | Task 1 | Graph | arangodb | `new`/`from_config` | base_url, db, username, password | 1 | Storage | 无（无分支可守） | 否 | 7 | +6 |
| neo4j | Task 2 | Graph | neo4j | `new`/`from_config` | base_url, username, password | 1 | Storage | 无 | 否 | 4 | +6 |
| nebulagraph | Task 3 | Graph | nebulagraph | `new`/`with_auth`/`from_config` | base_url, space | 1 | Storage | 本地早退 | 否 | 7 | +7 |
| elasticsearch | Task 4 | Search | elasticsearch | `new`/`with_auth`/`from_config` | base_url | 3 | Storage | `_with` 默认 ×2 | 否 | 11 | +8 |
| opensearch | Task 5 | Search | opensearch | 同 ES | base_url | 3 | Storage | 同 ES | 否 | 10 | +8 |
| influxdb | Task 6 | Tsdb | influxdb | `new`/`from_config` | base_url, org, bucket, token | 2 | Storage | `delete` 默认 | **是** | 10 | +8 |
| iotdb | Task 7 | Tsdb | iotdb | `new`/`from_config` | base_url, username, password | 2 | Storage | `delete` 默认 + 整调用预算 | **是** | 10 | +9 |
| tdengine | Task 8 | Tsdb | tdengine | `new`/`from_config` | base_url, username, password | 2 | Storage | `delete` 默认 + 整调用预算 | **是** | 11 | +9 |
| questdb | Task 9 | Rdbms | questdb | `new`/`with_auth`/`from_config` | base_url | 2 | Storage | `transaction` 常量错 | 否 | 9 | +8 |
| s3 | Task 10 | Storage | s3 | **只有 `from_config`** | endpoint, region, access_key, secret_key | 4 | **Cache** | 无 | **是** | 17 | +7 |
| mongodb | Task 11 | Document | mongodb | **只有 `from_config`（async）** | url, database | 4 | Storage | bson 前置转换 | 否 | 8 | +5 |

- **基线测试数**是写作时对 `src/` 里 `#[test]` + `#[tokio::test]` 的实测计数（s3 的 17 含
  `src/signing.rs` 与 `src/xml.rs` 的模块内测试：lib.rs 9 + 两个子模块 8）。**每条任务第 1 步都要求
  自己再跑一遍基线**：以你实测的数字为准，和本表不同就在提交信息里记一笔。
- 验收时**基线 + 新增**必须与 `cargo test` 的 `test result: ... passed` 数一致 —— 对不上就是
  `mod` 没生效（假绿的一种）。
- 带 `metrics` 跑时**再 +1**（`src/metrics.rs` 里那条），且标签在**该测试二进制内唯一**。
- **单方法 crate（arangodb / neo4j / nebulagraph）没有独立的「覆盖每个 I/O 方法」用例**：
  一条方法时它与主超时用例重复，合并为一条（这也是它们新增数是 6 的原因）。
- **MongoDB 没有模式 ④**（无信号量、无许可），它的 5 条见 Task 11。

---

## Task 1：`ecat-data-arangodb`（模板固化点，最细一跳）

> 后面 10 个 crate 都照这个任务抄。本任务不图快，图把模板钉死：**每一步都带完整代码与判据**。

**Files**（本任务只碰这 4 个文件）
- `ecat-data-arangodb/Cargo.toml`
- `ecat-data-arangodb/src/lib.rs`
- `ecat-data-arangodb/src/metrics.rs`（新建）
- `ecat-data-arangodb/src/tests/resilience.rs`（新建，`mod resilience;` 声明在 lib.rs 的 `mod tests` 内）

**对照**：checklist §1（配置字段）→ §2（变体 A）→ §3（访问器）→ §4（指标）→ §5（测试模式 ①④）→ §6（熔断用例）。
试点来源：`ecat-data-redis/src/lib.rs:36-66,141-166`（变体 A 逐字）+ `ecat-data-clickhouse/src/lib.rs:156-195`（许可层逐字）。

所有命令都在 `/home/wwwroot/e-cat` 下执行，且**每条都带 `export CARGO_TARGET_DIR=/var/tmp/ecat-target`**。

### Step 1（2 分钟）记基线

```bash
cd /home/wwwroot/e-cat
export CARGO_TARGET_DIR=/var/tmp/ecat-target
cargo test -p ecat-data-arangodb 2>&1 | tail -3
wc -l ecat-data-arangodb/src/lib.rs
```

期望：`test result: ok. 7 passed; 0 failed; ...` 与 `269 ecat-data-arangodb/src/lib.rs`。
**与计划不符就先别写代码**：把实测值记进脑内，末尾提交信息里带上你的实测基线。

### Step 2（3 分钟）`Cargo.toml` 三处

`[dependencies]` 的 `ecat-tls = { version = "6.0.0", path = "../ecat-tls" }` 之后追加（逐字）：

```toml
# 出站熔断：批次 4 已抽取的 Breaker，逐 client 一个。
ecat-circuit-breaker.workspace = true
# 并发上限用 Semaphore。tokio 现在只在 dev-dependencies 里，必须提到这里。
tokio = { workspace = true, features = ["sync"] }
# 指标依赖 optional：默认不把 axum（ecat-metrics 的依赖）拖进核心依赖树。
ecat-metrics = { workspace = true, optional = true }
```

新增 `[features]` 段（放在 `[dependencies]` 之后、`[dev-dependencies]` 之前，逐字）：

```toml
[features]
# 只有 ecat-metrics：三个出站指标家族的唯一 collector 在那边（批次 5a「出入 11」），
# 本 crate 不需要直接依赖 prometheus。
metrics = ["dep:ecat-metrics"]
```

`[dev-dependencies]` 的 tokio 加 `"time"`（既有是 `["macros", "rt", "net"]`）：

```toml
tokio = { workspace = true, features = ["macros", "rt", "net", "time"] }
```

判据：

```bash
export CARGO_TARGET_DIR=/var/tmp/ecat-target
grep -c 'ecat-metrics' ecat-data-arangodb/Cargo.toml   # 期望 4（含两行注释）
cargo metadata --format-version=1 --offline >/dev/null && echo METADATA_OK
```

期望 `4` 与 `METADATA_OK`。

### Step 3（3 分钟）配置字段

`ArangoConfig` 的 `tls` 字段块（`src/lib.rs:14-15`）之后插入（逐字，ClickHouse 版 rustdoc）：

```rust
    /// 单次调用超时秒数。`0` = 禁用；未配置 = 30 秒。
    ///
    /// 这是**外层**预算，与 reqwest 自带的总超时（`from_config` 建的 client 有
    /// 30 秒、`new` / `with_auth` 没有）取先到者。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    ///
    /// 熔断**默认开启** —— 保守阈值下只在持续失败时打开。**当前没有总开关**：
    /// `BreakerConfig` 只有阈值字段，没有 `enabled`（不许写 `{"enabled": false}`：
    /// 那是反序列化错误，或被 `#[serde(default)]` 静默吞掉后以为关掉了）。
    /// 真要停用，只能把阈值调到不可能触发（如 `failure_ratio: 1.1`）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 并发上限。reqwest **只有** `pool_max_idle_per_host`（空闲保留数），
    /// 没有「最大总连接数」—— 默认无上限意味着并发无背压。
    /// 未配置 = 32。上限由本 crate 的信号量实现，不是 reqwest 的旋钮。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
```

`ArangoClient` 的 `impl` 块**外面**（紧跟 `}` 之后、`percent_encode_segment` 之前，逐字）：

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

### Step 4（5 分钟）结构体字段 + 两个构造器

`ArangoClient` 结构体（`src/lib.rs:18-24`）末尾追加：

```rust
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
    semaphore: Arc<Semaphore>,
```

`new`（`src/lib.rs:27-40`）的 `Self { ... }` 里 `password: password.into(),` 之后追加：

```rust
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Arc::new(Semaphore::new(32)),
```

`from_config`（`src/lib.rs:42-52`）的 `Ok(Self { ... })` 里 `password: cfg.password,` 之后追加：

```rust
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
            semaphore: Arc::new(Semaphore::new(cfg.max_concurrency.unwrap_or(32))),
```

顶部 `use`（`src/lib.rs:2-6`）整体替换为：

```rust
use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{BackendKind, GraphClient, breaker_error_to_backend_error, run_with_timeout};
use ecat_errors::{Error, ErrorCode};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, SemaphorePermit};
```

### Step 5（5 分钟）访问器 / 许可 / `guarded`

`impl ArangoClient` 内、`from_config` 之后追加（逐字；`breaker` 与 `permit` 照抄 `ecat-data-redis/src/lib.rs:141-162`，
`guarded` 是 5b 模板 A —— 注意 `BackendKind::Graph` 与控制台标签 `"arangodb"` 是**写死**的）：

```rust
    /// 本 client 的熔断器。`metrics` feature 注册指标时要读它的状态与打开次数。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }

    /// 取一个并发许可。信号量从不 `close()`，`AcquireError` 不可达。
    async fn permit(&self) -> SemaphorePermit<'_> {
        self.semaphore
            .acquire()
            .await
            .expect("semaphore is never closed")
    }

    /// 一次出站调用的公共外壳：**许可 → 熔断 → 超时**。
    ///
    /// 顺序见 T0 模板（超时若在外层，熔断器会对卡死的后端永久失明）。
    /// 许可在最外：还在排队的请求**还没碰后端**，不该计入熔断失败、也不该被超时掐断。
    ///
    /// `kind` **写死**不收参数：本 crate 只有一条 I/O 路径（ClickHouse 收参数是因为
    /// 它有两条路径共用外壳）。多一个永不变化的入参就多一个填错的机会，
    /// 而填错只是静默少数（`ecat-data/src/timeout.rs:15-35`），没有编译期保护。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Graph, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "arangodb"))
    }
```

### Step 6（5 分钟）包装 `execute`（唯一一条 I/O 路径）

`GraphClient for ArangoClient` 的 `execute`（`src/lib.rs:72-100`）整体替换为——
**只多两层**：本地构造的 `body` 留在外面，其余进 `self.guarded(async { … }).await`：

```rust
    async fn execute(
        &self,
        aql: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        let body = serde_json::json!({"query": aql, "bindVars": params});
        self.guarded(async {
            let resp = self
                .client
                .post(format!(
                    "{}/_db/{}/_api/cursor",
                    self.base_url,
                    percent_encode_segment(&self.db)
                ))
                .basic_auth(&self.username, Some(&self.password))
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "arango", format!("arango: {e}")))?;
            if !resp.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Internal,
                    "arango",
                    resp.text().await.unwrap_or_default(),
                ));
            }
            resp.json().await.map_err(|e| {
                Error::new(
                    ErrorCode::Internal,
                    "arango",
                    format!("arango parse: {e}"),
                )
            })
        })
        .await
    }
```

**不包**：`percent_encode_segment`（纯本地字符串处理，无 I/O，包了等于给熔断器喂噪音）。

判据：

```bash
export CARGO_TARGET_DIR=/var/tmp/ecat-target
cargo check -p ecat-data-arangodb 2>&1 | tail -3
```

期望 `Finished`（或 `Finished \`dev\` profile`）。

### Step 7（5 分钟）`src/metrics.rs`

新建 `ecat-data-arangodb/src/metrics.rs`（逐字；只有标签与 kind 是本 crate 的）：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! feature = "metrics"：把 arangodb 的出站数据源挂进 `ecat-metrics` 的共用 collector。
//!
//! 三个指标家族（`ecat_outbound_timeouts_total` / `ecat_outbound_breaker_open_total`
//! / `ecat_outbound_breaker_state`）的 collector **不在本 crate** —— 指标名是全进程
//! 共享的命名空间，每个后端各建一份会在 `Registry` 里撞名（`AlreadyReg`），让后
//! 注册者的样本静默消失。理由见批次 5a 的「出入 11」与 `ecat-metrics/src/outbound.rs`。

use ecat_circuit_breaker::Breaker;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// 挂上 arangodb 的出站数据源。标签值固定为 `"arangodb"`（= 配置节名）。
pub fn register_outbound_metrics(breaker: Arc<Breaker>) {
    let opened = Arc::clone(&breaker);
    ecat_metrics::register_outbound_metrics(
        "arangodb",
        Box::new(|| timeout_counter(BackendKind::Graph).load(Ordering::Relaxed)),
        Box::new(move || opened.opened_total()),
        Box::new(move || breaker.state().code()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecat_circuit_breaker::{BreakerConfig, BreakerState};

    /// 抓取后按 `指标名{backend="arangodb"}` 找样本值（找不到 = None）。
    fn sample(text: &str, prefix: &str) -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix) && !l.starts_with('#'))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    }

    /// 三个指标都要出现，且**值是抓取时现读的**：先 +1000 再抓，快照实现只会给出 0。
    ///
    /// `timeout_counter(BackendKind::Graph)` 的 kind 误接（写成别的槽）在这里红：
    /// 两份样本都会存在，但值差着 1000。
    /// **本用例只推进 Graph 槽**（= 本 crate 自己的 kind），证人槽（Storage）纹丝不动。
    #[tokio::test]
    async fn outbound_metrics_appear_with_live_values() {
        let breaker = Arc::new(Breaker::new(BreakerConfig::default()));
        register_outbound_metrics(Arc::clone(&breaker));

        timeout_counter(BackendKind::Graph).fetch_add(1000, Ordering::Relaxed);

        let text = ecat_metrics::metrics_text();
        assert!(
            sample(&text, r#"ecat_outbound_timeouts_total{backend="arangodb"}"#)
                .is_some_and(|v| v >= 1000.0),
            "超时样本应现读静态量（先 +1000 再抓取），实际输出:\n{text}"
        );
        assert_eq!(
            sample(&text, r#"ecat_outbound_breaker_state{backend="arangodb"}"#),
            Some(0.0),
            "未打开时状态应为 0，实际输出:\n{text}"
        );

        let fail = || async { Err::<(), &str>("backend down") };
        for _ in 0..5 {
            let _ = breaker.call(fail).await;
        }
        assert_eq!(breaker.state(), BreakerState::Open);
        let text = ecat_metrics::metrics_text();
        assert_eq!(
            sample(&text, r#"ecat_outbound_breaker_open_total{backend="arangodb"}"#),
            Some(1.0),
            "打开次数应为 1，实际输出:\n{text}"
        );
        assert_eq!(
            sample(&text, r#"ecat_outbound_breaker_state{backend="arangodb"}"#),
            Some(1.0),
            "打开后状态应为 1（现读），实际输出:\n{text}"
        );
    }
}
```

`src/lib.rs` 顶部（`use` 块之后、`ArangoConfig` 之前）接线（逐字）：

```rust
#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::register_outbound_metrics;
```

判据：

```bash
export CARGO_TARGET_DIR=/var/tmp/ecat-target
cargo check -p ecat-data-arangodb --features metrics 2>&1 | tail -3
cargo tree -p ecat-data-arangodb --features metrics 2>&1 | grep -c 'ecat-metrics'  # 期望 ≥1
```

期望第二条 ≥ 1（说明 `ecat-metrics` 真的被 feature 拉进依赖图，不是被静默关掉）。

⚠️ **2026-10-08 更正**：原判据写的是 `cargo test … --features metrics | grep -c 'ecat_metrics'` 期望 ≥1 —— **那不可达**：测试全绿时的输出里根本不含 crate 名，实测（neo4j 落码后回查）在 arangodb 与 neo4j 上**都是 0**。要证「feature 真的把 `ecat-metrics` 拉进来了」，可达的判据是 `cargo tree --features metrics` 里能查到它；另一条等价证据是**测试数**：默认 N → 带 feature N+1（多的是 `metrics::tests::*` 那条），可用 `cargo test -p <crate> --features metrics -- --list` 确认该用例在场。

### Step 8（8 分钟）`src/tests/resilience.rs`

先新建目录与文件 `ecat-data-arangodb/src/tests/resilience.rs`（逐字）：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 并发上限）测试。
use super::*;
use ecat_circuit_breaker::BreakerState;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 「先拖住再回」的 mock（模式 ①）：`delay` 之后回 200 空体。
/// 既有的 `spawn_mock` 是「立即应答 + 记录请求」，拖不住时间，故另起一个。
async fn spawn_slow(delay: Duration, in_flight: Arc<AtomicUsize>) -> String {
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

/// 「第一次拖住、之后立刻回 200 + `{}`」的 mock（模式 ④）。`{}` 是 `execute`
/// 能直接解析的合法响应体，所以第二次调用必须**成功**（不只是不挂死）。
async fn spawn_slow_once(delay: Duration) -> String {
    let seen = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().fallback(move |_req: axum::http::Request<axum::body::Body>| {
        let seen = Arc::clone(&seen);
        async move {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(delay).await;
            }
            axum::response::Response::new(axum::body::Body::from("{}"))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// 一次调用必须在**外层 5 秒内**返回 `DeadlineExceeded`，且推进本维度。
///
/// 外层 5 秒是**把挂死变成红灯**：漏包 `guarded` 的后果不是报错而是永远不返回，
/// 没有这层的话整个测试二进制会卡住而不是 FAILED（`ecat-data-redis/src/tests.rs:345-364`）。
///
/// **泛型 `T`（2026-10-08 lead 裁决 B，Task 1 实测）**：被测方法的返回值各异
/// （图三兄弟的 `execute` 是 `Value`、S3 的 `get` 是 `Vec<u8>`……），调用点直接
/// 透传即可；`T = ()` 的老调用照样编译。这是 redis 先例（`assert_times_out("set",
/// … .map(|_| ())`，`ecat-data-redis/src/tests.rs:381`）的**推广**而非推翻。
/// 原片段写死 `Output = Result<(), Error>` 与 arangodb 的 `execute`（`Value`）
/// 冲突，Task 1 实测 E0271（三处）；`T: Debug` 是 `unwrap_err()` 的需要。
async fn assert_times_out<T, F>(label: &str, fut: F) -> Error
where
    T: std::fmt::Debug,
    F: std::future::Future<Output = Result<T, Error>>,
{
    let before = timeout_counter(BackendKind::Graph).load(Ordering::SeqCst);
    let err = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("{label}: 内层超时没开火（漏包 guarded？）"))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "{label}: {err}");
    // `reason` 是 kind 的 slug（`ecat-data/src/timeout.rs:108-118`）：
    // `guarded` 里 kind 传错（如传成 Rdbms）在这里红，而不是等到告警面板。
    assert_eq!(err.reason, "graph", "{label}: 超时 reason 应是 kind.slug()");
    assert!(
        timeout_counter(BackendKind::Graph).load(Ordering::SeqCst) > before,
        "{label}: 超时必须计入 Graph 维度"
    );
    err
}

/// `max_concurrency` 走**配置**（`Some` 显式给值、`None` 走 `from_config` 的默认 32）：
/// 直接改私有字段会让 `cfg.max_concurrency` 的装配路径失去覆盖 —— 那样
/// `from_config` 里删掉那行，并发用例照样绿。
fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> ArangoClient {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    let cfg: ArangoConfig = serde_json::from_str(&format!(
        r#"{{"base_url": "{url}", "db": "mydb", "username": "root", "password": "s",
            "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
    .unwrap();
    ArangoClient::from_config(cfg).unwrap()
}

/// `from_config` 把三个新字段都真的接上了（checklist §1 验收②）。
#[tokio::test]
async fn config_wires_timeout_concurrency_and_breaker() {
    let cfg: ArangoConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","db":"d","username":"u","password":"p",
            "query_timeout_secs":1,"max_concurrency":3}"#,
    )
    .unwrap();
    let c = ArangoClient::from_config(cfg).unwrap();
    assert_eq!(c.query_timeout, Some(Duration::from_secs(1)));
    assert_eq!(c.semaphore.available_permits(), 3);
    assert_eq!(c.breaker().state(), BreakerState::Closed);
}

/// `0` = 显式禁用（`None`），未配置 = 30 秒（`ecat-data-redis/src/tests.rs:334-343`）。
#[test]
fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));
    let cfg: ArangoConfig = serde_json::from_str(
        r#"{"base_url":"http://127.0.0.1:1","db":"d","username":"u","password":"p",
            "query_timeout_secs":0}"#,
    )
    .unwrap();
    assert_eq!(ArangoClient::from_config(cfg).unwrap().query_timeout, None);
}

/// 超时真的开火（spec §8 判据 2），且只落 Graph 槽。
///
/// **必须用 1 秒超时 + 5 秒 mock**：`from_config` 建的 client 自带 reqwest 的
/// 30 秒总超时，mock 只拖 5 秒时它来不及开火；若我们的外层没接上，
/// 调用会**成功返回** ⇒ 断言失败。这条测试不是空验收。
#[tokio::test]
async fn execute_times_out_and_counts_graph_dimension() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    // 证人槽必须是**本测试二进制内没有任何其它用例会写**的槽 —— `Storage` 满足
    // （本 crate 的代码只用 `BackendKind::Graph`；metrics 用例也只用 Graph）。
    // 谁要新增写 Storage 槽的用例，先换一个自由槽（见 T0 模板的槽分配规矩）。
    let witness = timeout_counter(BackendKind::Storage).load(Ordering::SeqCst);
    assert_times_out(
        "execute",
        ecat_data::GraphClient::execute(&c, "RETURN 1", &serde_json::json!({})),
    )
    .await;
    assert_eq!(
        timeout_counter(BackendKind::Storage).load(Ordering::SeqCst),
        witness,
        "Graph 的超时不得落到别的槽"
    );
}

/// 熔断真的打开（spec §8 判据 3）：连续超时后**快速失败**，不再等满超时。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    for _ in 0..5 {
        let _ = ecat_data::GraphClient::execute(&c, "RETURN 1", &serde_json::json!({})).await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);
    assert_eq!(
        c.breaker().opened_total(),
        1,
        "超时失败必须真的打开过熔断器"
    );

    let start = std::time::Instant::now();
    let err = ecat_data::GraphClient::execute(&c, "RETURN 1", &serde_json::json!({}))
        .await
        .expect_err("熔断已打开");
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
    assert_eq!(err.message, "circuit breaker is open", "got: {err}");
    assert_eq!(
        err.reason, "arangodb",
        "熔断错误的 reason 是产品名（超时路径才是 kind.slug()）"
    );
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "熔断拒绝必须立即返回，实际 {:?}",
        start.elapsed()
    );
}

/// 并发上限真的封顶（spec §8 判据 5）：起 3 个并发、断言同时在线 ≤ 2。
/// 每个请求 sleep 50ms，远小于 30 秒超时 —— 排队的那个不会因超时而失败。
#[tokio::test]
async fn concurrency_cap_limits_in_flight_requests() {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let url = spawn_slow(Duration::from_millis(50), Arc::clone(&in_flight)).await;
    let c = Arc::new(client_at(&url, 30, Some(2)));

    let mut handles = Vec::new();
    for _ in 0..3 {
        let c = Arc::clone(&c);
        let peak = Arc::clone(&peak);
        let in_flight = Arc::clone(&in_flight);
        handles.push(tokio::spawn(async move {
            let mut last = 0;
            let sampler = tokio::spawn(async move {
                for _ in 0..10 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    last = last.max(in_flight.load(Ordering::SeqCst));
                }
                last
            });
            let _ = ecat_data::GraphClient::execute(c.as_ref(), "RETURN 1", &serde_json::json!({}))
                .await;
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

/// **超时的那次调用必须归还许可**（checklist §5 模式 ④，并发受限的后端必做）。
///
/// 不归还的实现在这里红：第二发会永远排在 `acquire()` 上（超时层在许可**里面**，
/// 排队的请求根本走不到超时），只能被 2 秒保险丝抓住。
#[tokio::test]
async fn timed_out_request_returns_its_permit() {
    let url = spawn_slow_once(Duration::from_secs(5)).await;
    let c = client_at(&url, 1, Some(1));
    let first = ecat_data::GraphClient::execute(&c, "RETURN 1", &serde_json::json!({}))
        .await
        .expect_err("第一发必须超时");
    assert_eq!(first.code, ErrorCode::DeadlineExceeded, "got: {first}");

    let second = tokio::time::timeout(
        Duration::from_secs(2),
        ecat_data::GraphClient::execute(&c, "RETURN 1", &serde_json::json!({})),
    )
    .await
    .expect("第二发被排在许可上（超时路径没归还许可？）")
    .expect("许可归还后第二次必须成功");
    assert!(second.is_object(), "mock 回的是 `{{}}`，应解析为空对象");
}
```

`src/lib.rs` 底部现有的测试模块（`#[cfg(test)] mod tests {`, `src/lib.rs:103`）**第一行**加声明：

```rust
    mod resilience;
```

（写在 `mod tests` **里面**：文件落在 `src/tests/resilience.rs`，与 5a 的两个试点同构。
写在 `mod tests` 外面会让 `use super::*;` 指向 crate root 的私有项，且 `cfg(test)` 之外无法访问私有字段。）

判据：

```bash
export CARGO_TARGET_DIR=/var/tmp/ecat-target
cargo test -p ecat-data-arangodb 2>&1 | tail -3
```

期望 `test result: ok. 13 passed; 0 failed`（= 基线 7 + 新增 6）。**数字对不上就是 `mod resilience;` 没生效。**

### Step 9（3 分钟）带 feature 再跑一遍

```bash
export CARGO_TARGET_DIR=/var/tmp/ecat-target
cargo test -p ecat-data-arangodb --features metrics 2>&1 | tail -3
```

期望 `test result: ok. 14 passed; 0 failed`（= 13 + metrics 的 1 条）。

### Step 10（5 分钟）空验收自证

逐条问「**删掉这件新增物，上面哪条验收还绿？**」——绿的话那条验收是空的：

| 删掉什么 | 哪条必须变红 |
|---------|-------------|
| `mod resilience;` 这一行 | 第 8 步的 13 → 7（**当场跑一次验证**，2 分钟，最便宜的一条） |
| `execute` 外的 `self.guarded(async { … }).await`（拆回原体） | `execute_times_out_and_counts_graph_dimension` |
| `from_config` 里的 `semaphore: …cfg.max_concurrency…` 行（写死 32） | `config_wires_timeout_concurrency_and_breaker` + `concurrency_cap_limits_in_flight_requests` |
| `from_config` 里的 `breaker: …cfg.breaker…` 行 | `config_wires_timeout_concurrency_and_breaker` |
| **只改 `src/metrics.rs` 里注册闭包那一行**（`Box::new(|| timeout_counter(BackendKind::Graph)…)` → `BackendKind::Search`）；⚠️ **不要全局替换**：该串在 `metrics.rs` 出现 ≥3 次（注册行 + 测试自己的 `fetch_add` + rustdoc），全局改会把测试的**写槽**也改掉 ⇒ 读槽与写槽一起移动 ⇒ **探针静默变绿**（Task 3 实测踩到，第一遍就绿了） | `outbound_metrics_appear_with_live_values`（另两份样本仍出现，值差 1000） |
| `let _permit = self.permit().await;` 改成 `.forget()` | `timed_out_request_returns_its_permit` |

第一条**必须真跑**；其余靠读代码确认（它们都是「删掉就少一条断言/少一个样本」的形状）。

### Step 11（5 分钟）红探针（写进提交信息）

把 `guarded` 里的 `run_with_timeout(BackendKind::Graph, self.query_timeout, fut)` 临时换成 `fut`，
跑：

```bash
export CARGO_TARGET_DIR=/var/tmp/ecat-target
cargo test -p ecat-data-arangodb execute_times_out 2>&1 | tail -12
```

期望：**失败**（`内层超时没开火（漏包 guarded？）` 或超时后 5 秒 mock 才回、`expect_err` 落空）。
把这段输出贴进提交信息，然后**改回来**再跑一次确认转绿。

### Step 12（3 分钟）任务级闸门

```bash
export CARGO_TARGET_DIR=/var/tmp/ecat-target
cargo fmt -p ecat-data-arangodb
cargo clippy -p ecat-data-arangodb --all-targets -- -D warnings 2>&1 | tail -5
```

期望：`cargo fmt` 无输出（或只打印改动文件）；clippy 无 warning、无 error。
**不许 `--all` / `--workspace`** —— 那会排版到别人正在飞的文件。

### Step 13（3 分钟）提交

```bash
cd /home/wwwroot/e-cat
# 两个新建文件必须先 add 且**只 add 这两份**：`commit --only` 只接受 git 已知路径，
# 未跟踪的新文件会报「路径规格 … 未匹配任何 git 已知文件」（Task 1 实测）。
git add ecat-data-arangodb/src/metrics.rs ecat-data-arangodb/src/tests/resilience.rs
# Cargo.lock 要进清单：新增依赖会改它，不提交则 `cargo build --locked` 与工作区不一致
# （5a 先例 dee56c6 包含 Cargo.lock；Task 1 实测 `cargo metadata --locked` 通过）。
git commit --only Cargo.lock ecat-data-arangodb/Cargo.toml ecat-data-arangodb/src/lib.rs \
  ecat-data-arangodb/src/metrics.rs ecat-data-arangodb/src/tests/resilience.rs \
  -m "feat(ecat-data-arangodb): 出站韧性 —— 超时/熔断/并发上限 + metrics

- guarded 顺序：许可 → 熔断 → 超时（与 spec §3 相反，理由同批次 5a）
- 配置新增 query_timeout_secs / breaker / max_concurrency
- 指标标签 arangodb（= 配置节名），kind = Graph
- 基线 7 条 + 新增 6 条（默认）/ 7 条（--features metrics），实测见下
- 红探针：拆掉 guarded 后 execute_times_out… 报 ____（贴输出）"
```

> **其余任务的路径清单同理**：新建文件先 `git add`（只 add 本任务的新文件）；
> 清单里带上 `Cargo.lock`。

---

## Task 2：`ecat-data-neo4j`

**Files**：`ecat-data-neo4j/Cargo.toml`、`src/lib.rs`、`src/metrics.rs`（新建）、`src/tests/resilience.rs`（新建）。
**与 Task 1 的差异**（其余步骤逐字照抄 Task 1 的 Step 2/3/5/7，只换下面点名的词）：

| 项 | 值 |
|----|----|
| 基线 | `cargo test -p ecat-data-neo4j` → **4 passed**；`wc -l src/lib.rs` → 214 |
| kind / 标签 / slug | `BackendKind::Graph` / `"neo4j"` / `"graph"` |
| 构造器 | `new` + `from_config`（两个，同 Task 1） |
| 配置必填字段（`client_at` 用） | `base_url`, `username`, `password` |
| 方法数 | 1（`execute`） |
| 期望测试数 | 4 + 6 = **10**；`--features metrics` → **11** |
| 把守测试 | 无 |

**Step 1（2 分钟）基线**：同 Task 1 Step 1，crate 名换成 `ecat-data-neo4j`，期望 4 / 214。

**Step 2（3 分钟）`Cargo.toml`**：逐字抄 Task 1 Step 2（三处），判据 `grep -c 'ecat-metrics' ecat-data-neo4j/Cargo.toml` 期望 `4`（含两行注释，见 T0-A 自证）。

**Step 3（3 分钟）配置字段 + `query_timeout`**：逐字抄 Task 1 Step 3（`Neo4jConfig` 的 `tls` 字段之后插入同一块）。

**Step 4（5 分钟）结构体 + 构造器 + `use`**：`Neo4jClient` 末尾追加 `query_timeout` / `breaker` / `semaphore`
三个字段（逐字同 Task 1 Step 4）；`new` 与 `from_config` 的 `Self { ... }` 里追加同样三行装配；
`use` 块整体替换成 Task 1 Step 4 的那份（含 `use std::sync::Arc;` 等三行）。

**Step 5（5 分钟）访问器 / 许可 / `guarded`**：逐字抄 Task 1 Step 5，**只把两处词换掉**：

```rust
            .call(|| run_with_timeout(BackendKind::Graph, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "neo4j"))
```

**Step 6（5 分钟）包装 `execute`**（`src/lib.rs:52-77` 整体替换）：

```rust
    async fn execute(
        &self,
        cypher: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        let body = serde_json::json!({"statements": [{"statement": cypher, "parameters": params}]});
        self.guarded(async {
            let resp = self
                .client
                .post(format!("{}/db/data/transaction/commit", self.base_url))
                .basic_auth(&self.username, Some(&self.password))
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "neo4j", format!("neo4j: {e}")))?;
            if !resp.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Internal,
                    "neo4j",
                    resp.text().await.unwrap_or_default(),
                ));
            }
            resp.json().await.map_err(|e| {
                Error::new(ErrorCode::Internal, "neo4j", format!("neo4j parse: {e}"))
            })
        })
        .await
    }
```

判据：`cargo check -p ecat-data-neo4j` → `Finished`。

**Step 7（5 分钟）`src/metrics.rs`**：逐字抄 Task 1 Step 7，替换 `arangodb`→`neo4j`（3 处标签）、
`BackendKind::Graph` 不变（本来就是 Graph）。

**Step 8（8 分钟）`src/tests/resilience.rs`**：逐字抄 Task 1 Step 8，替换：

- `client_at` 的 JSON 体：`r#"{{"base_url": "{url}", "username": "neo4j", "password": "s",
  "query_timeout_secs": {timeout_secs}{mc}}}"#`（**没有** `db` 字段）。
- 主超时用例名 `execute_times_out_and_counts_graph_dimension` 不变；调用点改成
  `ecat_data::GraphClient::execute(&c, "MATCH (n) RETURN n", &serde_json::json!({}))`。
- 熔断用例里的 `err.reason` 断言改成 `"neo4j"`。
- 其余（`spawn_slow` / `spawn_slow_once` / `assert_times_out` / 见证槽 `Storage` / 六个用例名）逐字不动。

`mod resilience;` 加在 `src/lib.rs:80` 的 `mod tests` 第一行。

**Step 9（3 分钟）跑测试**：`cargo test -p ecat-data-neo4j` → 期望 **10 passed**；
`cargo test -p ecat-data-neo4j --features metrics` → 期望 **11 passed**。

**Step 10（3 分钟）空验收自证**：同 Task 1 Step 10 的表（把 `Graph` 用例名换成 neo4j 的；
`db` 字段那条不存在，删表里对应行）。

**Step 11（5 分钟）红探针 + 闸门 + 提交**：`cargo fmt -p ecat-data-neo4j`、`cargo clippy -p ecat-data-neo4j --all-targets -- -D warnings`，
然后：

```bash
git commit --only Cargo.lock ecat-data-neo4j/Cargo.toml ecat-data-neo4j/src/lib.rs \
  ecat-data-neo4j/src/metrics.rs ecat-data-neo4j/src/tests/resilience.rs \
  -m "feat(ecat-data-neo4j): 出站韧性 —— 超时/熔断/并发上限 + metrics

- guarded：许可 → 熔断 → 超时；kind = Graph，标签 neo4j
- 基线 4 + 新增 6 = 10（--features metrics 11）
- 红探针：____"
```

---

## Task 3：`ecat-data-nebulagraph`（本地早退必须留在 `guarded` 外）

**Files**：`ecat-data-nebulagraph/Cargo.toml`、`src/lib.rs`、`src/metrics.rs`（新建）、`src/tests/resilience.rs`（新建）。

| 项 | 值 |
|----|----|
| 基线 | `cargo test -p ecat-data-nebulagraph` → **7 passed**；`src/lib.rs` → 263 行 |
| kind / 标签 / slug | `BackendKind::Graph` / `"nebulagraph"` / `"graph"` |
| 构造器 | `new` / `with_auth` / `from_config`（**三个**，装配三行都要加） |
| 配置必填字段 | `base_url`, `space`（`client_at` 加这两个；`username`/`password` 可省） |
| 方法数 | 1（`execute`，含本地早退分支） |
| 期望测试数 | 7 + 7 = **14**；`--features metrics` → **15** |
| 把守测试 | `params_not_supported_does_not_touch_the_breaker` |

**Step 1（2 分钟）基线**：期望 7 / 263。

**Step 2（3 分钟）`Cargo.toml`**：逐字抄 Task 1 Step 2。

**Step 3（3 分钟）配置字段 + `query_timeout`**：逐字抄 Task 1 Step 3（插在 `NebulaGraphConfig` 的 `tls` 之后）。

**Step 4（5 分钟）结构体 + **三**个构造器 + `use`**：字段同 Task 1 Step 4；
`new`（`src/lib.rs:29-37`）、`with_auth`（`:39-52`）里的裸 `reqwest::Client::new()` 路径装配：

```rust
            query_timeout: query_timeout(None),
            breaker: Arc::new(Breaker::new(BreakerConfig::default())),
            semaphore: Arc::new(Semaphore::new(32)),
```

`from_config`（`:54-64`）装配走配置：

```rust
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
            semaphore: Arc::new(Semaphore::new(cfg.max_concurrency.unwrap_or(32))),
```

`use` 同 Task 1 Step 4。

**Step 5（5 分钟）访问器 / 许可 / `guarded`**：逐字抄 Task 1 Step 5，两处词替换成
`run_with_timeout(BackendKind::Graph, self.query_timeout, fut)` 与 `breaker_error_to_backend_error(e, "nebulagraph")`。

**Step 6（6 分钟）包装 `execute`**（`src/lib.rs:73-104` 整体替换）——**本地早退留在外面**，这是本 crate 的重点：

```rust
    async fn execute(
        &self,
        ngql: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        // 纯本地分支：**留在 `guarded` 外面**。半开态下每一次 `call` 都会借走一个
        // 探测名额，而「没发请求就返回」会以 `Ok` 记账 —— 名额被白白消耗，熔断器
        // 还可能被**误关回 `Closed`**。记账口径是「后端的表现」，不是「函数的返回值」。
        // （测试 `params_not_supported_does_not_touch_the_breaker` 盯着这条。）
        if !params.is_null() {
            return Err(Error::new(
                ErrorCode::Internal,
                "nebula",
                "params not supported",
            ));
        }
        // 请求构造（纯本地，不发 I/O）也可以留在外面；只有 `.send()` 那段进外壳。
        let req = self
            .client
            .post(format!("{}/api/ngql/execute", self.base_url))
            .json(&serde_json::json!({"gql": ngql, "space": self.space}));
        self.guarded(async {
            let resp = self.apply_auth(req).send().await.map_err(|e| {
                Error::new(ErrorCode::Internal, "nebula", format!("nebula: {e}"))
            })?;
            if !resp.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Internal,
                    "nebula",
                    resp.text().await.unwrap_or_default(),
                ));
            }
            resp.json().await.map_err(|e| {
                Error::new(ErrorCode::Internal, "nebula", format!("nebula parse: {e}"))
            })
        })
        .await
    }
```

判据：`cargo check -p ecat-data-nebulagraph` → `Finished`。

**Step 7（5 分钟）`src/metrics.rs`**：逐字抄 Task 1 Step 7，标签 `arangodb`→`nebulagraph`（3 处），kind 不变。

**Step 8（8 分钟）`src/tests/resilience.rs`**：逐字抄 Task 1 Step 8，替换：

- `client_at` 的 JSON 体：`r#"{{"base_url": "{url}", "space": "test_space",
  "query_timeout_secs": {timeout_secs}{mc}}}"#`。
- 调用点：`ecat_data::GraphClient::execute(&c, "SHOW SPACES", &serde_json::Value::Null)`
  （**必须 `Null`**，非 null 会命中早退分支）。
- 熔断用例的 `err.reason` 断言 → `"nebulagraph"`。
- 追加第 7 条（把守测试，逐字）：

```rust
/// **早退分支绝不能碰熔断器**（checklist §7 陷阱 2/3）。
/// 8 次「params not supported」若走了 `guarded`，就会以 8 次失败记进窗口、
/// 把熔断器打开 —— 之后**正常的 Null 查询全被拒绝**。
#[tokio::test]
async fn params_not_supported_does_not_touch_the_breaker() {
    let url = spawn_slow(Duration::from_millis(10), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 30, None);
    for _ in 0..8 {
        let err = ecat_data::GraphClient::execute(&c, "SHOW SPACES", &serde_json::json!({"x": 1}))
            .await
            .expect_err("params 非 null 必须早退报错");
        assert!(err.to_string().contains("params not supported"), "got: {err}");
    }
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "本地早退不是后端故障，不得计入熔断窗口"
    );
    assert_eq!(c.breaker().opened_total(), 0);
}
```

**Step 9（3 分钟）跑测试**：期望 **14 passed**；`--features metrics` → **15 passed**。

**Step 10（5 分钟）空验收自证**：Task 1 Step 10 的表 + 一条本 crate 专属：
**把 `if !params.is_null()` 那三行挪进 `guarded` 里面** ⇒ `params_not_supported_does_not_touch_the_breaker` 必红（8 次失败 > 5 次阈值）。

**Step 11（5 分钟）红探针 + 闸门 + 提交**（`git commit --only` 同样的路径 + **`Cargo.lock`**，消息同构）。

---

## Task 4：`ecat-data-elasticsearch`（3 条 I/O 路径 + 2 个 trait 默认）

**Files**：`ecat-data-elasticsearch/Cargo.toml`、`src/lib.rs`、`src/metrics.rs`（新建）、`src/tests/resilience.rs`（新建）。

| 项 | 值 |
|----|----|
| 基线 | `cargo test -p ecat-data-elasticsearch` → **11 passed**；`src/lib.rs` → 347 行 |
| kind / 标签 / slug | `BackendKind::Search` / `"elasticsearch"` / `"search"` |
| 构造器 | `new` / `with_auth` / `from_config`（三个） |
| 配置必填字段 | `base_url`（`username`/`password` 可省） |
| 要包的方法 | `index` / `search` / `delete` |
| **不包** | `bulk_index` / `update`（trait 默认实现，`ecat-data/src/search.rs:17-34`） |
| 期望测试数 | 11 + 8 = **19**；`--features metrics` → **20** |

**Step 1（2 分钟）基线**：期望 11 / 347。

**Step 2（3 分钟）`Cargo.toml`**：逐字抄 Task 1 Step 2。
**Step 3（3 分钟）配置字段 + `query_timeout`**：逐字抄 Task 1 Step 3（插在 `ElasticsearchConfig` 的 `tls` 之后）。
**Step 4（5 分钟）结构体 + 三个构造器 + `use`**：三个构造器（`new`/`with_auth`/`from_config`）
的装配同 Task 3 Step 4（裸构造器写死 32，`from_config` 走配置）。
**Step 5（5 分钟）访问器 / 许可 / `guarded`**：逐字抄 Task 1 Step 5，两处词换成
`run_with_timeout(BackendKind::Search, self.query_timeout, fut)` 与 `breaker_error_to_backend_error(e, "elasticsearch")`。

**Step 6（8 分钟）包装三个方法**（`src/lib.rs:101-164` 整体替换；`status_error` 是本地 helper，**保持不动**）：

```rust
    async fn index(&self, index: &str, id: &str, doc: &serde_json::Value) -> Result<(), Error> {
        let req = self
            .client
            .put(format!(
                "{}/{}/_doc/{}",
                self.base_url,
                percent_encode_segment(index),
                percent_encode_segment(id)
            ))
            .json(doc);
        self.guarded(async {
            let resp = self
                .apply_auth(req)
                .send()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "es", format!("es index: {e}")))?;
            if !resp.status().is_success() {
                return Err(status_error("es index", resp).await);
            }
            Ok(())
        })
        .await
    }

    async fn search(
        &self,
        index: &str,
        query: &serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        let req = self
            .client
            .post(format!(
                "{}/{}/_search",
                self.base_url,
                percent_encode_segment(index)
            ))
            .json(query);
        self.guarded(async {
            let resp = self
                .apply_auth(req)
                .send()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "es", format!("es search: {e}")))?;
            if !resp.status().is_success() {
                return Err(status_error("es search", resp).await);
            }
            resp.json().await.map_err(|e| {
                Error::new(ErrorCode::Internal, "es", format!("es parse: {e}"))
            })
        })
        .await
    }

    async fn delete(&self, index: &str, id: &str) -> Result<(), Error> {
        let req = self.client.delete(format!(
            "{}/{}/_doc/{}",
            self.base_url,
            percent_encode_segment(index),
            percent_encode_segment(id)
        ));
        self.guarded(async {
            let resp = self
                .apply_auth(req)
                .send()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "es", format!("es delete: {e}")))?;
            if !resp.status().is_success() {
                return Err(status_error("es delete", resp).await);
            }
            Ok(())
        })
        .await
    }
```

判据：`cargo check -p ecat-data-elasticsearch` → `Finished`。

**Step 7（5 分钟）`src/metrics.rs`**：逐字抄 Task 1 Step 7，标签 `arangodb`→`elasticsearch`（3 处），
`BackendKind::Graph`→`BackendKind::Search`（1 处，在 `register_outbound_metrics` 里）。

**Step 8（10 分钟）`src/tests/resilience.rs`**：逐字抄 Task 1 Step 8 的四个 helper
（`spawn_slow` / `spawn_slow_once` / `assert_times_out` / `client_at`），替换：

- `assert_times_out` 里 `BackendKind::Graph` → `BackendKind::Search`，`err.reason` 断言 → `"search"`。
- `client_at` 的 JSON 体：`r#"{{"base_url": "{url}", "query_timeout_secs": {timeout_secs}{mc}}}"#`。

用例（**8 条**）：前 7 条同 Task 1（`config_wires_timeout_concurrency_and_breaker` /
`zero_timeout_means_disabled` / `search_times_out_and_counts_search_dimension` /
`every_io_method_times_out_when_the_backend_stalls` / `repeated_timeouts_open_the_breaker_and_fail_fast` /
`concurrency_cap_limits_in_flight_requests` / `timed_out_request_returns_its_permit`），
其中三条要按本 crate 改写：

```rust
/// 三个 I/O 方法各打一次（3 次 < 5 条窗口 ⇒ 熔断不会在途中打开）。
#[tokio::test]
async fn every_io_method_times_out_when_the_backend_stalls() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    let doc = serde_json::json!({"msg": "hi"});
    assert_times_out("index", ecat_data::SearchClient::index(&c, "idx", "1", &doc)).await;
    assert_times_out("search", ecat_data::SearchClient::search(&c, "idx", &doc)).await;
    assert_times_out("delete", ecat_data::SearchClient::delete(&c, "idx", "1")).await;
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "3 次调用打不满 5 条窗口"
    );
}
```

```rust
/// 模式 ④ 用 `index`：mock 第二次回 `{}`，`index` 只看状态码 ⇒ 必须成功。
#[tokio::test]
async fn timed_out_request_returns_its_permit() {
    let url = spawn_slow_once(Duration::from_secs(5)).await;
    let c = client_at(&url, 1, Some(1));
    let first = ecat_data::SearchClient::index(&c, "idx", "1", &serde_json::json!({}))
        .await
        .expect_err("第一发必须超时");
    assert_eq!(first.code, ErrorCode::DeadlineExceeded, "got: {first}");
    tokio::time::timeout(
        Duration::from_secs(2),
        ecat_data::SearchClient::index(&c, "idx", "1", &serde_json::json!({})),
    )
    .await
    .expect("第二发被排在许可上（超时路径没归还许可？）")
    .expect("许可归还后第二次必须成功");
}
```

第 8 条（把守测试，逐字）：

```rust
/// **`bulk_index` / `update` 落到 trait 默认实现，绝不触碰熔断器。**
/// 默认返回「不支持」—— 那是**调用方的用法错**，不是后端故障。
/// 若有人给 ES 补上这两个方法并"顺手"包进 `guarded`，5 次「不支持」就会把
/// 熔断器打开，之后**正常检索全被拒绝**。
#[tokio::test]
async fn unsupported_ops_do_not_trip_the_breaker() {
    let url = spawn_slow(Duration::from_millis(10), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 30, None);
    let docs = [("1".to_string(), serde_json::json!({}))];
    for _ in 0..8 {
        assert!(ecat_data::SearchClient::bulk_index(&c, "idx", &docs).await.is_err());
        assert!(
            ecat_data::SearchClient::update(&c, "idx", "1", &serde_json::json!({}))
                .await
                .is_err()
        );
    }
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "「不支持」不是后端故障，不得计入熔断窗口"
    );
    assert_eq!(c.breaker().opened_total(), 0);
}
```

`mod resilience;` 加在 `src/lib.rs:168` 的 `mod tests` 第一行。

**Step 9（3 分钟）跑测试**：期望 **19 passed**；`--features metrics` → **20 passed**。
**Step 10（5 分钟）空验收自证**：Task 1 Step 10 的表 + 本 crate 专属两条：
删掉 `delete` 的 `guarded` ⇒ `every_io_method_times_out…` 红；把 `bulk_index` 默认实现覆写进 `guarded` ⇒ `unsupported_ops_do_not_trip_the_breaker` 红。
**Step 11（5 分钟）红探针 + 闸门 + 提交**（`cargo fmt -p ecat-data-elasticsearch`、clippy、`git commit --only` 路径 + **`Cargo.lock`**）。

---

## Task 5：`ecat-data-opensearch`（与 Task 4 同构）

**Files**：`ecat-data-opensearch/Cargo.toml`、`src/lib.rs`、`src/metrics.rs`、`src/tests/resilience.rs`。

| 项 | 值 |
|----|----|
| 基线 | **10 passed**；`src/lib.rs` → 336 行 |
| kind / 标签 / slug | `BackendKind::Search` / `"opensearch"` / `"search"` |
| 构造器 | `new` / `with_auth` / `from_config` |
| 期望测试数 | 10 + 8 = **18**；`--features metrics` → **19** |

**做法**：**逐字照做 Task 4 的全部 11 步**，只换这些词：

⚠️ **两条 Task 4 落地时实测出来的更正**（照抄前必读，否则会撞）：

1. **模式 ④ 片段要删掉 `let second =` 绑定**：计划 Task 4 Step 8 写的是 `let second = tokio::time::timeout(…).await.expect(…).expect(…)`，但 `index` 返回 `()` ⇒ `second` 未被使用 ⇒ **clippy `-D warnings` 会红**（`unused_variables`）。Task 4 实测后去掉绑定、只留两条 `.expect(…)`。
   opensearch 的 `index` 同样返回 `()`，**照做时会撞同一条**。
2. **`src/lib.rs` 行数余量只剩 52**：Task 4 落完是 **448 行**（= 500 的 90%）。opensearch 基线 336，按同构估计落在 **~437**；**若还需要加用例或注释，先按 T0-H 拆 `src/tests.rs`**（influxdb 那套做法），别让它顶破 500。

- crate 名 `ecat-data-elasticsearch` → `ecat-data-opensearch`；`src/lib.rs` 行号：三个方法的
  原体在 `:101-161`，`mod tests` 在 `:164`。
- 标签 `"elasticsearch"` → `"opensearch"`（metrics.rs 3 处 + `breaker_error_to_backend_error` 一处）。
- **错误前缀**：opensearch 的 `Error::reason` 是 `"opensearch"`（`status_error` 里也是），
  但 `.map_err(...)` 的 message 前缀去掉 `es `（如 `format!("index: {e}")`、`format!("parse: {e}")`）——
  **保持原样不要统一**，5b 不改既有错误文本。
- 三个被包方法的**原体**逐字照抄 `ecat-data-opensearch/src/lib.rs:101-161`（与 ES 的差异只在
  message 前缀与缩进），包装方式、`status_error("index", resp)` 的调用形式完全相同。
- 测试里 `assert_times_out` 的 `err.reason` 断言 → `"search"`（**不是** `"opensearch"`：超时走
  `kind.slug()`）；熔断用例里 `err.reason` → `"opensearch"`（熔断走产品名）。
- 判据：`cargo test -p ecat-data-opensearch` → **18 passed**；`--features metrics` → **19**。

**Step 1-11** 同 Task 4（命令里的 crate 名替换即可）。

---

## 通用步骤：把内联测试拆成 `src/tests.rs`（Task 6/7/8/10 的 Step 3）

> 触发条件：`lib.rs` 现在 400+ 行，加上本批约 80~100 行生产代码会顶破 500 行硬上限。
> **不改任何测试内容**，只搬位置。拆完必须**测试数与基线一致**（这是本步的验收）。

1. 新建 `src/tests.rs`，写入头部两行：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 单元测试。独立成文件：`lib.rs` 加出站韧性后会顶到 500 行硬上限。
```

2. 把 `src/lib.rs` 里从 `#[cfg(test)]` 到文件末尾的整块（含 `mod tests {` 与配对的 `}`）**剪切**进
   `src/tests.rs`，去掉外层的 `#[cfg(test)] mod tests {` 与最后的 `}` —— 文件本身就是模块。
   块内原有的 `use super::*;` 与其它 `use` **逐字保留**：在 `src/tests.rs` 里 `super` 同样指向 crate root。
3. `src/lib.rs` 底部改回一行：

```rust
#[cfg(test)]
mod tests;
```

4. 判据（**必须跑**）：

```bash
export CARGO_TARGET_DIR=/var/tmp/ecat-target
cargo test -p <crate> 2>&1 | tail -3          # 期望与 Step 1 记录的基线**完全相同**
wc -l src/tests.rs src/lib.rs                 # 两个文件都 < 500
```

数不对 ⇒ 搬漏了（常见：`use` 行没带过来，或 `mod tests` 的收尾 `}` 多删了一层）。

---

## Task 6：`ecat-data-influxdb`

**Files**：`ecat-data-influxdb/Cargo.toml`、`src/lib.rs`、`src/tests.rs`（**新建**，Step 3）、
`src/tests/resilience.rs`（新建）、`src/metrics.rs`（新建）。

| 项 | 值 |
|----|----|
| 基线 | **10 passed**；`src/lib.rs` → 426 行（**必须拆**） |
| kind / 标签 / slug | `BackendKind::Tsdb` / `"influxdb"` / `"tsdb"` |
| 构造器 | `new` / `from_config`（两个） |
| 配置必填字段 | `base_url`, `org`, `bucket`, `token` |
| 要包的方法 | `write` / `query` |
| **不包** | `delete`（trait 默认，`ecat-data/src/tsdb.rs:55`）、`escape_line_part` / `escape_field_string`（纯本地） |
| 期望测试数 | 10 + 8 = **18**；`--features metrics` → **19** |

**Step 1（2 分钟）基线**：期望 10 / 426。
**Step 2（3 分钟）`Cargo.toml`**：逐字抄 Task 1 Step 2。
**Step 3（8 分钟）拆测试文件**：按上面的《通用步骤：把内联测试拆成 `src/tests.rs`》做
（本 crate 的测试块从 `src/lib.rs:192` 的 `#[cfg(test)]` 到文件末尾 `:426`）。判据：仍是 **10 passed**，
且 `wc -l` 显示 `src/lib.rs` ≈ 190 行。
**Step 4（3 分钟）配置字段 + `query_timeout`**：逐字抄 Task 1 Step 3（插在 `InfluxConfig` 的 `tls` 之后）。
**Step 5（5 分钟）结构体 + 两个构造器 + `use`**：`InfluxClient` 末尾追加三个字段；
`new`（`src/lib.rs:34-49`，裸 client）与 `from_config`（`:51-63`）分别写死 32 / 走配置；
`use` 同 Task 1 Step 4。
**Step 6（5 分钟）访问器 / 许可 / `guarded`**：逐字抄 Task 1 Step 5，两处词换成
`run_with_timeout(BackendKind::Tsdb, self.query_timeout, fut)` 与 `breaker_error_to_backend_error(e, "influxdb")`。

**Step 7（8 分钟）包装 `write` / `query`**：
- `write`：`src/lib.rs:103-141` 的行协议构造（`let mut lines = String::new();` 到 `lines.push('\n');` 的
  整个 `for p in points` 循环）**留在外面**（纯本地），把 `:143` 起替换为：

```rust
        self.guarded(async {
            let resp = self
                .client
                .post(&self.write_url)
                .header("Authorization", format!("Token {}", self.token))
                .header("Content-Type", "text/plain; charset=utf-8")
                .query(&[
                    ("org", &self.org),
                    ("bucket", &self.bucket),
                    ("precision", &"ns".to_string()),
                ])
                .body(lines)
                .send()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "influx", format!("write: {e}")))?;

            if !resp.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Internal,
                    "influx",
                    format!("write failed: {}", resp.text().await.unwrap_or_default()),
                ));
            }
            Ok(())
        })
        .await
    }
```

- `query`（`:168-189`）：整个函数体进外壳，`async { ... }` 里的内容一字不改（`String` 体构造是纯本地的，
  放里面更省事，也不影响记账口径）：

```rust
    async fn query(&self, query: &str) -> Result<serde_json::Value, Error> {
        self.guarded(async {
            let resp = self
                .client
                .post(&self.query_url)
                .header("Authorization", format!("Token {}", self.token))
                .header("Content-Type", "application/vnd.flux")
                .query(&[("org", &self.org)])
                .body(query.to_string())
                .send()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "influx", format!("query: {e}")))?;
            if !resp.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Internal,
                    "influx",
                    format!("query failed: {}", resp.text().await.unwrap_or_default()),
                ));
            }
            resp.json()
                .await
                .map_err(|e| Error::new(ErrorCode::Internal, "influx", format!("parse: {e}")))
        })
        .await
    }
```

判据：`cargo check -p ecat-data-influxdb` → `Finished`。

**Step 8（5 分钟）`src/metrics.rs`**：逐字抄 Task 1 Step 7，标签 `arangodb`→`influxdb`（3 处），
`BackendKind::Graph`→`BackendKind::Tsdb`。

**Step 9（10 分钟）`src/tests/resilience.rs`**：逐字抄 Task 1 Step 8 的四个 helper 与 6 条用例，替换：

- `assert_times_out`：`BackendKind::Graph`→`BackendKind::Tsdb`，`err.reason` → `"tsdb"`，见证槽仍 `Storage`。
- `client_at` 的 JSON 体：`r#"{{"base_url": "{url}", "org": "o", "bucket": "b", "token": "t",
  "query_timeout_secs": {timeout_secs}{mc}}}"#`。
- 主超时用例名 `write_times_out_and_counts_tsdb_dimension`，调用
  `ecat_data::TsdbClient::write(&c, &[DataPoint::new("cpu").with_field("v", FieldValue::Int(1))])`。
- 模式 ④ 用 `write`（`{}` 体只看状态码）：
  `ecat_data::TsdbClient::write(&c, &[DataPoint::new("cpu").with_field("v", FieldValue::Int(1))])`。
- 熔断用例调用点同上，`err.reason` 断言 → `"influxdb"`。
- 追加**覆盖用例**（write/query 各一次 = 2 次 < 5 条窗口）：

```rust
/// 两条 I/O 路径各打一次（2 次 < 5 条窗口 ⇒ 熔断不会在途中打开）。
#[tokio::test]
async fn every_io_method_times_out_when_the_backend_stalls() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    assert_times_out(
        "write",
        ecat_data::TsdbClient::write(&c, &[DataPoint::new("cpu").with_field("v", FieldValue::Int(1))]),
    )
    .await;
    assert_times_out("query", ecat_data::TsdbClient::query(&c, "from(bucket: \"b\")")).await;
    assert_eq!(c.breaker().state(), BreakerState::Closed);
}
```

- 追加**把守用例**：

```rust
/// `delete` 落到 trait 默认实现（`ecat-data/src/tsdb.rs:55`），绝不触碰熔断器。
#[tokio::test]
async fn delete_default_does_not_trip_the_breaker() {
    let url = spawn_slow(Duration::from_millis(10), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 30, None);
    for _ in 0..8 {
        assert!(
            ecat_data::TsdbClient::delete(&c, "DELETE FROM cpu").await.is_err()
        );
    }
    assert_eq!(c.breaker().state(), BreakerState::Closed);
    assert_eq!(c.breaker().opened_total(), 0);
}
```

`mod resilience;` 加在 `src/tests.rs` 的**第一行**（`mod tests` 已经变成独立文件，声明放文件顶部）。

**Step 10（3 分钟）跑测试**：期望 **18 passed**；`--features metrics` → **19 passed**。
**Step 11（5 分钟）空验收自证**：Task 1 Step 10 的表 + 拆文件那条：把 `mod resilience;` 注释掉 ⇒ 回到 10；
删 `write` 的 `guarded` ⇒ `write_times_out_…` 与 `every_io_method_…` 都红。
**Step 12（5 分钟）红探针 + 闸门 + 提交**：

```bash
export CARGO_TARGET_DIR=/var/tmp/ecat-target
cargo fmt -p ecat-data-influxdb
cargo clippy -p ecat-data-influxdb --all-targets -- -D warnings 2>&1 | tail -5
git commit --only Cargo.lock ecat-data-influxdb/Cargo.toml ecat-data-influxdb/src/lib.rs \
  ecat-data-influxdb/src/tests.rs ecat-data-influxdb/src/tests/resilience.rs \
  ecat-data-influxdb/src/metrics.rs -m "feat(ecat-data-influxdb): 出站韧性 + metrics

- guarded：许可 → 熔断 → 超时；kind = Tsdb，标签 influxdb
- 测试从 lib.rs 拆到 src/tests.rs（行数 426 → 逼近 500 上限），拆分前后同为 10 条
- 基线 10 + 新增 8 = 18（--features metrics 19）
- 红探针：____"
```

---

## Task 7：`ecat-data-iotdb`（一次 `write` = 多个 HTTP，必须整体一个预算）

**Files**：`ecat-data-iotdb/Cargo.toml`、`src/lib.rs`、`src/tests.rs`（新建，Step 3）、
`src/tests/resilience.rs`（新建）、`src/metrics.rs`（新建）。

| 项 | 值 |
|----|----|
| 基线 | **10 passed**；`src/lib.rs` → 427 行（**必须拆**） |
| kind / 标签 / slug | `BackendKind::Tsdb` / `"iotdb"` / `"tsdb"` |
| 构造器 | `new` / `from_config` |
| 配置必填字段 | `base_url`, `username`, `password` |
| 要包的方法 | `write`（**整个循环一个预算**）/ `query` |
| **不包** | `delete`（trait 默认） |
| 期望测试数 | 10 + 9 = **19**；`--features metrics` → **20** |

**Step 1（2 分钟）基线**：期望 10 / 427。
**Step 2（3 分钟）`Cargo.toml`**：逐字抄 Task 1 Step 2。
**Step 3（8 分钟）拆测试文件**：测试块从 `src/lib.rs:147` 到文件末尾 `:427`；判据仍是 **10 passed**。
**Step 4（3 分钟）配置字段 + `query_timeout`**：逐字抄 Task 1 Step 3（插在 `IotdbConfig` 的 `tls` 之后）。
**Step 5（5 分钟）结构体 + 两个构造器 + `use`**：装配规则同 Task 6 Step 5（`new` 写死 32 / `from_config` 走配置）。
**Step 6（5 分钟）访问器 / 许可 / `guarded`**：逐字抄 Task 1 Step 5，两处词换成
`run_with_timeout(BackendKind::Tsdb, self.query_timeout, fut)` 与 `breaker_error_to_backend_error(e, "iotdb")`。

**Step 7（8 分钟）包装 `write` / `query`**：

- `write`（`src/lib.rs:52-122`）：在 `for p in points {` **之前**插入 `self.guarded(async {`，
  在末尾的 `Ok(())` 之后接 `})` 与 `.await`；循环体一字不改（缩进交给 `cargo fmt`）。改完形态：

```rust
    async fn write(&self, points: &[DataPoint]) -> Result<(), Error> {
        // 整个循环**一个预算**：本方法每个点发一次 POST（`rest/v2/insertTablet`），
        // 预算必须罩住整次调用 —— 包在循环里面就会变成「每个点一个预算」，
        // 一次 write 的墙钟上限随点数线性放大（测试 whole_call_budget_… 盯着这条）。
        self.guarded(async {
            for p in points {
                /* ……原体一字不改…… */
            }
            Ok(())
        })
        .await
    }
```

- `query`（`:124-144`）：函数体整体进外壳（`sql` 是入参，`.body(sql.to_string())` 在块内构造）。

判据：`cargo check -p ecat-data-iotdb` → `Finished`。

**Step 8（5 分钟）`src/metrics.rs`**：逐字抄 Task 1 Step 7，标签 → `"iotdb"`，kind → `BackendKind::Tsdb`。

**Step 9（12 分钟）`src/tests/resilience.rs`**：逐字抄 Task 1 Step 8 的四个 helper 与 6 条用例，替换：

- `assert_times_out`：kind → `BackendKind::Tsdb`，`err.reason` → `"tsdb"`，见证槽仍 `Storage`。
- `client_at` 的 JSON 体：`r#"{{"base_url": "{url}", "username": "root", "password": "root",
  "query_timeout_secs": {timeout_secs}{mc}}}"#`。
- 主超时用例名 `write_times_out_and_counts_tsdb_dimension`；模式 ④ 也用 `write`；
  熔断用例调用点用 `write`，`err.reason` → `"iotdb"`。
- 追加**覆盖用例**、**把守用例**（`delete` 默认），同 Task 6 Step 9 的两段（把 `influxdb` 的
  `client_at` 字段换成本 crate 的）。
- 追加**整调用预算用例**（出入 13 的判据）：

```rust
/// **一次 `write` 只有一个预算**：3 个点 = 3 次 HTTP（各 600ms），预算 1 秒 ⇒ 必须整体超时。
/// 若有人把 `guarded` 包到循环**里**（每点一个预算），三次都各自成功 ⇒ **本用例红**。
#[tokio::test]
async fn whole_call_budget_covers_every_request_in_write() {
    let url = spawn_slow(Duration::from_millis(600), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    let points: Vec<DataPoint> = (0..3)
        .map(|i| DataPoint::new("cpu").with_field("v", FieldValue::Int(i)))
        .collect();
    assert_times_out("write(3 点)", ecat_data::TsdbClient::write(&c, &points)).await;
}
```

`mod resilience;` 加在 `src/tests.rs` 第一行。

**Step 10（3 分钟）跑测试**：期望 **19 passed**；`--features metrics` → **20 passed**。
**Step 11（5 分钟）空验收自证**：Task 1 表 + 本 crate 专属：
把 `guarded` 从 `write` 挪到循环内部 ⇒ `whole_call_budget_covers_every_request_in_write` 红（三次各自成功、返回 Ok）。
**Step 12（5 分钟）红探针 + 闸门 + 提交**：同 Task 6 Step 12（路径换成 ecat-data-iotdb 的五个文件）。

---

## Task 8：`ecat-data-tdengine`（内部 `exec` 绝不能包）

**Files**：`ecat-data-tdengine/Cargo.toml`、`src/lib.rs`、`src/tests.rs`（新建，Step 3）、
`src/tests/resilience.rs`（新建）、`src/metrics.rs`（新建）。

| 项 | 值 |
|----|----|
| 基线 | **11 passed**；`src/lib.rs` → 408 行（**必须拆**） |
| kind / 标签 / slug | `BackendKind::Tsdb` / `"tdengine"` / `"tsdb"` |
| 构造器 | `new` / `from_config` |
| 配置必填字段 | `base_url`, `username`, `password` |
| 要包的方法 | `write`（**整个分块循环一个预算**）/ `query` |
| **不包** | 私有 `exec`（内部 helper —— 包了就把一次调用切成 N 份）、`delete`（trait 默认）、`percent_encode_segment`/`escape_*`/`point_to_insert`/`sql_url`（纯本地） |
| 期望测试数 | 11 + 9 = **20**；`--features metrics` → **21** |

**Step 1（2 分钟）基线**：期望 11 / 408。
**Step 2（3 分钟）`Cargo.toml`**：逐字抄 Task 1 Step 2。
**Step 3（8 分钟）拆测试文件**：测试块从 `src/lib.rs:178` 到文件末尾 `:408`；判据仍是 **11 passed**。
**Step 4（3 分钟）配置字段 + `query_timeout`**：逐字抄 Task 1 Step 3（插在 `TdengineConfig` 的 `tls` 之后；
本 crate 已有的可选 `database` 字段不动）。
**Step 5（5 分钟）结构体 + 两个构造器 + `use`**：装配规则同 Task 6 Step 5。
**Step 6（5 分钟）访问器 / 许可 / `guarded`**：逐字抄 Task 1 Step 5，两处词换成
`run_with_timeout(BackendKind::Tsdb, self.query_timeout, fut)` 与 `breaker_error_to_backend_error(e, "tdengine")`。

**Step 7（8 分钟）包装 `write` / `query`**（`src/lib.rs:161-175` 整体替换）：

```rust
    async fn write(&self, points: &[DataPoint]) -> Result<(), Error> {
        // 整个分块循环**一个预算**：`exec` 是内部 helper（一次 HTTP），
        // 它自己**不包** —— 包 `exec` 会把一次 `write` 切成 N 个独立预算，
        // 墙钟上限随分批数放大，而且熔断窗口会被同一批数据记 N 次。
        self.guarded(async {
            for chunk in points.chunks(BATCH_SIZE) {
                let sql = chunk
                    .iter()
                    .map(point_to_insert)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.exec(&sql).await?;
            }
            Ok(())
        })
        .await
    }

    async fn query(&self, sql: &str) -> Result<serde_json::Value, Error> {
        self.guarded(async { self.exec(sql).await }).await
    }
```

`exec`（`:61-90`）与 `sql_url`（`:54-59`）**一字不改**。
判据：`cargo check -p ecat-data-tdengine` → `Finished`。

**Step 8（5 分钟）`src/metrics.rs`**：逐字抄 Task 1 Step 7，标签 → `"tdengine"`，kind → `BackendKind::Tsdb`。

**Step 9（12 分钟）`src/tests/resilience.rs`**：抄 Task 1 Step 8 的四个 helper 与 6 条用例 + Task 7 Step 9 的
覆盖/把守/整调用预算三条，替换：

- `client_at` 的 JSON 体：`r#"{{"base_url": "{url}", "username": "root", "password": "taosdata",
  "query_timeout_secs": {timeout_secs}{mc}}}"#`。
- **模式 ④ 用 `query`**（`{}` 能直接解析成 `Value`）：`ecat_data::TsdbClient::query(&c, "SELECT 1")`。
- 主超时用例名 `query_times_out_and_counts_tsdb_dimension`，调用同上；`err.reason` → `"tsdb"`。
- 熔断用例调用点用 `query`，`err.reason` → `"tdengine"`。
- 覆盖用例：`query` + `write`（单点）各一次。
- **整调用预算用例改用 250 点**（3 批，`BATCH_SIZE = 100`）：

```rust
/// **一次 `write` 只有一个预算**（出入 13）：250 点分 3 批 = 3 次 HTTP（各 600ms），
/// 预算 1 秒 ⇒ 必须整体超时。把 `guarded` 包到 `exec` 上（每批一个预算）时
/// 三批都各自成功 ⇒ **本用例红**。
#[tokio::test]
async fn whole_call_budget_covers_every_batch_in_write() {
    let url = spawn_slow(Duration::from_millis(600), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    let points: Vec<DataPoint> = (0..250)
        .map(|i| DataPoint::new("m").with_field("v", FieldValue::Int(i)))
        .collect();
    assert_times_out("write(250 点 3 批)", ecat_data::TsdbClient::write(&c, &points)).await;
}
```

`mod resilience;` 加在 `src/tests.rs` 第一行。

**Step 10（3 分钟）跑测试**：期望 **20 passed**；`--features metrics` → **21 passed**。
**Step 11（5 分钟）空验收自证**：Task 1 表 + 两条专属：
(a) 把 `guarded` 从 `write` 挪到 `exec` 上 ⇒ `whole_call_budget_covers_every_batch_in_write` 与
`query_times_out_…`（多一层后仍超时，但 `write` 的那条会红）——**以预算用例为准**；
(b) 删掉 `query` 的 `guarded` ⇒ `query_times_out_…` 与 `every_io_method_…` 红。
**Step 12（5 分钟）红探针 + 闸门 + 提交**：同 Task 6 Step 12。

---

## Task 9：`ecat-data-questdb`（**`RdbmsError` 路径**，与其余 10 个不同）

**Files**：`ecat-data-questdb/Cargo.toml`、`src/lib.rs`、`src/metrics.rs`（新建）、`src/tests/resilience.rs`（新建）。
**没有 `ecat-errors` 依赖**（实测 `Cargo.toml` 的 `[dependencies]` 里就没有）⇒ `guarded` 走
`RdbmsError` + `ecat_data::map_breaker_error`，**与 `ecat-data-clickhouse` 的 `guarded` 同构**
（`ecat-data-clickhouse/src/lib.rs:170-183`），不是 Redis/变体 A。

| 项 | 值 |
|----|----|
| 基线 | **9 passed**；`src/lib.rs` → 270 行 |
| kind / 标签 / slug | `BackendKind::Rdbms` / `"questdb"` / `"rdbms"` |
| 构造器 | `new` / `with_auth` / `from_config`（`from_config` 返回 `Result<Self, RdbmsError>`） |
| 配置必填字段 | `base_url` |
| 要包的方法 | `execute` / `query` |
| **不包** | `transaction`（常量错误）、`dialect`（纯本地）、`apply_auth`（构造请求头） |
| 期望测试数 | 9 + 8 = **17**；`--features metrics` → **18** |

**Step 1（2 分钟）基线**：期望 9 / 270。
**Step 2（3 分钟）`Cargo.toml`**：逐字抄 Task 1 Step 2（本 crate 一样要加 `ecat-circuit-breaker` /
`tokio(sync)` / `ecat-metrics` optional / `[features] metrics` / dev-deps 加 `"time"`）。
**Step 3（3 分钟）配置字段 + `query_timeout`**：逐字抄 Task 1 Step 3（插在 `QuestdbConfig` 的 `tls` 之后）。

**Step 4（5 分钟）结构体 + 三个构造器 + `use`**：字段同 Task 1 Step 4；三个构造器（`new`/`with_auth`/`from_config`）
按 Task 3 Step 4 的规则装配（`from_config` 走配置，另两个写死 32）。顶部 `use`（`src/lib.rs:8-11`）整体替换为：

```rust
use async_trait::async_trait;
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{
    BackendKind, Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, map_breaker_error,
    run_with_timeout,
};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, SemaphorePermit};
```

**Step 5（5 分钟）访问器 / 许可 / `guarded`（`RdbmsError` 版）**：

```rust
    /// 本 client 的熔断器。`metrics` feature 注册指标时要读它的状态与打开次数。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }

    /// 取一个并发许可。信号量从不 `close()`，`AcquireError` 不可达。
    async fn permit(&self) -> SemaphorePermit<'_> {
        self.semaphore
            .acquire()
            .await
            .expect("semaphore is never closed")
    }

    /// 一次出站调用的外壳：**许可 → 熔断 → 超时**（理由见 T0 模板）。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, RdbmsError>
    where
        F: std::future::Future<Output = Result<T, RdbmsError>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Rdbms, self.query_timeout, fut))
            .await
            .map_err(map_breaker_error)
    }
```

**Step 6（6 分钟）包装 `execute` / `query`**（`src/lib.rs:72-143`；`transaction` 与 `dialect` 一字不改）：

```rust
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        let req = self
            .client
            .post(format!("{}/exec", self.base_url))
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(sql.to_string());
        self.guarded(async {
            let resp = self
                .apply_auth(req)
                .send()
                .await
                .map_err(|e| RdbmsError::Database(format!("questdb: {e}")))?;
            if !resp.status().is_success() {
                return Err(RdbmsError::Database(
                    resp.text()
                        .await
                        .unwrap_or_else(|e| format!("questdb: {e}")),
                ));
            }
            Ok(0)
        })
        .await
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let req = self
            .client
            .post(format!("{}/exec?count=true", self.base_url))
            .header("Content-Type", "text/plain; charset=utf-8")
            .header("Accept", "application/json")
            .body(sql.to_string());
        self.guarded(async {
            let resp = self
                .apply_auth(req)
                .send()
                .await
                .map_err(|e| RdbmsError::Database(format!("questdb: {e}")))?;
            if !resp.status().is_success() {
                return Err(RdbmsError::Database(
                    resp.text()
                        .await
                        .unwrap_or_else(|e| format!("questdb: {e}")),
                ));
            }
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| RdbmsError::Database(format!("questdb parse: {e}")))?;
            // 2xx 响应也可能携带 error 字段（无 columns/dataset 时）
            if let Some(err) = body
                .get("error")
                .and_then(|e| e.as_str())
                .filter(|e| !e.is_empty())
            {
                return Err(RdbmsError::Database(err.to_string()));
            }
            let mut rows = Vec::new();
            if let Some(columns) = body.get("columns").and_then(|c| c.as_array()) {
                let cols: Vec<String> = columns
                    .iter()
                    .filter_map(|c| {
                        c.get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                if let Some(dataset) = body.get("dataset").and_then(|d| d.as_array()) {
                    for row in dataset {
                        if let Some(vals) = row.as_array() {
                            rows.push(Row::new(cols.clone(), vals.clone()));
                        }
                    }
                }
            }
            Ok(rows)
        })
        .await
    }
```

判据：`cargo check -p ecat-data-questdb` → `Finished`。

**Step 7（5 分钟）`src/metrics.rs`**：逐字抄 Task 1 Step 7，标签 → `"questdb"`，
`BackendKind::Graph` → `BackendKind::Rdbms`。

**Step 8（10 分钟）`src/tests/resilience.rs`**：抄 Task 1 Step 8 的 `spawn_slow` / `spawn_slow_once` /
`client_at`，但 **`assert_times_out` 换成 `RdbmsError` 版**（`ecat-data-clickhouse/src/tests/resilience.rs:50-63` 同形）：

```rust
/// 一次调用必须在**外层 5 秒内**返回 `RdbmsError::Timeout`，且推进 Rdbms 维度。
///
/// **泛型 `T`（2026-10-08 lead 裁决 B，Task 1 实测）**：`execute` 返回 `u64`、
/// `query` 返回 `Vec<Row>`，都不是 `()` —— 写死 `Output = Result<(), RdbmsError>`
/// 会让两个调用点 E0271。`T: Debug` 是 `unwrap_err()` 的需要。
async fn assert_times_out<T, F>(label: &str, fut: F) -> RdbmsError
where
    T: std::fmt::Debug,
    F: std::future::Future<Output = Result<T, RdbmsError>>,
{
    let before = timeout_counter(BackendKind::Rdbms).load(Ordering::SeqCst);
    let err = tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("{label}: 内层超时没开火（漏包 guarded？）"))
        .unwrap_err();
    assert!(matches!(err, RdbmsError::Timeout(_)), "{label}: got {err:?}");
    assert!(
        timeout_counter(BackendKind::Rdbms).load(Ordering::SeqCst) > before,
        "{label}: 超时必须计入 Rdbms 维度"
    );
    err
}

fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> QuestdbClient {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    let cfg: QuestdbConfig = serde_json::from_str(&format!(
        r#"{{"base_url": "{url}", "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
    .unwrap();
    QuestdbClient::from_config(cfg).unwrap()
}
```

用例（**8 条**）：`config_wires_timeout_concurrency_and_breaker` / `zero_timeout_means_disabled` /
`execute_times_out_and_counts_rdbms_dimension`（主超时；断言 `RdbmsError::Timeout`，**没有 reason 断言**）/
`every_io_method_times_out_when_the_backend_stalls`（`execute` + `query` 各一次）/
`repeated_timeouts_open_the_breaker_and_fail_fast`（断言 `matches!(err, RdbmsError::Connection(_))` 且
`err.to_string().contains("circuit breaker is open")`）/ `concurrency_cap_limits_in_flight_requests` /
`timed_out_request_returns_its_permit`（模式 ④ 用 `execute`）+ 把守测试：

```rust
/// `transaction()` 是常量错误、不含 I/O —— 同样不得触碰熔断器（出入 2/6）。
#[tokio::test]
async fn transaction_error_does_not_trip_the_breaker() {
    let url = spawn_slow(Duration::from_millis(10), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 30, None);
    for _ in 0..8 {
        assert!(ecat_data::RdbmsClient::transaction(&c).await.is_err());
    }
    assert_eq!(c.breaker().state(), BreakerState::Closed);
    assert_eq!(c.breaker().opened_total(), 0);
}
```

见证槽仍 `Storage`（`use ecat_data::{BackendKind, timeout_counter};` 照抄）。
`mod resilience;` 加在 `src/lib.rs:161` 的 `mod tests` 第一行。

**Step 9（3 分钟）跑测试**：期望 **17 passed**；`--features metrics` → **18 passed**。
**Step 10（5 分钟）空验收自证**：Task 1 Step 10 的表 + 本 crate 专属：
删掉 `query` 的 `guarded` ⇒ `every_io_method_…` 红；把 `transaction` 包进 `guarded` ⇒
`transaction_error_does_not_trip_the_breaker` 红。
**Step 11（5 分钟）红探针 + 闸门 + 提交**（同 Task 4 Step 11）。

---

## Task 10：`ecat-data-s3`（只有 `from_config`；trait 全部四个方法）

**Files**：`ecat-data-s3/Cargo.toml`、`src/lib.rs`、`src/tests.rs`（新建，Step 3）、
`src/tests/resilience.rs`（新建）、`src/metrics.rs`（新建）。

| 项 | 值 |
|----|----|
| 基线 | **17 passed**（lib.rs 9 + `signing.rs`/`xml.rs` 8）；`src/lib.rs` → 419 行（**必须拆**） |
| kind / 标签 / slug | `BackendKind::Storage` / `"s3"` / `"storage"` |
| 构造器 | **只有 `from_config`**（没有 `new` / `with_auth`，**不要为对称性加**） |
| 配置必填字段 | `endpoint`, `region`, `access_key`, `secret_key` |
| 要包的方法 | `put` / `get` / `delete` / `list`（**整个翻页循环一个预算**） |
| **不包** | `object_path` / `signed_request` / `check_status`（纯本地） |
| **证人槽** | **`Cache`**（本 crate 自己是 `Storage`，不能用它当证人） |
| 期望测试数 | 17 + 7 = **24**；`--features metrics` → **25** |

**Step 1（2 分钟）基线**：期望 17 / 419。
**Step 2（3 分钟）`Cargo.toml`**：本 crate 的 `[dev-dependencies]` 只有
`tokio = { workspace = true, features = ["macros", "rt"] }`，且**没有 axum** ⇒ 按 Task 1 Step 2 加三处之外，
另加两处（逐字）：

```toml
# [dev-dependencies]（本 crate 既有测试用裸 std TcpListener，保持不动；新增的是 axum 的慢 mock）
axum.workspace = true
tokio = { workspace = true, features = ["macros", "rt", "net", "time"] }
```

判据：`grep -c 'ecat-metrics' ecat-data-s3/Cargo.toml` 期望 `4`（含两行注释，见 T0-A 自证）；
`grep -c 'axum' ecat-data-s3/Cargo.toml` 期望 `1`（只算 dev-deps 那一行 —— 别给这行加含
"axum" 的注释，否则该数会随注释漂移）。

**Step 3（8 分钟）拆测试文件**：测试块从 `src/lib.rs:228` 到文件末尾 `:419`
（块内 `use std::io::{Read, Write};` 要跟着搬）。判据：仍是 **17 passed**。
**Step 4（3 分钟）配置字段 + `query_timeout`**：逐字抄 Task 1 Step 3（插在 `S3Config` 的 `tls` 之后）。

**Step 5（6 分钟）结构体 + **唯一**构造器 + `use` + 6 处测试字面量**：
- 结构体末尾追加三个字段；
- `from_config`（`src/lib.rs:47-70`）的 `Ok(Self { ... })` 里追加：

```rust
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
            semaphore: Arc::new(Semaphore::new(cfg.max_concurrency.unwrap_or(32))),
```

- `use`（`src/lib.rs:17-25`）追加四行：

```rust
use ecat_circuit_breaker::{Breaker, BreakerConfig};
use ecat_data::{BackendKind, breaker_error_to_backend_error, run_with_timeout};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, SemaphorePermit};
```

（`BackendKind` / `run_with_timeout` / `breaker_error_to_backend_error` 来自 `ecat_data`，
与既有的 `use ecat_data::StorageClient;` 合并成一行即可；`ecat_errors::{Error as StorageError, ErrorCode}` 不动。）

- **`src/tests.rs` 里 6 处 `S3Config { ... }` 字面量**（原 `src/lib.rs:249/263/277/294/359/388`）各加三个字段，
  否则 E0063 编译不过：

```rust
            query_timeout_secs: None,
            breaker: None,
            max_concurrency: None,
```

**Step 6（5 分钟）访问器 / 许可 / `guarded`**：

```rust
    /// 本 client 的熔断器。`metrics` feature 注册指标时要读它的状态与打开次数。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }

    /// 取一个并发许可。信号量从不 `close()`，`AcquireError` 不可达。
    async fn permit(&self) -> SemaphorePermit<'_> {
        self.semaphore
            .acquire()
            .await
            .expect("semaphore is never closed")
    }

    /// 一次出站调用的外壳：**许可 → 熔断 → 超时**（理由见 T0 模板）。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, StorageError>
    where
        F: std::future::Future<Output = Result<T, StorageError>> + Send,
    {
        let _permit = self.permit().await;
        self.breaker
            .call(|| run_with_timeout(BackendKind::Storage, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "s3"))
    }
```

注：`StorageError` 是 `ecat_errors::Error` 的**别名**（`src/lib.rs:19`），所以 `guarded` 的签名与 T0 模板
一字不差 —— `TimeoutError` 早已为 `Error` 实现（`ecat-data/src/timeout.rs:108-118`），**不需要**在 `ecat-data` 加任何东西。

**Step 7（10 分钟）包装四个方法**（`src/lib.rs:133-226`；`object_path` / `signed_request` / `check_status` 一字不改）：

```rust
    async fn put(&self, bucket: &str, key: &str, data: &[u8]) -> Result<(), StorageError> {
        let path = self.object_path(bucket, key);
        let (url, auth, amz_date, payload_hash) = self.signed_request("PUT", &path, &[], data);
        self.guarded(async {
            let resp = self
                .client
                .put(url)
                .header(AUTHORIZATION, auth)
                .header("x-amz-date", amz_date)
                .header("x-amz-content-sha256", payload_hash)
                .body(data.to_vec())
                .send()
                .await
                .map_err(|e| {
                    StorageError::new(ErrorCode::Internal, "s3", format!("s3 put: {e}"))
                })?;
            Self::check_status(resp, "put").await?;
            Ok(())
        })
        .await
    }

    async fn get(&self, bucket: &str, key: &str) -> Result<Vec<u8>, StorageError> {
        let path = self.object_path(bucket, key);
        let (url, auth, amz_date, payload_hash) = self.signed_request("GET", &path, &[], b"");
        self.guarded(async {
            let resp = self
                .client
                .get(url)
                .header(AUTHORIZATION, auth)
                .header("x-amz-date", amz_date)
                .header("x-amz-content-sha256", payload_hash)
                .send()
                .await
                .map_err(|e| {
                    StorageError::new(ErrorCode::Internal, "s3", format!("s3 get: {e}"))
                })?;
            let resp = Self::check_status(resp, "get").await?;
            Ok(resp
                .bytes()
                .await
                .map_err(|e| {
                    StorageError::new(ErrorCode::Internal, "s3", format!("s3 get body: {e}"))
                })?
                .to_vec())
        })
        .await
    }

    async fn delete(&self, bucket: &str, key: &str) -> Result<(), StorageError> {
        let path = self.object_path(bucket, key);
        let (url, auth, amz_date, payload_hash) = self.signed_request("DELETE", &path, &[], b"");
        self.guarded(async {
            let resp = self
                .client
                .delete(url)
                .header(AUTHORIZATION, auth)
                .header("x-amz-date", amz_date)
                .header("x-amz-content-sha256", payload_hash)
                .send()
                .await
                .map_err(|e| {
                    StorageError::new(ErrorCode::Internal, "s3", format!("s3 delete: {e}"))
                })?;
            Self::check_status(resp, "delete").await?;
            Ok(())
        })
        .await
    }

    /// List object keys under `prefix`, following continuation tokens across
    /// pages (same behavior as the previous rust-s3 backend).
    ///
    /// **整个翻页循环一个预算**：一次 `list` 可能发很多次 GET（continuation token），
    /// 预算必须罩住整次调用，否则一次调用会变成 N 个独立预算。
    async fn list(&self, bucket: &str, prefix: &str) -> Result<Vec<String>, StorageError> {
        let path = format!("/{bucket}");
        self.guarded(async {
            let mut keys = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let mut query: Vec<(&str, &str)> = vec![("list-type", "2"), ("prefix", prefix)];
                let token_owned;
                if let Some(t) = &token {
                    token_owned = t.clone();
                    query.push(("continuation-token", &token_owned));
                }
                let (url, auth, amz_date, payload_hash) =
                    self.signed_request("GET", &path, &query, b"");
                let resp = self
                    .client
                    .get(url)
                    .header(AUTHORIZATION, auth)
                    .header("x-amz-date", amz_date)
                    .header("x-amz-content-sha256", payload_hash)
                    .send()
                    .await
                    .map_err(|e| {
                        StorageError::new(ErrorCode::Internal, "s3", format!("s3 list: {e}"))
                    })?;
                let resp = Self::check_status(resp, "list").await?;
                let body = resp.text().await.map_err(|e| {
                    StorageError::new(ErrorCode::Internal, "s3", format!("s3 list body: {e}"))
                })?;
                let (page_keys, next) = xml::parse_list_xml(&body);
                keys.extend(page_keys);
                match next {
                    Some(t) => token = Some(t),
                    None => break,
                }
            }
            Ok(keys)
        })
        .await
    }
```

判据：`cargo check -p ecat-data-s3` → `Finished`。

**Step 8（5 分钟）`src/metrics.rs`**：逐字抄 Task 1 Step 7，标签 → `"s3"`，
`BackendKind::Graph` → `BackendKind::Storage`。

**Step 9（12 分钟）`src/tests/resilience.rs`**：抄 Task 1 Step 8 的四个 helper 与 6 条用例，替换：

- `assert_times_out`：kind → `BackendKind::Storage`，`err.reason` → `"storage"`；
  **见证槽改 `Cache`**（`BackendKind::Cache`），注释也要跟着写清「本 crate 自己是 Storage」。
- `client_at` **不走 `from_config` 的简写**（必须给全 4 个必填字段）：

```rust
fn client_at(url: &str, timeout_secs: u64, max_concurrency: Option<usize>) -> S3Client {
    let mc = match max_concurrency {
        Some(n) => format!(r#", "max_concurrency": {n}"#),
        None => String::new(),
    };
    let cfg: S3Config = serde_json::from_str(&format!(
        r#"{{"endpoint": "{url}", "region": "us-east-1", "access_key": "a", "secret_key": "b",
            "query_timeout_secs": {timeout_secs}{mc}}}"#
    ))
    .unwrap();
    S3Client::from_config(cfg).unwrap()
}
```

- 主超时用例名 `put_times_out_and_counts_storage_dimension`，调用
  `ecat_data::StorageClient::put(&c, "bucket", "key", b"data")`。
- 模式 ④ 也用 `put`（第二次的 `{}` 响应体只看状态码）。
- 熔断用例调用点用 `put`，`err.reason` 断言 → `"s3"`。
- 覆盖用例（4 个方法各一次 = 4 次 < 5 条窗口）：

```rust
/// 四个 I/O 方法各打一次（4 次 < 5 条窗口 ⇒ 熔断不会在途中打开）。
#[tokio::test]
async fn every_io_method_times_out_when_the_backend_stalls() {
    let url = spawn_slow(Duration::from_secs(5), Arc::new(AtomicUsize::new(0))).await;
    let c = client_at(&url, 1, None);
    assert_times_out(
        "put",
        ecat_data::StorageClient::put(&c, "bucket", "key", b"data"),
    )
    .await;
    assert_times_out("get", ecat_data::StorageClient::get(&c, "bucket", "key")).await;
    assert_times_out("delete", ecat_data::StorageClient::delete(&c, "bucket", "key")).await;
    assert_times_out("list", ecat_data::StorageClient::list(&c, "bucket", "p")).await;
    assert_eq!(
        c.breaker().state(),
        BreakerState::Closed,
        "4 次调用打不满 5 条窗口"
    );
}
```

（`list` 的响应体是空的 —— 超时先开火，解析根本不会发生，所以不需要合法 XML。）

`mod resilience;` 加在 `src/tests.rs` 第一行。

**Step 10（3 分钟）跑测试**：期望 **24 passed**；`--features metrics` → **25 passed**
（`cargo test -p ecat-data-s3 --features metrics`）。**17 → 24 的数字必须精确对上**；
对不上先看 `signing.rs` / `xml.rs` 的 8 条是否还在跑。
**Step 11（5 分钟）空验收自证**：Task 1 表 + 本 crate 专属：
(a) 见证槽写错成 `Storage` ⇒ `put_times_out_…` 在**单独跑**时可能仍绿（本 crate 自己会写 Storage），
这条只能靠 `grep -rn 'BackendKind::Cache' ecat-data-s3/src/` 只命中 witness 行来保证；
(b) `put` 的 `guarded` 删掉 ⇒ `put_times_out_…` 与 `every_io_method_…` 红。
**Step 12（5 分钟）红探针 + 闸门 + 提交**：`cargo fmt -p ecat-data-s3`、clippy（`--all-targets`）、
`git commit --only` 六个文件（含 **`Cargo.lock`**）。

---

## Task 11：`ecat-data-mongodb`（**池配置，不是 HTTP**）

**Files**：`ecat-data-mongodb/Cargo.toml`、`src/lib.rs`、`src/metrics.rs`（新建）、`src/tests/resilience.rs`（新建）。
**没有信号量、没有 `max_concurrency`、没有模式 ④**（出入 8）：并发背压交给**驱动的连接池**，
暴露的是 `max_pool_size` / `min_pool_size`（spec §5）。

| 项 | 值 |
|----|----|
| 基线 | **8 passed**；`src/lib.rs` → 228 行（**不用拆**） |
| kind / 标签 / slug | `BackendKind::Document` / `"mongodb"` / `"document"` |
| 构造器 | **只有 `from_config`（async）** |
| 配置必填字段 | `url`, `database` |
| 要包的方法 | `insert` / `find` / `update` / `delete` 的**网络段** |
| **不包** | bson 转换（本地，且是**调用方**的输入错误） |
| 期望测试数 | 8 + 5 = **13**；`--features metrics` → **14** |

**Step 1（2 分钟）基线**：期望 8 / 228。
**Step 2（3 分钟）`Cargo.toml`**：与 HTTP crate **不同** —— 不需要 `tokio(sync)`（无信号量）：

```toml
# [dependencies] 追加（**没有** tokio/sync：本 crate 无信号量）
ecat-circuit-breaker.workspace = true
ecat-metrics = { workspace = true, optional = true }

[features]
metrics = ["dep:ecat-metrics"]

# [dev-dependencies]（既有是 ["macros", "rt"]，加 time 给 5 秒保险丝）
tokio = { workspace = true, features = ["macros", "rt", "time"] }
```

判据：`grep -c 'ecat-metrics' ecat-data-mongodb/Cargo.toml` 期望 `4`（含两行注释，见 T0-A 自证）。

**Step 3（3 分钟）配置字段**：`MongoConfig` 的 `tls` 之后插入（Redis 版 rustdoc + 池字段，逐字）：

```rust
    /// 单次命令超时秒数。`0` = 禁用；未配置 = 30 秒。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略则用保守默认（失败率 0.5、窗口 30 秒、打开 10 秒）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 连接池上限。未配置 = 驱动默认（mongodb 3.8.0 实测 **10**，`src/cmap.rs:50`；
    /// **不是** spec §5 写的 100 —— 那是 Node 驱动的默认值）。
    #[serde(default)]
    pub max_pool_size: Option<u32>,
    /// 连接池下限（后台保活连接数）。未配置 = 驱动默认 0。
    #[serde(default)]
    pub min_pool_size: Option<u32>,
```

`impl MongoClient` 之前（`MongoConfig` 之后）加 `query_timeout`（逐字，同 T0-B）。

**Step 4（3 分钟）`tls` 死字段**：`MongoConfig.tls` 从未被使用（出入 7）。**本批不动它**，
但要在结构体那行上方补一句 `// TODO(5c): 未接线（spec 未要求；mongodb 3.x 的 TLS 走 URI 选项）`
**不允许改行为** —— 只加注释留痕。

**Step 5（8 分钟）`from_config` + `build_options` + `guarded`**（`src/lib.rs:24-38` 整体替换；`use` 块加
`ecat_circuit_breaker::{Breaker, BreakerConfig}`、`ecat_data::{BackendKind, breaker_error_to_backend_error, run_with_timeout}`、
`std::sync::Arc`、`std::time::Duration`）：

```rust
    /// 从配置建 `ClientOptions`。**单独成函数是为了可测**：`mongodb://` URI 的解析
    /// 不发网络请求，所以池大小的接线能在没有服务器的单测里被断言到
    /// （删掉下面两行赋值，`config_wires_timeout_pool_and_breaker` 立刻红）。
    async fn build_options(cfg: &MongoConfig) -> Result<mongodb::options::ClientOptions, Error> {
        // `ClientOptions::parse` 不是 async fn，返回的是可 await 的 action builder
        // （mongodb 3.8.0 `src/action/client_options.rs:68-79`）。
        let mut options = mongodb::options::ClientOptions::parse(&cfg.url)
            .await
            .map_err(|e| {
                Error::new(ErrorCode::Internal, "mongodb", format!("mongodb connect: {e}"))
            })?;
        // 池大小走**驱动的旋钮**：本 crate 没有信号量。`None` = 不覆盖，交给驱动默认。
        options.max_pool_size = cfg.max_pool_size;
        options.min_pool_size = cfg.min_pool_size;
        Ok(options)
    }

    pub async fn from_config(cfg: MongoConfig) -> Result<Self, Error> {
        let options = Self::build_options(&cfg).await?;
        let client = mongodb::Client::with_options(options).map_err(|e| {
            Error::new(ErrorCode::Internal, "mongodb", format!("mongodb connect: {e}"))
        })?;
        Ok(Self {
            client,
            database: cfg.database,
            query_timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
        })
    }

    /// 本 client 的熔断器。`metrics` feature 注册指标时要读它的状态与打开次数。
    pub fn breaker(&self) -> Arc<Breaker> {
        Arc::clone(&self.breaker)
    }

    /// 一次出站调用的外壳：**熔断 → 超时**（**没有许可层** —— 并发背压交给
    /// 驱动连接池 `max_pool_size`，见 spec §5）。
    async fn guarded<F, T: 'static>(&self, fut: F) -> Result<T, Error>
    where
        F: std::future::Future<Output = Result<T, Error>> + Send,
    {
        self.breaker
            .call(|| run_with_timeout(BackendKind::Document, self.query_timeout, fut))
            .await
            .map_err(|e| breaker_error_to_backend_error(e, "mongodb"))
    }
```

结构体末尾加两个字段：

```rust
    query_timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器要挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
```

**Step 6（8 分钟）包装四个方法的网络段**（`src/lib.rs:42-128`）——
**bson 转换全部留在外面**，理由见代码注释：

```rust
    async fn insert(&self, collection: &str, doc: &Value) -> Result<String, Error> {
        // bson 转换是**调用方的输入错误**（不是后端故障），留在外壳外面 ——
        // 否则 5 次「传了非文档」就会把熔断器打开、之后正常写入全被拒绝。
        let doc = bson::to_document(doc).map_err(|e| {
            Error::new(ErrorCode::Internal, "mongodb", format!("mongodb bson: {e}"))
        })?;
        self.guarded(async {
            let result = self
                .client
                .database(&self.database)
                .collection::<bson::Document>(collection)
                .insert_one(doc)
                .await
                .map_err(|e| {
                    Error::new(ErrorCode::Internal, "mongodb", format!("mongodb insert: {e}"))
                })?;
            Ok(result.inserted_id.to_string())
        })
        .await
    }

    async fn find(&self, collection: &str, filter: &Value) -> Result<Vec<Value>, Error> {
        let filter = bson::to_document(filter).map_err(|e| {
            Error::new(ErrorCode::Internal, "mongodb", format!("mongodb bson: {e}"))
        })?;
        self.guarded(async {
            let cursor = self
                .client
                .database(&self.database)
                .collection::<bson::Document>(collection)
                .find(filter)
                .await
                .map_err(|e| {
                    Error::new(ErrorCode::Internal, "mongodb", format!("mongodb find: {e}"))
                })?;
            let docs: Vec<bson::Document> = cursor.try_collect().await.map_err(|e| {
                Error::new(ErrorCode::Internal, "mongodb", format!("mongodb find: {e}"))
            })?;
            docs.iter()
                .map(|d| {
                    serde_json::to_value(d).map_err(|e| {
                        Error::new(ErrorCode::Internal, "mongodb", format!("mongodb json: {e}"))
                    })
                })
                .collect()
        })
        .await
    }

    async fn update(&self, collection: &str, filter: &Value, update: &Value) -> Result<u64, Error> {
        let filter = bson::to_document(filter).map_err(|e| {
            Error::new(ErrorCode::Internal, "mongodb", format!("mongodb bson: {e}"))
        })?;
        let update = bson::to_document(update).map_err(|e| {
            Error::new(ErrorCode::Internal, "mongodb", format!("mongodb bson: {e}"))
        })?;
        self.guarded(async {
            let result = self
                .client
                .database(&self.database)
                .collection::<bson::Document>(collection)
                .update_many(filter, update)
                .await
                .map_err(|e| {
                    Error::new(ErrorCode::Internal, "mongodb", format!("mongodb update: {e}"))
                })?;
            Ok(result.modified_count)
        })
        .await
    }

    async fn delete(&self, collection: &str, filter: &Value) -> Result<u64, Error> {
        let filter = bson::to_document(filter).map_err(|e| {
            Error::new(ErrorCode::Internal, "mongodb", format!("mongodb bson: {e}"))
        })?;
        self.guarded(async {
            let result = self
                .client
                .database(&self.database)
                .collection::<bson::Document>(collection)
                .delete_many(filter)
                .await
                .map_err(|e| {
                    Error::new(ErrorCode::Internal, "mongodb", format!("mongodb delete: {e}"))
                })?;
            Ok(result.deleted_count)
        })
        .await
    }
```

判据（**这条是「四个方法都包了」的机器判据**）：

```bash
export CARGO_TARGET_DIR=/var/tmp/ecat-target
grep -c 'self\.guarded(' ecat-data-mongodb/src/lib.rs    # 期望 4
cargo check -p ecat-data-mongodb                          # Finished
```

**Step 7（5 分钟）`src/metrics.rs`**：逐字抄 Task 1 Step 7，标签 → `"mongodb"`，
`BackendKind::Graph` → `BackendKind::Document`。

**Step 8（3 分钟）`src/tests.rs`… 不适用**：本 crate **不拆**测试文件（228 行）。
直接在 `src/lib.rs` 的 `mod tests`（`:131`）第一行加 `mod resilience;`。
**另需改两处测试字面量**（否则 E0063）：`src/lib.rs:147` 与 `:218` 的 `MongoConfig { ... }` 各加四行：

```rust
            query_timeout_secs: None,
            breaker: None,
            max_pool_size: None,
            min_pool_size: None,
```

**Step 9（10 分钟）`src/tests/resilience.rs`**（**没有模式 ①/④**：线协议没有进程内 mock，
用 checklist §5 模式 ③「`future::pending()`」盯外壳本身）：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 出站韧性（超时 / 熔断 / 池配置）测试。
//!
//! **没有模式 ①/④**：MongoDB 的线协议没有进程内 mock，真实方法打不到「慢后端」。
//! 所以这里用模式 ③（`std::future::pending()`）直接盯 `guarded` 外壳；
//! 「四个方法是否都包了」由 `grep -c 'self.guarded(' == 4` 守着。
use super::*;
use ecat_circuit_breaker::BreakerState;
use ecat_data::{BackendKind, timeout_counter};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// 指向一个**不会真的连接**的 URI：`ClientOptions::parse` 是纯本地解析，
/// 所以这个 client 建得起来，只是任何真实命令都会失败。
async fn client_at(timeout_secs: u64, max_pool_size: Option<u32>) -> MongoClient {
    let mp = match max_pool_size {
        Some(n) => format!(r#", "max_pool_size": {n}"#),
        None => String::new(),
    };
    let cfg: MongoConfig = serde_json::from_str(&format!(
        r#"{{"url": "mongodb://127.0.0.1:27017", "database": "app",
            "query_timeout_secs": {timeout_secs}{mp}}}"#
    ))
    .unwrap();
    MongoClient::from_config(cfg).await.unwrap()
}

/// 池大小真的从配置接到了驱动旋钮上（spec §8 对 MongoDB 的判据）。
/// 删掉 `build_options` 里那两行赋值 ⇒ 本用例红（`.is_some()` 落空）。
#[tokio::test]
async fn config_wires_timeout_pool_and_breaker() {
    let cfg: MongoConfig = serde_json::from_str(
        r#"{"url":"mongodb://127.0.0.1:27017","database":"app","query_timeout_secs":1,
            "max_pool_size":7,"min_pool_size":2}"#,
    )
    .unwrap();
    let options = MongoClient::build_options(&cfg).await.unwrap();
    assert_eq!(
        options.max_pool_size,
        Some(7),
        "池上限必须来自配置（省略 = 驱动默认）"
    );
    assert_eq!(options.min_pool_size, Some(2));

    let c = MongoClient::from_config(cfg).await.unwrap();
    assert_eq!(c.query_timeout, Some(Duration::from_secs(1)));
    assert_eq!(c.breaker().state(), BreakerState::Closed);
}

/// 省略池字段 = `None` = **不覆盖**，交给驱动默认（mongodb 3.8.0 实测 10）。
/// 谁「顺手」填一个默认数字（哪怕填 10），这条就把「默认值来源」这点改变了。
#[tokio::test]
async fn omitted_pool_size_leaves_the_driver_default() {
    let cfg: MongoConfig =
        serde_json::from_str(r#"{"url":"mongodb://127.0.0.1:27017","database":"app"}"#).unwrap();
    let options = MongoClient::build_options(&cfg).await.unwrap();
    assert_eq!(options.max_pool_size, None, "省略时不写死数字");
    assert_eq!(options.min_pool_size, None);
}

/// `0` = 显式禁用（`None`），未配置 = 30 秒。
#[tokio::test]
async fn zero_timeout_means_disabled() {
    assert_eq!(query_timeout(Some(0)), None);
    assert_eq!(query_timeout(None), Some(Duration::from_secs(30)));
    assert_eq!(client_at(0, None).await.query_timeout, None);
}

/// 模式 ③：`future::pending()` 永不就绪 —— 内层超时没接上就会挂死，被 5 秒保险丝抓住。
#[tokio::test]
async fn guarded_times_out_with_deadline_exceeded() {
    let c = client_at(1, None).await;
    let witness = timeout_counter(BackendKind::Storage).load(Ordering::SeqCst);
    let before = timeout_counter(BackendKind::Document).load(Ordering::SeqCst);
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        c.guarded(std::future::pending::<Result<serde_json::Value, Error>>()),
    )
    .await
    .unwrap_or_else(|_| panic!("内层超时没开火（漏包 guarded？）"))
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::DeadlineExceeded, "got: {err}");
    assert_eq!(err.reason, "document", "超时 reason 应是 kind.slug()");
    assert!(timeout_counter(BackendKind::Document).load(Ordering::SeqCst) > before);
    assert_eq!(
        timeout_counter(BackendKind::Storage).load(Ordering::SeqCst),
        witness,
        "Document 的超时不得落到别的槽"
    );
}

/// 5 次超时 ⇒ 熔断打开、快速失败（每发 1 秒，共约 5 秒）。
#[tokio::test]
async fn repeated_timeouts_open_the_breaker_and_fail_fast() {
    let c = client_at(1, None).await;
    for _ in 0..5 {
        let _ = c
            .guarded(std::future::pending::<Result<(), Error>>())
            .await;
    }
    assert_eq!(c.breaker().state(), BreakerState::Open);
    assert_eq!(c.breaker().opened_total(), 1);

    let start = std::time::Instant::now();
    let err = c
        .guarded(std::future::pending::<Result<(), Error>>())
        .await
        .expect_err("熔断已打开");
    assert_eq!(err.code, ErrorCode::Unavailable, "got: {err}");
    assert_eq!(err.message, "circuit breaker is open", "got: {err}");
    assert_eq!(err.reason, "mongodb");
    assert!(start.elapsed() < Duration::from_millis(500), "熔断拒绝必须立即返回");
}

/// bson 转换失败**不是后端故障**，不得计入熔断窗口
/// （否则 5 次传错参就把正常写入也熔断了 —— 这正是「本地分支留在外面」的理由）。
#[tokio::test]
async fn bson_conversion_error_does_not_trip_the_breaker() {
    let c = client_at(1, None).await;
    for _ in 0..8 {
        let err = c.insert("col", &Value::Null).await.expect_err("null 不是文档");
        assert!(err.to_string().contains("mongodb bson:"), "got: {err}");
    }
    assert_eq!(c.breaker().state(), BreakerState::Closed);
    assert_eq!(c.breaker().opened_total(), 0);
}
```

**Step 10（3 分钟）跑测试**：期望 **13 passed**（8 + 5）；`--features metrics` → **14 passed**。
**Step 11（5 分钟）空验收自证**：Task 1 表 + 本 crate 专属：
(a) 删 `build_options` 里两行赋值 ⇒ `config_wires_timeout_pool_and_breaker` 红；
(b) 把 `insert` 的 bson 转换挪进 `guarded` ⇒ `bson_conversion_error_does_not_trip_the_breaker` 红；
(c) 删 `guarded` 里的 `run_with_timeout` ⇒ `guarded_times_out_…` 红。
**Step 12（5 分钟）红探针 + 闸门 + 提交**：`cargo fmt -p ecat-data-mongodb`、
`cargo clippy -p ecat-data-mongodb --all-targets -- -D warnings`、`git commit --only` 五个文件（含 **`Cargo.lock`**）。

---

## Task 12：文档同步（**28 个文件**：1 + 13 + 14）

**Files**（四组，各自独立提交；**不要**在同一个 commit 里混组）：

| 组 | 文件 | 数量 |
|----|------|------|
| 0 | `docs/superpowers/checklists/backend-resilience-onboarding.md` | 1 |
| A | `config/databases.example.yaml` | 1 |
| B | `docs/database-config-tutorial.md` + `docs/i18n/{ar,bn,de,en,es,fr,hi,id,ja,ko,pt,ru}/database-config-tutorial.md` | 13 |
| C | `README.md`、`README.en.md` + `docs/i18n/{12 语言}/README.md` | 14 |

**已实测的前提**（别照 §1 的旧描述做）：教程里 **Redis / ClickHouse 两节 5a 已更新**
（`docs/database-config-tutorial.md:228-290`：yaml 块里的三个新字段 + 四列表格三行 +
「两层超时」「熔断默认开启」两段说明）；缺的是**其余 8 节的新字段**与
**TDengine / MongoDB / S3 三节**。`config/databases.example.yaml` **连 5a 都没更新**
（`redis:` 节 35-37、`clickhouse:` 节 45-49 都只有旧字段，且**没有** tdengine / mongodb / s3 三节）。

**Step 0（3 分钟）0 组：先把 checklist 的口径改成与 T0 一致**（**本批次开工前做**）。
T0-C 用的是「`kind` 写死」的变体，而 checklist §2 的变体 B 模板带 `kind: BackendKind` 参数
（照抄自 `ecat-data-clickhouse`，它是**双路径** crate）。两者不统一，下一个照 checklist 写的人
就会给单路径 crate 平白加一个能传错的旋钮。**已回写进 checklist**，核对下述文本在位即可
（若已存在 = 别人已改，跳过）：

```bash
grep -n 'kind` 写死，除非你的 crate 有多条 I/O 路径' docs/superpowers/checklists/backend-resilience-onboarding.md
# 期望：命中 1 行（§2 变体 B 代码块之后）；没有就按下面的文本补
grep -c 'BackendKind::Graph, self.query_timeout, fut' docs/superpowers/checklists/backend-resilience-onboarding.md
# 期望：1（写死 kind 的正例；与字面量参数形式并存，因为 clickhouse 确实是双路径）
```

补充文本（若缺失则插在 checklist §2 变体 B 代码块之后、`**包哪些方法**：` 之前）：
「**`kind` 写死，除非你的 crate 有多条 I/O 路径**：`kind` 是维度选择器，只有当一个 crate
同时实现多个 I/O trait、必须按调用点区分维度时（ClickHouse：`SqlExecutor` → `Rdbms` 与
`TsdbClient` → `Tsdb`）才做成参数；单路径 crate 一律写死，否则调用点可以传任意维度，
把「维度由 trait 家族决定」降级成「调用者说了算」，而填错只会静默少数。5b 的 10 个 HTTP 后端
全是单路径 ⇒ 全写死；`ecat-data-questdb` 虽是 `RdbmsError` 路径但也只有一条路径 ⇒ 写死 `BackendKind::Rdbms`。」

**Step 1（3 分钟）基线**（把 4 个数字记进 commit message）：

```bash
awk -F'|' '/^\| .*ecat-data-/ {s=$(NF-1); gsub(/ /,"",s); if (s ~ /^✅/) n++} END{print n+0}' README.md   # 期望 7
ls README.md README.en.md docs/i18n/*/README.md | wc -l                                                    # 期望 14
ls docs/database-config-tutorial.md docs/i18n/*/database-config-tutorial.md | wc -l                        # 期望 13
grep -c 'query_timeout_secs' config/databases.example.yaml                                                 # 期望 1（只有 mssql 那行注释）
grep -c '^tdengine:\|^mongodb:\|^s3:' config/databases.example.yaml                                        # 期望 0
```

**Step 2（6 分钟）A 组：`config/databases.example.yaml`**。
(a) **10 处**既有节（`redis` / `clickhouse` / `elasticsearch` / `opensearch` / `neo4j` /
`nebulagraph` / `arangodb` / `influxdb` / `iotdb` / `questdb`）在各自**最后一个可选字段注释之后**插入：

```yaml
  # 出站韧性（可选）：query_timeout_secs（0 = 禁用，默认 30 秒）
  #   breaker: { failure_ratio: 0.5, window: 30, half_open_probes: 3, open_duration: 10 }
  #   max_concurrency（默认 32，本 crate 的信号量）
```

（字段名来自实测：`ecat-circuit-breaker/src/breaker.rs:37-49` —— `failure_ratio: f64` /
`window: Duration`（serde 按**秒**数）/ `half_open_probes: u32` / `open_duration: Duration`；
**没有** `enabled`。）

(b) 文件末尾（`# ── TLS 证书自动生成示例` 之前）新增三节，逐字：

```yaml
# ── 文档 ────────────────────────────────────────────────────
mongodb:
  url: "mongodb://localhost:27017"
  database: "app"
  # 出站韧性（可选）：query_timeout_secs（0 = 禁用，默认 30 秒）
  #   breaker: { failure_ratio: 0.5, window: 30, half_open_probes: 3, open_duration: 10 }
  #   max_pool_size / min_pool_size（连接池上下限；省略 = 驱动默认 10 / 0 —— 本 crate 没有信号量）

# ── 对象存储 ────────────────────────────────────────────────
s3:
  endpoint: "http://localhost:9000"
  region: "us-east-1"
  access_key: "minioadmin"     # 复制本文件后请换成真实密钥
  secret_key: "minioadmin"
  # 出站韧性（可选）：query_timeout_secs（0 = 禁用，默认 30 秒）
  #   breaker: { failure_ratio: 0.5, window: 30, half_open_probes: 3, open_duration: 10 }
  #   max_concurrency（默认 32，本 crate 的信号量）
```

`tdengine` 节插在 `# ── 文档 ──` 之前（时序家族，与 iotdb/questdb 相邻）：

```yaml
tdengine:
  base_url: "http://localhost:6041"
  username: "root"
  password: "taosdata"         # 复制本文件后请改成强口令
  # database: "my_db"          # 可选: 不填则用 REST 路径里的默认库
  # 出站韧性（可选）：query_timeout_secs（0 = 禁用，默认 30 秒）
  #   breaker: { failure_ratio: 0.5, window: 30, half_open_probes: 3, open_duration: 10 }
  #   max_concurrency（默认 32，本 crate 的信号量）
```

(c) 文件头 `# e-cat 数据库配置示例 — v2.4.2` 改成 `— v6.0.0`（与当前 Cargo.toml 一致；
Task 13 的 bump 会把它一起改成 7.0.0）。

**验收（A 组，非空）**：

```bash
grep -c 'query_timeout_secs' config/databases.example.yaml    # 期望 14（13 处新增 + mssql 原有 1）
grep -c '^tdengine:\|^mongodb:\|^s3:' config/databases.example.yaml   # 期望 3
grep -c 'max_pool_size' config/databases.example.yaml         # 期望 1
```

**Step 3（12 分钟）B 组根文件：`docs/database-config-tutorial.md`**。
(a) **8 个既有节**（`QuestDB—QuestdbConfig`、`Elasticsearch`、`OpenSearch`、`InfluxDB`、
`Neo4j`、`NebulaGraph`、`ArangoDB`、`IoTDB`）各做两处插入 —— 注意**这 8 节的表格是三列**
（`| 字段 | 类型 | 说明 |`，与 Redis/ClickHouse 的四列不同，照抄四列会渲染错）：

yaml 块的最后一个注释行之后：

```yaml
  # query_timeout_secs: 30   # 可选：单次调用超时，0 = 禁用
  # breaker: {}              # 可选：熔断配置，省略 = 保守默认（0.5 / 30s / 打开 10s）
  # max_concurrency: 32      # 可选：并发上限（本 crate 的信号量）
```

表格末尾（三列）：

```
| `query_timeout_secs` | `Option<u64>` | 单次调用超时秒数；省略 = `30`，**`0` = 禁用** |
| `breaker` | `Option<BreakerConfig>` | 熔断阈值与窗口，省略 = 保守默认（0.5 / 30s / 打开 10s）；没有 `enabled` 总开关 |
| `max_concurrency` | `Option<usize>` | 并发上限（默认 `32`）；**本 crate 自己的信号量**，不是 reqwest 的旋钮 |
```

QuestDB 那节表格后**另加一段**（它的错误类型与其余 10 个不同，见 Task 9）：

```
**错误类型**：QuestDB 走 `SqlExecutor`（RDBMS 家族）—— 超时是 `RdbmsError::Timeout`，
熔断拒绝是 `RdbmsError::Connection("circuit breaker is open")`；其余 HTTP 后端统一为
`ecat_errors::Error`（`code = DeadlineExceeded` / `Unavailable`，`reason` = 后端名）。
```

(b) `## 程序化创建` 之前的 `---` 之前，**新增三节**（标题、yaml、表格逐字照下面写；
`###` 层级与既有节一致）：

```
### TDengine — TdengineConfig
```

```yaml
tdengine:
  base_url: "http://host:6041"
  username: "root"
  password: "taosdata"
  # database: "my_db"        # 可选：不填则用 REST 路径里的默认库
  # query_timeout_secs: 30   # 可选：单次调用超时，0 = 禁用
  # breaker: {}              # 可选：熔断配置，省略 = 保守默认（0.5 / 30s / 打开 10s）
  # max_concurrency: 32      # 可选：并发上限（本 crate 的信号量）
```

```
| 字段 | 类型 | 说明 |
|------|------|------|
| `base_url` | `String` | REST 接口地址（taosAdapter，默认端口 6041） |
| `username` | `String` | 用户名 |
| `password` | `String` | 密码 |
| `database` | `Option<String>` | 可选：默认库名（拼进 REST 路径） |
| `query_timeout_secs` | `Option<u64>` | 单次调用超时秒数；省略 = `30`，**`0` = 禁用** |
| `breaker` | `Option<BreakerConfig>` | 熔断阈值与窗口；没有 `enabled` 总开关 |
| `max_concurrency` | `Option<usize>` | 并发上限（默认 `32`）；本 crate 自己的信号量 |

**整次调用一个预算**：`write()` 会把一批数据点分片成多次 HTTP 请求，`query_timeout_secs` 罩住**整次调用**（所有分片），不是每片一个预算。
```

```
### MongoDB — MongoConfig
```

```yaml
mongodb:
  url: "mongodb://host:27017"
  database: "app"
  # max_pool_size: 10        # 可选：连接池上限，省略 = 驱动默认（**10**）
  # min_pool_size: 0         # 可选：连接池下限（后台保活连接数）
  # query_timeout_secs: 30   # 可选：单次命令超时，0 = 禁用
  # breaker: {}              # 可选：熔断配置，省略 = 保守默认（0.5 / 30s / 打开 10s）
```

```
| 字段 | 类型 | 说明 |
|------|------|------|
| `url` | `String` | 连接 URI（认证、副本集、TLS 选项都写在 URI 里） |
| `database` | `String` | 数据库名 |
| `max_pool_size` | `Option<u32>` | 连接池上限；省略 = 驱动默认 **10**（`mongodb` 3.8.0 实测，非 100） |
| `min_pool_size` | `Option<u32>` | 连接池下限；省略 = 驱动默认 `0` |
| `query_timeout_secs` | `Option<u64>` | 单次命令超时秒数；省略 = `30`，**`0` = 禁用** |
| `breaker` | `Option<BreakerConfig>` | 熔断阈值与窗口；没有 `enabled` 总开关 |

**并发背压走驱动连接池**：本 crate **没有** `max_concurrency`（也不是 HTTP）—— 驱动自带连接池，需要更高并发就显式配 `max_pool_size`。
```

```
### S3 / MinIO — S3Config
```

```yaml
s3:
  endpoint: "http://host:9000"
  region: "us-east-1"
  access_key: "minioadmin"
  secret_key: "minioadmin"
  # query_timeout_secs: 30   # 可选：单次调用超时，0 = 禁用
  # breaker: {}              # 可选：熔断配置，省略 = 保守默认（0.5 / 30s / 打开 10s）
  # max_concurrency: 32      # 可选：并发上限（本 crate 的信号量）
```

```
| 字段 | 类型 | 说明 |
|------|------|------|
| `endpoint` | `String` | S3 兼容服务地址（MinIO / 自建网关） |
| `region` | `String` | 签名用的区域；MinIO 对取值不敏感，填 `us-east-1` 即可 |
| `access_key` | `String` | Access Key |
| `secret_key` | `String` | Secret Key |
| `query_timeout_secs` | `Option<u64>` | 单次调用超时秒数；省略 = `30`，**`0` = 禁用** |
| `breaker` | `Option<BreakerConfig>` | 熔断阈值与窗口；没有 `enabled` 总开关 |
| `max_concurrency` | `Option<usize>` | 并发上限（默认 `32`）；本 crate 自己的信号量 |

**整次调用一个预算**：`list()` 跟随 continuation token 翻页、一次调用发多个 GET，超时罩住**整次翻页**。
```

(c) 三节之后、`---` 之前加一条共同说明（避免在 8 个节点里重复）：

```
> **出站韧性共同点**（本节全部 HTTP 后端）：顺序 **许可 → 熔断 → 超时**；超时错误 `code = DeadlineExceeded`，熔断拒绝 `code = Unavailable` 且 `message = "circuit breaker is open"`；`metrics` feature 下按 `ecat_outbound_timeouts_total{backend="<配置节名>"}` 计数 —— 标签是**配置节名**（`"arangodb"` / `"mongodb"` / …），不是 trait 类别名。
```

**Step 4（15 分钟）B 组 12 个镜像**：同一文件路径把 `docs/database-config-tutorial.md` 换成
`docs/i18n/<lang>/database-config-tutorial.md`，**逐语言翻译 Step 3 的每一段**
（字段名、crate 名、`RdbmsError::Timeout` 等代码标识符不译；表格列数与该文件既有表格保持一致）。
**先 `grep -c '### ' docs/i18n/ja/database-config-tutorial.md` 确认镜像结构同构**再动手；
若某语言镜像的既有节数/表列数与根文件不同，**停下报 lead**，不要猜。

**验收（B 组，非空）**：

```bash
for f in docs/database-config-tutorial.md docs/i18n/*/database-config-tutorial.md; do
  printf '%s %s %s %s\n' "$(grep -c 'query_timeout_secs' "$f")" "$(grep -c 'max_concurrency' "$f")" "$(grep -c 'max_pool_size' "$f")" "$f"
done
# 期望：13 行的前三个数字**全等**（且等于根文件的值；具体数值以实现为准，先记下再比）
for f in docs/database-config-tutorial.md docs/i18n/*/database-config-tutorial.md; do
  for h in TdengineConfig MongoConfig S3Config; do grep -q "$h" "$f" || echo "MISS $h in $f"; done
done; echo DONE
# 期望：只有最后的 DONE（13 个文件都有三节）
```

**Step 5（12 分钟）C 组 14 个 README**。每个文件两处改动：

(a) 「支持的数据库」表尾列的 **13 个单元格**（行由 `ecat-data-<crate>` 定位，**不要按行号改** ——
各语言表格行序相同，但行号会漂）：

| crate | 现在 | 改成 |
|-------|------|------|
| `ecat-data-redis` / `ecat-data-opensearch` / `ecat-data-elasticsearch` / `ecat-data-neo4j` / `ecat-data-nebulagraph` / `ecat-data-arangodb` / `ecat-data-influxdb` / `ecat-data-iotdb` / `ecat-data-tdengine` / `ecat-data-mongodb` / `ecat-data-s3` | `—` | 「超时 + 熔断」 |
| `ecat-data-clickhouse` / `ecat-data-questdb` | 「熔断」 | 「超时 + 熔断」 |
| `ecat-data-memcached` | `—` | **不动**（内存实现，无出站调用） |

**文案取该文件自己已有的写法**（各语言都不译 ❌/✅，从**同一个文件的 sqlx 行**里抠出尾列文本即可）：

```bash
awk -F'|' '/ecat-data-sqlx/ {print $(NF-1); exit}' README.en.md   # 该文件该列的既有文案
```

(b) 「超时/熔断」脚注（`README.md:159` 那条）。中文版逐字替换：

```
> **超时/熔断**：超时 = 查询超时 `query_timeout_secs`（默认 30 秒，`0` = 禁用；除 memcached 内存实现外，**所有**数据后端均可配，MongoDB 的并发背压走驱动连接池 `max_pool_size`）；熔断 = **每个 client 自带** `ecat_circuit_breaker::Breaker`（失败率 0.5 / 窗口 30 秒 / 半开探测 3 / 打开 10 秒，无 `enabled` 总开关）；HTTP 后端另有 `max_concurrency`（默认 32）。`ecat_data::CircuitBreakerExecutor` 仍可包装任意 `SqlExecutor` 后端。读写分离用 `ecat_data::RdbmsRouting`：写落主库、读落副本轮询，并**跳过已熔断的副本**；副本全不可用时默认降级读主，`fallback_to_primary(false)` 则报 `RdbmsError::NoAvailableReplica`。
```

其余 13 个文件按各自语言改写同一句（`query_timeout_secs` / `max_pool_size` / `max_concurrency`
等标识符与产品名不译）。

**验收（C 组，非空）**：

```bash
for f in README.md README.en.md docs/i18n/*/README.md; do
  n=$(awk -F'|' '/^\| .*ecat-data-/ {s=$(NF-1); gsub(/ /,"",s); if (s ~ /^✅/) n++} END{print n+0}' "$f")
  [ "$n" = 18 ] || echo "MISMATCH $n $f"
done; echo DONE
# 期望：只有 DONE（14 份基线都是 7 → 18；某个镜像没改到就在上面打印 MISMATCH）

# 脚注是否 14 份都改到了：新脚注里有**不可翻译**的标识符 `max_concurrency`
# （实测：改之前 14 份 README 里 0 次 —— 这条不会因老文本而假绿）
for f in README.md README.en.md docs/i18n/*/README.md; do
  grep -q 'max_concurrency' "$f" || echo "MISS footnote $f"
done; echo DONE
# 期望：只有 DONE
```

**Step 6（4 分钟）四组各自提交**：

```bash
git add docs/superpowers/checklists/backend-resilience-onboarding.md
git commit --only docs/superpowers/checklists/backend-resilience-onboarding.md -m "docs(checklist): guarded 的 kind 写死（仅多 I/O 路径的 crate 才带参数）—— 与 5b 的 T0 口径统一"
git add config/databases.example.yaml
git commit --only config/databases.example.yaml -m "docs(config): 示例配置补齐出站韧性字段与 tdengine/mongodb/s3 三节"
git commit --only docs/database-config-tutorial.md docs/i18n/*/database-config-tutorial.md -m "docs(tutorial): 补齐 5b 十一个后端的出站韧性字段与三节新后端（13 份镜像）"
git commit --only README.md README.en.md docs/i18n/*/README.md -m "docs(readme): 支持数据库表「超时/熔断」列更新到 18/19（14 份镜像）"
```

（0 组**必做、且最先做** —— 它改的是 5b 工人照着写的模板；A/B/C 三组可任意顺序。
这四处提交都**不要**带上别人的改动。**若某组此刻已经没有 diff**（例如 0 组已由 lead 提前
改并提交），那条 `--only` 提交会报 `nothing to commit` —— 跳过它，**不要**为了让命令有输出
而制造改动。）

> `git commit --only <paths>` 会**只**提交列出的路径（共享 index 里别人的暂存文件不受影响）；
> 命中 `.index.lock` **不要删锁**，等 15 秒重试。

---

## Task 13：CHANGELOG + 版本号 6.0.0 → **7.0.0** + 全量闸门

> **版本号已由 lead 裁决 = `7.0.0`（major）**，判据是本仓自己的先例，不是偏好：
> 6.0.0 的 CHANGELOG 已经把**同一处改动**（`RedisConfig` / `ClickhouseConfig` 加公开字段）
> 写进**破坏性变更**段，理由原文是「两个结构体都没有 `Default` 也没有 `#[non_exhaustive]`，
> 用**结构体字面量**构造的用户代码需要补字段」。实测确认 11 个目标 crate 的 Config 同样是
> 朴素 `#[derive(Debug, Clone, Deserialize)]`（`impl Default` / `derive(Default)` 命中均为 0，
> 无 `#[non_exhaustive]`）⇒ 同一个改动在相邻两次发布里不能两套标准 ⇒ **7.0.0**。
> 因此本任务**必须有破坏性变更段**（见 Step 2），且 sed 的替换串是 `7.0.0`。
>
> 另记一条**本批不做**的债（lead 自行跟踪，别写进 CHANGELOG）：给这些 Config 加
> `#[non_exhaustive]` 能让将来再加字段是 minor —— 但那本身也是破坏性变更，值得单独一批。

**Files**：`CHANGELOG.md`、`Cargo.lock`、14 个 README、`config/databases.example.yaml`、
以及 61 个受版本串影响的 `Cargo.toml`（其中 **38 个**带 `"6.0.0"`；其余是纯 path 依赖）。

**Step 1（3 分钟）改版前测量**（把数字记进 commit message）：

```bash
git ls-files '*Cargo.toml' | wc -l                                        # 期望 61（实测）
git ls-files '*Cargo.toml' | xargs grep -l '"6\.0\.0"' | wc -l            # 记作 N —— 实测 38
grep -h '6\.0\.0' README.md README.en.md docs/i18n/*/README.md | wc -l    # 记作 M —— 期望 28（每份 2 处）
grep -c 'v6\.0\.0' config/databases.example.yaml                          # 记作 K —— Task 12 后 = 1
```

**README 的版本串是「裸」的**（实测 `README.md:8` `（v6.0.0 · 56 crates）`、
`README.md:557` `（当前 6.0.0）`；14 份都是这两处 = 28）—— **不要**带引号 sed，
那会一个都改不到（实测 `grep -h '"6\.0\.0"' README*` 为 0 行）。

**Step 2（6 分钟）`CHANGELOG.md` 顶部插入**（`# Changelog` 与 `## [6.0.0]` 之间，逐字；
标题用**破折号** `—`，与既有条目一致）：

```markdown
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
- 11 个 crate 各新增 `metrics` feature：把 `ecat_outbound_timeouts_total` /
  `ecat_outbound_breaker_opened_total` / `ecat_outbound_breaker_state` 挂进
  `ecat-metrics` 的共用 collector，标签 = **配置节名**（`"arangodb"` / `"mongodb"` / …）。
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
```

**Step 3（3 分钟）bump 版本号**：

```bash
git ls-files '*Cargo.toml' | xargs sed -i 's/"6\.0\.0"/"7.0.0"/g'          # Cargo.toml 里带引号
sed -i 's/6\.0\.0/7.0.0/g' README.md README.en.md docs/i18n/*/README.md   # README 里是裸串
sed -i 's/6\.0\.0/7.0.0/g' config/databases.example.yaml                  # `— v6.0.0` → `— v7.0.0`
```

**验收（不变式，非空）**：

```bash
git ls-files '*Cargo.toml' | xargs grep -l '"6\.0\.0"' | wc -l     # 期望 0
git ls-files '*Cargo.toml' | xargs grep -l '"7\.0\.0"' | wc -l     # 期望 = N（= 38，与 Step 1 相等：文件数不变）
grep -h '6\.0\.0' README.md README.en.md docs/i18n/*/README.md | wc -l    # 期望 0
grep -h '7\.0\.0' README.md README.en.md docs/i18n/*/README.md | wc -l    # 期望 = M（= 28，裸串两处 × 14 份）
grep -c '6\.0\.0' config/databases.example.yaml                    # 期望 0
grep -c 'v7\.0\.0' config/databases.example.yaml                   # 期望 = K（= 1）
# 只允许版本行的改动（防止 sed 误伤别的 "6.0.0"）：
git diff -U0 -- '*.toml' | grep -E '^-[^-]' | grep -vc '6\.0\.0'   # 期望 0
git diff -U0 -- '*.toml' | grep -E '^\+[^+]' | grep -vc '7\.0\.0'  # 期望 0
```

**Step 4（3 分钟）重生成 `Cargo.lock`**（不改 lock 的话 workspace 成员版本会与 Cargo.toml 不一致）：

```bash
export CARGO_TARGET_DIR=/var/tmp/ecat-target
cargo metadata --format-version=1 --offline > /dev/null
git diff --stat Cargo.lock          # 期望：只有 Cargo.lock 与 11 个 workspace 成员版本行变化
git diff Cargo.lock | grep -c '^[-+]version = "6\.1\.0"'   # > 0（成员版本被写回）
```

**Step 5（15 分钟）全量闸门** —— **仅在 11 个任务都已提交、且确认没有别的 agent 在飞时执行**
（`--all` 与全 workspace 测试会与并行任务抢 target dir / 触发 fmt 冲突）：

```bash
export CARGO_TARGET_DIR=/var/tmp/ecat-target
cargo fmt --all -- --check                                              # 期望：无输出
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3    # 期望：无 error（warning 也不允许）
cargo test --workspace 2>&1 | grep -cE 'test result: FAILED|^error'    # 期望 0
cargo test --workspace 2>&1 | grep -c 'test result: ok'                 # 记下；改版前跑一次，两次数值必须相等
for c in arangodb neo4j nebulagraph elasticsearch opensearch influxdb iotdb tdengine questdb s3 mongodb; do
  cargo test -p ecat-data-$c --features metrics 2>&1 | grep -q 'test result: FAILED' && echo "FAIL $c"
done; echo DONE
# 期望：只有 DONE（11 个 crate 的 metrics 变体全绿 —— 含各自 +1 条指标现读用例）
```

**Step 6（5 分钟）提交**：

```bash
git commit --only CHANGELOG.md Cargo.lock config/databases.example.yaml README.md README.en.md docs/i18n/*/README.md $(git ls-files '*Cargo.toml') -m "chore(release): 6.0.0 → 7.0.0（批次 5b：11 个后端出站韧性；Config 新增公开字段 = major）"
```

（命中 `.index.lock` 等 15 秒重试，**不要删锁**。）

---

## 批次完成判据（全部命中才算 5b 完成）

| # | 判据 | 命令 | 期望 |
|---|------|------|------|
| 1 | 11 个 crate 主测试全绿 | 逐个 `cargo test -p ecat-data-<c>` | arangodb 13 / neo4j 10 / nebulagraph 14 / elasticsearch 19 / opensearch 18 / influxdb 18 / iotdb 19 / tdengine 20 / questdb 17 / s3 24 / mongodb 13 |
| 2 | 11 个 crate 的 metrics 变体全绿 | 逐个 `--features metrics` | 各自再 +1（14/11/15/20/19/19/20/21/18/25/14） |
| 3 | 无 crate 漏包 I/O 方法 | `for c in arangodb neo4j nebulagraph elasticsearch opensearch influxdb iotdb tdengine questdb s3 mongodb; do echo "$c $(grep -c 'self\.guarded(' ecat-data-$c/src/lib.rs)"; done` | 与 T0-D 的方法数逐 crate 相等：arangodb 1 / neo4j 1 / nebulagraph 1 / elasticsearch 3 / opensearch 3 / influxdb 2 / iotdb 2 / tdengine 2 / questdb 2 / s3 4 / mongodb 4（`guarded` 的定义行是 `async fn guarded<…>`，不含 `self.` 前缀，数不到；4 个拆了测试的 crate 其 lib.rs 里没有测试调用点，不会虚高） |
| 4 | 见证槽无写者冲突 | `grep -rn "TIMEOUTS\[" ecat-data-{11 个}/src/` | 只有 `witness_*_slot_is_untouched_here` 里的**读**，没有任何测试写非本 crate 维度 |
| 5 | 文件行数 | `wc -l ecat-data-*/src/*.rs ecat-data-*/src/tests/*.rs` | 全部 < 500 |
| 6 | 文档同步 | Task 12 的验收 | README 14 份均 18；教程 13 份含三节且计数全等；example.yaml `query_timeout_secs` = 14 |
| 7 | 版本与 lock 一致 | Task 13 Step 3/4 的不变式 | 见上 |
| 8 | 全量闸门 | `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets`、`cargo test --workspace` | 全绿、零 warning |
| 9 | 空验收自证 | 每个 Task 的「空验收自证」步 | 逐条**实测复现**（删掉新增物 → 指定用例变红），失败即该任务的判据不成立 |

## 落码期片段纠偏条款（本计划**必然**有片段与实测不一致）

5a 的经验：计划里的代码片段在真编译前是**待验证假设**，实测出入要**单列成「偏离」**并附证据，
由 lead 逐条裁决，**不准**默默改掉。本批同样适用：

1. **片段编译不过 / API 形状不同**（如 `ClientOptions` 字段名、`Semaphore` 方法签名、
   mongodb 3.8.0 的 `try_collect` 是否在 prelude）→ 以**实测**为准改代码，
   在该任务末尾追加 `> **偏离 <n>**：<原片段> → <实际>（证据：<命令 + 输出要点 / 文件:行>）`。
2. **期望测试数与实测不符** → 先怀疑**漏加 `mod resilience;`** 或测试块被 `#[cfg]` 掉
   （本仓已因此犯过三次），确认不是这两个原因后再按实测更新数字，并说明差额来自哪几条。
3. **验收命令本身失效**（如 `grep -c` 数到别处、`awk` 取错列）→ 换**结构化**判据
   （`git ls-files` / `cargo metadata --format-version=1` / `cargo test --format json`），
   不要用「数字看起来对」蒙过去。
4. **与 T0 模板的偏离**（如某 crate 的 `guarded` 签名必须不同）→ 只允许「模板 + 该 crate 的
   显式偏离说明」这一种形态；**不允许**顺手把模板改成「更通用」的样子（模板是 11 个 crate 的
   共同契约，改一处等于改十一处）。
5. **发现 5a 的坑复发**（见证槽被别人写、`cargo fmt --all` 误伤、`git commit` 误带别人的文件）
   → 立刻停下报 lead，不要自行「顺手修」别人的 crate。

---

**计划完。核对清单（作者自检）**：

- [x] 11 个后端逐个成任务，每个都点名「照 checklist 的哪一节 + 抄的 pilot 哪几行」（§2 表 + 各任务 Files 段）
- [x] 每个 Task 都有：Files（绝对路径）→ 2-5 分钟粒度步骤 → 每步可运行命令 + 期望输出 → 提交
- [x] 新 `.rs` 一律带「在父模块加 `mod xxx;` + 测试数必须增加」的判据
- [x] 每个验收都能回答「删掉本任务新增物后，它还通过吗」（各 Task 的「空验收自证」步）
- [x] 见证槽只读不写、不用锁兜底（T0-F + 各任务 Step）
- [x] 任务级 fmt/clippy 一律 `-p <crate>`，`--all` 只出现在 Task 13 的发布闸门
- [x] 文件 < 500 行（4 个必须拆测试文件的 crate 单列拆法）
- [x] 所有 cargo 命令带 `CARGO_TARGET_DIR=/var/tmp/ecat-target`；提交一律 `git commit --only`；锁命中等 15 秒不删锁
- [x] 与 spec / checklist 的 15 处实测出入逐条附证据（§1）





