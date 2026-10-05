# 批次 1 — 地基 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `ecat-data` 的数据访问地基打好：拆出 `SqlExecutor`（事务内可执行 SQL）、引入 `Dialect`、加查询超时与泄漏计数，并把 `ecat-data-sqlx` 从 `AnyPool` 重写为原生池（PG / MySQL / SQLite 三路分派）。

**Architecture:** `ecat-data` 只定义 trait 与枚举，不含具体驱动；`ecat-data-sqlx` 用一个私有 `enum Pool` 承载三种原生 sqlx 池，`SqlExecutor` 的每个方法做一次 match 分派。方言由池的类型决定，不再解析 URL。

**Tech Stack:** Rust 2024 · async-trait · sqlx 0.8.6（`runtime-tokio` / `sqlite` / `postgres` / `mysql` / `time`）· tokio（`sync` + `time`）· `time` 0.3 · serde

**Spec:** [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../specs/2026-10-05-orm-and-mssql-design.md) §2、§4

**前置:** 分支 `feat/orm-mssql`（已存在，spec 已提交）

---

### Task 1: `Dialect` 枚举与 URL 推断

**Files:**
- Create: `ecat-data/src/dialect.rs`
- Modify: `ecat-data/src/lib.rs`（加 `mod dialect;` 与 `pub use`）
- Test: `ecat-data/src/dialect.rs`（内联 `#[cfg(test)]`，与仓库既有风格一致）

- [ ] **Step 1: 写失败测试**

创建 `ecat-data/src/dialect.rs`，先只写测试：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_url_recognizes_each_scheme() {
        assert_eq!(Dialect::from_url("postgres://localhost/db"), Dialect::Postgres);
        assert_eq!(Dialect::from_url("postgresql://localhost/db"), Dialect::Postgres);
        assert_eq!(Dialect::from_url("mysql://localhost/db"), Dialect::MySql);
        assert_eq!(Dialect::from_url("mariadb://localhost/db"), Dialect::MySql);
        assert_eq!(Dialect::from_url("sqlite::memory:"), Dialect::Sqlite);
        assert_eq!(Dialect::from_url("sqlite:app.db"), Dialect::Sqlite);
        assert_eq!(Dialect::from_url("mssql://host:1433/db"), Dialect::Mssql);
        assert_eq!(Dialect::from_url("sqlserver://host:1433/db"), Dialect::Mssql);
        assert_eq!(Dialect::from_url("postgres"), Dialect::Postgres);
    }

    /// RFC 3986 §3.1：scheme 大小写不敏感。sqlx 侧同样会小写化，
    /// 不归一化就会在能连通的情况下静默给出 Standard。
    #[test]
    fn from_url_is_case_insensitive() {
        assert_eq!(Dialect::from_url("POSTGRES://localhost/db"), Dialect::Postgres);
        assert_eq!(Dialect::from_url("MySQL://localhost/db"), Dialect::MySql);
        assert_eq!(Dialect::from_url("SQLite:app.db"), Dialect::Sqlite);
    }

    /// url crate 会 trim 首尾 U+0000–U+0020，故带空白的 URL 能连通 sqlx；
    /// 不 trim 就会静默返回 Standard。
    #[test]
    fn from_url_tolerates_surrounding_whitespace() {
        assert_eq!(Dialect::from_url(" postgres://host/db"), Dialect::Postgres);
        assert_eq!(Dialect::from_url("\tmysql://host/db"), Dialect::MySql);
        assert_eq!(Dialect::from_url("sqlite:app.db\n"), Dialect::Sqlite);
        // 谓词与 url crate 一致（U+0000–U+0020）：NUL/控制字符同样被 trim。
        assert_eq!(Dialect::from_url("\0postgres://host/db"), Dialect::Postgres);
        assert_eq!(Dialect::from_url("\u{1}mysql://host/db"), Dialect::MySql);
    }

    #[test]
    fn from_url_unknown_scheme_is_standard() {
        assert_eq!(Dialect::from_url("oracle://host/db"), Dialect::Standard);
        assert_eq!(Dialect::from_url(""), Dialect::Standard);
        assert_eq!(Dialect::from_url("nonsense"), Dialect::Standard);
    }

    /// sqlite 的 URL 没有 `://`，是最容易写错的一类，单独钉住。
    #[test]
    fn from_url_handles_sqlite_without_authority() {
        assert_eq!(Dialect::from_url("sqlite:ecat-test.db?mode=memory"), Dialect::Sqlite);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p ecat-data dialect`
Expected: 编译失败，`cannot find type Dialect in this scope`

- [ ] **Step 3: 实现**

在 `ecat-data/src/dialect.rs` 顶部（`#[cfg(test)]` 之前）插入：

```rust
/// 数据库方言标识。
///
/// 放在 `ecat-data` 而非 `ecat-orm`：`ecat-data-sqlx` 需要上报自己的方言，
/// 而它不能依赖 `ecat-orm`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// ANSI 近似（双引号标识符、`?` 占位符、`LIMIT`）。
    /// 第三方 `SqlExecutor` 实现未声明方言时的默认值。
    Standard,
    Sqlite,
    Postgres,
    MySql,
    Mssql,
}

impl Dialect {
    /// 从连接串推断方言，无法识别时返回 [`Dialect::Standard`]。
    ///
    /// scheme 的大小写，以及前导/尾随的 C0 控制字符与空格（U+0000–U+0020），
    /// 均被忽略（与 sqlx 底层 `url` crate 的判定一致）。
    /// 同时兼容有 `://` 的形式（`postgres://host/db`）与 sqlite 的无 authority
    /// 形式（`sqlite:app.db`）。
    pub fn from_url(url: &str) -> Self {
        // 谓词必须与 sqlx 底层 url crate 一致：url-2.5.8/src/parser.rs:1745-1747 的
        // `c0_control_or_space` 是 `ch <= ' '`（U+0000–U+0020，含 C0 控制字符）。
        // 不能用 str::trim()：它走 Unicode White_Space 属性，不含 NUL 等 C0 控制字符，
        // 会让 "\0postgres://host/db" 落回 Standard —— sqlx 侧却能连通，又是静默错答。
        let scheme = url
            .trim_matches(|c: char| c <= ' ')
            .split("://")
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match scheme.as_str() {
            "postgres" | "postgresql" => Self::Postgres,
            "mysql" | "mariadb" => Self::MySql,
            "sqlite" => Self::Sqlite,
            "mssql" | "sqlserver" => Self::Mssql,
            _ => Self::Standard,
        }
    }
}
```

> **归一化是必需的，三轮各暴露一条同类的静默错答**（代码质量审查 + 实现者实测）：
> ① scheme 大小写不敏感（RFC 3986 §3.1）；② 首尾空白被 url crate 的
> `new_trim_c0_control_and_space` trim 掉；③ 该 trim 的谓词是 `ch <= ' '`
> （含 NUL 等 C0 控制字符），**不等于** `str::trim()` 的 Unicode White_Space。
> 不处理则 `"POSTGRES://host/db"` / `" postgres://host/db"` / `"\0postgres://host/db"`
> 三种输入都会被 sqlx 正常连通，而 `from_url` 静默返回 `Standard` ——
> **静默错答而非报错**，下游一路劣化（会话初始化被跳过、错误信息误导、
> ORM 对 PostgreSQL 生成 ANSI SQL）。
>
> 注释里那句「不能用 str::trim()」是**故意留的**：后来者看到
> `trim_matches(|c| c <= ' ')` 的第一反应必然是「这不就是 `.trim()` 吗」，
> 抹掉就退回这个 bug。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p ecat-data dialect`
Expected: 3 个测试全部 PASS

- [ ] **Step 5: 导出**

`ecat-data/src/lib.rs` 加模块声明与再导出（放在既有 `mod` 列表里，保持字母序）：

```rust
mod dialect;
```
```rust
pub use dialect::Dialect;
```

- [ ] **Step 6: 提交**

```bash
git add ecat-data/src/dialect.rs ecat-data/src/lib.rs
git commit -m "feat(ecat-data): 新增 Dialect 枚举与 URL 方言推断"
```

---

### Task 2: 拆出 `SqlExecutor` supertrait

**Files:**
- Modify: `ecat-data/src/rdbms.rs`（trait 拆分 + 新增 `RdbmsError::Timeout`）
- Modify: `ecat-data/src/lib.rs`（导出 `SqlExecutor`）
- Modify: `ecat-data-sqlx/src/lib.rs`（最小适配，恢复编译）
- Modify: `ecat-data-clickhouse/src/lib.rs`（实现者适配）
- Modify: `ecat-data-clickhouse/src/tests.rs`（3 处 UFCS 调用改 `SqlExecutor::query`）
- Modify: `ecat-data-questdb/src/lib.rs`（实现者适配）
- Test: `ecat-data/src/rdbms.rs` 内联测试；其余 crate 既有测试须全绿

- [ ] **Step 1: 写失败测试**

在 `ecat-data/src/rdbms.rs` 的 `mod tests` 里，把既有的
`parameterized_ops_default_to_not_supported_error` 保留，并新增：

```rust
    /// 默认的 `query_write` 必须委托给 `query_with`：这样只有读写分离路由
    /// 需要覆写它，其余后端（含第三方实现）零改动即可支持写路径。
    struct CountingClient {
        query_with_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SqlExecutor for CountingClient {
        async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
            Ok(0)
        }
        async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
            Ok(vec![])
        }
        async fn query_with(
            &self,
            _sql: &str,
            _params: &[serde_json::Value],
        ) -> Result<Vec<Row>, RdbmsError> {
            self.query_with_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![])
        }
        fn dialect(&self) -> Dialect {
            Dialect::Standard
        }
    }

    #[tokio::test]
    async fn query_write_defaults_to_query_with() {
        let client = CountingClient {
            query_with_calls: Arc::new(AtomicUsize::new(0)),
        };
        client.query_write("SELECT 1", &[]).await.unwrap();
        assert_eq!(client.query_with_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn timeout_error_renders_message() {
        let err = RdbmsError::Timeout("query exceeded 30s".into());
        assert!(err.to_string().contains("timeout"));
        assert!(err.to_string().contains("30s"));
    }
```

同时，既有的 `RawOnlyClient` 与 `tracking` 测试桩需要跟着改（见 Step 3）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p ecat-data rdbms`
Expected: 编译失败，`no method named query_write` / `cannot find variant Timeout` / `cannot find trait SqlExecutor`

- [ ] **Step 3: 实现 trait 拆分**

把 `ecat-data/src/rdbms.rs` 里的 `RdbmsClient` trait 定义整段替换为：

```rust
#[async_trait]
pub trait SqlExecutor: Send + Sync {
    /// 执行一条 SQL 语句，返回受影响行数。
    /// 用户提供的值请走 [`SqlExecutor::execute_with`]。
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError>;
    /// 查询多行。用户提供的值请走 [`SqlExecutor::query_with`]。
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError>;
    /// 参数化执行，防注入。无法绑定参数的后端返回错误。
    async fn execute_with(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        Err(RdbmsError::Database(
            "parameterized execute not supported by this backend".into(),
        ))
    }
    /// 参数化查询，防注入。无法绑定参数的后端返回错误。
    async fn query_with(
        &self,
        _sql: &str,
        _params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        Err(RdbmsError::Database(
            "parameterized query not supported by this backend".into(),
        ))
    }
    /// 写路径且需要返回结果（`INSERT ... RETURNING` / `OUTPUT INSERTED`）。
    /// 默认委托给 [`SqlExecutor::query_with`]；只有读写分离路由需要覆写，
    /// 否则写语句会被路由到从库。
    async fn query_write(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.query_with(sql, params).await
    }
    /// 本执行器背后的数据库方言。
    fn dialect(&self) -> Dialect;
}

#[async_trait]
pub trait RdbmsClient: SqlExecutor {
    async fn transaction(&self) -> Result<Transaction, RdbmsError>;
}
```

在 `RdbmsError` 里加一个变体（放在 `Config` 之后）：

```rust
    #[error("timeout: {0}")]
    Timeout(String),
```

在文件顶部加 `use crate::dialect::Dialect;`。

`RawOnlyClient` 测试桩改为 `impl SqlExecutor for RawOnlyClient`，
并去掉其 `transaction` 方法（该测试桩不再需要 `RdbmsClient`）：

```rust
    #[async_trait]
    impl SqlExecutor for RawOnlyClient {
        async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
            Ok(0)
        }
        async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
            Ok(vec![])
        }
        fn dialect(&self) -> Dialect {
            Dialect::Standard
        }
    }
```

`ecat-data/src/lib.rs` 的再导出行改为：

```rust
pub use rdbms::{RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction, TransactionInner};
```

- [ ] **Step 4: 最小适配 `ecat-data-sqlx`（恢复 workspace 编译）**

`ecat-data-sqlx/src/lib.rs`：

1. 顶部导入加 `SqlExecutor`（**注意 `RdbmsClient` 也要保留** —— `impl RdbmsClient for
   SqlxClient` 还需要它；初版计划这行漏了它，按原样编译不过）：
   ```rust
   use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, TransactionInner};
   ```
2. `SqlxClient` 结构体加字段：
   ```rust
   pub struct SqlxClient {
       pool: AnyPool,
       dialect: Dialect,
   }
   ```
3. `connect` 填方言：
   ```rust
       pub async fn connect(url: &str) -> Result<Self, sqlx::Error> {
           ensure_drivers();
           let pool = AnyPool::connect(url).await?;
           Ok(Self {
               pool,
               dialect: Dialect::from_url(url),
           })
       }
   ```
4. `from_pool` 改为接收方言（签名变更在 spec §4 已声明，此处一次性改到位）：
   ```rust
       /// 用已有连接池构造客户端。
       ///
       /// `dialect` 无法从 `AnyPool` 反推，必须显式传入。
       pub fn from_pool(pool: AnyPool, dialect: Dialect) -> Self {
           Self { pool, dialect }
       }
   ```

   > 初版计划此处多一句「批次 1 Task 6 换成原生池后，本参数会由池的类型本身承载」。
   > 代码审查判定**删掉更好**（Task 6 落地后那句即成陈词），实际代码按删掉版落地，
   > 计划已对齐。另记一条**已知接受风险**：`AnyPool` 反推不出真实驱动，
   > 池与声明的 `dialect` 不一致时会静默生成错误 SQL —— Task 6 换原生池后
   > 由类型本身承载，此风险自动消失。
5. `impl RdbmsClient for SqlxClient` 拆成两个 impl ——
   把 `execute` / `query` / `execute_with` / `query_with` 四个方法搬进
   `impl SqlExecutor for SqlxClient`，并加 `dialect()`；`transaction` 留在
   `impl RdbmsClient for SqlxClient`：

   ```rust
   #[async_trait]
   impl SqlExecutor for SqlxClient {
       // execute / query / execute_with / query_with 四个方法体原样搬运，不改逻辑

       fn dialect(&self) -> Dialect {
           self.dialect
       }
   }

   #[async_trait]
   impl RdbmsClient for SqlxClient {
       async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
           // 原方法体不变
       }
   }
   ```
6. 测试里的 `_check_sig` 与 `single_conn_client` 跟着改：
   ```rust
       #[test]
       fn from_pool_is_constructible() {
           fn _check_sig(pool: sqlx::AnyPool) -> SqlxClient {
               SqlxClient::from_pool(pool, Dialect::Sqlite)
           }
       }
   ```
   ```rust
           SqlxClient::from_pool(pool, Dialect::Sqlite)
   ```

- [ ] **Step 4b: 适配另外两个 `RdbmsClient` 实现者**

> ⚠️ 初版计划声称「仓库内实现者只有 `SqlxClient` 与测试桩」，**这是错的**（照 2026-08-01
> 审计报告抄的，未 grep `impl`）。实际共 4 个实现者 + 3 处 UFCS 调用，由实现者在本批次发现：
>
> ```
> ecat-data/src/rdbms.rs:268          impl RdbmsClient for RawOnlyClient   (测试桩)
> ecat-data-clickhouse/src/lib.rs:228 impl RdbmsClient for ClickhouseClient
> ecat-data-sqlx/src/lib.rs:142       impl RdbmsClient for SqlxClient
> ecat-data-questdb/src/lib.rs:71     impl RdbmsClient for QuestdbClient
> ecat-data-clickhouse/src/tests.rs:246/267/278  RdbmsClient::query UFCS
> ```

对 `ClickhouseClient` 与 `QuestdbClient` 做与 `SqlxClient` **完全相同的机械适配**：
四个执行方法搬进 `impl SqlExecutor`，`transaction` 留在 `impl RdbmsClient`，
`dialect()` 返回 `Dialect::Standard`，不覆写 `query_write`（用默认委托）。

**为什么是 `Standard` 而不是 `Postgres`**（QuestDB 的 SQL 是 Postgres 味，容易顺手写错）：
两者都走 HTTP 接口、**不支持参数绑定**（`_with` 方法落到默认的「not supported」错误）。
声明 `Postgres` 会让 ORM 生成 `$1` 占位符发给一个无法绑定参数的传输层 —— 承诺不存在的
能力。`Standard` 生成 `?`，错误仍来自客户端真实的能力缺失，语义准确。

`ecat-data-clickhouse/src/tests.rs` 的三处 UFCS 同步改：

```rust
    let rows = ecat_data::SqlExecutor::query(&client, "SELECT * FROM t")
```

两个 crate 必须**一起进本次提交**，否则该提交自身编译不过。

- [ ] **Step 5: 跑全 workspace 测试**

Run: `cargo test --workspace`
Expected: 全绿；**测试数不下降，且 0 failed**

> 初版计划写「既有 675 个测试一个不少」—— 675 出自 `CHANGELOG.md` 的 3.0.2 条目
> （2026-08-27），**早已过期**，不是任何一次实测。改为「不下降」这个可验证的判据：
> 本任务改动前实测 **681**（Task 2 后为 683，本任务恰好 +2），只增不减即通过。

- [ ] **Step 6: 提交**

```bash
git add ecat-data/src/rdbms.rs ecat-data/src/lib.rs \
        ecat-data-sqlx/src/lib.rs \
        ecat-data-clickhouse/src/lib.rs ecat-data-clickhouse/src/tests.rs \
        ecat-data-questdb/src/lib.rs
git commit -m "refactor(ecat-data)!: 拆出 SqlExecutor supertrait，新增 query_write 与 dialect"
```

---

### Task 3: 事务内可执行 SQL

**Files:**
- Modify: `ecat-data/src/rdbms.rs`
- Modify: `ecat-data/Cargo.toml`（加 tokio `sync` + `time`）
- Modify: `ecat-data-sqlx/src/lib.rs`（`SqlxTransactionWrapper` 补方法）
- Test: `ecat-data/src/rdbms.rs` 内联测试

- [ ] **Step 1: 加依赖**

`ecat-data/Cargo.toml` 的 `[dependencies]` 末尾加：

```toml
tokio = { workspace = true, features = ["sync", "time"] }
```

- [ ] **Step 2: 写失败测试**

在 `ecat-data/src/rdbms.rs` 的 `mod tests` 里，`TrackingInner` 补上新方法并新增测试：

```rust
    #[async_trait]
    impl TransactionInner for TrackingInner {
        async fn execute(&mut self, _sql: &str) -> Result<u64, RdbmsError> {
            self.track.executes.fetch_add(1, Ordering::SeqCst);
            Ok(7)
        }
        async fn query(&mut self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
            Ok(vec![Row::new(vec!["n".into()], vec![serde_json::json!(1)])])
        }
        async fn execute_with(
            &mut self,
            _sql: &str,
            _p: &[serde_json::Value],
        ) -> Result<u64, RdbmsError> {
            Ok(0)
        }
        async fn query_with(
            &mut self,
            _sql: &str,
            _p: &[serde_json::Value],
        ) -> Result<Vec<Row>, RdbmsError> {
            Ok(vec![])
        }
        fn dialect(&self) -> Dialect {
            Dialect::Sqlite
        }
        async fn commit(&mut self) -> Result<(), RdbmsError> {
            self.track.commits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn rollback(&mut self) -> Result<(), RdbmsError> {
            self.track.rollbacks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
```

`Tracked` 结构体加一个计数字段：

```rust
    #[derive(Clone, Default)]
    struct Tracked {
        commits: Arc<AtomicUsize>,
        rollbacks: Arc<AtomicUsize>,
        executes: Arc<AtomicUsize>,
    }
```

新增测试：

```rust
    #[tokio::test]
    async fn transaction_executes_within_scope() {
        let track = Tracked::default();
        let tx = Transaction::with_inner(Box::new(TrackingInner {
            track: track.clone(),
        }));
        assert_eq!(tx.execute("UPDATE t SET x = 1").await.unwrap(), 7);
        assert_eq!(track.executes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn transaction_reports_inner_dialect() {
        let tx = Transaction::with_inner(Box::new(TrackingInner {
            track: Tracked::default(),
        }));
        assert_eq!(tx.dialect(), Dialect::Sqlite);
    }

    /// 空事务执行 SQL 必须报错，而不是静默返回 0 行影响 ——
    /// 后者会让写操作无声丢失（代码审查发现）。
    #[tokio::test]
    async fn empty_transaction_rejects_execution() {
        let tx = Transaction::new();
        assert!(tx.execute("SELECT 1").await.is_err());
        assert!(tx.query("SELECT 1").await.is_err());
        // 没有东西要提交，不应视为错误
        tx.commit().await.unwrap();
    }

    #[test]
    fn dropped_uncommitted_transaction_still_warns() {
        let warns = Arc::new(AtomicUsize::new(0));
        let tx = Transaction::with_inner(Box::new(TrackingInner {
            track: Tracked::default(),
        }));
        with_warn_counter(Arc::clone(&warns), || drop(tx));
        assert_eq!(warns.load(Ordering::SeqCst), 1);
    }
```

> 泄漏**计数**（`TRANSACTIONS_LEAKED`）在 Task 4 创建计数器后再补进
> `Drop` impl —— 本任务只保留既有告警行为。

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p ecat-data rdbms`
Expected: 编译失败，`no method named execute for Transaction` / `TransactionInner` 缺少方法

- [ ] **Step 4: 实现**

`TransactionInner` 换成完整签名（保留 `commit` / `rollback` 不变，前置四个执行方法 +
`dialect`）：

```rust
/// 事务内部实现。后端（sqlx / tiberius）实现它，`Transaction` 转发调用。
#[async_trait]
pub trait TransactionInner: Send {
    async fn execute(&mut self, sql: &str) -> Result<u64, RdbmsError>;
    async fn query(&mut self, sql: &str) -> Result<Vec<Row>, RdbmsError>;
    async fn execute_with(
        &mut self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError>;
    async fn query_with(
        &mut self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError>;
    fn dialect(&self) -> Dialect;
    async fn commit(&mut self) -> Result<(), RdbmsError>;
    async fn rollback(&mut self) -> Result<(), RdbmsError>;
}
```

`Transaction` 结构与实现替换为：

```rust
/// 无 backing 连接的事务（[`Transaction::new`]）执行 SQL 时的错误文案。
/// 这类事务只能作为空占位，执行任何语句都是编程错误 —— 必须报错而非
/// 静默返回 0 行影响，否则写操作会无声丢失。
const NO_BACKING: &str = "transaction has no backing connection (created via Transaction::new)";

pub struct Transaction {
    committed: bool,
    rolled_back: bool,
    /// 在 `with_inner` 时从 inner 拷贝，避免 `dialect(&self)` 这个同步方法
    /// 需要等待异步锁。
    dialect: Dialect,
    inner: tokio::sync::Mutex<Option<Box<dyn TransactionInner>>>,
}

impl Transaction {
    /// 创建一个无 backing 连接的空事务。
    ///
    /// 只能作为占位符用于「不执行任何语句」的场景；在其中执行 SQL 会返回错误。
    /// 需要真正执行语句时用 [`Transaction::with_inner`] 或后端的 `transaction()`。
    pub fn new() -> Self {
        Self {
            committed: false,
            rolled_back: false,
            dialect: Dialect::Standard,
            inner: tokio::sync::Mutex::new(None),
        }
    }

    pub fn with_inner(inner: Box<dyn TransactionInner>) -> Self {
        let dialect = inner.dialect();
        Self {
            committed: false,
            rolled_back: false,
            dialect,
            inner: tokio::sync::Mutex::new(Some(inner)),
        }
    }

    pub async fn commit(mut self) -> Result<(), RdbmsError> {
        if let Some(inner) = self.inner.get_mut().as_mut() {
            inner.commit().await?;
        }
        self.committed = true;
        Ok(())
    }

    pub async fn rollback(mut self) -> Result<(), RdbmsError> {
        if let Some(inner) = self.inner.get_mut().as_mut() {
            inner.rollback().await?;
        }
        self.committed = false;
        self.rolled_back = true;
        Ok(())
    }
}

impl Default for Transaction {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SqlExecutor for Transaction {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.execute(sql).await,
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.query(sql).await,
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    async fn execute_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.execute_with(sql, params).await,
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    async fn query_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.query_with(sql, params).await,
            None => Err(RdbmsError::Database(NO_BACKING.into())),
        }
    }

    fn dialect(&self) -> Dialect {
        self.dialect
    }
}
```

`Drop` impl 保持既有告警行为不变：

```rust
impl Drop for Transaction {
    fn drop(&mut self) {
        // 这里只记日志：Drop 里无法执行异步回滚，实际回滚依赖
        // 底层 sqlx / tiberius 事务在未提交时 Drop 自动回滚。
        if !self.committed && !self.rolled_back {
            tracing::warn!("transaction dropped without commit — rolling back");
        }
    }
}
```

> 泄漏**计数**在 Task 4 创建 `TRANSACTIONS_LEAKED` 后补入此 Drop。

`ecat-data-sqlx/src/lib.rs` 的 `SqlxTransactionWrapper` 补齐方法：

```rust
    async fn execute(&mut self, sql: &str) -> Result<u64, RdbmsError> {
        let tx = self
            .inner
            .as_mut()
            .ok_or_else(|| RdbmsError::Database("transaction already finished".into()))?;
        tx.execute(sql)
            .await
            .map(|r| r.rows_affected())
            .map_err(|e| RdbmsError::Database(e.to_string()))
    }

    async fn query(&mut self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let tx = self
            .inner
            .as_mut()
            .ok_or_else(|| RdbmsError::Database("transaction already finished".into()))?;
        let rows: Vec<AnyRow> = tx
            .fetch_all(sql)
            .await
            .map_err(|e| RdbmsError::Database(e.to_string()))?;
        Ok(rows_to_result(rows))
    }

    async fn execute_with(
        &mut self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        let tx = self
            .inner
            .as_mut()
            .ok_or_else(|| RdbmsError::Database("transaction already finished".into()))?;
        let mut q = sqlx::query(sql);
        for p in params {
            q = bind_json(q, p);
        }
        q.execute(&mut **tx)
            .await
            .map(|r| r.rows_affected())
            .map_err(|e| RdbmsError::Database(e.to_string()))
    }

    async fn query_with(
        &mut self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        let tx = self
            .inner
            .as_mut()
            .ok_or_else(|| RdbmsError::Database("transaction already finished".into()))?;
        let mut q = sqlx::query(sql);
        for p in params {
            q = bind_json(q, p);
        }
        let rows: Vec<AnyRow> = q
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| RdbmsError::Database(e.to_string()))?;
        Ok(rows_to_result(rows))
    }

    fn dialect(&self) -> Dialect {
        self.dialect
    }
```

> **参数绑定不抽公共函数**：`sqlx::query::Query` 的类型参数带生命周期，
> 抽成助手需要写一长串 `where` 约束，得不偿失。直接在各方法内联原来的
> match 块（`ecat-data-sqlx/src/lib.rs:164-181`）即可。
>
> `SqlxTransactionWrapper` 新增字段 `dialect: Dialect`，
> 在 `transaction()` 里用 `self.dialect` 填。

> **实施记录（2026-10-05）——两处计划缺陷 + 一处补强，实际落地已按此**：
>
> 1. 上面片段需要 **`use sqlx::Executor as _;`** 才能编译（`tx.execute` / `tx.fetch_all`
>    依赖该 trait 在作用域内）。初版计划漏了这行。
> 2. 实施时**额外补了一个 e2e 测试** `transaction_executes_and_scopes_changes`
>    （`ecat-data-sqlx/src/lib.rs`），验证「回滚不可见 / 提交可见 / 事务内参数化绑定正确」。
>    初版计划只测了 `ecat-data` 侧的桩，**真实 sqlx wrapper 零运行时覆盖** ——
>    而「事务里能执行 SQL」正是本任务的全部意义。该补强已采纳。
> 3. `drop_without_commit_warns_once` 按 spec 的「改写」意图重命名为
>    `dropped_uncommitted_transaction_still_warns`，断言等价（均为 `warns == 1`）。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p ecat-data && cargo test -p ecat-data-sqlx`
Expected: 全绿

- [ ] **Step 6: 提交**

```bash
git add ecat-data/src/rdbms.rs ecat-data/Cargo.toml ecat-data-sqlx/src/lib.rs
git commit -m "feat(ecat-data)!: 事务内可执行 SQL（TransactionInner 扩展 + tokio 异步锁）"
```

---

### Task 4: 查询超时助手与计数器

**Files:**
- Create: `ecat-data/src/timeout.rs`
- Modify: `ecat-data/src/lib.rs`（`mod timeout;` + `pub use`）
- Test: `ecat-data/src/timeout.rs` 内联测试

- [ ] **Step 1: 写失败测试**

创建 `ecat-data/src/timeout.rs`：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdbms::RdbmsError;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn none_timeout_passes_result_through() {
        let r: Result<u64, RdbmsError> = run_with_timeout(None, async { Ok(42) }).await;
        assert_eq!(r.unwrap(), 42);
    }

    #[tokio::test]
    async fn fast_future_completes_within_timeout() {
        let r: Result<u64, RdbmsError> =
            run_with_timeout(Some(Duration::from_secs(5)), async { Ok(1) }).await;
        assert_eq!(r.unwrap(), 1);
    }

    #[tokio::test]
    async fn slow_future_times_out_and_counts() {
        let before = QUERY_TIMEOUTS.load(Ordering::SeqCst);
        let r: Result<(), RdbmsError> = run_with_timeout(Some(Duration::from_millis(10)), async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(())
        })
        .await;
        let err = r.unwrap_err();
        assert!(matches!(err, RdbmsError::Timeout(_)), "got: {err:?}");
        assert_eq!(QUERY_TIMEOUTS.load(Ordering::SeqCst), before + 1);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p ecat-data timeout`
Expected: 编译失败，`cannot find function run_with_timeout`

- [ ] **Step 3: 实现**

在 `ecat-data/src/timeout.rs` 顶部（`#[cfg(test)]` 之前）插入：

```rust
use crate::rdbms::RdbmsError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 查询超时累计次数。`metrics` feature 开启时由后端注册为
/// `ecat_rdbms_query_timeout_total`。
///
/// 用进程级 `AtomicU64` 而非依赖 `ecat-metrics`：本 crate 保持零外部依赖，
/// 指标的读取方按需接入。
pub static QUERY_TIMEOUTS: AtomicU64 = AtomicU64::new(0);

/// 未提交即 Drop 的事务累计数（`Transaction` 的 Drop guard 递增）。
pub static TRANSACTIONS_LEAKED: AtomicU64 = AtomicU64::new(0);

/// 给数据库调用套一层超时。
///
/// `None` 表示禁用超时，直接透传结果。超时发生时递增 [`QUERY_TIMEOUTS`]
/// 并返回 [`RdbmsError::Timeout`]。
///
/// 这是池耗尽的头号防线：`acquire_timeout` 只约束"等连接"，
/// 拿到连接后卡死的查询会一直占着它。
pub async fn run_with_timeout<F, T>(timeout: Option<Duration>, fut: F) -> Result<T, RdbmsError>
where
    F: std::future::Future<Output = Result<T, RdbmsError>>,
{
    match timeout {
        None => fut.await,
        Some(d) => match tokio::time::timeout(d, fut).await {
            Ok(result) => result,
            Err(_) => {
                QUERY_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
                Err(RdbmsError::Timeout(format!("query exceeded {d:?}")))
            }
        },
    }
}
```

- [ ] **Step 4: 导出**

`ecat-data/src/lib.rs`：

```rust
mod timeout;
```
```rust
pub use timeout::{QUERY_TIMEOUTS, TRANSACTIONS_LEAKED, run_with_timeout};
```

- [ ] **Step 5: 把泄漏计数接进 `Transaction::Drop`**

在 `ecat-data/src/rdbms.rs` 的 `Drop for Transaction` 里追加一行，
把原先只写日志的告警变成可告警指标：

```rust
impl Drop for Transaction {
    fn drop(&mut self) {
        // 这里只记日志与计数：Drop 里无法执行异步回滚，实际回滚依赖
        // 底层 sqlx / tiberius 事务在未提交时 Drop 自动回滚。
        if !self.committed && !self.rolled_back {
            crate::timeout::TRANSACTIONS_LEAKED
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!("transaction dropped without commit — rolling back");
        }
    }
}
```

在 `mod tests` 里加计数断言：

```rust
    #[test]
    fn dropped_uncommitted_transaction_counts_as_leak() {
        let before = crate::timeout::TRANSACTIONS_LEAKED.load(Ordering::SeqCst);
        drop(Transaction::with_inner(Box::new(TrackingInner {
            track: Tracked::default(),
        })));
        assert_eq!(
            crate::timeout::TRANSACTIONS_LEAKED.load(Ordering::SeqCst),
            before + 1
        );
    }
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p ecat-data`
Expected: 全绿，含 3 个 timeout 测试与新增的泄漏计数测试

- [ ] **Step 7: 提交**

```bash
git add ecat-data/src/timeout.rs ecat-data/src/lib.rs ecat-data/src/rdbms.rs ecat-data/Cargo.toml
git commit -m "feat(ecat-data): 查询超时助手与超时/事务泄漏计数器"
```

---

### Task 5: `SqlxConfig` 独立文件与池参数

**Files:**
- Create: `ecat-data-sqlx/src/config.rs`
- Modify: `ecat-data-sqlx/src/lib.rs`（移除旧 `SqlxConfig`，改为引用）
- Test: `ecat-data-sqlx/src/config.rs` 内联测试

- [ ] **Step 1: 写失败测试**

创建 `ecat-data-sqlx/src/config.rs`，先写测试：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_uses_documented_defaults() {
        let cfg: SqlxConfig = serde_json::from_str(r#"{"url": "postgres://localhost/db"}"#).unwrap();
        let p = cfg.pool();
        assert_eq!(p.max_connections, 10);
        assert_eq!(p.min_connections, 0);
        assert_eq!(p.acquire_timeout, Duration::from_secs(30));
        assert_eq!(p.idle_timeout, Duration::from_secs(600));
        assert_eq!(p.max_lifetime, Duration::from_secs(1800));
        assert_eq!(p.query_timeout, Some(Duration::from_secs(30)));
        assert!(!p.test_before_acquire);
        assert!(cfg.session_init.is_none());
    }

    #[test]
    fn pool_params_can_be_overridden() {
        let cfg: SqlxConfig = serde_json::from_str(
            r#"{"url": "mysql://localhost/db",
                "max_connections": 32,
                "query_timeout_secs": 0,
                "test_before_acquire": true,
                "session_init": ["SET time_zone = '+00:00'"]}"#,
        )
        .unwrap();
        assert_eq!(cfg.pool().max_connections, 32);
        assert_eq!(cfg.pool().query_timeout, None);
        assert!(cfg.pool().test_before_acquire);
        assert_eq!(
            cfg.effective_session_init(),
            vec!["SET time_zone = '+00:00'".to_string()]
        );
    }

    /// 未显式配置时按方言给默认会话设置：库侧直接返回 UTC，
    /// 与 ORM 的「时间统一 UTC」约定对齐（spec §2.7）。
    #[test]
    fn session_init_defaults_per_dialect() {
        let pg: SqlxConfig = serde_json::from_str(r#"{"url": "postgres://h/db"}"#).unwrap();
        assert!(
            pg.effective_session_init()
                .iter()
                .any(|s| s.contains("TIME ZONE"))
        );

        let sqlite: SqlxConfig =
            serde_json::from_str(r#"{"url": "sqlite::memory:"}"#).unwrap();
        assert!(sqlite.effective_session_init().is_empty());
    }

    /// `query_timeout_secs: 0` 是「禁用」而非「0 秒立刻超时」。
    #[test]
    fn zero_query_timeout_means_disabled() {
        let cfg: SqlxConfig =
            serde_json::from_str(r#"{"url": "sqlite::memory:", "query_timeout_secs": 0}"#).unwrap();
        assert_eq!(cfg.query_timeout(), None);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p ecat-data-sqlx config`
Expected: 编译失败，`cannot find type SqlxConfig` / `no field pool`

- [ ] **Step 3: 实现**

在 `config.rs` 顶部插入：

```rust
use ecat_data::Dialect;
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct SqlxConfig {
    pub url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// SQLx 的 TLS 通过 URL 参数配置（如 `?sslmode=require`）。
    /// 本字段预留给未来的程序化 TLS 支持。
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    #[serde(default)]
    pub session_init: Option<Vec<String>>,

    #[serde(default)]
    pub max_connections: Option<u32>,
    #[serde(default)]
    pub min_connections: Option<u32>,
    #[serde(default)]
    pub acquire_timeout_secs: Option<u64>,
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_lifetime_secs: Option<u64>,
    /// `0` = 禁用查询超时。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 每次从池取连接是否先 ping。默认 `false`：sqlx 默认在**每次** acquire
    /// 都 ping（不论上次 ping 多近，见 sqlx issue #1743），在高频路径上是
    /// 一笔无谓往返。关掉后由 `max_lifetime` + 查询超时 + 首次使用报错兜底。
    #[serde(default)]
    pub test_before_acquire: Option<bool>,
}
```

> 上面把手写字段与展平的池参数放在同一个结构体里（`SqlxConfig` 直接收
> `max_connections` 等顶层键），与既有配置文件风格一致。测试里读的是
> `cfg.pool.max_connections`，因此需要一个访问器结构：

```rust
/// 池参数的解析结果（带默认值）。由 [`SqlxConfig::pool`] 产出。
#[derive(Debug, Clone)]
pub struct PoolParams {
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_lifetime: Duration,
    pub query_timeout: Option<Duration>,
    pub test_before_acquire: bool,
    /// 每条新连接建立后执行的会话初始化语句（spec §2.7）。
    pub session_init: Vec<String>,
}

impl Default for PoolParams {
    fn default() -> Self {
        Self {
            max_connections: 10,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(600),
            max_lifetime: Duration::from_secs(1800),
            query_timeout: Some(Duration::from_secs(30)),
            test_before_acquire: false,
            session_init: Vec::new(),
        }
    }
}

impl SqlxConfig {
    /// 未配置的字段一律取 [`PoolParams::default`]，**字面量只保留一份**。
    ///
    /// 初版这里逐个 `unwrap_or(<字面量>)`，与 `PoolParams::default()` 形成两处定义 ——
    /// 改一处默认值会让 `for_url`（`connect` 走）与 `pool()`（`from_config` 走）
    /// 静默分叉，且现有测试抓不到。实施时由实现者发现并订正。
    pub fn pool(&self) -> PoolParams {
        let d = PoolParams::default();
        PoolParams {
            max_connections: self.max_connections.unwrap_or(d.max_connections),
            min_connections: self.min_connections.unwrap_or(d.min_connections),
            acquire_timeout: self
                .acquire_timeout_secs
                .map_or(d.acquire_timeout, Duration::from_secs),
            idle_timeout: self
                .idle_timeout_secs
                .map_or(d.idle_timeout, Duration::from_secs),
            max_lifetime: self
                .max_lifetime_secs
                .map_or(d.max_lifetime, Duration::from_secs),
            query_timeout: self.query_timeout(),
            test_before_acquire: self.test_before_acquire.unwrap_or(d.test_before_acquire),
            session_init: self.effective_session_init(),
        }
    }

    /// `0` 或未配置以外的值转为 `Some`；`0` 表示显式禁用。
    pub fn query_timeout(&self) -> Option<Duration> {
        match self.query_timeout_secs {
            None => Some(Duration::from_secs(30)),
            Some(0) => None,
            Some(s) => Some(Duration::from_secs(s)),
        }
    }

    /// 未显式配置时按方言给默认会话初始化语句。
    /// SQLite 无会话概念，返回空。
    pub fn effective_session_init(&self) -> Vec<String> {
        if let Some(custom) = &self.session_init {
            return custom.clone();
        }
        match Dialect::from_url(&self.url) {
            Dialect::Postgres => vec![
                "SET TIME ZONE 'UTC'".to_string(),
                "SET application_name = 'ecat'".to_string(),
            ],
            Dialect::MySql => vec!["SET time_zone = '+00:00'".to_string()],
            _ => Vec::new(),
        }
    }
}
```

> 测试里的 `cfg.pool.max_connections` 要改成 `cfg.pool().max_connections`。

- [ ] **Step 4: 更新 `lib.rs`**

删除 `ecat-data-sqlx/src/lib.rs` 里原来的 `SqlxConfig` 定义（第 10–21 行），改为：

```rust
mod config;
pub use config::{PoolParams, SqlxConfig};
```

`TlsClientConfig` 的 `use` 从 `lib.rs` 移到 `config.rs`。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p ecat-data-sqlx`
Expected: 新增 4 个 config 测试 + 既有测试全绿

- [ ] **Step 6: 提交**

```bash
git add ecat-data-sqlx/src/config.rs ecat-data-sqlx/src/lib.rs
git commit -m "feat(ecat-data-sqlx): SqlxConfig 独立成模块并补全池参数"
```

---

### Task 6: 原生池 —— `Pool` 枚举与连接分派

**Files:**
- Create: `ecat-data-sqlx/src/pool.rs`
- Modify: `ecat-data-sqlx/Cargo.toml`（sqlx 加 `time` feature，去掉 `any` 相关）
- Modify: `ecat-data-sqlx/src/lib.rs`
- Test: `ecat-data-sqlx/src/pool.rs` 内联测试

- [ ] **Step 1: 改依赖**

`ecat-data-sqlx/Cargo.toml`：

```toml
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite", "postgres", "mysql", "time"] }
time = { workspace = true }
```

`ecat-data-sqlx` 的 dev-dependencies 保持 `tokio = { workspace = true, features = ["macros", "rt"] }`，
若 Task 8 的测试用到 `sleep` 再补 `"time"`。

- [ ] **Step 2: 写失败测试**

创建 `ecat-data-sqlx/src/pool.rs`：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

#[cfg(test)]
mod tests {
    use super::*;
    use ecat_data::Dialect;

    #[tokio::test]
    async fn sqlite_url_builds_sqlite_pool() {
        let pool = Pool::connect("sqlite::memory:", &PoolParams::default())
            .await
            .unwrap();
        assert_eq!(pool.dialect(), Dialect::Sqlite);
        assert!(matches!(pool, Pool::Sq(_)));
    }

    /// 无法识别的 scheme 必须被拒，且**错误信息要点名出问题的 scheme** ——
    /// `Dialect::Standard` 是回退值，把它打进错误等于丢掉诊断线索。
    #[test]
    fn unsupported_scheme_is_rejected_and_names_the_scheme() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(Pool::connect("oracle://h/db", &PoolParams::default()))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unsupported"), "got: {msg}");
        assert!(msg.contains("oracle"), "错误信息必须点名 scheme，got: {msg}");
    }

    /// Mssql 归 ecat-data-mssql（tiberius），不应被 sqlx 后端静默接受。
    #[test]
    fn mssql_scheme_is_rejected_by_sqlx_backend() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(Pool::connect("mssql://h:1433/db", &PoolParams::default()))
            .unwrap_err();
        assert!(
            err.to_string().contains("ecat-data-mssql"),
            "got: {err}"
        );
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p ecat-data-sqlx pool`
Expected: 编译失败，`cannot find type Pool`

- [ ] **Step 4: 实现**

在 `pool.rs` 顶部插入：

```rust
use crate::config::PoolParams;
use ecat_data::Dialect;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::time::Duration;

/// 三种原生池。方言由变体本身承载 —— 不再解析 URL 猜测。
pub enum Pool {
    Pg(PgPool),
    My(MySqlPool),
    Sq(SqlitePool),
}

impl Pool {
    /// 按 URL scheme 建立对应的原生池。
    pub async fn connect(url: &str, params: &PoolParams) -> Result<Self, sqlx::Error> {
        match Dialect::from_url(url) {
            Dialect::Postgres => Ok(Self::Pg(
                common_pg(PgPoolOptions::new(), params).connect(url).await?,
            )),
            Dialect::MySql => Ok(Self::My(
                common_mysql(MySqlPoolOptions::new(), params)
                    .connect(url)
                    .await?,
            )),
            Dialect::Sqlite => Ok(Self::Sq(
                common_sqlite(SqlitePoolOptions::new(), params)
                    .connect(url)
                    .await?,
            )),
            // 报 scheme 原文而非 Dialect：`Dialect::Standard` 是「无法识别」的
            // 回退值，把它打进错误信息等于丢掉了唯一的诊断线索。
            Dialect::Standard => Err(sqlx::Error::Configuration(Box::new(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "unsupported database scheme for sqlx backend: {}",
                        url.split("://")
                            .next()
                            .unwrap_or(url)
                            .split(':')
                            .next()
                            .unwrap_or(url)
                    ),
                ),
            ))),
            // Mssql 走 tiberius 后端（ecat-data-mssql），不是 sqlx
            Dialect::Mssql => Err(sqlx::Error::Configuration(Box::new(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "mssql:// is served by ecat-data-mssql, not the sqlx backend",
                ),
            ))),
        }
    }

    pub fn dialect(&self) -> Dialect {
        match self {
            Self::Pg(_) => Dialect::Postgres,
            Self::My(_) => Dialect::MySql,
            Self::Sq(_) => Dialect::Sqlite,
        }
    }

    /// 池内已建立的连接总数。
    pub fn size(&self) -> u32 {
        match self {
            Self::Pg(p) => p.size(),
            Self::My(p) => p.size(),
            Self::Sq(p) => p.size(),
        }
    }

    /// 池内空闲连接数。
    pub fn idle(&self) -> u32 {
        match self {
            Self::Pg(p) => p.num_idle() as u32,
            Self::My(p) => p.num_idle() as u32,
            Self::Sq(p) => p.num_idle() as u32,
        }
    }

    /// 从池中取一条连接（预热用）。归还由 guard 的 Drop 完成。
    pub async fn acquire(&self) -> Result<PoolGuard, sqlx::Error> {
        Ok(match self {
            Self::Pg(p) => PoolGuard::Pg(p.acquire().await?),
            Self::My(p) => PoolGuard::My(p.acquire().await?),
            Self::Sq(p) => PoolGuard::Sq(p.acquire().await?),
        })
    }
}

/// 三种驱动的连接 guard 的聚合类型。Drop 即归还连接。
/// `PoolConnection<DB>` 自身持有 `Arc`，无需生命周期参数。
pub enum PoolGuard {
    Pg(sqlx::pool::PoolConnection<sqlx::Postgres>),
    My(sqlx::pool::PoolConnection<sqlx::MySql>),
    Sq(sqlx::pool::PoolConnection<sqlx::Sqlite>),
}

fn common_pg(opts: PgPoolOptions, p: &PoolParams) -> PgPoolOptions {
    opts.max_connections(p.max_connections)
        .min_connections(p.min_connections)
        .acquire_timeout(p.acquire_timeout)
        .idle_timeout(Some(p.idle_timeout))
        .max_lifetime(Some(p.max_lifetime))
        .test_before_acquire(p.test_before_acquire)
}

fn common_mysql(opts: MySqlPoolOptions, p: &PoolParams) -> MySqlPoolOptions {
    opts.max_connections(p.max_connections)
        .min_connections(p.min_connections)
        .acquire_timeout(p.acquire_timeout)
        .idle_timeout(Some(p.idle_timeout))
        .max_lifetime(Some(p.max_lifetime))
        .test_before_acquire(p.test_before_acquire)
}

fn common_sqlite(opts: SqlitePoolOptions, p: &PoolParams) -> SqlitePoolOptions {
    opts.max_connections(p.max_connections)
        .min_connections(p.min_connections)
        .acquire_timeout(p.acquire_timeout)
        .idle_timeout(Some(p.idle_timeout))
        .max_lifetime(Some(p.max_lifetime))
        .test_before_acquire(p.test_before_acquire)
}
```

`PoolParams` 需要 `Default` —— 已在 Task 5 实现（`PoolParams` 是纯参数结构，
无必填字段，直接手写 `Default` 即可，不必给 `SqlxConfig` 加测试专用构造器）。

> `sqlx::Error::Configuration` 接收 `BoxDynError` 而非 `String`。
> 写法：
> ```rust
> sqlx::Error::Configuration(
>     Box::new(std::io::Error::new(
>         std::io::ErrorKind::InvalidInput,
>         format!("unsupported database scheme for sqlx backend: {other:?}"),
>     )),
> )
> ```

- [ ] **Step 5: 改 `SqlxClient` 用 `Pool`**

`ecat-data-sqlx/src/lib.rs`：

1. 删除 `ensure_drivers` / `DRIVERS_INSTALLED`（`AnyPool` 专用，原生池不需要；
   连带去掉「No drivers installed」panic 面）。
2. 结构体与构造器：

```rust
pub struct SqlxClient {
    pool: Pool,
    query_timeout: Option<Duration>,
    min_connections: u32,
}

impl SqlxClient {
    pub async fn connect(url: &str) -> Result<Self, sqlx::Error> {
        // 必须走 for_url 而非 default()：否则 connect() 拿不到方言默认的
        // session_init，与 from_config() 行为静默不一致（Task 5 审查发现）。
        Self::connect_with_params(url, &PoolParams::for_url(url)).await
    }

    pub async fn connect_with_params(url: &str, params: &PoolParams) -> Result<Self, sqlx::Error> {
        Ok(Self {
            pool: Pool::connect(url, params).await?,
            query_timeout: params.query_timeout,
            min_connections: params.min_connections,
        })
    }

    pub async fn connect_with_auth(
        url: &str,
        username: &str,
        password: &str,
    ) -> Result<Self, sqlx::Error> {
        Self::connect(&with_auth_in_url(url, username, password)).await
    }

    pub async fn from_config(cfg: SqlxConfig) -> Result<Self, sqlx::Error> {
        let params = cfg.pool();
        let url = match (&cfg.username, &cfg.password) {
            (Some(u), Some(p)) if !u.is_empty() || !p.is_empty() => {
                with_auth_in_url(&cfg.url, u, p)
            }
            _ => cfg.url.clone(),
        };
        Self::connect_with_params(&url, &params).await
    }

    pub fn from_pool(pool: Pool) -> Self {
        Self {
            pool,
            query_timeout: None,
        }
    }

    pub fn dialect(&self) -> Dialect {
        self.pool.dialect()
    }
}
```

3. `with_auth_in_url` 就是把原来的 `connect_with_auth` 里的 URL 拼接逻辑抽成纯函数
   （保留 `percent_encode`），便于单测：

```rust
fn with_auth_in_url(url: &str, username: &str, password: &str) -> String {
    if url.contains('@') {
        return url.to_string();
    }
    let encoded_user = percent_encode(username);
    let encoded_pass = percent_encode(password);
    url.replacen("://", &format!("://{encoded_user}:{encoded_pass}@"), 1)
}
```

4. `apply_session_init`：用 `after_connect` 在每个新连接上执行初始化语句。
   由于三种池的 `after_connect` 类型不同，且池已建成，这里改为**建池前**通过
   `PoolParams` 携带 `session_init`，在 `Pool::connect` 内部挂 `after_connect`。

   调整：`PoolParams` 增加字段 `pub session_init: Vec<String>`，`Pool::connect`
   里对三种 options 都调用 `.after_connect(move |conn, _meta| { ... })`，
   闭包内按序 `sqlx::query(stmt).execute(&mut *conn)`。sqlx 0.8.6 的签名是：

   ```rust
   for<'c> F: Fn(&'c mut DB::Connection, PoolConnectionMetadata) -> BoxFuture<'c, Result<(), Error>> + 'static + Send + Sync
   ```

   即闭包返回 `Box::pin(async move { ... })`。三种池的 `Connection` 类型不同，
   因此每个 `common_*` 函数各自挂一份闭包（代码重复三份，但类型不同无法合并）。

   > 任一语句失败 → 返回 `Err` → 该连接创建失败（不静默降级），符合 spec §2.7。

- [ ] **Step 6: 跑测试**

Run: `cargo test -p ecat-data-sqlx`
Expected: 全绿。此时 `lib.rs` 里所有 `AnyPool` 引用应已清除：

Run: `grep -n "AnyPool\|install_default_drivers\|AnyRow\|sqlx::Any" ecat-data-sqlx/src/`
Expected: 无输出

- [ ] **Step 7: 提交**

```bash
git add ecat-data-sqlx/src/pool.rs ecat-data-sqlx/src/config.rs ecat-data-sqlx/src/lib.rs ecat-data-sqlx/Cargo.toml Cargo.toml
git commit -m "refactor(ecat-data-sqlx)!: 弃用 AnyPool，改用 PG/MySQL/SQLite 原生池"
```

---

### Task 7: 行转换（含 `time` 类型）

**Files:**
- Create: `ecat-data-sqlx/src/cell.rs`
- Modify: `ecat-data-sqlx/src/lib.rs`（移除旧 `cell_to_json` / `rows_to_result`，改为引用）
- Test: `ecat-data-sqlx/src/cell.rs` 内联测试

- [ ] **Step 1: 写失败测试**

创建 `ecat-data-sqlx/src/cell.rs`：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

#[cfg(test)]
mod tests {
    use crate::SqlxClient;
    use time::macros::datetime;

    async fn mem() -> SqlxClient {
        SqlxClient::connect("sqlite::memory:").await.unwrap()
    }

    /// 核心回归：原生池必须能读时间列 —— 这正是弃用 AnyPool 的动因。
    #[tokio::test]
    async fn datetime_round_trips_as_rfc3339_utc() {
        let db = mem().await;
        db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, at TEXT NOT NULL)")
            .await
            .unwrap();
        let at = datetime!(2026-10-05 12:34:56 UTC);
        db.execute_with(
            "INSERT INTO t (id, at) VALUES (?, ?)",
            &[serde_json::json!(1), serde_json::json!(at.to_string())],
        )
        .await
        .unwrap();

        let rows = db.query("SELECT at FROM t").await.unwrap();
        let value = rows[0].get("at").unwrap().as_str().unwrap();
        assert!(value.starts_with("2026-10-05"), "got: {value}");
        assert_eq!(value, at.to_string());
    }

    #[tokio::test]
    async fn bytes_become_base64() {
        let db = mem().await;
        db.execute("CREATE TABLE b (id INTEGER PRIMARY KEY, raw BLOB)")
            .await
            .unwrap();
        db.execute("INSERT INTO b (id, raw) VALUES (1, X'0102FF')")
            .await
            .unwrap();
        let rows = db.query("SELECT raw FROM b").await.unwrap();
        assert_eq!(rows[0].get("raw").unwrap().as_str().unwrap(), "AQL/");
    }

    #[tokio::test]
    async fn null_survives_conversion() {
        let db = mem().await;
        db.execute("CREATE TABLE n (id INTEGER PRIMARY KEY, v TEXT)")
            .await
            .unwrap();
        db.execute("INSERT INTO n (id, v) VALUES (1, NULL)")
            .await
            .unwrap();
        let rows = db.query("SELECT v FROM n").await.unwrap();
        assert!(rows[0].get("v").unwrap().is_null());
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p ecat-data-sqlx cell`
Expected: 编译失败（`cell` 模块不存在）或断言失败

- [ ] **Step 3: 实现**

把 `lib.rs` 里的 `cell_to_json` 与 `rows_to_result` 移到 `cell.rs`，
`cell_to_json` 的类型链插入时间分支（**放在 `String` 之前**）：

```rust
use base64::Engine as _;
use ecat_data::Row;
use sqlx::{Column as SqlxColumn, Row as SqlxRow};

/// 三种驱动的 Row 类型不同，用宏生成三份同构实现。
/// 类型链：bool → i64 → i32 → f64（NaN/Inf 转字符串）→
/// OffsetDateTime（→ RFC3339 UTC）→ String → Blob（base64）→ Null。
macro_rules! cell_fn {
    ($name:ident, $row:ty) => {
        pub fn $name(row: &$row, col: &str) -> serde_json::Value {
            row.try_get::<bool, _>(col)
                .map(serde_json::Value::Bool)
                .or_else(|_| {
                    row.try_get::<i64, _>(col)
                        .map(|n| serde_json::Value::Number(n.into()))
                })
                .or_else(|_| {
                    row.try_get::<i32, _>(col)
                        .map(|n| serde_json::Value::Number((n as i64).into()))
                })
                .or_else(|_| {
                    row.try_get::<f64, _>(col)
                        .ok()
                        .and_then(|n| {
                            if n.is_finite() {
                                serde_json::Number::from_f64(n).map(serde_json::Value::Number)
                            } else if n.is_nan() {
                                Some(serde_json::Value::String("NaN".into()))
                            } else if n > 0.0 {
                                Some(serde_json::Value::String("Infinity".into()))
                            } else {
                                Some(serde_json::Value::String("-Infinity".into()))
                            }
                        })
                        .ok_or(())
                })
                // 时间分支：原生池支持 time 类型，这正是弃用 AnyPool 的收益。
                // 统一转 UTC，避免带偏移量的字符串破坏排序（spec §5.4）。
                .or_else(|_| {
                    row.try_get::<time::OffsetDateTime, _>(col).map(|dt| {
                        serde_json::Value::String(
                            dt.to_offset(time::UtcOffset::UTC).to_string(),
                        )
                    })
                })
                .or_else(|_| row.try_get::<String, _>(col).map(serde_json::Value::String))
                .or_else(|_| {
                    row.try_get::<Vec<u8>, _>(col).map(|b| {
                        serde_json::Value::String(
                            base64::engine::general_purpose::STANDARD.encode(b),
                        )
                    })
                })
                .unwrap_or(serde_json::Value::Null)
        }
    };
}

cell_fn!(pg_cell_to_json, sqlx::postgres::PgRow);
cell_fn!(mysql_cell_to_json, sqlx::mysql::MySqlRow);
cell_fn!(sqlite_cell_to_json, sqlx::sqlite::SqliteRow);

macro_rules! rows_fn {
    ($name:ident, $row:ty, $cell:ident) => {
        pub fn $name(rows: Vec<$row>) -> Vec<Row> {
            if rows.is_empty() {
                return Vec::new();
            }
            let columns: Vec<String> = rows[0]
                .columns()
                .iter()
                .map(|c| c.name().to_string())
                .collect();
            rows.iter()
                .map(|row| {
                    let values: Vec<serde_json::Value> =
                        columns.iter().map(|col| $cell(row, col)).collect();
                    Row::new(columns.clone(), values)
                })
                .collect()
        }
    };
}

rows_fn!(pg_rows_to_result, sqlx::postgres::PgRow, pg_cell_to_json);
rows_fn!(mysql_rows_to_result, sqlx::mysql::MySqlRow, mysql_cell_to_json);
rows_fn!(sqlite_rows_to_result, sqlx::sqlite::SqliteRow, sqlite_cell_to_json);
```

> `Row::new` 内部有 `debug_assert_eq!(columns.len(), values.len())`，
> 两者由同一 `columns` 迭代产出，长度必然一致。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p ecat-data-sqlx`
Expected: 全绿。SQLite 上时间、BLOB、NULL 三个用例通过

- [ ] **Step 5: 提交**

```bash
git add ecat-data-sqlx/src/cell.rs ecat-data-sqlx/src/lib.rs
git commit -m "feat(ecat-data-sqlx): 行转换支持 time 类型，时间统一为 RFC3339 UTC"
```

---

### Task 8: 事务 wrapper 三路重写 + 分派测试

> **职责边界（2026-10-06 修正）**：初版把本任务题为「`SqlExecutor` 三路分派实现」，
> 与 Task 6 重叠 —— Task 6 的验收项「`lib.rs` 里所有 `AnyPool` 引用应已清除」本身
> 就要求客户端层的三路分派落地（否则 `SqlxClient.pool` 从 `AnyPool` 换成 `Pool` 后
> `impl SqlExecutor` 无法编译）。**修正后的边界**：
>
> - **Task 6 负责**：`Pool` 枚举、`connect` 分派、**客户端层** `impl SqlExecutor` 的三路 match、
>   测试助手 `mem_sqlite` 改造（它是 Task 6/7/8 测试的共同前置）。
> - **Task 8 负责**：`SqlxTransactionWrapper` 从「持 `sqlx::Transaction<'static, sqlx::Any>`」
>   改为**三种原生事务的 wrapper**（`Pg` / `MySql` / `Sqlite` 类型不同，宏生成三份），
>   `transaction()` 的三路分派，以及本任务列出的分派/事务测试。
>
> 两者**不重复**：Task 6 动的是 `&self.pool` 上的执行分派，Task 8 动的是事务句柄的类型。

**Files:**
- Modify: `ecat-data-sqlx/src/lib.rs`
- Test: `ecat-data-sqlx/src/lib.rs` 内联测试（改造既有测试到原生池）

- [ ] **Step 1: 写失败测试**

替换测试助手（去掉 `init_drivers`，改原生 SQLite 池）：

```rust
    /// 内存 SQLite：无外部服务即可做端到端往返。
    /// `cache=shared` + 单连接池保证同测试内所有语句命中同一个库。
    async fn mem_sqlite(name: &str) -> SqlxClient {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let url = format!("sqlite:ecat-test-{name}{n}?mode=memory&cache=shared");
        let params = PoolParams {
            max_connections: 1,
            ..PoolParams::default()
        };
        SqlxClient::connect_with_params(&url, &params).await.unwrap()
    }
```

新增测试：

```rust
    #[tokio::test]
    async fn dialect_is_reported_from_pool_variant() {
        let db = mem_sqlite("dialect").await;
        assert_eq!(db.dialect(), Dialect::Sqlite);
    }

    #[tokio::test]
    async fn transaction_rolls_back_on_drop() {
        let db = mem_sqlite("tx_rollback").await;
        db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
            .await
            .unwrap();

        let tx = db.transaction().await.unwrap();
        tx.execute("INSERT INTO t (id, v) VALUES (1, 'x')")
            .await
            .unwrap();
        drop(tx); // 未提交

        let rows = db.query("SELECT id FROM t").await.unwrap();
        assert!(rows.is_empty(), "未提交事务必须回滚");
    }

    #[tokio::test]
    async fn transaction_commit_persists() {
        let db = mem_sqlite("tx_commit").await;
        db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
            .await
            .unwrap();

        let tx = db.transaction().await.unwrap();
        tx.execute("INSERT INTO t (id, v) VALUES (1, 'x')")
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let rows = db.query("SELECT id FROM t").await.unwrap();
        assert_eq!(rows.len(), 1);
    }

    /// 事务内的方言必须透传，否则 ORM 在事务里会生成错误占位符。
    #[tokio::test]
    async fn transaction_reports_dialect() {
        let db = mem_sqlite("tx_dialect").await;
        let tx = db.transaction().await.unwrap();
        assert_eq!(tx.dialect(), Dialect::Sqlite);
    }

    /// `query_timeout_secs: 0` 时禁用超时：慢查询不应被杀。
    #[tokio::test]
    async fn zero_timeout_disables_timeout() {
        let params = PoolParams {
            max_connections: 1,
            query_timeout: None,
            ..PoolParams::default()
        };
        let db = SqlxClient::connect_with_params("sqlite::memory:", &params)
            .await
            .unwrap();
        db.query("SELECT 1").await.unwrap();
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p ecat-data-sqlx`
Expected: 编译失败（`Pool` 已换但 `SqlExecutor` 实现仍引用 `AnyPool`）

- [ ] **Step 3: 实现三路分派**

`impl SqlExecutor for SqlxClient`：每个方法先取 `sql`/`params`，
用 `self.query_timeout` 包一层 `run_with_timeout`，再按 `self.pool` 分派。
以 `execute` 与 `query_with` 为例（另两个同构）：

```rust
#[async_trait]
impl SqlExecutor for SqlxClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        run_with_timeout(self.query_timeout, async {
            let affected = match &self.pool {
                Pool::Pg(p) => sqlx::query(sql).execute(p).await,
                Pool::My(p) => sqlx::query(sql).execute(p).await,
                Pool::Sq(p) => sqlx::query(sql).execute(p).await,
            }
            .map_err(|e| RdbmsError::Database(e.to_string()))?
            .rows_affected();
            Ok(affected)
        })
        .await
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        run_with_timeout(self.query_timeout, async {
            match &self.pool {
                Pool::Pg(p) => {
                    let rows = sqlx::query(sql)
                        .fetch_all(p)
                        .await
                        .map_err(|e| RdbmsError::Database(e.to_string()))?;
                    Ok(pg_rows_to_result(rows))
                }
                Pool::My(p) => {
                    let rows = sqlx::query(sql)
                        .fetch_all(p)
                        .await
                        .map_err(|e| RdbmsError::Database(e.to_string()))?;
                    Ok(mysql_rows_to_result(rows))
                }
                Pool::Sq(p) => {
                    let rows = sqlx::query(sql)
                        .fetch_all(p)
                        .await
                        .map_err(|e| RdbmsError::Database(e.to_string()))?;
                    Ok(sqlite_rows_to_result(rows))
                }
            }
        })
        .await
    }

    async fn execute_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        run_with_timeout(self.query_timeout, async {
            let affected = match &self.pool {
                Pool::Pg(p) => {
                    let mut q = sqlx::query(sql);
                    for v in params {
                        q = bind_pg(q, v);
                    }
                    q.execute(p).await
                }
                Pool::My(p) => {
                    let mut q = sqlx::query(sql);
                    for v in params {
                        q = bind_mysql(q, v);
                    }
                    q.execute(p).await
                }
                Pool::Sq(p) => {
                    let mut q = sqlx::query(sql);
                    for v in params {
                        q = bind_sqlite(q, v);
                    }
                    q.execute(p).await
                }
            }
            .map_err(|e| RdbmsError::Database(e.to_string()))?
            .rows_affected();
            Ok(affected)
        })
        .await
    }

    async fn query_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        run_with_timeout(self.query_timeout, async {
            match &self.pool {
                Pool::Pg(p) => {
                    let mut q = sqlx::query(sql);
                    for v in params {
                        q = bind_pg(q, v);
                    }
                    let rows = q
                        .fetch_all(p)
                        .await
                        .map_err(|e| RdbmsError::Database(e.to_string()))?;
                    Ok(pg_rows_to_result(rows))
                }
                Pool::My(p) => {
                    let mut q = sqlx::query(sql);
                    for v in params {
                        q = bind_mysql(q, v);
                    }
                    let rows = q
                        .fetch_all(p)
                        .await
                        .map_err(|e| RdbmsError::Database(e.to_string()))?;
                    Ok(mysql_rows_to_result(rows))
                }
                Pool::Sq(p) => {
                    let mut q = sqlx::query(sql);
                    for v in params {
                        q = bind_sqlite(q, v);
                    }
                    let rows = q
                        .fetch_all(p)
                        .await
                        .map_err(|e| RdbmsError::Database(e.to_string()))?;
                    Ok(sqlite_rows_to_result(rows))
                }
            }
        })
        .await
    }

    fn dialect(&self) -> Dialect {
        self.pool.dialect()
    }
}
```

三个 `bind_*` 助手用宏生成（`Query` 类型随驱动不同，无法合并成一个函数）：

```rust
/// 参数绑定：值类型分派与原实现一致。
macro_rules! bind_fn {
    ($name:ident, $db:ty) => {
        fn $name<'q>(
            q: sqlx::query::Query<'q, $db, <$db as sqlx::Database>::Arguments<'q>>,
            p: &'q serde_json::Value,
        ) -> sqlx::query::Query<'q, $db, <$db as sqlx::Database>::Arguments<'q>> {
            match p {
                serde_json::Value::String(s) => q.bind(s.as_str()),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        q.bind(i)
                    } else if let Some(f) = n.as_f64() {
                        q.bind(f)
                    } else {
                        q.bind(n.to_string())
                    }
                }
                serde_json::Value::Bool(b) => q.bind(*b),
                serde_json::Value::Null => q.bind(None::<String>),
                other => q.bind(other.to_string()),
            }
        }
    };
}

bind_fn!(bind_pg, sqlx::Postgres);
bind_fn!(bind_mysql, sqlx::MySql);
bind_fn!(bind_sqlite, sqlx::Sqlite);
```

`transaction()` 也按变体分派：

```rust
#[async_trait]
impl RdbmsClient for SqlxClient {
    async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
        let dialect = self.pool.dialect();
        let inner: Box<dyn TransactionInner> = match &self.pool {
            Pool::Pg(p) => Box::new(PgTransactionWrapper {
                inner: Some(p.begin().await.map_err(db_err)?),
            }),
            Pool::My(p) => Box::new(MyTransactionWrapper {
                inner: Some(p.begin().await.map_err(db_err)?),
            }),
            Pool::Sq(p) => Box::new(SqTransactionWrapper {
                inner: Some(p.begin().await.map_err(db_err)?),
            }),
        };
        Ok(ecat_data::Transaction::with_inner(inner))
    }
}
```

> 三个 wrapper 结构相同（`inner: Option<sqlx::Transaction<'static, DB>>`），
> 用 `macro_rules!` 生成，避免三份重复代码。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p ecat-data-sqlx && cargo test --workspace`
Expected: 全绿

- [ ] **Step 5: 提交**

```bash
git add ecat-data-sqlx/src/lib.rs
git commit -m "refactor(ecat-data-sqlx): SqlExecutor 三路原生分派 + 事务 wrapper 重写"
```

---

### Task 9: `warm_up()` 与池状态

**Files:**
- Modify: `ecat-data-sqlx/src/lib.rs`
- Modify: `ecat-data-sqlx/src/pool.rs`
- Test: `ecat-data-sqlx/src/lib.rs` 内联测试

- [ ] **Step 1: 写失败测试**

```rust
    /// 预热必须真正建满 min_connections：MSSQL 建连含 TDS 握手 + TLS + 认证，
    /// 首个请求踩上去就是几十到几百 ms。SQLite 是本地无握手，但逻辑一致。
    #[tokio::test]
    async fn warm_up_creates_min_connections() {
        let params = PoolParams {
            max_connections: 4,
            min_connections: 3,
            ..PoolParams::default()
        };
        let db = SqlxClient::connect_with_params("sqlite::memory:", &params)
            .await
            .unwrap();
        db.warm_up().await.unwrap();
        // 用 >= 而非 ==：sqlx 的后台保底任务可能同时在建连。
        assert!(
            db.pool_size() >= 3,
            "warm_up 后应至少有 3 条连接，实际 {}",
            db.pool_size()
        );
    }

    #[tokio::test]
    async fn warm_up_without_min_connections_is_a_noop() {
        let params = PoolParams {
            max_connections: 4,
            min_connections: 0,
            ..PoolParams::default()
        };
        let db = SqlxClient::connect_with_params("sqlite::memory:", &params)
            .await
            .unwrap();
        db.warm_up().await.unwrap();
        assert_eq!(db.pool_size(), 0);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p ecat-data-sqlx warm_up`
Expected: 编译失败，`no method named warm_up`

- [ ] **Step 3: 实现**

`pool.rs` 的 `Pool` 增加：

```rust
    /// 已建立的连接总数。
    pub fn size(&self) -> u32 {
        match self {
            Self::Pg(p) => p.size(),
            Self::My(p) => p.size(),
            Self::Sq(p) => p.size(),
        }
    }
```

（Task 6 里的 `size()` 返回 `(u32, u32)` 的版本替换为这个单值版本 +
下面这个 `idle()`。）

```rust
    /// 空闲连接数。
    pub fn idle(&self) -> u32 {
        match self {
            Self::Pg(p) => p.num_idle() as u32,
            Self::My(p) => p.num_idle() as u32,
            Self::Sq(p) => p.num_idle() as u32,
        }
    }
```

`lib.rs` 的 `SqlxClient` 增加字段与两个方法。字段在 `connect_with_params` 里填：

```rust
pub struct SqlxClient {
    pool: Pool,
    query_timeout: Option<Duration>,
    min_connections: u32,
}
```
```rust
    pub async fn connect_with_params(url: &str, params: &PoolParams) -> Result<Self, sqlx::Error> {
        Ok(Self {
            pool: Pool::connect(url, params).await?,
            query_timeout: params.query_timeout,
            min_connections: params.min_connections,
        })
    }
```
```rust
    /// 同步建满 `min_connections` 条连接并归还，让服务启动后立刻处于就绪态。
    /// 在服务启动时调用一次。
    ///
    /// 为什么 sqlx 已有 `min_connections` 还需要它：sqlx 的保底由**后台任务
    /// 异步维护**（`sqlx-core-0.8.6/src/pool/inner.rs:514-517`），`connect()`
    /// 返回时不保证已建满 —— 启动后第一波请求会与后台任务抢跑。`warm_up()`
    /// 把它变成启动期的同步等待。（对 batch 2 的 deadpool 侧更是必需：
    /// deadpool 没有 `min_connections` 概念。）
    pub async fn warm_up(&self) -> Result<(), RdbmsError> {
        let mut guards = Vec::with_capacity(self.min_connections as usize);
        while (guards.len() as u32) < self.min_connections {
            guards.push(
                self.pool
                    .acquire()
                    .await
                    .map_err(|e| RdbmsError::Connection(e.to_string()))?,
            );
        }
        // guards 在此处 drop，连接归还池中
        Ok(())
    }

    /// 池内已建立的连接总数（供 metrics 与测试）。
    pub fn pool_size(&self) -> u32 {
        self.pool.size()
    }
```

> 测试断言用 `>=` 而非 `==`：sqlx 的后台保底任务可能同时在建连。
> 目标连接数由 `min_connections` 字段承载，`Pool` 的三个变体不必改结构。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p ecat-data-sqlx`
Expected: 全绿

- [ ] **Step 5: 全套 CI 命令**

Run:
```bash
cargo test --workspace && cargo fmt --check && cargo clippy --workspace -- -D warnings
```
Expected: 全绿，无 warning

- [ ] **Step 6: 提交**

```bash
git add ecat-data-sqlx/src/lib.rs ecat-data-sqlx/src/pool.rs
git commit -m "feat(ecat-data-sqlx): warm_up 预热与池状态查询"
```

---

### Task 10: 文档 —— 数据库配置教程 ×13

**Files:**
- Modify: `docs/database-config-tutorial.md`
- Modify: `docs/i18n/{ar,bn,de,en,es,fr,hi,id,ja,ko,pt,ru}/database-config-tutorial.md`

- [ ] **Step 1: 更新根文件**

`docs/database-config-tutorial.md` 中与 `SqlxConfig` 相关的段落改为反映：

- 新增池参数：`max_connections`（默认 10）、`min_connections`（默认 0）、
  `acquire_timeout_secs`（30）、`idle_timeout_secs`（600）、`max_lifetime_secs`（1800）、
  `query_timeout_secs`（30，`0` 为禁用）、`test_before_acquire`（默认 `false`）
- 新增 `session_init`：默认按方言给（PG `SET TIME ZONE 'UTC'` + `application_name`，
  MySQL `SET time_zone = '+00:00'`，SQLite 无）
- 新增 `warm_up()` 用法与建议调用位置（服务启动时一次）
- 原生池说明：不再使用 `AnyPool`，方言由 URL scheme 决定；
  时间类型原生支持，无需 CAST

同时更新所有出现 `SqlxClient::from_pool(pool)` 的示例为 `from_pool(Pool::Sq(pool))`。

- [ ] **Step 2: 同步 12 语言副本**

逐份更新 `docs/i18n/{lang}/database-config-tutorial.md`，与根文件同结构同内容
（保持各语言既有的术语与译法，只改差异部分）。

- [ ] **Step 3: 校验一致性**

Run:
```bash
grep -rn "AnyPool" docs/database-config-tutorial.md docs/i18n/*/database-config-tutorial.md
```
Expected: 无输出

Run:
```bash
for f in docs/database-config-tutorial.md docs/i18n/*/database-config-tutorial.md; do
  printf "%s: %s\n" "$f" "$(grep -c 'query_timeout_secs' $f)"
done
```
Expected: 每份都 ≥ 1

- [ ] **Step 4: 提交**

```bash
git add docs/database-config-tutorial.md docs/i18n/*/database-config-tutorial.md
git commit -m "docs: 数据库配置教程同步原生池与会话初始化（×13）"
```

---

## 批次完成检查

- [ ] `cargo test --workspace` 全绿，**测试数不下降**且 0 failed
- [ ] **fmt：本批改动过的文件** `cargo fmt -- --check` 干净（见下：全量 fmt 有既有失败）
- [ ] **clippy：除已知的 `double_must_use` 误报外，workspace 不得有任何其它诊断**
- [ ] **`cargo doc -p <改动过的 crate> --no-deps` 无 warning**（clippy 与 fmt 都不覆盖
      文档链接合法性 —— 实施中发现「公开文档链接到私有模块条目」这类问题只有 `cargo doc` 报）
- [ ] `cargo audit --deny warnings` 通过（本批未新增外部依赖，应无变化）
- [ ] `grep -rn "AnyPool\|install_default_drivers" ecat-data-sqlx/` 无输出
- [ ] `grep -c "query_timeout_secs" docs/database-config-tutorial.md` ≥ 1
- [ ] 12 个 i18n 副本与根文件同结构
- [ ] **`ecat-data-sqlx/src/lib.rs` 回到 500 行以内**（见下）

### 已知的 500 行超限（两处，均须在批次 4 收尾前清掉）

**① `ecat-data-sqlx/src/lib.rs`：Task 3 后 629 行**（改动前 491），超出仓库 500 行约定。
**暂不单独拆分** —— 后续任务本就要把它拆开，现在拆是重复劳动：

| 任务 | 抽走的模块 | 约计 |
|---|---|---|
| Task 5 | `src/config.rs`（`SqlxConfig` / `PoolParams`） | ~80 行 |
| Task 6 | `src/pool.rs`（`Pool` 枚举 + 三路分派 + `PoolGuard`） | ~120 行 |
| Task 7 | `src/cell.rs`（三个 `cell_fn!` / `rows_fn!` 宏） | ~90 行 |

**验收项**：Task 7 结束时该文件必须 ≤ 500 行；若仍超，把
`SqlxTransactionWrapper` + 其测试移到 `src/transaction.rs`（最自然的下一刀）。

**② `ecat-data/src/rdbms.rs`：Task 4 后 499 行** —— 距 500 行上限**只剩 1 行余量**。
批次 1 剩余任务（5–10）都不碰该文件，所以不会立刻爆，但下一次任何改动都会撞线。

Task 4 的实施者已经为此被迫让步一次（计划原样落地实测 503 行，rustfmt 回弹 3 种压缩形态，
最后改用 `Transaction::new()` 构造测试才压到 499）。**不要再让后续任务在这个文件上做让步。**

**处置**：批次 4 收尾时把 `Transaction` + `TransactionInner` + 相关测试抽到
`ecat-data/src/transaction.rs`（`rdbms.rs` 保留 `Row` / `SqlExecutor` / `RdbmsClient` /
`RdbmsError`）。

**注意不要顺手"修复"那个放宽的断言**：`dropped_uncommitted_transaction_counts_as_leak`
现在用 `after > before` 而非 `after == before + 1`，原因是同一测试二进制里相邻的
测试会并发递增同一个进程级计数器（实测 400 次失败 5 次）。**拆文件不解决这个问题**
（仍在同一二进制内），只有给两个测试加互斥才行 —— 那是用测试间耦合换一个
并不更真实的断言（`>` 已能抓到「Drop 不再计数」这个真回归）。**保持放宽版。**

### 闸门为何不是「全绿」——两个既有红灯（2026-10-05 实测）

用当前工具链（clippy / rustfmt 1.99.0，`rust-toolchain.toml` 钉 `stable`），
**仓库在 base commit `76014b6` 就有两个闸门是红的**，与本批改动无关：

| 闸门 | base 状态 | 成因 |
|---|---|---|
| `cargo clippy --workspace -- -D warnings` | 红：workspace 47 条 warning | 30+ 条 `clippy::double_must_use`，全部来自 `#[async_trait]` 宏展开的误报（每个 async trait 方法计 1 条） |
| `cargo fmt --check` | 红：`ecat-security/src/lib.rs:107` | 一行超 100 列（`&&` 链换行），该文件不在本批 diff 内 |

验证方式：在 base commit 的 detached worktree 里单独跑，得到同样的结果。

### 正确的闸门命令（旧版是空转的）

> ⚠️ 初版计划给的是 `cargo clippy --workspace 2>&1 | grep -c "^error"` —— **恒为 0**：
> 没加 `-- -D warnings` 时这些是 `warning:` 而非 `error:`，闸门永远不会失败。
> 这类「空转的验证会伪装成通过」，本批次已被审查者抓到一次。

```bash
# ① fmt：只看本批改动过的文件（全量会撞上 ecat-security 的既有失败）
git diff --name-only <批次起点>..HEAD -- '*.rs' | xargs -r rustfmt --check

# ② clippy：列出所有诊断措辞，应当**只有一种**（即 must_use 那条）—— 这是真信号
cargo clippy --workspace --all-targets 2>&1 \
  | grep "^warning: " | grep -v "generated" \
  | sed 's/[0-9]\+/N/g' | sort -u

# ③a 口径 A：ecat-data 单 crate（与「30 → 31」的说法对齐）
cargo clippy -p ecat-data --all-targets 2>&1 | grep -c "^warning: this function"

# ③b 口径 B：全 workspace（CI 真正跑的范围）
cargo clippy --workspace --all-targets 2>&1 | grep -c "^warning: this function"
```

**两个口径不能混用** —— 「30」是 ecat-data 口径，「47」是 workspace 口径：

| 口径 | base `76014b6` | Task 2 后 | 本批终值 = base + 新增 async trait 方法数 |
|---|---|---|---|
| `ecat-data` | 30 | 31 | ≤ 35 |
| `--workspace` | 47 | 48 | ≤ 52 |

> ⚠️ **两个已踩过的坑，别重犯**：
>
> 1. 裸 `grep -c "^error"` 即使加 `-- -D warnings` 也会**多算 1** —— 收尾行
>    `` error: could not compile `ecat-data` (lib) due to 31 previous errors `` 同样匹配 `^error`。
> 2. `grep -c "clippy::double_must_use"` **恒返回 1** —— 该字符串在整份输出里只出现一次
>    （clippy 仅对首个诊断打印 ``= note: `#[warn(clippy::double_must_use)]` ``）。
>
> 可靠口径是**诊断头行** `^warning: this function`。

**本批次已知的合法增量**（逐项可对账）：

| 来源 | 累计 |
|---|---|
| 基线（base `76014b6`） | 30 |
| Task 2 新增 `query_write` | 31（实测确认） |
| Task 3 新增 `TransactionInner` 的 4 个执行方法 | ~35（预计） |

**真信号是 ②**：只要出现第二种诊断措辞，就是新增了别的 lint —— 那才是要拦的东西。

**这笔债的正当修法**（需用户拍板，独立提交，不并入本批）：
- `ecat-data` 加 crate 级 `[lints.clippy] double_must_use = "allow"` + 理由注释
- `ecat-security/src/lib.rs:107` 跑一次 `cargo fmt -p ecat-security`
