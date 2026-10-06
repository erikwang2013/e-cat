# 批次 2 — `ecat-data-mssql`（SQL Server 后端）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增 `ecat-data-mssql` crate，用 `tiberius-ng` + `deadpool` 实现 `SqlExecutor`，让 e-cat 的数据后端从 15 个增至 **16 个**。

**Architecture:** 驱动细节全部封装在本 crate 内（`tiberius::Client` 不外泄）。`MssqlClient` 持一个 `deadpool` 池；`SqlExecutor` 的每个方法取池连接、按 `@P1..@Pn` 绑定参数、执行、把 `ColumnData` 映射成 `ecat_data::Row`。

**Tech Stack:** Rust 2024 · `tiberius-ng` 0.13（`rustls` + `tds73` + `time`）· `deadpool` 0.13 · `tokio-util`（`compat`）· `time` · `async-trait`

**Spec:** [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../specs/2026-10-05-orm-and-mssql-design.md) §3

**前置:** 批次 1 已合入（`SqlExecutor` / `Dialect` / `run_with_timeout` 均在）

---

## 已核实的 API 事实（**不要重新猜，直接用**）

来源：`tiberius-ng-0.13.1` 源码（已解包核对）。

> ### ⚠️ 第一条事实：crate 名是 **`tiberius`**，不是 `tiberius_ng`
>
> 包名与 lib 名不同：`Cargo.toml:15` 是 `name = "tiberius-ng"`（**依赖里写这个**），
> 但 `Cargo.toml:98-100` 有 `[lib] name = "tiberius"` —— **代码里必须写
> `use tiberius::{Config, AuthMethod};`**。
>
> 写 `tiberius_ng::Config` **直接编译不过**。本计划的初稿在 7 处写错了这个前缀
> （已订正），Task 2 的实现者第一次就是这么挂的。**Task 3/4 的注意。**

| 事实 | 出处 |
|---|---|
| `Client::connect(config: Config, tcp_stream: S) -> Result<Client<S>>` | `src/client.rs:94` |
| 连接用法：`TcpStream::connect(config.get_addr()).await?` 后 **`tcp.compat_write()`** | `src/lib.rs:28-34` |
| `client.query(sql, &[&dyn ToSql]) -> Result<QueryStream>` —— **需要 `&mut self`** | `src/client.rs:202` |
| `client.execute(sql, &[&dyn ToSql]) -> Result<ExecuteResult>` —— 同样 `&mut self` | `src/client.rs:143` |
| 占位符是 **`@PN`**，N 从 **1** 开始 | `src/client.rs:100-105` 文档 |
| `Row::get_column_data(idx) -> Result<&ColumnData<'static>>` | `src/row.rs:433` |
| `Row::columns() -> &[Column]` | `src/row.rs:323` |
| **`ColumnData` 的每个变体的值都是 `Option<T>`** → NULL 就是 `None` | `src/tds/codec/column_data.rs:104-149` |
| `Config::from_ado_string(s)` / `from_jdbc_string(s)` 可解析连接串 | `src/client/config.rs:489,500` |
| TLS 由 `Config::encryption(EncryptionLevel)` 控制，**默认 `Required`**，握手在 `connect()` 内部完成，**无需外部 TLS connector** | `src/client/config.rs:137,254` |
| `EncryptionLevel`：`Off=0`（仅登录加密）/ `On=1`（能加就加）/ `NotSupported=2`（不加密）/ `Required=3`（必须，默认） | `src/tds.rs:19-27` |
| 证书相关：`trust_cert()`（跳过校验）/ `trust_cert_ca(path)` / `client_certificate(cert, key)` / `hostname_in_certificate(h)` | `src/client/config.rs:268,284,384,302` |

### 由此得到的三个**结构性简化**（与 sqlx 路径对比）

1. **没有类型链，也不需要 NULL 闸门。** `ColumnData` 是带标签的枚举 —— 直接 `match`
   变体即可。批次 1 那两个 Critical（`bool` 抢整数、`u64` 被漏）根源于 sqlx 路径的
   **「按顺序试探类型」**；tiberius 直接给出类型，**这类坑结构性地不存在**。
   同样，`Option<T>` 让 NULL 天然是 `None`，**不需要额外的 NULL 分支**。
2. **TLS 不用自己搭 connector。** 设 `EncryptionLevel` + 几张证书开关即可。
3. **不必只支持 URL** —— `from_ado_string` 能直接吃 MSSQL 惯用的
   `Server=...;Database=...;User Id=...` 形态。

---

## 文件结构

```
ecat-data-mssql/
  Cargo.toml
  src/lib.rs          MssqlClient + SqlExecutor 实现 + 参数绑定 + warm_up/pool_status
  src/config.rs       MssqlConfig + URL/ADO 解析 + TLS 映射 + 池参数
  src/pool.rs         deadpool Manager（create / 智能 recycle）
  src/cell.rs         ColumnData → serde_json::Value（单个 match，无需链）
  src/tests.rs        单元测试（不连库）
  src/live_tests.rs   env 门控真库测试（ECAT_TEST_MSSQL_URL / ECAT_REQUIRE_LIVE_DB）
```

（与 `ecat-data-sqlx` 重构后的形状一致：配置 / 池 / 行转换 / 测试各自独立，
文件均 < 500 行。）

---

## Task 1: crate 骨架与依赖

**Files:**
- Create: `ecat-data-mssql/Cargo.toml`
- Create: `ecat-data-mssql/src/lib.rs`（最小可编译骨架）
- Modify: 根 `Cargo.toml`（members + workspace dependencies）

- [ ] **Step 1: 根 Cargo.toml 加 workspace 依赖**

```toml
tiberius-ng = { version = "0.13", default-features = false, features = ["rustls", "tds73", "time"] }
deadpool = "0.13"
tokio-util = { version = "0.7", features = ["compat"] }
```

> `default-features = false` **是必需的**：默认开 `native-tls` + `winauth`，与项目 rustls 栈冲突。
> `time` feature 让它原生支持 `time` 类型（行转换要用）。
> `tds73` 覆盖 SQL Server 2008+；`tds80` 会放弃较老的服务器。

把 `"ecat-data-mssql"` 加进 `[workspace] members`。

- [ ] **Step 2: crate 的 Cargo.toml**

```toml
[package]
name = "ecat-data-mssql"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Microsoft SQL Server client for e-cat (tiberius-ng + deadpool)"
repository.workspace = true
homepage.workspace = true
keywords.workspace = true
categories.workspace = true

[dependencies]
ecat-data = { version = "3.0.3", path = "../ecat-data" }
ecat-tls = { version = "3.0.3", path = "../ecat-tls" }
tiberius-ng = { workspace = true }
deadpool = { workspace = true }
tokio-util = { workspace = true }
tokio = { workspace = true, features = ["net", "sync", "time"] }
time = { workspace = true }
async-trait.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
base64 = "0.22"

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
```

- [ ] **Step 3: 最小骨架**

`src/lib.rs` 首行版权注释 `// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz`，
先只放 `pub struct MssqlClient;` 让 crate 能编译。

- [ ] **Step 4: 验收**

```bash
cargo check -p ecat-data-mssql
# 预期：编译通过（会拉取 tiberius-ng / deadpool）
cargo audit --deny warnings
# 预期：通过（tiberius-ng 0 条告警，是选它的原因）
```

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml Cargo.lock ecat-data-mssql/
git commit -m "feat(ecat-data-mssql): crate 骨架与依赖（tiberius-ng + deadpool）"
```

---

## Task 2: `MssqlConfig`（连接串解析 + TLS 映射 + 池参数）

**Files:**
- Create: `ecat-data-mssql/src/config.rs`
- Test: `config.rs` 内联测试

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_form_parses_into_fields() {
        let c = MssqlConfig::from_str("mssql://sa:pw@db.local:1433/app").unwrap();
        assert_eq!(c.host, "db.local");
        assert_eq!(c.port, 1433);
        assert_eq!(c.database.as_deref(), Some("app"));
        assert_eq!(c.username.as_deref(), Some("sa"));
        assert_eq!(c.password.as_deref(), Some("pw"));
    }

    /// ADO 串是 MSSQL 的惯用形态，直接交给 tiberius 的解析器，不自己写。
    #[test]
    fn ado_string_is_accepted() {
        let c = MssqlConfig::from_str(
            "Server=db.local,1433;Database=app;User Id=sa;Password=pw",
        )
        .unwrap();
        assert_eq!(c.host, "db.local");
        assert_eq!(c.database.as_deref(), Some("app"));
    }

    /// `skip_verify` 与 `ca_cert` 互斥 —— 与 `TlsClientConfig` 同一条规则：
    /// 同时配置等于「既要校验又不校验」，必须报错而非静默选一个。
    #[test]
    fn skip_verify_and_ca_cert_are_mutually_exclusive() {
        let c: MssqlConfig = serde_json::from_str(
            r#"{"url": "mssql://h/db", "tls": {"skip_verify": true, "ca_cert": "/tmp/ca.pem"}}"#,
        )
        .unwrap();
        assert!(c.build_config().is_err());
    }

    #[test]
    fn pool_defaults_match_documented_values() {
        let c: MssqlConfig = serde_json::from_str(r#"{"url": "mssql://h/db"}"#).unwrap();
        let p = c.pool();
        assert_eq!(p.max_connections, 10);
        assert_eq!(p.acquire_timeout, Duration::from_secs(30));
        assert_eq!(p.query_timeout, Some(Duration::from_secs(30)));
    }

    /// `query_timeout_secs: 0` = 禁用（与 SqlxConfig 同约定）。
    #[test]
    fn zero_query_timeout_means_disabled() {
        let c: MssqlConfig =
            serde_json::from_str(r#"{"url": "mssql://h/db", "query_timeout_secs": 0}"#).unwrap();
        assert_eq!(c.query_timeout(), None);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p ecat-data-mssql config`
Expected: 编译失败，`cannot find type MssqlConfig`

- [ ] **Step 3: 实现**

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct MssqlConfig {
    /// 连接串。两种形态都接受：
    /// - URL：`mssql://user:pass@host:1433/db?encrypt=required`
    /// - ADO：`Server=host,1433;Database=db;User Id=user;Password=pass`
    pub url: String,
    #[serde(default)] pub database: Option<String>,
    #[serde(default)] pub tls: Option<TlsClientConfig>,
    /// 会话初始化语句。默认 `SET ARITHABORT ON`（见 §2.7 的理由：
    /// 避免 ARITHABORT 取值不同导致同一查询产生多份执行计划）。
    #[serde(default)] pub session_init: Option<Vec<String>>,

    #[serde(default)] pub max_connections: Option<u32>,
    #[serde(default)] pub min_connections: Option<u32>,   // deadpool 无保底概念，warm_up 用
    #[serde(default)] pub acquire_timeout_secs: Option<u64>,
    #[serde(default)] pub idle_timeout_secs: Option<u64>,
    /// `0` = 禁用。
    #[serde(default)] pub query_timeout_secs: Option<u64>,
    /// 逐端点熔断——本批次**不做**（批次 4），字段留待那时。
    #[serde(default)] pub encrypt: Option<bool>,
}
```

`MssqlConfig` 需要：
- `from_str(s) -> Result<Self, MssqlError>` —— 判定 URL vs ADO。**判定规则**：以
  `mssql://` / `sqlserver://` 开头视作 URL，否则交给 `tiberius::Config::from_ado_string`
- `build_config() -> Result<tiberius::Config, MssqlError>` —— 把字段落到
  `tiberius::Config`：`host` / `port` / `database` / `application_name("ecat")` /
  `encryption(...)`；TLS 映射见下；**`skip_verify` 与 `ca_cert` 同时存在则返回 `Err`**
- `pool() -> MssqlParams`（与 `SqlxConfig::pool()` 同构，含 `min` 夹到 `max`）
- `query_timeout() -> Option<Duration>`（三态，同 `SqlxConfig`）
- `effective_session_init() -> Vec<String>`（默认 `["SET ARITHABORT ON"]`）

**TLS 映射**（`TlsClientConfig` → tiberius）：

| 我们的字段 | tiberius 调用 |
|---|---|
| `skip_verify == Some(true)` | `config.trust_cert()` |
| `ca_cert = Some(path)` | `config.trust_cert_ca(path)` |
| `client_cert` + `client_key` | `config.client_certificate(cert, key)` |
| 都未设 | 不动（保持默认 `EncryptionLevel::Required`） |

> **互斥规则照抄 `TlsClientConfig` 自己的约定**：它已明确「`skip_verify` 与 `ca_cert`
> 同时配置时构建会报错，防止误配静默关闭证书校验」。这里保持一致 —— 本项目不接受
> 「静默选一个」。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p ecat-data-mssql config`
Expected: 5 个测试全过

- [ ] **Step 5: 提交**

```bash
git add ecat-data-mssql/src/config.rs ecat-data-mssql/src/lib.rs
git commit -m "feat(ecat-data-mssql): MssqlConfig（URL/ADO 解析 + TLS 映射 + 池参数）"
```

---

### ⚠️ Task 5 必读：deadpool 侧的三个超时缺一不可（Task 3 实施者预警）

`recycle` 的探活面对**半开连接**（服务端已消失但 TCP 未 RST）会**阻塞在 read 上**，
直到 OS 重传超时 —— 可能几十秒到分钟级。而 `recycle` 是在 **`Pool::get()` 路径上同步等**的。

**后果**：一次死连接就能把取连接卡到 `acquire_timeout` 为止。

**建池时必须设三个超时**（`PoolBuilder`）：

| 超时 | 作用 |
|---|---|
| `wait_timeout` | 取连接的总等待（= `MssqlParams.acquire_timeout`） |
| **`create_timeout`** | 建连（含 TCP + TDS 握手 + TLS + `session_init`）的上限 |
| **`recycle_timeout`** | **探活的上限** —— 缺它则半开连接会卡住 `Pool::get()` |

spec §2.8 那条链（「关闭每次 ping 后由 `max_lifetime` + 查询超时 + 首次使用报错兜底」）
在 deadpool 侧**还缺 recycle/create 超时这一环** —— 补上它，链条才完整。

> 这条是 Task 3 的实施者主动向后续任务预警的（它的原话：「请务必在池上设 …，
> 否则一次死连接可能把取连接卡死到 acquire 超时」）。**一个任务发现的坑变成下个任务的护栏**，
> 比只在自己这里绕过有价值得多。

## Task 3: `MssqlManager`（deadpool）

**Files:**
- Create: `ecat-data-mssql/src/pool.rs`
- Test: `pool.rs` 内联测试（只测可测的部分，真连接在 Task 8）

- [ ] **Step 1: 实现**

```rust
/// deadpool 的连接管理器。
pub struct MssqlManager {
    config: tiberius::Config,
    session_init: Vec<String>,
    /// 空闲多久之内跳过 `SELECT 1` 探活。每次归还都探活会多一个网络往返；
    /// 而 SQL Server 的往返不便宜。默认 5 秒。
    recycle_idle_threshold: Duration,
}

#[async_trait]
impl deadpool::managed::Manager for MssqlManager {
    type Type = tiberius::Client<tokio_util::compat::Compat<tokio::net::TcpStream>>;
    type Error = RdbmsError;

    async fn create(&self) -> Result<Self::Type, Self::Error> {
        // 1. TcpStream::connect(self.config.get_addr())
        // 2. Client::connect(self.config.clone(), tcp.compat_write())
        // 3. 逐条执行 session_init —— **任一条失败即连接创建失败**（不静默降级）
    }

    async fn recycle(
        &self,
        client: &mut Self::Type,
        metrics: &deadpool::managed::Metrics,
    ) -> deadpool::managed::RecycleResult<Self::Error> {
        // metrics 给出空闲时长；低于阈值直接 Ok（省一个往返），超阈值才 SELECT 1。
        // **先核对 deadpool 0.13 的 Metrics 实际字段名再写**（`src/managed/metrics.rs`）。
    }
}
```

**要点**：
- `Manager::Type` 是**泛型具体化的** `Client<Compat<TcpStream>>` —— 驱动类型不外泄
- `create` 里执行 `session_init`；**失败即让连接创建失败**（spec §2.7：不静默降级）
- `recycle` 用 `metrics` 判断空闲时长

- [ ] **Step 2: 验收**

```bash
cargo check -p ecat-data-mssql
cargo doc -p ecat-data-mssql --no-deps    # 0 warning
```

- [ ] **Step 3: 提交**

```bash
git add ecat-data-mssql/src/pool.rs
git commit -m "feat(ecat-data-mssql): deadpool Manager（建连 + 会话初始化 + 智能探活）"
```

---

## Task 4: 行转换 `cell.rs`

**Files:**
- Create: `ecat-data-mssql/src/cell.rs`
- Test: `cell.rs` 内联测试

- [ ] **Step 1: 实现**

```rust
/// `ColumnData` → `serde_json::Value`。
///
/// **单个 `match`，不是链** —— `ColumnData` 是带标签的枚举，直接知道类型；
/// sqlx 路径那条「按顺序试探」的链（批次 1 踩过 `bool` 抢整数、`u64` 漏掉两个 Critical）
/// 在这里**结构性地不存在**。NULL 也天然是 `None`，无需额外闸门。
pub fn cell_to_json(data: &ColumnData<'static>, col: &str) -> Result<serde_json::Value, RdbmsError> {
    use ColumnData as C;
    Ok(match data {
        C::U8(Some(v)) => json!(v),
        C::I16(Some(v)) => json!(v),
        C::I32(Some(v)) => json!(v),
        C::I64(Some(v)) => json!(v),
        C::F32(Some(v)) => float_to_json(*v as f64),   // 注意：f32 加宽，见下
        C::F64(Some(v)) => float_to_json(*v),
        C::Bit(Some(v)) => json!(v),
        C::String(Some(s)) => json!(s.as_ref()),
        C::Guid(Some(g)) => json!(g.to_string()),      // 标准带连字符 UUID
        C::Binary(Some(b)) => json!(BASE64.encode(b.as_ref())),
        // 时间：与 sqlx 路径**同一套契约**
        C::DateTime(Some(dt)) => json!(rfc3339(dt.into(), col)?),
        C::SmallDateTime(Some(dt)) => json!(rfc3339(dt.into(), col)?),
        C::DateTime2(Some(dt)) => json!(rfc3339(dt.into(), col)?),
        C::DateTimeOffset(Some(dt)) => json!(rfc3339(dt.into(), col)?),
        C::Date(Some(d)) => json!(d.to_string()),      // 纯 `YYYY-MM-DD`，与批次 1 定案一致
        C::Time(Some(t)) => json!(t.to_string()),      // 纯时刻，无日期
        // NULL：所有变体的 None
        C::U8(None) | C::I16(None) | /* …全部变体… */ => serde_json::Value::Null,
        // 链尾：不在支持范围内的类型**报错**，不返回 null
        other => return Err(unsupported(col, other)),
    })
}
```

**关键约束（与批次 1 的定案保持一致，不要自作主张）**：

| 类型 | 处理 | 依据 |
|---|---|---|
| 时间戳（`DateTime`/`DateTime2`/`SmallDateTime`/`DateTimeOffset`） | **RFC3339 UTC 字符串** | spec §6 契约 |
| **`Date`** | **纯 `YYYY-MM-DD`** | 批次 1 定案：源数据没有时刻/时区，**不伪造**。`time::Date` 的 `Display` 就是 `YYYY-MM-DD` |
| `Time` | 纯时刻字符串 | 同 `Date` 的理由（无日期分量） |
| `Numeric`（decimal） | **报错** | 与 sqlx 路径一致：Decimal 的表示形态要等 ORM 定，报错保证那时一定撞上 |
| `Xml` | **报错** | 同上，「不在支持范围内 → 响亮失败」 |
| `Guid` | 字符串 | 无歧义，直接映射 |

**`f32` 的处理**：与 sqlx 路径一样走**最短往返表示**（`n.to_string().parse::<f64>()`），
避免 `0.1f32` → `0.10000000149011612`。

**错误信息要带列名与类型名**（与 sqlx 路径的链尾一致）：
```
unsupported column type in result set: {col} ({type_name})
```

- [ ] **Step 2: 测试**

单测直接构造 `ColumnData` 值断言输出，**不需要数据库**：

```rust
#[test]
fn date_is_plain_yyyy_mm_dd_not_a_fabricated_instant() {
    let d = time::Date::from_calendar_date(2026, time::Month::October, 5).unwrap();
    let v = cell_to_json(&ColumnData::Date(Some(d)), "d").unwrap();
    assert_eq!(v, json!("2026-10-05"));
}

#[test]
fn null_is_null_for_every_variant() {
    assert!(cell_to_json(&ColumnData::I64(None), "x").unwrap().is_null());
    assert!(cell_to_json(&ColumnData::String(None), "x").unwrap().is_null());
}

#[test]
fn f32_uses_shortest_round_trip() {
    let v = cell_to_json(&ColumnData::F32(Some(0.1)), "f").unwrap();
    assert_eq!(v, json!(0.1));
}

#[test]
fn numeric_errors_loudly_with_column_and_type() {
    // 注意是 Some(_)：None 是「没有值」→ NULL，不是「表示形态未定」。
    // 初版计划这里写的是 Numeric(None)，与映射表里「任意变体的 None → Value::Null」
    // 自相矛盾 —— 由 Task 4 的实施者发现并订正。
    let err = cell_to_json(&ColumnData::Numeric(Some(dec)), "amount").unwrap_err();
    let m = err.to_string();
    assert!(m.contains("amount"), "got: {m}");
}
```

- [ ] **Step 3: 验收**

Run: `cargo test -p ecat-data-mssql cell`
Expected: 全绿

- [ ] **Step 4: 提交**

```bash
git add ecat-data-mssql/src/cell.rs
git commit -m "feat(ecat-data-mssql): ColumnData → JSON 行转换（单 match，无类型链）"
```

---

## Task 5: 参数绑定与 `SqlExecutor` 实现

**Files:**
- Modify: `ecat-data-mssql/src/lib.rs`
- Test: `src/tests.rs`（不连库的部分）

- [ ] **Step 1: 参数绑定**

`serde_json::Value` → `&dyn ToSql`。tiberius 的 `params: &[&dyn ToSql]` 需要**借用**，
所以先物化成一个持有所有值的枚举，再构造引用切片：

```rust
enum Bind { I64(i64), F64(f64), Str(String), Bool(bool), Null }

impl Bind {
    fn from_json(v: &serde_json::Value) -> Self { /* String/Number/Bool/Null → 对应变体 */ }
    fn as_tosql(&self) -> &dyn tiberius::ToSql {
        match self {
            Self::I64(n) => n,
            Self::F64(n) => n,
            Self::Str(s) => s,
            Self::Bool(b) => b,
            Self::Null => &Option::<String>::None,
        }
    }
}
```

> `Null` 用 `&Option::<String>::None` —— 需要确认 `Option<T>` 实现了 `ToSql`；
> 若不行，查 `IntoSql`/`ToSql` 的源码看如何表达 NULL。

- [ ] **Step 2: `MssqlClient`**

```rust
pub struct MssqlClient {
    pool: deadpool::managed::Pool<MssqlManager>,
    query_timeout: Option<Duration>,
    min_connections: u32,
}

impl MssqlClient {
    pub async fn connect(url: &str) -> Result<Self, RdbmsError>;
    pub async fn from_config(cfg: MssqlConfig) -> Result<Self, RdbmsError>;
    pub fn from_pool(pool: Pool<MssqlManager>) -> Self;
    pub async fn warm_up(&self) -> Result<(), RdbmsError>;
    pub fn pool_status(&self) -> deadpool::Status;   // 供 metrics（批次 4）
}
```

`warm_up`：deadpool **没有** sqlx 的 `min_connections` 概念，所以要自己取
`min_connections` 条连接再放回。这也是 spec 里说「MSSQL 侧 warm_up 更是必需」的原因。

`from_config`：构造 `MssqlManager` → `Pool::builder(manager).max_size(...).build()` → 池。

- [ ] **Step 3: `impl SqlExecutor for MssqlClient`**

每个方法：`run_with_timeout(kind, self.query_timeout, async { ... })` 包一层，
内部取池连接（`self.pool.get().await`）→ `client.query(sql, &params)` / `execute` →
映射成 `Row` 或行数。

**注意**：`query`/`execute` 需要 `&mut client`，而 deadpool 的 `PooledConnection`
实现了 `DerefMut` —— 用 `let mut conn = self.pool.get().await?; conn.query(...)`。

`dialect()` 返回 **`Dialect::Mssql`**。

`query_write` **不覆写**（用默认委托，与其它后端一致）。

- [ ] **Step 4: 提交**

```bash
git add ecat-data-mssql/src/lib.rs ecat-data-mssql/src/tests.rs
git commit -m "feat(ecat-data-mssql): SqlExecutor 实现 + 参数绑定 + warm_up"
```

---

## Task 6: env 门控真库测试

**Files:**
- Create: `ecat-data-mssql/src/live_tests.rs`
- Create: `docker-compose.dev.yml`（仓库根）

- [ ] **Step 1: `docker-compose.dev.yml`**

```yaml
# 本地联调用：三库一键起。不进 CI。
services:
  mssql:
    image: mcr.microsoft.com/mssql/server:2022-latest
    environment:
      ACCEPT_EULA: "Y"
      MSSQL_SA_PASSWORD: "Ecat_Test_2026!"
      MSSQL_PID: Developer
    ports: ["1433:1433"]
  postgres:
    image: postgres:16-alpine
    environment: { POSTGRES_PASSWORD: postgres }
    ports: ["5432:5432"]
  mysql:
    image: mysql:8.0
    environment: { MYSQL_ROOT_PASSWORD: root, MYSQL_DATABASE: bee_test }
    ports: ["3306:3306"]
```

> SQL Server 容器**约需 2GB 内存** —— 起之前先确认机器有余量。

- [ ] **Step 2: 真库测试**

沿用批次 1 已确立的模式（`ecat-data-sqlx/src/live_tests.rs` 是参照实现）：

```rust
fn live_db_url() -> Option<String> {
    let url = std::env::var("ECAT_TEST_MSSQL_URL").ok();
    if url.is_none() && std::env::var("ECAT_REQUIRE_LIVE_DB").is_ok() {
        panic!("ECAT_TEST_MSSQL_URL 未设，但 ECAT_REQUIRE_LIVE_DB 要求真库用例必须运行（跳过即失败）");
    }
    url
}
```

**要覆盖的断言**（用**会话级临时表** `#tmp`，不留持久痕迹）：

- 各整数类型（`TINYINT`/`SMALLINT`/`INT`/`BIGINT`）→ 数字
- `BIT` → 布尔
- `NVARCHAR` → 字符串
- **`DATE` → `"2026-10-05"`**（纯日期，验证与批次 1 的定案一致）
- **`DATETIME2` → RFC3339 UTC**
- `UNIQUEIDENTIFIER` → 字符串
- `VARBINARY` → base64
- **`DECIMAL` → 响亮报错**（带列名与类型名）
- NULL 在各类上都是 `null`
- 参数绑定：`@P1` / `@P2` 往返
- 事务：未提交即 drop 回滚、提交可见
- `warm_up` 建满 `min_connections`
- **`SET ARITHABORT ON` 真的生效**（`SELECT SESSIONPROPERTY('ARITHABORT')` → 1）

- [ ] **Step 3: 跑（**先确认机器内存**）**

```bash
docker compose -f docker-compose.dev.yml up -d mssql   # 等 30-60s 就绪
ECAT_TEST_MSSQL_URL='mssql://sa:Ecat_Test_2026!@localhost:1433' cargo test -p ecat-data-mssql
# 反向验证（必须失败，证明不是空转）：
ECAT_TEST_MSSQL_URL='mssql://sa:x@localhost:1' cargo test -p ecat-data-mssql   # 预期 FAIL
env -u ECAT_TEST_MSSQL_URL ECAT_REQUIRE_LIVE_DB=1 cargo test -p ecat-data-mssql # 预期 2 红且点名
```

- [ ] **Step 4: 提交**

```bash
git add docker-compose.dev.yml ecat-data-mssql/src/live_tests.rs
git commit -m "test(ecat-data-mssql): env 门控真库测试 + docker-compose.dev.yml"
```

---

## Task 7: 文档 ×14

**Files:**
- Modify: `README.md` / `README.en.md` / `docs/i18n/{12}/README.md`
- Modify: `config/databases.example.yaml`

- [ ] **Step 1: README 后端表补第 16 行**

```
| RDBMS | SQL Server | `ecat-data-mssql` | ✅ tiberius-ng |
```

**注意**（批次 1 的教训）：根 README 一次改完，**12 个 i18n 副本同步改**
（`docs-a` 的报告证明「只改根文件」是这类漂移的源头）。同时核对 **trait 抽象那句**
是否需要更新（`Dialect` 现在多了 `Mssql` 变体）。

- [ ] **Step 2: 配置示例补 `mssql:` 段**

- [ ] **Step 3: 提交**

```bash
git add README.md README.en.md docs/i18n/*/README.md config/databases.example.yaml
git commit -m "docs: README 后端表补 SQL Server（第 16 个）+ 配置示例"
```

---

## 批次完成检查

- [ ] `cargo test --workspace` 全绿，**测试数不下降**且 0 failed
- [ ] 改动文件 fmt 干净；`cargo doc -p ecat-data-mssql --no-deps` 0 warning
- [ ] clippy：除已知的 `double_must_use` 误报外无其它诊断
- [ ] `cargo audit --deny warnings` 通过（验证 tiberius-ng 路径无告警）
- [ ] 所有新文件 < 500 行
- [ ] 真库三向验证：有 URL 通过 / 死端口失败 / 无 URL 且 `ECAT_REQUIRE_LIVE_DB` 则 panic
- [ ] **12 个 i18n README 与根文件同步**（用 `grep -c 'ecat-data-mssql'` 逐份核，不要只看根文件）
