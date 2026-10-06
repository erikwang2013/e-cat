# 批次 3 实施计划：`ecat-orm` + `ecat-orm-derive`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付 `ecat-orm`（实体宏、查询构建器、CRUD、关联预加载、批量分块、分页、软删除/时间戳/乐观锁、迁移系统）与 `ecat-orm-derive`，使 5 个方言上都能用同一套 API 操作数据库。

**Architecture:** `ecat-orm` 建立在批次 1 拆出的 `SqlExecutor` 之上 —— 所有数据操作取 `&impl SqlExecutor`，因此**客户端与 `Transaction` 通吃**。方言差异全部下沉到 `DialectSpec` 纯函数层（无数据库即可全量单测），ORM 本体只做 SQL 拼装与行/值映射。实体元数据由 `#[derive(Entity)]` 生成为 `&'static EntityMeta`（const 可构造），查询构建器与迁移 DDL 共用同一份元数据，避免宏生成大段重复代码。

**Tech Stack:** Rust edition 2024 · `ecat-data`（批次 1 的 `SqlExecutor`/`Dialect`/`Row`）· `serde_json::Value` 作为统一单元格类型 · `time` 0.3 · `syn` 2 / `quote` 1 / `proc-macro2` 1（派生宏）· SQLite in-memory 做全链路集成测试。

**设计依据:** `docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md` §5–§8、§11.5、§12

---

## 已核实的 API 事实（**不要重新猜，直接用**）

以下每一条都是本计划写作时**实际读过源码/实测**确认的。实施者直接采信，不要再猜。

### `ecat-data`（批次 1 已发布 4.0.0）

```rust
// ecat-data/src/rdbms.rs:7-29
pub struct Row { /* 私有：columns: Vec<String>, values: Vec<serde_json::Value> */ }
impl Row {
    pub fn new(columns: Vec<String>, values: Vec<serde_json::Value>) -> Self;
    pub fn get(&self, col: &str) -> Option<&serde_json::Value>;   // 按列名取，重名取第一个
}
```

- **`Row` 没有任何列名遍历/按索引取值的方法** —— 只有 `get(name)`。ORM 的 `from_row` 按列名取值，够用。
- `Row::new` 有 `debug_assert_eq!`：columns 与 values 长度必须相等。
- `get()` 返回 `Option`：`None` = **列不存在**；`Some(&Value::Null)` = **列存在但为 NULL**。两者语义不同，`from_row` 必须区分。

```rust
// ecat-data/src/rdbms.rs:171-210
#[async_trait]
pub trait SqlExecutor: Send + Sync {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError>;
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError>;
    async fn execute_with(&self, sql: &str, params: &[serde_json::Value]) -> Result<u64, RdbmsError>;  // 默认实现 = 报错
    async fn query_with(&self, sql: &str, params: &[serde_json::Value]) -> Result<Vec<Row>, RdbmsError>; // 默认实现 = 报错
    async fn query_write(&self, sql: &str, params: &[serde_json::Value]) -> Result<Vec<Row>, RdbmsError>; // 默认 = 委托 query_with
    fn dialect(&self) -> Dialect;
}
```

- **全部方法取 `&self`**（不是 `&mut self`）—— ORM 的 `insert(&db, ...)` 直接传 `&impl SqlExecutor` 即可。
- `Transaction` 也 `impl SqlExecutor`（`rdbms.rs:113-158`），所以 `&tx` 能直接当 executor 传。**事务与客户端通吃是免费的，不需要额外抽象。**

```rust
// ecat-data/src/rdbms.rs:217-227
pub enum RdbmsError { Database(String), Connection(String), Config(String), Timeout(String) }

// ecat-data/src/dialect.rs:8-16
pub enum Dialect { Standard, Sqlite, Postgres, MySql, Mssql }
```

### `time`

- workspace 依赖：`time = { version = "0.3", features = ["formatting", "parsing"] }`（根 `Cargo.toml:101`）
- **缺 `macros` feature** —— 解析纯日期 `YYYY-MM-DD` 需要 `format_description!`，该宏在 `macros` feature 下。见「任务 4」处理。
- RFC3339 用 `time::format_description::well_known::Rfc3339`（不需要 `macros`），两个 cell.rs 已在用：
  `OffsetDateTime::format(&Rfc3339)` （`ecat-data-sqlx/src/cell.rs:44`、`ecat-data-mssql/src/cell.rs:57`）

### 派生宏

- **本工作区目前没有任何 proc-macro crate** —— `ecat-orm-derive` 是第一个。
- `syn` / `quote` / `proc-macro2` **都不在 workspace.dependencies 里**，需要新增。

### 文件头约定

每个源文件首行是该 crate 的版权注释，照抄即可：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
```

---

## 两处必须在动工前裁决的设计缺口

> 这两处是**设计 spec 内部不自洽**的地方。计划里给出的裁决是规范，不是建议 —— 实施者照计划做，不要照 spec 里那两段做。

### 缺口 A：`spec §5.4` 的 `filter()` 示例与 `spec §5.5(a)` 硬约束**互相矛盾**

§5.4 的示例（spec:495）是**不可失败的**：

```rust
User::query().filter("name", Op::Like, "%e%").fetch(&db).await?;
//             ^^^^^^ 无 ? —— 隐含 filter 不返回 Result
```

但 §5.5(a)（spec:534-536）是硬约束：

> 这些 API 必须把列名对照 `EntityMeta.columns` 校验，未命中即返回 `OrmError::UnknownColumn`，绝不拼进 SQL。

一个不返回 `Result` 的函数**无法**返回 `OrmError::UnknownColumn`。

**裁决：`filter` / `order_by` / `group_by` 一律返回 `Result`**，示例改为：

```rust
User::query().filter("name", Op::Like, "%e%")?.fetch(&db).await?;
//                                        ^ 必须有
```

**理由**：§5.5(a) 是**安全约束**（防 SQL 注入的标识符白名单），§5.4 是**用法示意**。安全约束赢。批次 1 的教训是「把『一个格式更简单』当硬约束去压语义正确性，是错的」（spec:644-647）—— 这里反过来，不能为了示例好看把安全校验降级成 panic 或静默忽略。

### 缺口 B：`Query` 的类型状态需要第三个态，spec 只列了两个

§5.2（spec:446）写的是「类型状态：Unfiltered → Filtered」。但 `with_trashed()`（spec:525）出现在**取数据之前**、可能在有 filter 之前调用：

```rust
User::query().with_trashed().filter(...)?   // 若 with_trashed 只能作用于 Filtered，这行要调换顺序
```

**裁决：`with_trashed()` 在**两个状态上都实现**，返回同一状态（`Query<E, Unfiltered>` → `Query<E, Unfiltered>`，`Query<E, Filtered>` → `Query<E, Filtered>`）。** 用两个 `impl` 块，不是第三个状态位。

**理由**：`with_trashed` 是**读取开关**，与「有没有过滤条件」正交。它既不构成安全风险（不会让 `delete_where` 变成全表删除 —— delete 路径的 `with_trashed` 是另一个显式方法），也不该强迫用户调整调用顺序。引入第三个类型参数只为表达正交的两个布尔位，是过度设计。

---

## 文件结构

```
ecat-orm/
  Cargo.toml
  src/
    lib.rs             re-export + crate 文档（`Entity` derive 从 ecat-orm-derive 转出）
    error.rs           OrmError（thiserror）
    entity.rs          Entity trait / EntityMeta / ColumnMeta / EntityFlags / ColType / RelationMeta / RelationKind
    time.rs            OffsetDateTime ↔ RFC3339、Date ↔ YYYY-MM-DD 互转
    value.rs           serde_json::Value ↔ ColType 取值的统一转换（from_row 用）
    crud.rs            insert / find_by_id / find_all / update / delete / save
    batch.rs           insert_many / update_many / delete_where / upsert
    page.rs            Page<T> + paginate / paginate_without_count
    relation.rs        RelationMeta 驱动的一对多/一对一预加载（IN 分块，杜绝 N+1）
    query/
      mod.rs           Query<E, S> 构建器（类型状态 Unfiltered → Filtered）
      filter.rs        Op + Expr 表达式树
      sql.rs           SELECT 生成 + join + 分页 + COUNT 生成
    dialect/
      mod.rs           DialectSpec trait + lookup(Dialect) -> &'static dyn DialectSpec
      standard.rs
      sqlite.rs
      postgres.rs
      mysql.rs
      mssql.rs
    migrate/
      mod.rs           Migrator（new / add / status / run / down）
      ddl.rs           EntityMeta → CREATE TABLE / DROP TABLE / 类型映射
      version.rs       _ecat_migrations 版本表读写
  tests/
    sqlite_e2e.rs      SQLite in-memory 全链路集成测试

ecat-orm-derive/
  Cargo.toml
  src/
    lib.rs             #[proc_macro_derive(Entity, attributes(entity))]
    attr.rs            #[entity(...)] 属性解析
    expand.rs          代码生成（Entity impl + META static + XxxRelation 枚举）
```

**每个源文件 < 500 行**（项目硬规则）。上表已按此拆分；`clippy` 不许有 `-D warnings` 之外的告警。

---

## 批次完成检查（每个任务提交前跑，最后的全量检查单列在文末）

```bash
cd /home/wwwroot/e-cat
cargo test -p ecat-orm -p ecat-orm-derive 2>&1 | tail -20
cargo fmt --check
cargo clippy -p ecat-orm -p ecat-orm-derive --all-targets -- -D warnings; echo "rc=$?"
```

⚠️ **不要用 `cmd | tail -3; echo rc=$?`** —— 那取的是 `tail` 的退出码，恒为 0，闸门形同空转。要真实码用 `${PIPESTATUS[0]}` 或不接管道。

---

## Task 1: 两个 crate 的骨架与依赖

**Files:**
- Create: `ecat-orm/Cargo.toml`
- Create: `ecat-orm/src/lib.rs`
- Create: `ecat-orm-derive/Cargo.toml`
- Create: `ecat-orm-derive/src/lib.rs`
- Modify: `Cargo.toml`（根：members 列表 + workspace.dependencies）

- [ ] **Step 1: 先看根 `Cargo.toml` 的 members 与 workspace.dependencies 现状**

```bash
cd /home/wwwroot/e-cat
sed -n '/^\[workspace\]/,/^\[workspace.package\]/p' Cargo.toml
sed -n '/^\[workspace.dependencies\]/,/^$/p' Cargo.toml | head -40
```

记下 members 数组的确切写法（批次 2 加过一个 member，照它的格式加）。

- [ ] **Step 2: 建 `ecat-orm-derive/Cargo.toml`**

```toml
[package]
name = "ecat-orm-derive"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Derive macros for ecat-orm (#[derive(Entity)])"
repository.workspace = true
homepage.workspace = true
keywords.workspace = true
categories.workspace = true

[lib]
proc-macro = true

[dependencies]
syn = { workspace = true }
quote = { workspace = true }
proc-macro2 = { workspace = true }
```

- [ ] **Step 3: 建 `ecat-orm/Cargo.toml`**

```toml
[package]
name = "ecat-orm"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "ORM for e-cat: entity derive, query builder, migrations"
repository.workspace = true
homepage.workspace = true
keywords.workspace = true
categories.workspace = true

[dependencies]
ecat-data = { workspace = true }
ecat-orm-derive = { workspace = true }
serde_json = { workspace = true }
time = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tokio = { workspace = true }
ecat-data-sqlx = { workspace = true }
```

- [ ] **Step 4: 在根 `Cargo.toml` 补 members 与 workspace.dependencies**

members 补两项（按现有格式，注意逗号与缩进）：

```toml
    "ecat-orm",
    "ecat-orm-derive",
```

`[workspace.dependencies]` 补四项（`ecat-data` / `ecat-orm-derive` 照该表里其它 `ecat-*` 的写法，即 `{ version = "4.0.0", path = "..." }`）：

```toml
ecat-orm = { version = "4.0.0", path = "ecat-orm" }
ecat-orm-derive = { version = "4.0.0", path = "ecat-orm-derive" }
syn = { version = "2", features = ["full"] }
quote = "1"
proc-macro2 = "1"
```

⚠️ `ecat-data` 在表里应已存在（批次 1 加的）。**先 grep 确认再动**，不要重复插入：

```bash
grep -n '^ecat-data\s*=\|^ecat-data-sqlx' Cargo.toml
```

- [ ] **Step 5: 建两个 `lib.rs` 占位**

`ecat-orm-derive/src/lib.rs`：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `ecat-orm` 的派生宏。请勿直接依赖本 crate —— 经由 `ecat-orm` 重导出使用。

use proc_macro::TokenStream;

/// 为结构体生成 [`ecat_orm::Entity`] 实现与实体元数据。
#[proc_macro_derive(Entity, attributes(entity))]
pub fn derive_entity(_input: TokenStream) -> TokenStream {
    TokenStream::new() // Task 5 实现
}
```

`ecat-orm/src/lib.rs`：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `ecat-orm` —— e-cat 的 ORM。见 `docs/api.md` 的 ORM 段。

pub use ecat_orm_derive::Entity;
```

- [ ] **Step 6: 确认能编译**

```bash
cd /home/wwwroot/e-cat
cargo check -p ecat-orm -p ecat-orm-derive; echo "rc=$?"
```

期望：`rc=0`。

- [ ] **Step 7: 提交**

```bash
git add Cargo.toml Cargo.lock ecat-orm ecat-orm-derive
git commit -m "feat(ecat-orm): 两个 crate 的骨架与依赖"
```

### ⚠️ Task 1 必读：`ecat-orm-derive` 占位宏为何返回空 `TokenStream`

`proc_macro_derive` 返回空流**不是错误** —— 宏展开为「什么都不加」，用它的代码会因为找不到 `Entity` impl 而编译失败（这正是我们想要的：Task 5 之前没人能用它）。

**不要**在 Task 1 里就写 `compile_error!("unimplemented")` —— 那会让 Task 1 的 `cargo check` 一用到这个宏就炸，而 Task 1 的目标只是骨架可编译。

---

## Task 2: `OrmError`

**Files:**
- Create: `ecat-orm/src/error.rs`
- Modify: `ecat-orm/src/lib.rs`（加 `mod error; pub use error::OrmError;`）

- [ ] **Step 1: 写失败测试**

`ecat-orm/src/error.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_column_names_the_column() {
        let e = OrmError::UnknownColumn("naem".into());
        assert!(e.to_string().contains("naem"), "got: {e}");
    }

    #[test]
    fn rdbms_error_converts_via_from() {
        let e: OrmError = ecat_data::RdbmsError::Database("boom".into()).into();
        assert!(matches!(e, OrmError::Rdbms(_)));
        assert!(e.to_string().contains("boom"), "got: {e}");
    }

    #[test]
    fn optimistic_lock_conflict_is_distinguishable() {
        // 调用方必须能靠 matches! 分辨「乐观锁冲突」与一般错误，
        // 否则只能靠字符串匹配 —— 那是脆的。
        let e = OrmError::OptimisticLockConflict;
        assert!(matches!(e, OrmError::OptimisticLockConflict));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

期望：编译失败（`OrmError` 未定义）。

- [ ] **Step 3: 实现**

`ecat-orm/src/error.rs`（测试模块之前）：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

/// `ecat-orm` 的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum OrmError {
    /// 后端返回的错误，原样透传（含超时、连接、语法错误）。
    #[error(transparent)]
    Rdbms(#[from] ecat_data::RdbmsError),

    /// 列名/表名不在实体元数据里。**这是安全边界**：白名单未命中的标识符
    /// 绝不拼进 SQL，见 spec §5.5(a)。
    #[error("unknown column or table: {0}")]
    UnknownColumn(String),

    /// 乐观锁冲突：`UPDATE ... WHERE id = ? AND version = ?` 影响了 0 行。
    #[error("optimistic lock conflict")]
    OptimisticLockConflict,

    /// 迁移不可逆（未提供反向 SQL）。
    #[error("migration is not reversible: {0}")]
    MigrationIrreversible(String),

    /// 元数据与数据不匹配（如 `from_row` 拿到 NULL 但字段非 `Option`）。
    #[error("column `{column}` is NULL but the field is not optional")]
    UnexpectedNull { column: &'static str },

    /// 值无法转换为目标 Rust 类型。
    #[error("column `{column}`: cannot convert value to {expected}")]
    TypeMismatch {
        column: &'static str,
        expected: &'static str,
    },

    /// 迁移失败。携带版本号便于定位是哪一个。
    #[error("migration {version} ({name}) failed: {source}")]
    Migration {
        version: i64,
        name: String,
        #[source]
        source: Box<OrmError>,
    },
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

期望：3 passed。若 `Migration` 变体的 `#[source] Box<OrmError>` 触发 clippy 的 `result_large_err`，**先别急着 Box 化整个 `Result`** —— 记录实测大小再决定。

- [ ] **Step 5: 提交**

```bash
git add ecat-orm/src/error.rs ecat-orm/src/lib.rs
git commit -m "feat(ecat-orm): OrmError"
```

---

## Task 3: 实体元数据（`entity.rs`）

**Files:**
- Create: `ecat-orm/src/entity.rs`
- Modify: `ecat-orm/src/lib.rs`

- [ ] **Step 1: 写失败测试**

`ecat-orm/src/entity.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 元数据必须是 const 可构造的 —— 这是 `EntityMeta` 用 `&'static [..]`
    /// 而非 `Vec` 的**唯一理由**。这个测试在编译期就把它钉死：
    /// 一旦有人把 columns 改回 Vec，本测试无法编译。
    static COLS: [ColumnMeta; 2] = [
        ColumnMeta { name: "id", ty: ColType::I64, nullable: false, pk: true, auto_increment: true },
        ColumnMeta { name: "name", ty: ColType::Text, nullable: false, pk: false, auto_increment: false },
    ];
    static META: EntityMeta = EntityMeta {
        table: "users",
        pk: "id",
        columns: &COLS,
        relations: &[],
        flags: EntityFlags { created_at: None, updated_at: None, soft_delete: None, version: None },
    };

    #[test]
    fn meta_is_const_constructible() {
        assert_eq!(META.table, "users");
        assert_eq!(META.columns.len(), 2);
    }

    #[test]
    fn column_lookup_hits_and_misses() {
        assert_eq!(META.column("name").map(|c| c.ty), Some(ColType::Text));
        assert!(META.column("nope").is_none());
    }

    #[test]
    fn pk_column_is_marked() {
        let pk = META.column(META.pk).expect("pk column must exist");
        assert!(pk.pk);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

期望：编译失败（类型未定义）。

- [ ] **Step 3: 实现**

`ecat-orm/src/entity.rs`：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use ecat_data::Row;

use crate::error::OrmError;

/// 列的存储类型。方言层据此映射 DDL 类型名（见 `dialect::DialectSpec::col_type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColType {
    I64,
    I32,
    F64,
    Bool,
    /// 变长文本。DDL 上 SQL Server 用 `NVARCHAR(MAX)`，其余 `TEXT`。
    Text,
    /// 二进制，`Row` 内以 base64 字符串呈现。
    Bytes,
    /// 时间戳，`Row` 内以 **RFC3339 UTC** 字符串呈现（spec §6 时间类型策略）。
    Timestamp,
    /// 纯日期，`Row` 内以 **`YYYY-MM-DD`** 呈现。**不补 UTC 午夜** —— 源数据里
    /// 没有时刻、没有时区，补出来就是凭空断言（spec:638-642）。
    Date,
    /// JSON 文本，按文本处理（spec §10 非目标：不做类型化映射）。
    Json,
}

/// 单列元数据。`Copy` + 全 `&'static`，因此可放进 `static` 数组。
#[derive(Debug, Clone, Copy)]
pub struct ColumnMeta {
    pub name: &'static str,
    pub ty: ColType,
    pub nullable: bool,
    pub pk: bool,
    pub auto_increment: bool,
}

/// 实体的自动行为标志位。值为**列名**（不是布尔）—— 标记「哪一列承担这个职责」。
#[derive(Debug, Clone, Copy)]
pub struct EntityFlags {
    pub created_at: Option<&'static str>,
    pub updated_at: Option<&'static str>,
    pub soft_delete: Option<&'static str>,
    pub version: Option<&'static str>,
}

impl EntityFlags {
    /// 无任何自动行为的空标志位。
    pub const NONE: Self = Self {
        created_at: None,
        updated_at: None,
        soft_delete: None,
        version: None,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    HasMany,
    HasOne,
    BelongsTo,
}

/// 一条关联的元数据。
///
/// - `HasMany` / `HasOne`：本表 `local_key` → 目标表 `foreign_key`
/// - `BelongsTo`：本表 `foreign_key` → 目标表 `local_key`
#[derive(Debug, Clone, Copy)]
pub struct RelationMeta {
    /// 关系名，与 derive 生成的 `XxxRelation` 枚举变体一一对应（如 `"posts"`）。
    pub name: &'static str,
    pub kind: RelationKind,
    pub target_table: &'static str,
    pub foreign_key: &'static str,
    pub local_key: &'static str,
}

/// 实体元数据。**必须 const 可构造** —— 因此列/关联是 `&'static [..]`，不是 `Vec`。
#[derive(Debug)]
pub struct EntityMeta {
    pub table: &'static str,
    pub pk: &'static str,
    pub columns: &'static [ColumnMeta],
    pub relations: &'static [RelationMeta],
    pub flags: EntityFlags,
}

impl EntityMeta {
    /// 按列名查元数据。查询构建器的**标识符白名单**就是靠它实现的（spec §5.5a）。
    pub fn column(&self, name: &str) -> Option<&'static ColumnMeta> {
        // 返回 &'static：columns 本身是 'static 切片，元素可安全提升生命周期。
        self.columns.iter().find(|c| c.name == name)
    }

    /// 非主键、且非自增的列 —— INSERT 时要写的列。
    pub fn insertable_columns(&self) -> impl Iterator<Item = &'static ColumnMeta> + '_ {
        self.columns
            .iter()
            .filter(|c| !(c.pk && c.auto_increment))
            .copied()
    }

    /// 非主键列 —— UPDATE 时要写的列。
    pub fn updatable_columns(&self) -> impl Iterator<Item = &'static ColumnMeta> + '_ {
        self.columns.iter().filter(|c| !c.pk).copied()
    }

    /// 按关系名查关联元数据。
    pub fn relation(&self, name: &str) -> Option<&'static RelationMeta> {
        self.relations.iter().find(|r| r.name == name)
    }
}

/// 一个可持久化的实体。
///
/// 由 `#[derive(Entity)]` 生成实现；手写实现也被支持（元数据用 `const` 静态量）。
pub trait Entity: Sized + Send + Sync {
    const TABLE: &'static str;
    const PK: &'static str;
    const META: &'static EntityMeta;

    /// 从一行构造。列缺失与 NULL 是**两种不同的错误/行为** ——
    /// `Option<T>` 字段接受 NULL，非 `Option` 字段遇到 NULL 必须报
    /// [`OrmError::UnexpectedNull`]，不得静默取默认值。
    fn from_row(row: &Row) -> Result<Self, OrmError>;

    /// 全部列（含主键）的「列名 → 值」。**按 `META.columns` 顺序**。
    /// insert 会自行剔除自增主键，update 会自行剔除主键 —— 本方法不做过滤，
    /// 这样调用方与测试都能看到完整快照。
    fn to_values(&self) -> Vec<(&'static str, serde_json::Value)>;

    /// 主键值。
    fn pk_value(&self) -> serde_json::Value;
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

期望：6 passed（Task 2 的 3 个 + 本任务 3 个）。

- [ ] **Step 5: 提交**

```bash
git add ecat-orm/src/entity.rs ecat-orm/src/lib.rs
git commit -m "feat(ecat-orm): 实体元数据与 Entity trait"
```

### ⚠️ Task 3 必读：为什么 `column()` 能返回 `&'static`

`self.columns` 的类型是 `&'static [ColumnMeta]`，对它 `.iter()` 得到的 `&ColumnMeta` 生命周期**已经**是 `'static`。所以 `.find(...)` 的结果是 `Option<&'static ColumnMeta>`，不需要 `unsafe`、也不需要把 `&self` 标成 `'static`。

**这里最容易犯的错**是给 `fn column(&self)` 加 `-> Option<&ColumnMeta>`（省略生命周期会绑到 `&self`），导致调用方拿到的引用活不过 `self`。实施时若编译器报生命周期错，检查是不是漏了 `'static`。

`insertable_columns` / `updatable_columns` 返回 `impl Iterator<...> + '_` 而不是 `+ 'static`：虽然元素是 `'static`，但迭代器借用 `&self`。**不要**为了「更严格」把它写成 `+ 'static` —— 那需要 `self: &'static Self`，用不了。

---

## Task 4: 时间转换（`time.rs`）

**Files:**
- Create: `ecat-orm/src/time.rs`
- Modify: `ecat-orm/src/lib.rs`
- Modify: `Cargo.toml`（根：给 workspace 的 `time` 加 `macros` feature）

**本任务解决 spec 的两条硬约束：**

1. **写入路径一律归一化为 UTC**（spec:529-530）。带 `+08:00` 的值必须先转 UTC 再落库 —— SQLite/MySQL 侧存的是 RFC3339 文本，带偏移量的字符串比较会错乱。
2. **纯日期不补时刻**（spec:638-642）。`Date` → `"2026-10-05"`，**不是** `"2026-10-05T00:00:00Z"`。

- [ ] **Step 1: 给 workspace 的 `time` 加 `macros` feature**

根 `Cargo.toml` 当前是（第 101 行）：

```toml
time = { version = "0.3", features = ["formatting", "parsing"] }
```

改成：

```toml
time = { version = "0.3", features = ["formatting", "parsing", "macros"] }
```

**为什么需要**：解析纯日期要 `time::macros::format_description!` 来构造格式描述符，该宏在 `macros` feature 下。本工作区的 `ecat-data-s3` 已经在用 `features = ["formatting", "macros"]`（`ecat-data-s3/Cargo.toml:22`），feature 是现成验证过的。

**为什么不用手写解析代替**：手写 `split('-')` + 逐段 `parse` 要自己处理补零、范围校验、月份枚举转换，是**更多**可能写错的代码。用现成宏是更短的路径。

- [ ] **Step 2: 写失败测试**

`ecat-orm/src/time.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn timestamp_is_normalized_to_utc() {
        // +08:00 的 12:00 == UTC 的 04:00。**写库前必须转 UTC**，
        // 否则不同偏移写入的同一时刻在文本比较下不相等（spec:529-530）。
        let t = datetime!(2026-10-05 12:00:00 +8);
        assert_eq!(to_rfc3339_utc(t), "2026-10-05T04:00:00Z");
    }

    #[test]
    fn rfc3339_roundtrip_preserves_instant() {
        let t = datetime!(2026-10-05 04:00:00 UTC);
        let s = to_rfc3339_utc(t);
        assert_eq!(from_rfc3339(&s).unwrap(), t);
    }

    #[test]
    fn from_rfc3339_accepts_offset_and_converts() {
        let t = from_rfc3339("2026-10-05T12:00:00+08:00").unwrap();
        assert_eq!(t, datetime!(2026-10-05 04:00:00 UTC));
    }

    /// 纯日期**不补时刻、不补时区** —— 源数据里没有这些信息（spec:638-642）。
    #[test]
    fn date_is_plain_without_time_or_zone() {
        let d = time::macros::date!(2026 - 10 - 05);
        assert_eq!(to_date_string(d), "2026-10-05");
        assert!(!to_date_string(d).contains('T'));
        assert!(!to_date_string(d).contains('Z'));
    }

    #[test]
    fn date_roundtrip() {
        let d = time::macros::date!(2026 - 10 - 05);
        assert_eq!(from_date_string(&to_date_string(d)).unwrap(), d);
    }

    /// 回归：`OffsetDateTime` 的 `Display` **不是** RFC3339（批次 1 踩过）。
    /// 这个测试钉死我们走的是 `.format(&Rfc3339)` 而不是 `to_string()`。
    #[test]
    fn output_is_not_the_display_impl() {
        let t = datetime!(2026-10-05 04:00:00 UTC);
        assert_ne!(to_rfc3339_utc(t), t.to_string());
        assert!(to_rfc3339_utc(t).ends_with('Z'));
    }

    #[test]
    fn malformed_input_errors_instead_of_guessing() {
        assert!(from_rfc3339("not a date").is_err());
        assert!(from_date_string("2026-10-05T00:00:00Z").is_err()); // 日期列不该拿到时间戳
        assert!(from_date_string("").is_err());
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

期望：编译失败（函数未定义）。

- [ ] **Step 4: 实现**

`ecat-orm/src/time.rs`（测试模块之前）：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `Row` 内的时间值 ↔ Rust 时间类型。契约按**语义**分两种形态，不是按驱动分
//! （spec §6 时间类型策略）：
//!
//! | 语义 | `Row` 内呈现 | Rust 类型 |
//! |---|---|---|
//! | 时间戳 | RFC3339 UTC（`2026-10-05T12:34:56Z`） | [`time::OffsetDateTime`] |
//! | 纯日期 | `YYYY-MM-DD`（`2026-10-05`） | [`time::Date`] |
//!
//! 两者都是 ISO 8601；RFC3339 是时间戳子集，纯日期不在其内。

use time::OffsetDateTime;
use time::UtcOffset;
use time::format_description::FormatItem;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;

use crate::error::OrmError;

/// `YYYY-MM-DD`。解析与格式化共用同一描述符，避免两边补零规则不一致。
const DATE_FMT: &[FormatItem<'static>] = format_description!("[year]-[month]-[day]");

/// 转 RFC3339 **并归一化到 UTC**。
///
/// # Panics
///
/// 仅当年份落在 RFC3339 可表示的 `0000..=9999` 之外时 panic。四个受支持后端的
/// 日期范围都在此界内（MySQL `TIMESTAMP` 是 1970–2038；SQL Server `DATETIME2`
/// 是 0001–9999；SQLite/PG 实际使用同样远窄于 ±9999）。这个界写在签名里而不是
/// 悄悄取模或截断 —— 越界说明数据本身已经坏了。
pub fn to_rfc3339_utc(t: OffsetDateTime) -> String {
    t.to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .expect("year outside RFC3339's 0000..=9999 range")
}

/// 解析 RFC3339（接受带偏移量的输入，内部转 UTC）。
///
/// 报错信息里带上**原始输入**：只报「parse failed」无法定位是哪个值坏了。
pub fn from_rfc3339(s: &str) -> Result<OffsetDateTime, OrmError> {
    OffsetDateTime::parse(s, &Rfc3339)
        .map(|t| t.to_offset(UtcOffset::UTC))
        .map_err(|e| {
            OrmError::Rdbms(ecat_data::RdbmsError::Database(format!(
                "invalid RFC3339 timestamp `{s}`: {e}"
            )))
        })
}

/// `Date` → `YYYY-MM-DD`。**不补时刻、不补时区。**
pub fn to_date_string(d: time::Date) -> String {
    d.format(DATE_FMT).expect("DATE_FMT is a valid format description")
}

/// `YYYY-MM-DD` → `Date`。
pub fn from_date_string(s: &str) -> Result<time::Date, OrmError> {
    time::Date::parse(s, DATE_FMT).map_err(|e| {
        OrmError::Rdbms(ecat_data::RdbmsError::Database(format!(
            "invalid date `{s}` (expected YYYY-MM-DD): {e}"
        )))
    })
}
```

（`DATE_FMT` 用 `[year]-[month]-[day]`：**格式化时** `time` 默认零填充，输出恒为 `2026-10-05` 而非 `2026-1-5`；**解析时**兼容不补零写法。输出格式是 spec 要求的，输入宽松无害。）

- [ ] **Step 5: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

期望：13 passed（Task 2/3 的 6 个 + 本任务 7 个）。

- [ ] **Step 6: 提交**

```bash
git add Cargo.toml Cargo.lock ecat-orm/src/time.rs ecat-orm/src/lib.rs
git commit -m "feat(ecat-orm): 时间双向转换（时间戳归一化 UTC，纯日期不补时刻）"
```

---

## Task 5: 值转换（`value.rs`）

**Files:**
- Create: `ecat-orm/src/value.rs`
- Modify: `ecat-orm/src/lib.rs`
- Modify: `ecat-orm/Cargo.toml`（加 `base64 = "0.22"`）

**为什么要有这一层**：派生宏要为每个字段生成「转成 `serde_json::Value`」和「从 `Value` 转回」两段代码。**不能**用 `serde_json::to_value(&self.field)` 一把梭 —— `OffsetDateTime` 与 `Date` 在 `time` 的 serde 实现下会序列化成 **`[year, ordinal, hour, ...]` 数组**，不是我们要的 RFC3339/`YYYY-MM-DD` 字符串。`Vec<u8>` 也会变成数字数组。所以必须逐类型显式转换。

用**一个 trait + 对 `Option<T>` 的 blanket impl**，避免「每种类型 × 可空/非可空」的组合爆炸（否则是 9 × 2 = 18 个函数）。

- [ ] **Step 1: 写失败测试**

`ecat-orm/src/value.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ecat_data::Row;
    use serde_json::json;

    fn row(pairs: Vec<(&str, serde_json::Value)>) -> Row {
        let (cols, vals) = pairs.into_iter().map(|(c, v)| (c.to_string(), v)).unzip();
        Row::new(cols, vals)
    }

    #[test]
    fn integers_roundtrip() {
        let r = row(vec![("n", json!(42))]);
        assert_eq!(from_row_col::<i64>(&r, "n").unwrap(), 42);
    }

    #[test]
    fn missing_column_is_an_error_not_a_default() {
        // 列不在结果集里 ≠ 列为 NULL。前者是查询写错了，必须响亮报错。
        let r = row(vec![("other", json!(1))]);
        let e = from_row_col::<i64>(&r, "n").unwrap_err();
        assert!(matches!(e, OrmError::UnknownColumn(_)), "got: {e:?}");
    }

    #[test]
    fn null_into_non_optional_is_an_error() {
        let r = row(vec![("n", json!(null))]);
        let e = from_row_col::<i64>(&r, "n").unwrap_err();
        assert!(matches!(e, OrmError::UnexpectedNull { column: "n" }), "got: {e:?}");
    }

    #[test]
    fn null_into_optional_is_none() {
        let r = row(vec![("n", json!(null))]);
        assert_eq!(from_row_col::<Option<i64>>(&r, "n").unwrap(), None);
    }

    #[test]
    fn optional_some_roundtrips() {
        let r = row(vec![("n", json!(7))]);
        assert_eq!(from_row_col::<Option<i64>>(&r, "n").unwrap(), Some(7));
    }

    /// 时间戳必须是 RFC3339 字符串，不是 `time` crate 的 serde 数组形态。
    #[test]
    fn timestamp_uses_rfc3339_string_not_serde_array() {
        let t = time::macros::datetime!(2026-10-05 04:00:00 UTC);
        assert_eq!(t.to_json(), json!("2026-10-05T04:00:00Z"));
    }

    #[test]
    fn date_uses_plain_string() {
        let d = time::macros::date!(2026 - 10 - 05);
        assert_eq!(d.to_json(), json!("2026-10-05"));
    }

    #[test]
    fn bytes_use_base64_standard() {
        assert_eq!(b"\xff\xfe".to_vec().to_json(), json!("//4="));
    }

    #[test]
    fn bad_type_reports_column_name() {
        let r = row(vec![("n", json!("not a number"))]);
        let e = from_row_col::<i64>(&r, "n").unwrap_err();
        assert!(matches!(e, OrmError::TypeMismatch { column: "n", .. }), "got: {e:?}");
    }

    /// 浮点的 JSON 表示沿用批次 1 的约定：NaN/±Inf 装不进 serde_json::Number，
    /// 转字符串而不是静默变 null（`ecat-data-sqlx/src/cell.rs:9-12` 同款处理）。
    #[test]
    fn non_finite_floats_become_strings() {
        assert_eq!(f64::NAN.to_json(), json!("NaN"));
        assert_eq!(f64::INFINITY.to_json(), json!("inf"));
        assert_eq!(1.5_f64.to_json(), json!(1.5));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

期望：编译失败。

- [ ] **Step 3: 实现**

`ecat-orm/src/value.rs`（测试模块之前）：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 实体字段 ↔ `serde_json::Value`。
//!
//! **不要**用 `serde_json::to_value(&field)` 代替本模块：`time` 的 serde 实现会把
//! `OffsetDateTime` 序列化成 `[year, ordinal, hour, minute, second, nanosecond, offset]`
//! 数组，`Vec<u8>` 会变成数字数组 —— 两者都不是 `Row` 的线格式。

use base64::Engine as _;
use ecat_data::Row;
use serde_json::Value;
use time::OffsetDateTime;

use crate::error::OrmError;
use crate::entity::ColType;
use crate::time::{from_date_string, from_rfc3339, to_date_string, to_rfc3339_utc};

/// 一个能作为实体列存储的 Rust 类型。
pub trait ColumnValue: Sized {
    /// 对应的存储类型，供迁移 DDL 使用。
    const COL_TYPE: ColType;

    fn to_json(&self) -> Value;

    /// `v` 保证不是 `Value::Null`（调用方已拦掉）。
    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError>;
}

impl<T: ColumnValue> ColumnValue for Option<T> {
    const COL_TYPE: ColType = T::COL_TYPE;

    fn to_json(&self) -> Value {
        match self {
            None => Value::Null,
            Some(v) => v.to_json(),
        }
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        if v.is_null() {
            return Ok(None);
        }
        T::from_json(column, v).map(Some)
    }
}

/// 从一行里取一列并转换。
///
/// **三种情形必须可区分**（这是本函数的全部存在理由）：
/// - 列不在结果集里 → [`OrmError::UnknownColumn`]（查询写错了，不是数据问题）
/// - 列在但为 NULL，字段非 `Option` → [`OrmError::UnexpectedNull`]
/// - 列在但为 NULL，字段是 `Option<T>` → `Ok(None)`
pub fn from_row_col<T: ColumnValue>(row: &Row, column: &'static str) -> Result<T, OrmError> {
    match row.get(column) {
        None => Err(OrmError::UnknownColumn(column.into())),
        Some(v) => T::from_json(column, v),
    }
}

/// 类型不符时的统一报错。带上列名与期望类型 —— 否则用户只看到
/// 「expected i64」却不知道是哪一列。
fn mismatch(column: &'static str, expected: &'static str) -> OrmError {
    OrmError::TypeMismatch { column, expected }
}
```

各类型的实现（同文件，紧随其后）：

```rust
macro_rules! int_col {
    ($t:ty, $ct:expr, $expected:expr) => {
        impl ColumnValue for $t {
            const COL_TYPE: ColType = $ct;
            fn to_json(&self) -> Value {
                Value::from(*self)
            }
            fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
                // i32 也接受 Number 里的 i64（SQLite 只存 i64），越界才报错 ——
                // 直接 as 截断会把 70000 静默变成 4464。
                v.as_i64()
                    .and_then(|n| <$t>::try_from(n).ok())
                    .ok_or_else(|| mismatch(column, $expected))
            }
        }
    };
}

int_col!(i64, ColType::I64, "i64");
int_col!(i32, ColType::I32, "i32");

impl ColumnValue for f64 {
    const COL_TYPE: ColType = ColType::F64;

    fn to_json(&self) -> Value {
        if self.is_finite() {
            serde_json::Number::from_f64(*self).map_or(Value::Null, Value::Number)
        } else if self.is_nan() {
            Value::String("NaN".into())
        } else if *self > 0.0 {
            Value::String("inf".into())
        } else {
            Value::String("-inf".into())
        }
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        // 与 to_json 对称：字符串形态的 NaN/±inf 要能转回来。
        match v {
            Value::Number(n) => n.as_f64().ok_or_else(|| mismatch(column, "f64")),
            Value::String(s) => match s.as_str() {
                "NaN" => Ok(f64::NAN),
                "inf" => Ok(f64::INFINITY),
                "-inf" => Ok(f64::NEG_INFINITY),
                _ => Err(mismatch(column, "f64")),
            },
            _ => Err(mismatch(column, "f64")),
        }
    }
}

impl ColumnValue for bool {
    const COL_TYPE: ColType = ColType::Bool;

    fn to_json(&self) -> Value {
        Value::Bool(*self)
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        // 后端对布尔的呈现不一致（SQLite/MySQL 是 1/0，PG 是真布尔，
        // SQL Server 是 BIT=1/0），两种都收。
        match v {
            Value::Bool(b) => Ok(*b),
            Value::Number(n) => n.as_i64().map(|i| i != 0).ok_or_else(|| mismatch(column, "bool")),
            _ => Err(mismatch(column, "bool")),
        }
    }
}

impl ColumnValue for String {
    const COL_TYPE: ColType = ColType::Text;

    fn to_json(&self) -> Value {
        Value::String(self.clone())
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        v.as_str()
            .map(str::to_owned)
            .ok_or_else(|| mismatch(column, "string"))
    }
}

impl ColumnValue for Vec<u8> {
    const COL_TYPE: ColType = ColType::Bytes;

    fn to_json(&self) -> Value {
        Value::String(base64::engine::general_purpose::STANDARD.encode(self))
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        v.as_str()
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
            .ok_or_else(|| mismatch(column, "base64 bytes"))
    }
}

impl ColumnValue for OffsetDateTime {
    const COL_TYPE: ColType = ColType::Timestamp;

    fn to_json(&self) -> Value {
        Value::String(to_rfc3339_utc(*self))
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        v.as_str()
            .ok_or_else(|| mismatch(column, "RFC3339 timestamp string"))
            .and_then(from_rfc3339)
    }
}

impl ColumnValue for time::Date {
    const COL_TYPE: ColType = ColType::Date;

    fn to_json(&self) -> Value {
        Value::String(to_date_string(*self))
    }

    fn from_json(column: &'static str, v: &Value) -> Result<Self, OrmError> {
        v.as_str()
            .ok_or_else(|| mismatch(column, "YYYY-MM-DD date string"))
            .and_then(from_date_string)
    }
}

impl ColumnValue for Value {
    const COL_TYPE: ColType = ColType::Json;

    fn to_json(&self) -> Value {
        self.clone()
    }

    fn from_json(_column: &'static str, v: &Value) -> Result<Self, OrmError> {
        Ok(v.clone())
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

期望：23 passed（此前 13 + 本任务 10）。

- [ ] **Step 5: 提交**

```bash
git add ecat-orm/Cargo.toml ecat-orm/src/value.rs ecat-orm/src/lib.rs
git commit -m "feat(ecat-orm): 值转换层（含 Option 泛型实现，避免组合爆炸）"
```

### ⚠️ Task 5 必读：三个容易写错的地方

1. **`i32` 不能 `as i32` 强转。** SQLite 只有 i64，PG `integer` 也是 i64 上行。`70000 as i32` 是 `4464` —— 静默错值。必须 `try_from` 后报错。

2. **`f64` 的 NaN/±Inf 装不进 `serde_json::Number`。** `Number::from_f64(NaN)` 返回 `None`，傻瓜写法 `.unwrap_or(Value::Null)` 会把 NaN **静默变成 NULL**（写进库就是丢失）。批次 1 在 `ecat-data-sqlx/src/cell.rs:9-12` 已经定过这条约定：转字符串。本任务与它对齐，并且 `from_json` 要能转回来。

3. **`ColumnValue for Value` 的 `to_json` 是克隆。** 这是有意的：`serde_json::Value` 字段按 JSON 原样存取。**不要**优化成 `std::mem::take` —— `to_values(&self)` 取 `&self`，没有可变性可偷。

---

## Task 6: `ecat-orm/src/lib.rs` 的公开面 + `extern crate self`

**Files:**
- Modify: `ecat-orm/src/lib.rs`

**本任务必须先做**：Task 7 的派生宏会生成形如 `::ecat_orm::ColumnMeta{...}` 的代码。这类**绝对路径**在 `ecat-orm` **自己的**测试里解析不了 —— 一个 crate 不能通过自身名字引用自己（没有对应的 `extern crate` 绑定）。这是写派生宏时的经典坑，先解掉再写宏。

- [ ] **Step 1: 写 `ecat-orm/src/lib.rs`**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `ecat-orm` —— e-cat 的 ORM。实体宏、查询构建器、CRUD、关联预加载、迁移。
//!
//! 所有数据操作取 `&impl SqlExecutor`，因此**客户端与 `Transaction` 通吃**：
//!
//! ```ignore
//! User::insert(&db, &user).await?;      // db: SqlxClient
//! let tx = db.transaction().await?;
//! User::insert(&tx, &user).await?;      // tx: Transaction —— 同一个 API
//! tx.commit().await?;
//! ```
//!
//! 用法见 `docs/api.md` 的 ORM 段。

// 让派生宏生成的绝对路径 `::ecat_orm::…` 在本 crate 内部（含测试）也能解析。
// 派生宏无法知道用户在哪个 crate，只能生成绝对路径；没有这一行，本 crate
// 自己的 `#[derive(Entity)]` 测试会报 "use of undeclared crate or module"。
// serde 用同样的手法（`extern crate self as serde;`）。
extern crate self as ecat_orm;

mod entity;
mod error;
mod time;
pub mod value;

pub use ecat_data::{Row, SqlExecutor};
pub use ecat_orm_derive::Entity;
pub use entity::{ColType, ColumnMeta, EntityFlags, EntityMeta, RelationKind, RelationMeta};
pub use error::OrmError;

/// 重导出 `serde_json`：派生宏生成的代码要写 `::ecat_orm::serde_json::Value`。
/// 不重导的话，每个用户 crate 都得自己把 `serde_json` 加进依赖才能用 ORM —— 而
/// 它其实只是 ORM 公开签名里的类型（`to_values` 的返回类型）。
pub use serde_json;

/// 实体 trait。与派生宏同名，`use ecat_orm::{Entity, ...}` 一次拿到两者。
pub use entity::Entity;
```

- [ ] **Step 2: 确认能编译且既有测试仍通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

期望：23 passed（Task 5 结束时的数字，本任务不新增测试）。

- [ ] **Step 3: 提交**

```bash
git add ecat-orm/src/lib.rs
git commit -m "feat(ecat-orm): 公开面与 extern crate self（供派生宏的绝对路径解析）"
```

### ⚠️ Task 6 必读：`pub use entity::Entity` 会不会和 `pub use ecat_orm_derive::Entity` 撞名？

**不会。** 两者在**不同的命名空间**里：`entity::Entity` 是 **trait**（trait 命名空间），`ecat_orm_derive::Entity` 是 **derive 宏**（macro 命名空间）。同一个名字在两处 `pub use` 是合法的，且这正是用户想要的 —— `use ecat_orm::Entity;` 之后既能 `impl Entity for X` 又能 `#[derive(Entity)]`。

若编译器报 `E0255`（the name `Entity` is defined multiple times），说明你把其中一个写进了 `pub mod Entity` 之类的**类型命名空间**，检查是不是写错了行。

---

## Task 7: 派生宏 —— 容器属性与列字段（`ecat-orm-derive`）

**Files:**
- Create: `ecat-orm-derive/src/attr.rs`
- Create: `ecat-orm-derive/src/expand.rs`
- Modify: `ecat-orm-derive/src/lib.rs`
- Create: `ecat-orm/tests/derive_columns.rs`

**本任务只做列字段**（表名、列、主键、标志位、`from_row`、`to_values`、`pk_value`）。
关联字段（`has_many` / `has_one` / `belongs_to`）在 Task 8 —— 本任务遇到它们时**必须响亮报错**，不能静默当成列（那会生成 `Vec<Post>` 的列元数据，错误信息极难懂）。

### 属性文法（规范，Task 8 会扩展）

容器：`#[entity(table = "users")]`，可省略 —— 省略时表名取结构体名的 `snake_case`（`User` → `user`，`UserProfile` → `user_profile`）。

字段：

| 写法 | 含义 |
|---|---|
| `#[entity(column = "user_name")]` | 列名覆盖，默认取字段名 |
| `#[entity(pk)]` | 主键 |
| `#[entity(auto_increment)]` | 自增（隐含 pk） |
| `#[entity(created_at)]` / `updated_at` / `soft_delete` / `version` | 自动行为标志位 |

**类型映射不在本 crate 里做。** 宏生成的 `ColumnMeta.ty` 是
`<#field_ty as ::ecat_orm::value::ColumnValue>::COL_TYPE` ——
类型与 `ColType` 的对应关系**只存在于 `value.rs` 的 impl 里**，宏不复制一份映射表。
好处：加一种新列类型只需实现 `ColumnValue` 一次，宏自动跟上，不会两边漂移。
不认识的类型会得到指向该字段的 `T: ColumnValue` 未满足错误 —— 比宏自己抛
「unsupported type」更能说明缺什么。

- [ ] **Step 1: 写失败测试**

`ecat-orm/tests/derive_columns.rs`：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `#[derive(Entity)]` 的列字段路径（不含关联，关联见 derive_relations.rs）。

use ecat_orm::{ColType, Entity, OrmError, Row};
use time::OffsetDateTime;

#[derive(Entity, Debug, PartialEq)]
#[entity(table = "users")]
pub struct User {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
    pub score: f64,
    pub active: bool,
    #[entity(created_at)]
    pub created_at: Option<OffsetDateTime>,
    #[entity(version)]
    pub version: i64,
}

/// 表名省略时取结构体名的 snake_case。
#[derive(Entity)]
pub struct UserProfile {
    #[entity(pk)]
    pub id: i64,
}

fn row(cols: &[&str], vals: Vec<serde_json::Value>) -> Row {
    Row::new(cols.iter().map(|s| s.to_string()).collect(), vals)
}

#[test]
fn table_name_from_attribute() {
    assert_eq!(User::TABLE, "users");
    assert_eq!(User::PK, "id");
}

#[test]
fn table_name_defaults_to_snake_case_of_struct() {
    assert_eq!(UserProfile::TABLE, "user_profile");
}

#[test]
fn columns_are_declared_in_field_order() {
    let names: Vec<_> = User::META.columns.iter().map(|c| c.name).collect();
    assert_eq!(
        names,
        vec!["id", "name", "email", "score", "active", "created_at", "version"]
    );
}

#[test]
fn column_types_come_from_the_value_impls() {
    let ty = |n: &str| User::META.column(n).unwrap().ty;
    assert_eq!(ty("id"), ColType::I64);
    assert_eq!(ty("name"), ColType::Text);
    assert_eq!(ty("email"), ColType::Text); // Option<String> → String 的列类型
    assert_eq!(ty("score"), ColType::F64);
    assert_eq!(ty("active"), ColType::Bool);
    assert_eq!(ty("created_at"), ColType::Timestamp);
}

#[test]
fn nullability_follows_the_option_wrapper() {
    let nullable = |n: &str| User::META.column(n).unwrap().nullable;
    assert!(!nullable("name"));
    assert!(nullable("email"));
    assert!(nullable("created_at"));
}

#[test]
fn pk_and_auto_increment_flags() {
    let id = User::META.column("id").unwrap();
    assert!(id.pk);
    assert!(id.auto_increment);
    assert!(!User::META.column("name").unwrap().pk);
}

#[test]
fn flags_point_at_the_right_columns() {
    assert_eq!(User::META.flags.created_at, Some("created_at"));
    assert_eq!(User::META.flags.version, Some("version"));
    assert_eq!(User::META.flags.updated_at, None);
    assert_eq!(User::META.flags.soft_delete, None);
}

#[test]
fn insertable_columns_skip_the_auto_increment_pk() {
    let names: Vec<_> = User::META.insertable_columns().map(|c| c.name).collect();
    assert!(!names.contains(&"id"), "自增主键不该出现在 INSERT 里");
    assert!(names.contains(&"name"));
}

#[test]
fn updatable_columns_skip_the_pk() {
    let names: Vec<_> = User::META.updatable_columns().map(|c| c.name).collect();
    assert!(!names.contains(&"id"));
    assert!(names.contains(&"version"));
}

#[test]
fn from_row_builds_the_struct() {
    let r = row(
        &["id", "name", "email", "score", "active", "created_at", "version"],
        vec![
            serde_json::json!(1),
            serde_json::json!("alice"),
            serde_json::json!(null),
            serde_json::json!(9.5),
            serde_json::json!(true),
            serde_json::json!(null),
            serde_json::json!(3),
        ],
    );
    let u = User::from_row(&r).unwrap();
    assert_eq!(u.id, 1);
    assert_eq!(u.name, "alice");
    assert_eq!(u.email, None);
    assert_eq!(u.score, 9.5);
    assert!(u.active);
    assert_eq!(u.version, 3);
}

/// 非 Option 字段遇到 NULL 必须报错，不得静默取默认值。
#[test]
fn from_row_rejects_null_for_non_optional_field() {
    let r = row(
        &["id", "name", "email", "score", "active", "created_at", "version"],
        vec![
            serde_json::json!(1),
            serde_json::json!(null), // name 非 Option
            serde_json::json!(null),
            serde_json::json!(1.0),
            serde_json::json!(false),
            serde_json::json!(null),
            serde_json::json!(0),
        ],
    );
    let e = User::from_row(&r).unwrap_err();
    assert!(
        matches!(e, OrmError::UnexpectedNull { column: "name" }),
        "got: {e:?}"
    );
}

#[test]
fn to_values_returns_every_column_in_meta_order() {
    let u = User {
        id: 1,
        name: "alice".into(),
        email: None,
        score: 9.5,
        active: true,
        created_at: None,
        version: 3,
    };
    let vals = u.to_values();
    let names: Vec<_> = vals.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        names,
        vec!["id", "name", "email", "score", "active", "created_at", "version"]
    );
    assert_eq!(vals[1].1, serde_json::json!("alice"));
    assert_eq!(vals[2].1, serde_json::json!(null));
}

#[test]
fn pk_value_returns_the_pk_field() {
    let u = User {
        id: 42,
        name: "alice".into(),
        email: None,
        score: 0.0,
        active: false,
        created_at: None,
        version: 0,
    };
    assert_eq!(u.pk_value(), serde_json::json!(42));
}

/// `OffsetDateTime` 必须是 RFC3339 字符串 —— 走 `time` 的 serde 会变成数组。
#[test]
fn timestamp_serializes_as_rfc3339() {
    let u = User {
        id: 1,
        name: "a".into(),
        email: None,
        score: 0.0,
        active: false,
        created_at: Some(time::macros::datetime!(2026-10-05 04:00:00 UTC)),
        version: 0,
    };
    let (_, v) = u.to_values().into_iter().find(|(n, _)| *n == "created_at").unwrap();
    assert_eq!(v, serde_json::json!("2026-10-05T04:00:00Z"));
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm --test derive_columns 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
```

期望：编译失败（占位宏展开为空 → 找不到 `Entity` 的实现，也找不到 `META`）。

- [ ] **Step 3: 实现 `ecat-orm-derive/src/attr.rs`**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `#[entity(...)]` 属性解析。

use syn::Attribute;
use syn::Meta;
use syn::Result;

/// 容器级属性（结构体上）。
#[derive(Default)]
pub struct ContainerAttrs {
    pub table: Option<String>,
}

/// 字段级属性。
#[derive(Default)]
pub struct FieldAttrs {
    pub column: Option<String>,
    pub pk: bool,
    pub auto_increment: bool,
    pub created_at: bool,
    pub updated_at: bool,
    pub soft_delete: bool,
    pub version: bool,
    pub relation: Option<Relation>,
}

/// 关联字段（Task 8 使用）。
pub struct Relation {
    pub kind: RelationKind,
    /// 目标实体类型，如 `Post`。生成代码里会写成 `Post::TABLE`，
    /// 因此**该类型必须在 derive 处的作用域内可见**。
    pub target: syn::Path,
    pub foreign_key: String,
    pub local_key: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    HasMany,
    HasOne,
    BelongsTo,
}

impl RelationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HasMany => "has_many",
            Self::HasOne => "has_one",
            Self::BelongsTo => "belongs_to",
        }
    }
}

fn entity_attrs(attrs: &[Attribute]) -> impl Iterator<Item = &Attribute> {
    attrs.iter().filter(|a| a.path().is_ident("entity"))
}

pub fn parse_container(attrs: &[Attribute]) -> Result<ContainerAttrs> {
    let mut out = ContainerAttrs::default();
    for a in entity_attrs(attrs) {
        a.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                out.table = Some(lit.value());
                Ok(())
            } else {
                Err(meta.error(
                    "unknown `#[entity(...)]` on a struct; only `table = \"...\"` is supported here",
                ))
            }
        })?;
    }
    Ok(out)
}

pub fn parse_field(attrs: &[Attribute]) -> Result<FieldAttrs> {
    let mut out = FieldAttrs::default();
    for a in entity_attrs(attrs) {
        a.parse_nested_meta(|meta| {
            let p = &meta.path;
            if p.is_ident("column") {
                out.column = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if p.is_ident("pk") {
                out.pk = true;
            } else if p.is_ident("auto_increment") {
                // 自增必然是主键；显式写出 pk 不报错，但语义上等价。
                out.auto_increment = true;
                out.pk = true;
            } else if p.is_ident("created_at") {
                out.created_at = true;
            } else if p.is_ident("updated_at") {
                out.updated_at = true;
            } else if p.is_ident("soft_delete") {
                out.soft_delete = true;
            } else if p.is_ident("version") {
                out.version = true;
            } else if p.is_ident("has_many") || p.is_ident("has_one") || p.is_ident("belongs_to") {
                let kind = if p.is_ident("has_many") {
                    RelationKind::HasMany
                } else if p.is_ident("has_one") {
                    RelationKind::HasOne
                } else {
                    RelationKind::BelongsTo
                };
                let target_lit: syn::LitStr = meta.value()?.parse()?;
                let target = target_lit.parse::<syn::Path>().map_err(|e| {
                    syn::Error::new(target_lit.span(), format!("expected an entity type path: {e}"))
                })?;
                // foreign_key 是必填的。parse_nested_meta 在这个位置拿不到逗号后的内容，
                // 需要一个内部循环：
                let mut foreign_key = None;
                let mut local_key = None;
                while meta.input.peek(syn::Token![,]) {
                    let _: syn::Token![,] = meta.input.parse()?;
                    let id: syn::Ident = meta.input.parse()?;
                    let _: syn::Token![=] = meta.input.parse()?;
                    let lit: syn::LitStr = meta.input.parse()?;
                    if id == "foreign_key" {
                        foreign_key = Some(lit.value());
                    } else if id == "local_key" {
                        local_key = Some(lit.value());
                    } else {
                        return Err(syn::Error::new(
                            id.span(),
                            "expected `foreign_key` or `local_key`",
                        ));
                    }
                }
                let foreign_key = foreign_key.ok_or_else(|| {
                    syn::Error::new(
                        target_lit.span(),
                        format!("`{}` requires `foreign_key = \"...\"`", kind.as_str()),
                    )
                })?;
                out.relation = Some(Relation {
                    kind,
                    target,
                    foreign_key,
                    local_key,
                });
            } else {
                return Err(meta.error("unknown `#[entity(...)]` attribute on a field"));
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// 结构体名 → snake_case（`UserProfile` → `user_profile`）。
/// 只在表名省略时用作默认值。
pub fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// 字段名 → PascalCase（`posts` → `Posts`，`user_profile` → `UserProfile`）。
/// 供 Task 8 生成 `XxxRelation` 的变体名。
pub fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}
```

- [ ] **Step 4: 实现 `ecat-orm-derive/src/expand.rs`**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;
use syn::Fields;
use syn::Result;
use syn::spanned::Spanned;

use crate::attr::{self, FieldAttrs};

/// 一个列字段的全部生成所需信息。
struct Column {
    ident: syn::Ident,
    ty: syn::Type,
    name: String,
    nullable: bool,
    pk: bool,
    auto_increment: bool,
}

/// `Option<T>` 拆出 `T` 并标记可空；否则原样返回。
fn unwrap_option(ty: &syn::Type) -> (syn::Type, bool) {
    if let syn::Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
        && seg.ident == "Option"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && args.args.len() == 1
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return (inner.clone(), true);
    }
    (ty.clone(), false)
}

pub fn expand(input: DeriveInput) -> Result<TokenStream> {
    let container = attr::parse_container(&input.attrs)?;
    let struct_ident = input.ident.clone();
    let table = container
        .table
        .unwrap_or_else(|| attr::to_snake_case(&struct_ident.to_string()));

    let named = match &input.data {
        syn::Data::Struct(s) => match &s.fields {
            Fields::Named(n) => n,
            _ => {
                return Err(syn::Error::new(
                    input.span(),
                    "Entity requires a struct with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new(
                input.span(),
                "Entity can only be derived for structs",
            ));
        }
    };

    let mut columns: Vec<Column> = Vec::new();
    let mut flags = FlagsAcc::default();
    let mut pk_ident: Option<syn::Ident> = None;
    let mut pk_name: Option<String> = None;

    for field in &named.named {
        let ident = field.ident.clone().expect("named field");
        let fa = attr::parse_field(&field.attrs)?;

        if let Some(rel) = &fa.relation {
            // Task 8 才实现。此处必须响亮报错：静默当作列的话，会去求
            // `Vec<Post>: ColumnValue`，用户看到的是一个和实际问题无关的
            // trait 未满足错误。
            return Err(syn::Error::new(
                field.span(),
                format!(
                    "`{}` is not implemented yet (batch-3 Task 8); \
                     remove it or implement relations first",
                    rel.kind.as_str()
                ),
            ));
        }

        let name = fa.column.clone().unwrap_or_else(|| ident.to_string());
        let (inner_ty, nullable) = unwrap_option(&field.ty);

        if fa.created_at {
            flags.created_at = Some(name.clone());
        }
        if fa.updated_at {
            flags.updated_at = Some(name.clone());
        }
        if fa.soft_delete {
            flags.soft_delete = Some(name.clone());
        }
        if fa.version {
            flags.version = Some(name.clone());
        }
        if fa.pk {
            if pk_ident.is_some() {
                return Err(syn::Error::new(
                    field.span(),
                    "multiple `pk` fields; ecat-orm does not support composite primary keys",
                ));
            }
            pk_ident = Some(ident.clone());
            pk_name = Some(name.clone());
        }

        columns.push(Column {
            ident,
            ty: inner_ty,
            name,
            nullable,
            pk: fa.pk,
            auto_increment: fa.auto_increment,
        });
    }

    let pk_ident = pk_ident.ok_or_else(|| {
        syn::Error::new(
            struct_ident.span(),
            "no `#[entity(pk)]` field; every entity needs a single-column primary key",
        )
    })?;
    let pk_name = pk_name.expect("set alongside pk_ident");

    // 把这些绑定成 Vec<lit>，供 quote 的重复插值使用。
    let col_names: Vec<_> = columns.iter().map(|c| c.name.as_str()).collect();
    let col_tys: Vec<_> = columns.iter().map(|c| &c.ty).collect();
    let col_nullable: Vec<_> = columns.iter().map(|c| c.nullable).collect();
    let col_pk: Vec<_> = columns.iter().map(|c| c.pk).collect();
    let col_ai: Vec<_> = columns.iter().map(|c| c.auto_increment).collect();
    let col_idents: Vec<_> = columns.iter().map(|c| &c.ident).collect();

    let opt_str = |v: &Option<String>| match v {
        Some(s) => quote!(::core::option::Option::Some(#s)),
        None => quote!(::core::option::Option::None),
    };
    let created_at = opt_str(&flags.created_at);
    let updated_at = opt_str(&flags.updated_at);
    let soft_delete = opt_str(&flags.soft_delete);
    let version = opt_str(&flags.version);

    Ok(quote! {
        impl ::ecat_orm::Entity for #struct_ident {
            const TABLE: &'static str = #table;
            const PK: &'static str = #pk_name;
            const META: &'static ::ecat_orm::EntityMeta = &META;

            fn from_row(
                row: &::ecat_orm::Row,
            ) -> ::core::result::Result<Self, ::ecat_orm::OrmError> {
                ::core::result::Result::Ok(Self {
                    #(
                        #col_idents: ::ecat_orm::value::from_row_col::<#col_tys>(row, #col_names)?,
                    )*
                })
            }

            fn to_values(
                &self,
            ) -> ::std::vec::Vec<(&'static str, ::ecat_orm::serde_json::Value)> {
                ::std::vec![
                    #(
                        (
                            #col_names,
                            ::ecat_orm::value::ColumnValue::to_json(&self.#col_idents),
                        ),
                    )*
                ]
            }

            fn pk_value(&self) -> ::ecat_orm::serde_json::Value {
                ::ecat_orm::value::ColumnValue::to_json(&self.#pk_ident)
            }
        }

        // META 是本 impl 之外的一个 static：`impl` 里不能放 `static` 项，
        // 而 `META: &'static EntityMeta` 必须指向一个真正 'static 的值。
        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        static META: ::ecat_orm::EntityMeta = ::ecat_orm::EntityMeta {
            table: #table,
            pk: #pk_name,
            columns: &[
                #(
                    ::ecat_orm::ColumnMeta {
                        name: #col_names,
                        // 列类型由 value.rs 的 ColumnValue impl 提供 —— 宏不复制
                        // 一份类型映射表，避免两处漂移。
                        ty: <#col_tys as ::ecat_orm::value::ColumnValue>::COL_TYPE,
                        nullable: #col_nullable,
                        pk: #col_pk,
                        auto_increment: #col_ai,
                    },
                )*
            ],
            relations: &[],
            flags: ::ecat_orm::EntityFlags {
                created_at: #created_at,
                updated_at: #updated_at,
                soft_delete: #soft_delete,
                version: #version,
            },
        };
    })
}

#[derive(Default)]
struct FlagsAcc {
    created_at: Option<String>,
    updated_at: Option<String>,
    soft_delete: Option<String>,
    version: Option<String>,
}
```

> `quote::format_ident` 在 Task 8 生成枚举变体名时才需要，本任务不要 import —— 项目要求 `-D warnings` 通过，用不到的 import 会直接让闸门变红。

- [ ] **Step 5: 改 `ecat-orm-derive/src/lib.rs`**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `ecat-orm` 的派生宏。请勿直接依赖本 crate —— 经由 `ecat-orm` 重导出使用。

use proc_macro::TokenStream;
use syn::DeriveInput;

mod attr;
mod expand;

/// 为结构体生成 [`ecat_orm::Entity`] 实现与实体元数据。
///
/// 属性文法见 `docs/api.md` 的 ORM 段。
#[proc_macro_derive(Entity, attributes(entity))]
pub fn derive_entity(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand::expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
```

- [ ] **Step 6: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm --test derive_columns 2>&1 | tail -25; echo "rc=${PIPESTATUS[0]}"
```

期望：14 passed。

- [ ] **Step 7: 跑全 crate 测试 + 闸门**

```bash
cd /home/wwwroot/e-cat
cargo test -p ecat-orm -p ecat-orm-derive 2>&1 | tail -10
cargo fmt --check && cargo clippy -p ecat-orm -p ecat-orm-derive --all-targets -- -D warnings; echo "rc=$?"
```

- [ ] **Step 8: 提交**

```bash
git add ecat-orm-derive ecat-orm/tests/derive_columns.rs
git commit -m "feat(ecat-orm-derive): 列字段的 #[derive(Entity)]"
```

### ⚠️ Task 7 必读：`parse_nested_meta` 的逗号行为

`meta.parse_nested_meta` 的闭包**每调用一次只处理一个逗号分隔项**。对 `pk, auto_increment` 这种纯标志位，闭包会被调用两次，各自 `return Ok(())` —— 这就是 `pk` / `auto_increment` 分支不需要自己吃逗号的原因。

但 `has_many = "Post", foreign_key = "user_id"` 是**一个**语法项后面跟着**额外的键值对**。syn 没有为这种情况提供现成 API，所以 Task 8 里那段 `while meta.input.peek(Token![,])` 的内部循环是**必需的**，不是多余代码。

**验证方法**：写完 Task 8 后，故意把 `foreign_key` 写错成 `foreignkey`，应当得到 "expected `foreign_key` or `local_key`" 而不是被静默忽略。这类属性解析最容易出的 bug 就是静默忽略 —— 拼错的键被当成没写，用户配了不生效。

---

## Task 8: 派生宏 —— 关联字段与 `XxxRelation` 枚举

**Files:**
- Modify: `ecat-orm-derive/src/expand.rs`
- Create: `ecat-orm/tests/derive_relations.rs`

**要生成的**（spec §5.2、§5.4）：每个实体一个 `XxxRelation` 枚举，每个关联字段一个变体；`EntityMeta.relations` 填上对应元数据，供 Task 13 的预加载使用。

- [ ] **Step 1: 写失败测试**

`ecat-orm/tests/derive_relations.rs`：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 关联字段的派生（元数据部分；预加载执行见 relation.rs 的测试）。

use ecat_orm::{Entity, RelationKind};

#[derive(Entity)]
#[entity(table = "posts")]
pub struct Post {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub user_id: i64,
    pub title: String,
}

#[derive(Entity)]
#[entity(table = "profiles")]
pub struct Profile {
    #[entity(pk)]
    pub id: i64,
    pub user_id: i64,
    pub bio: String,
}

#[derive(Entity)]
#[entity(table = "comments")]
pub struct Comment {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub post_id: i64,
    pub body: String,
}

#[derive(Entity)]
#[entity(table = "users")]
pub struct User {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub name: String,

    #[entity(has_many = "Post", foreign_key = "user_id")]
    pub posts: Vec<Post>,

    #[entity(has_one = "Profile", foreign_key = "user_id")]
    pub profile: Option<Profile>,

    #[entity(has_many = "Comment", foreign_key = "post_id", local_key = "id")]
    pub comments: Vec<Comment>,
}

#[test]
fn relation_names_match_the_field_names() {
    let names: Vec<_> = User::META.relations.iter().map(|r| r.name).collect();
    assert_eq!(names, vec!["posts", "profile", "comments"]);
}

#[test]
fn relation_kinds_are_captured() {
    let k = |n: &str| User::META.relation(n).unwrap().kind;
    assert_eq!(k("posts"), RelationKind::HasMany);
    assert_eq!(k("profile"), RelationKind::HasOne);
    assert_eq!(k("comments"), RelationKind::HasMany);
}

#[test]
fn has_many_targets_the_right_table_and_fk() {
    let r = User::META.relation("posts").unwrap();
    assert_eq!(r.target_table, "posts");
    assert_eq!(r.foreign_key, "user_id");
    // local_key 省略时取本表主键
    assert_eq!(r.local_key, "id");
}

#[test]
fn explicit_local_key_is_honoured() {
    let r = User::META.relation("comments").unwrap();
    assert_eq!(r.foreign_key, "post_id");
    assert_eq!(r.local_key, "id");
}

#[test]
fn relation_fields_are_not_columns() {
    let names: Vec<_> = User::META.columns.iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["id", "name"], "关联字段不得混进列里");
}

/// 每个实体都生成自己的 `XxxRelation`，变体名是字段名的 PascalCase。
#[test]
fn relation_enum_variants_are_generated() {
    use ecat_orm::relation::RelationSelector;
    assert_eq!(UserRelation::Posts.name(), "posts");
    assert_eq!(UserRelation::Profile.name(), "profile");
    assert_eq!(UserRelation::Comments.name(), "comments");
}

/// 关联目标类型必须在 derive 处可见 —— 这条测试同时证明了生成的代码
/// 引用的是目标类型的 `META` 而不是硬编码字符串。
#[test]
fn target_meta_is_reachable_from_the_relation() {
    assert_eq!(User::META.relation("posts").unwrap().target_table, Post::TABLE);
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm --test derive_relations 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
```

期望：编译失败（`UserRelation` 不存在；且 `has_many` 触发 Task 7 留的错误）。

- [ ] **Step 3: 在 `expand.rs` 里实现关联**

替换 Task 7 里那段 `if let Some(rel) = &fa.relation { … return Err(…) }`，改为把关联收集进 `relations` 向量并在循环外一并生成：

```rust
    let mut relations: Vec<RelationOut> = Vec::new();

    // …循环内，替换原来的 return Err(…)：
        if let Some(rel) = &fa.relation {
            let target = rel.target.clone();
            let target_str = quote!(#target).to_string();
            // HasMany/HasOne 的 local_key 省略时为本表主键；BelongsTo 为目标表主键。
            let local_key = match (&rel.local_key, rel.kind) {
                (Some(k), _) => quote!(#k),
                (None, attr::RelationKind::BelongsTo) => {
                    quote!(<#target as ::ecat_orm::Entity>::PK)
                }
                (None, _) => quote!(<#struct_ident as ::ecat_orm::Entity>::PK),
            };
            relations.push(RelationOut {
                variant: format_ident!("{}", attr::to_pascal_case(&ident.to_string())),
                name: ident.to_string(),
                kind: rel.kind,
                target,
                target_str,
                foreign_key: rel.foreign_key.clone(),
                local_key,
            });
            continue; // 关联字段不是列
        }
```

并新增：

```rust
struct RelationOut {
    variant: syn::Ident,
    name: String,
    kind: attr::RelationKind,
    target: syn::Path,
    target_str: String,
    foreign_key: String,
    local_key: TokenStream,
}
```

生成部分（加在 `Ok(quote! { … })` 里，`impl Entity` 之后）：

```rust
    let rel_variants: Vec<_> = relations.iter().map(|r| &r.variant).collect();
    let rel_names: Vec<_> = relations.iter().map(|r| r.name.as_str()).collect();
    let rel_kinds: Vec<_> = relations.iter().map(|r| match r.kind {
        attr::RelationKind::HasMany => quote!(::ecat_orm::RelationKind::HasMany),
        attr::RelationKind::HasOne => quote!(::ecat_orm::RelationKind::HasOne),
        attr::RelationKind::BelongsTo => quote!(::ecat_orm::RelationKind::BelongsTo),
    }).collect();
    let rel_targets: Vec<_> = relations.iter().map(|r| &r.target).collect();
    let rel_fks: Vec<_> = relations.iter().map(|r| r.foreign_key.as_str()).collect();
    let rel_lks: Vec<_> = relations.iter().map(|r| &r.local_key).collect();
    let rel_enum = format_ident!("{}Relation", struct_ident);
```

`relations: &[]` 换成：

```rust
            relations: &[
                #(
                    ::ecat_orm::RelationMeta {
                        name: #rel_names,
                        kind: #rel_kinds,
                        target_table: <#rel_targets as ::ecat_orm::Entity>::TABLE,
                        foreign_key: #rel_fks,
                        local_key: #rel_lks,
                    },
                )*
            ],
```

并追加枚举本身：

```rust
        /// 由 `#[derive(Entity)]` 生成的关联选择器。
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum #rel_enum {
            #( #rel_variants, )*
        }

        impl #rel_enum {
            /// 关联名，与 `EntityMeta::relations` 里的 `name` 对应。
            pub fn name(self) -> &'static str {
                match self {
                    #( Self::#rel_variants => #rel_names, )*
                }
            }
        }

        impl ::ecat_orm::relation::RelationSelector for #rel_enum {
            fn name(self) -> &'static str {
                self.name()
            }
        }
```

### ⚠️ Task 8 补充（必做）：`from_row` 必须也初始化**关联字段**

Task 7 生成的 `from_row` 是：

```rust
Ok(Self { #( #col_idents: ..., )* })
```

结构体字面量**必须列出全部字段**。实体里一旦有关联字段（`posts: Vec<Post>`），
这段就缺字段、直接编译失败。

所以 Task 8 还要：收集关联字段的**默认值表达式**，一并放进 `from_row` 的字段列表。
按容器类型选默认值（语法层面判定，不需要知道 `T` 是什么）：

| 字段类型 | 默认值 |
|---|---|
| `Vec<T>` | `::std::vec::Vec::new()` |
| `Option<T>` | `::core::option::Option::None()` |
| 其它 | **报错** —— 关联字段只能是这两个容器之一 |

在 `RelationOut` 里加一个 `default: TokenStream` 字段：

```rust
/// 关联字段在「未预加载」状态下的值。
/// `Vec<T>` → 空向量，`Option<T>` → None。其它类型直接报错 ——
/// 一个裸的 `Post` 字段无法表达「没查到」，静默给它一个默认实体是错的。
fn relation_default(ty: &syn::Type, span: proc_macro2::Span) -> Result<TokenStream> {
    if let syn::Type::Path(tp) = ty {
        if let Some(seg) = tp.path.segments.last() {
            if seg.ident == "Vec" {
                return Ok(quote!(::std::vec::Vec::new()));
            }
            if seg.ident == "Option" {
                return Ok(quote!(::core::option::Option::None()));
            }
        }
    }
    Err(syn::Error::new(
        span,
        "a relation field must be `Vec<Target>` (has_many) or `Option<Target>` (has_one)",
    ))
}
```

`from_row` 的生成改成把「列字段」与「关联字段+默认值」两组合并：

```rust
Ok(Self {
    #( #col_idents: ::ecat_orm::value::from_row_col::<#col_tys>(row, #col_names)?, )*
    #( #rel_field_idents: #rel_defaults, )*
})
```

**测试**（追加到 `derive_relations.rs`）：

```rust
/// 未预加载时关联字段是空的 —— 「没查」与「查了没有」在批次 3 里不可区分，
/// 所以文档要写明：要区分就必须显式 `with()`。
#[test]
fn from_row_leaves_relations_empty_until_loaded() {
    let r = Row::new(
        vec!["id".into(), "name".into()],
        vec![serde_json::json!(1), serde_json::json!("alice")],
    );
    let u = User::from_row(&r).unwrap();
    assert!(u.posts.is_empty());
    assert!(u.profile.is_none());
    assert!(u.comments.is_empty());
}

/// 裸实体类型（非 Vec / 非 Option）的关联字段必须在**编译期**被拒。
///
/// ```compile_fail
/// #[derive(ecat_orm::Entity)]
/// struct Bad {
///     #[entity(pk)] id: i64,
///     #[entity(has_one = "Other", foreign_key = "bad_id")] other: Other,
/// }
/// ```
#[test]
fn bare_relation_field_is_rejected() {
    // 真正的断言是上面的 compile_fail doctest —— 裸关联字段无法表达「没查到」。
}
```

> **为什么裸类型要报错而不是给它 `Default::default()`**：`Option` 能表达「没有」，
> `Vec` 能表达「空」 —— 而裸的 `Post` 字段**必须**有一个值。给它一个默认实例
> 等于凭空造了一条不存在的数据，正是本项目一路在防的「静默伪造」。**要么用
> `Option`，要么用 `Vec`，没有第三条路。**

### ⚠️ Task 8 补充（必做二）：生成 `set_relation` —— 关联预加载的写回口

Task 16 的预加载拿到目标表的 `Vec<Row>` 后，必须把它们写回主体的关联字段。
`Entity` trait **无法泛型地做到这件事** —— 它不知道「`posts` 这个关联对应哪个字段、
目标类型是什么」。只有派生宏知道。

给 `Entity` trait 加：

```rust
    /// 关联预加载的写回口。由派生宏按关联名分派到具体字段。
    ///
    /// **即使 `rows` 为空也必须被调用**（`set_relation(name, vec![])`）——
    /// 否则前一次加载残留在字段里的旧数据不会被清掉（静默给过时数据）。
    ///
    /// 单值关联（HasOne / BelongsTo）只取第一行，其余忽略。
    fn set_relation(&mut self, name: &str, rows: Vec<Row>) -> Result<(), OrmError>;
```

派生宏生成（放在 `impl Entity` 里）：

```rust
    fn set_relation(
        &mut self,
        name: &str,
        rows: ::std::vec::Vec<::ecat_orm::Row>,
    ) -> ::core::result::Result<(), ::ecat_orm::OrmError> {
        match name {
            #(
                #rel_names => {
                    #rel_setters
                }
            )*
            _ => {
                return ::core::result::Result::Err(
                    ::ecat_orm::OrmError::UnknownColumn(
                        ::std::format!("unknown relation `{name}`"),
                    ),
                );
            }
        }
        ::core::result::Result::Ok(())
    }
```

每个关联的 `#rel_setters` 按 `is_single_valued` 二选一：

```rust
// HasMany（Vec<Target>）
let mut out = ::std::vec::Vec::with_capacity(rows.len());
for r in &rows {
    out.push(<#target as ::ecat_orm::Entity>::from_row(r)?);
}
self.#field_ident = out;

// HasOne / BelongsTo（Option<Target>）
self.#field_ident = match rows.first() {
    ::core::option::Option::Some(r) => ::core::option::Option::Some(
        <#target as ::ecat_orm::Entity>::from_row(r)?,
    ),
    ::core::option::Option::None => ::core::option::Option::None,
};
```

**测试**（追加到 `derive_relations.rs`）：

```rust
/// 预加载写回：多值全收，单值只取第一行。
#[test]
fn set_relation_dispatches_by_name_and_arity() {
    let mk = |id: i64, title: &str| {
        Row::new(
            vec!["id".into(), "user_id".into(), "title".into()],
            vec![serde_json::json!(id), serde_json::json!(1), serde_json::json!(title)],
        )
    };
    let mut u = User::from_row(&Row::new(
        vec!["id".into(), "name".into()],
        vec![serde_json::json!(1), serde_json::json!("alice")],
    ))
    .unwrap();

    u.set_relation("posts", vec![mk(10, "a"), mk(11, "b")]).unwrap();
    assert_eq!(u.posts.len(), 2);
    assert_eq!(u.posts[0].title, "a");

    // 单值关联：给三行，只留第一行
    assert_eq!(User::META.relation("profile").unwrap().kind, RelationKind::HasOne);
    u.set_relation("profile", vec![mk(20, "x"), mk(21, "y"), mk(22, "z")])
        .unwrap();
    assert!(u.profile.is_some());
    assert_eq!(u.profile.as_ref().unwrap().id, 20);
}

/// 空 `rows` 必须**清空**字段，不是「什么都不做」。
/// 否则重新加载时前一次的旧关联会留在字段里（静默给过时数据）。
#[test]
fn set_relation_with_no_rows_clears_the_field() {
    let mut u = User::from_row(&Row::new(
        vec!["id".into(), "name".into()],
        vec![serde_json::json!(1), serde_json::json!("alice")],
    ))
    .unwrap();
    u.set_relation(
        "posts",
        vec![Row::new(
            vec!["id".into(), "user_id".into(), "title".into()],
            vec![serde_json::json!(1), serde_json::json!(1), serde_json::json!("old")],
        )],
    )
    .unwrap();
    assert_eq!(u.posts.len(), 1);

    u.set_relation("posts", vec![]).unwrap();
    assert!(u.posts.is_empty(), "空结果必须清空残留");
}

/// 未声明的关联名必须报错，不能静默忽略。
#[test]
fn set_relation_rejects_unknown_names() {
    let mut u = User::from_row(&Row::new(
        vec!["id".into(), "name".into()],
        vec![serde_json::json!(1), serde_json::json!("alice")],
    ))
    .unwrap();
    let e = u.set_relation("nope", vec![]).unwrap_err();
    assert!(matches!(e, OrmError::UnknownColumn(_)), "got: {e:?}");
}
```

- [ ] **Step 4: 建 `ecat-orm/src/relation.rs` 的最小骨架**

Task 13 才做预加载执行，本任务只需要 trait 让上一步的 `impl` 能编译：

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

/// 由 `#[derive(Entity)]` 为每个实体生成的 `XxxRelation` 枚举实现它。
///
/// 存在的理由：`Query::with(&[UserRelation::Posts])` 要接受**任意实体**的
/// 关联枚举，而它们在编译期是不同类型。trait object 把它们的差异收敛到
/// 「能报出自己的关联名」这一点上，预加载随后按名字去 `EntityMeta` 查细节。
pub trait RelationSelector: Copy + Send + Sync {
    fn name(self) -> &'static str;
}
```

并在 `lib.rs` 加 `pub mod relation;`。

- [ ] **Step 5: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
```

期望：`derive_columns` 14 + `derive_relations` 7 + 单元测试 23 = 44 passed。

- [ ] **Step 6: 闸门 + 提交**

```bash
cd /home/wwwroot/e-cat
cargo fmt --check && cargo clippy -p ecat-orm -p ecat-orm-derive --all-targets -- -D warnings; echo "rc=$?"
git add ecat-orm-derive ecat-orm/src/relation.rs ecat-orm/src/lib.rs ecat-orm/tests/derive_relations.rs
git commit -m "feat(ecat-orm-derive): 关联字段与 XxxRelation 枚举"
```

### ⚠️ Task 8 必读：属性解析的静默失败是这个任务最大的风险

`parse_nested_meta` 对**不认识的键**默认会报错（因为我们每个分支都显式 `Err(meta.error(...))`）。但 `has_many = "Post", foreign_key = "user_id"` 这条路径上，`foreign_key` 是靠**手写的内部循环**解析的 —— 如果你在内部循环里写 `if id == "foreign_key" { … }` 而**没有 else 分支报错**，那么 `foreignkey`（少个下划线）会被静默丢弃，用户会拿到 "requires `foreign_key`" 的报错却明明写了。

**必须**保留 `else { return Err(...) }`。测试方法是把测试里的 `foreign_key` 改成 `foreignkey`，确认报错信息指向那个拼错的键。

---

## 第三处设计缺口（裁决 C）：`limit_clause` 返回 `String` 表达不了 SQL Server 的分页

spec §6 的 trait 签名（spec:597）是：

```rust
fn limit_clause(&self, limit: u64, offset: u64, has_order: bool) -> String;
```

但同一节的矩阵（spec:574）要求 SQL Server「无 ORDER BY → `TOP n`」：

```
SELECT TOP 10 * FROM users          ← TOP 在 SELECT 之后
SELECT * FROM users LIMIT 10        ← LIMIT 在语句末尾
```

**`TOP` 不是后缀，是插在 `SELECT` 和列清单之间的前缀。** 一个返回 `String` 的 `limit_clause` 无法同时表达这两者 —— 除非让调用方去猜返回值该放哪，那是个靠约定维持的隐式契约。

**裁决：返回值改为一个带 `prefix` / `suffix` 的结构体。**

```rust
/// 分页片段的**位置不唯一**：SQL Server 的 `TOP` 是 SELECT 前缀，
/// 其余方言的 `LIMIT/OFFSET` 是语句后缀。
pub struct Limit {
    pub prefix: String,
    pub suffix: String,
}

impl Limit {
    pub fn none() -> Self {
        Self { prefix: String::new(), suffix: String::new() }
    }
}
```

SQL 生成方写成 `SELECT {prefix}{cols} FROM {table} …{suffix}`。

**为什么不用「SQL Server 一律走 `OFFSET/FETCH`」回避**：那条路要求给每条无 ORDER BY 的分页查询补一个 `ORDER BY (SELECT NULL)` —— 为了迁就 trait 形状而给每次分页查询加一次无谓排序。`TOP` 是 SQL Server 上表达「只取前 n 行」的原生方式，矩阵里也是这么写的。

**为什么现在改是免费的**：`DialectSpec` 是批次 3 首次实现，**此前没有任何实现者**，改签名不破坏任何人。

**顺带**：`Limit::none()` 而不是 `Default` —— 这里的「空」是语义上的「不分页」，不是「默认值」，用关联函数让调用点读起来是 `Limit::none()` 而非 `Limit::default()`。

---

## Task 9: 方言层骨架 + Standard + SQLite

**Files:**
- Create: `ecat-orm/src/dialect/mod.rs`
- Create: `ecat-orm/src/dialect/standard.rs`
- Create: `ecat-orm/src/dialect/sqlite.rs`
- Modify: `ecat-orm/src/lib.rs`（加 `pub mod dialect;`）

**为什么方言层要独立成纯函数**（spec:591）：SQL 生成是整个 ORM 里最容易出错、又最难测的部分。做成「输入 → 字符串」的纯函数后，**不需要数据库就能穷举所有分支** —— 而真库只有 SQLite 能进 CI。

- [ ] **Step 1: 写失败测试**

`ecat-orm/src/dialect/mod.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ecat_data::Dialect;

    #[test]
    fn lookup_covers_every_dialect_variant() {
        // 五个变体一个都不能漏 —— 漏了就是运行期 panic。
        for d in [
            Dialect::Standard,
            Dialect::Sqlite,
            Dialect::Postgres,
            Dialect::MySql,
            Dialect::Mssql,
        ] {
            let spec = lookup(d);
            assert!(!spec.quote("x").is_empty(), "{d:?} 的 quote 返回了空串");
        }
    }

    #[test]
    fn mssql_is_looked_up_not_falling_back_to_standard() {
        // 最容易犯的错：忘了给某个方言接线，它静默退回 Standard，
        // 于是生成 `"col"` 而 SQL Server 要 `[col]` —— 语法错误。
        assert_eq!(lookup(Dialect::Mssql).quote("id"), "[id]");
        assert_eq!(lookup(Dialect::Standard).quote("id"), "\"id\"");
    }

    #[test]
    fn max_params_reflects_the_tightest_backend() {
        // SQL Server 的 2100 是最紧的，写错成 65535 会让批量插入在真库上炸。
        assert_eq!(lookup(Dialect::Mssql).max_params_per_stmt(), 2100);
        assert_eq!(lookup(Dialect::Sqlite).max_params_per_stmt(), 999);
        assert_eq!(lookup(Dialect::Postgres).max_params_per_stmt(), 65535);
    }

    #[test]
    fn limit_none_is_empty_on_both_sides() {
        let l = Limit::none();
        assert!(l.prefix.is_empty());
        assert!(l.suffix.is_empty());
    }
}
```

`ecat-orm/src/dialect/sqlite.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::ColType;

    fn s() -> SqliteSpec {
        SqliteSpec
    }

    #[test]
    fn quotes_with_double_quotes() {
        assert_eq!(s().quote("user"), "\"user\"");
    }

    #[test]
    fn placeholder_ignores_the_index() {
        assert_eq!(s().placeholder(1), "?");
        assert_eq!(s().placeholder(42), "?");
    }

    #[test]
    fn limit_is_a_suffix() {
        let l = s().limit_clause(10, 0, false);
        assert_eq!(l.prefix, "");
        assert_eq!(l.suffix, " LIMIT 10");
    }

    #[test]
    fn limit_with_offset() {
        let l = s().limit_clause(10, 20, false);
        assert_eq!(l.suffix, " LIMIT 10 OFFSET 20");
    }

    #[test]
    fn insert_uses_returning() {
        let plan = s().insert_plan(
            "users",
            &["name".into()],
            "id",
            1,
        );
        match plan {
            InsertPlan::Single { sql } => {
                assert_eq!(sql, "INSERT INTO \"users\" (\"name\") VALUES (?) RETURNING \"id\"");
            }
            other => panic!("sqlite 应走一步式 RETURNING，得到 {other:?}"),
        }
    }

    #[test]
    fn upsert_uses_on_conflict() {
        let sql = s().upsert("users", &["id".into(), "name".into()], "id", 2);
        assert!(sql.contains("ON CONFLICT"), "got: {sql}");
        assert!(sql.contains("DO UPDATE"), "got: {sql}");
    }

    #[test]
    fn bool_literal_is_1_and_0() {
        assert_eq!(s().bool_literal(true), "1");
        assert_eq!(s().bool_literal(false), "0");
    }

    #[test]
    fn column_types_match_the_matrix() {
        assert_eq!(s().col_type(ColType::I64), "INTEGER");
        assert_eq!(s().col_type(ColType::Text), "TEXT");
        assert_eq!(s().col_type(ColType::Timestamp), "TEXT");
        assert_eq!(s().col_type(ColType::Date), "TEXT");
        assert_eq!(s().col_type(ColType::Bool), "INTEGER");
        assert_eq!(s().col_type(ColType::F64), "REAL");
        assert_eq!(s().col_type(ColType::Bytes), "BLOB");
    }

    #[test]
    fn autoincrement_uses_the_sqlite_spelling() {
        assert_eq!(
            s().autoincrement_ddl(ColType::I64),
            "INTEGER PRIMARY KEY AUTOINCREMENT"
        );
    }

    #[test]
    fn table_exists_uses_if_not_exists() {
        assert_eq!(
            s().table_exists_sql("users"),
            "CREATE TABLE IF NOT EXISTS \"users\""
        );
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm dialect 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

- [ ] **Step 3: 实现 `dialect/mod.rs`**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 方言差异层。**全部是纯函数**（输入 → 字符串），因此不需要数据库就能穷举
//! 所有分支 —— 而 CI 里只有 SQLite 能真跑。

use ecat_data::Dialect;

use crate::entity::ColType;

mod sqlite;
mod standard;

/// 分页片段。**位置不唯一**：SQL Server 的 `TOP` 是 SELECT 前缀，
/// 其余方言的 `LIMIT/OFFSET` 是语句后缀。见「裁决 C」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limit {
    pub prefix: String,
    pub suffix: String,
}

impl Limit {
    pub fn none() -> Self {
        Self {
            prefix: String::new(),
            suffix: String::new(),
        }
    }
}

/// 主键回填方案。
///
/// `InsertThen` 存在的原因是 **MySQL 的 `LAST_INSERT_ID()` 是连接作用域的** ——
/// 在连接池下 `INSERT` 与 `SELECT LAST_INSERT_ID()` 是两次独立的池取用，可能落到
/// 不同连接，取回别的会话的值（**静默错值**）。因此 MySQL 必须把两条语句包进
/// 同一个事务以保证同连接（spec:585-589）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertPlan {
    /// 一步式：`INSERT … RETURNING pk` / `INSERT … OUTPUT INSERTED.pk`。
    Single { sql: String },
    /// 两步式：先 INSERT，再 `SELECT LAST_INSERT_ID()`。**两条必须在同一事务内**。
    InsertThen { insert: String, fetch: String },
}

pub trait DialectSpec: Send + Sync {
    /// 引用标识符。**输入必须已经过白名单校验**（见 `EntityMeta::column`）。
    fn quote(&self, ident: &str) -> String;

    /// 绑定占位符，1-based。`?` 类方言忽略下标。
    fn placeholder(&self, index: usize) -> String;

    fn limit_clause(&self, limit: u64, offset: u64, has_order: bool) -> Limit;

    fn insert_plan(&self, table: &str, cols: &[String], pk: &str, n_params: usize) -> InsertPlan;

    fn upsert(&self, table: &str, cols: &[String], pk: &str, n_params: usize) -> String;

    fn bool_literal(&self, b: bool) -> String;

    fn col_type(&self, ty: ColType) -> String;

    /// 建表语句的**开头部分**。SQLite/PG/MySQL 带 `IF NOT EXISTS`；
    /// SQL Server 没有该语法，改为返回空串并让调用方先查
    /// `INFORMATION_SCHEMA.TABLES`（见 `migrate::ddl`）。
    fn table_exists_sql(&self, table: &str) -> String;

    /// 自增主键列的完整 DDL 片段（含类型）。
    fn autoincrement_ddl(&self, ty: ColType) -> String;

    /// 单语句参数上限。批量操作据此分块（spec §5.5b）：
    /// **不分块 = 几千行批量插入必然报错。**
    fn max_params_per_stmt(&self) -> usize;
}

/// 按方言取实现。**穷举匹配，不用 `_ =>` 兜底** —— 新增方言变体时
/// 编译器会直接报错，而不是静默退回 Standard 生成错误 SQL。
pub fn lookup(d: Dialect) -> &'static dyn DialectSpec {
    match d {
        Dialect::Standard => &standard::StandardSpec,
        Dialect::Sqlite => &sqlite::SqliteSpec,
        Dialect::Postgres => &crate::dialect::postgres::PostgresSpec,
        Dialect::MySql => &crate::dialect::mysql::MySqlSpec,
        Dialect::Mssql => &crate::dialect::mssql::MssqlSpec,
    }
}
```

⚠️ 上面 `lookup` 引用了 Task 10 才会创建的 `postgres` / `mysql` / `mssql` 模块。**本任务只写 `mod sqlite; mod standard;` 两行**，并把 `lookup` 里那三个分支暂时写成：

```rust
        Dialect::Postgres | Dialect::MySql | Dialect::Mssql => &standard::StandardSpec,
```

Task 10 再把它们替换成真实现。**替换时不要用 `_ =>`** —— 保留穷举是为了让「新增方言」在编译期暴露。

- [ ] **Step 4: 实现 `dialect/standard.rs`**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! ANSI 近似方言。也是第三方 `SqlExecutor` 未声明方言时的回退。

use super::DialectSpec;
use super::InsertPlan;
use super::Limit;
use crate::entity::ColType;

pub(crate) struct StandardSpec;

impl DialectSpec for StandardSpec {
    fn quote(&self, ident: &str) -> String {
        format!("\"{ident}\"")
    }

    fn placeholder(&self, _index: usize) -> String {
        "?".into()
    }

    fn limit_clause(&self, limit: u64, offset: u64, _has_order: bool) -> Limit {
        let suffix = if offset == 0 {
            format!(" LIMIT {limit}")
        } else {
            format!(" LIMIT {limit} OFFSET {offset}")
        };
        Limit {
            prefix: String::new(),
            suffix,
        }
    }

    fn insert_plan(&self, table: &str, cols: &[String], pk: &str, _n_params: usize) -> InsertPlan {
        let cols_sql = cols
            .iter()
            .map(|c| self.quote(c))
            .collect::<Vec<_>>()
            .join(", ");
        let ph = (1..=cols.len())
            .map(|i| self.placeholder(i))
            .collect::<Vec<_>>()
            .join(", ");
        InsertPlan::Single {
            sql: format!(
                "INSERT INTO {} ({cols_sql}) VALUES ({ph}) RETURNING {}",
                self.quote(table),
                self.quote(pk)
            ),
        }
    }

    fn upsert(&self, table: &str, cols: &[String], pk: &str, _n_params: usize) -> String {
        let cols_sql = cols
            .iter()
            .map(|c| self.quote(c))
            .collect::<Vec<_>>()
            .join(", ");
        let ph = (1..=cols.len())
            .map(|i| self.placeholder(i))
            .collect::<Vec<_>>()
            .join(", ");
        let assigns = cols
            .iter()
            .filter(|c| c.as_str() != pk)
            .map(|c| {
                let q = self.quote(c);
                format!("{q} = excluded.{q}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "INSERT INTO {} ({cols_sql}) VALUES ({ph}) ON CONFLICT ({}) DO UPDATE SET {assigns}",
            self.quote(table),
            self.quote(pk)
        )
    }

    fn bool_literal(&self, b: bool) -> String {
        if b { "TRUE".into() } else { "FALSE".into() }
    }

    fn col_type(&self, ty: ColType) -> String {
        match ty {
            ColType::I64 => "BIGINT".into(),
            ColType::I32 => "INTEGER".into(),
            ColType::F64 => "DOUBLE PRECISION".into(),
            ColType::Bool => "BOOLEAN".into(),
            ColType::Text | ColType::Json => "TEXT".into(),
            ColType::Bytes => "BLOB".into(),
            ColType::Timestamp => "TIMESTAMP".into(),
            ColType::Date => "DATE".into(),
        }
    }

    fn table_exists_sql(&self, table: &str) -> String {
        format!("CREATE TABLE IF NOT EXISTS {}", self.quote(table))
    }

    fn autoincrement_ddl(&self, ty: ColType) -> String {
        format!("{} GENERATED BY DEFAULT AS IDENTITY", self.col_type(ty))
    }

    fn max_params_per_stmt(&self) -> usize {
        65535
    }
}
```

- [ ] **Step 5: 实现 `dialect/sqlite.rs`**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use super::DialectSpec;
use super::InsertPlan;
use super::Limit;
use crate::entity::ColType;

pub(crate) struct SqliteSpec;

impl DialectSpec for SqliteSpec {
    fn quote(&self, ident: &str) -> String {
        format!("\"{ident}\"")
    }

    fn placeholder(&self, _index: usize) -> String {
        "?".into()
    }

    fn limit_clause(&self, limit: u64, offset: u64, _has_order: bool) -> Limit {
        Limit {
            prefix: String::new(),
            suffix: if offset == 0 {
                format!(" LIMIT {limit}")
            } else {
                format!(" LIMIT {limit} OFFSET {offset}")
            },
        }
    }

    fn insert_plan(&self, table: &str, cols: &[String], pk: &str, _n_params: usize) -> InsertPlan {
        // SQLite 3.35+ 支持 RETURNING（随 sqlx 的 bundled 版本提供）。
        InsertPlan::Single {
            sql: format!(
                "INSERT INTO {} ({}) VALUES ({}) RETURNING {}",
                self.quote(table),
                cols.iter().map(|c| self.quote(c)).collect::<Vec<_>>().join(", "),
                (1..=cols.len()).map(|i| self.placeholder(i)).collect::<Vec<_>>().join(", "),
                self.quote(pk)
            ),
        }
    }

    fn upsert(&self, table: &str, cols: &[String], pk: &str, _n_params: usize) -> String {
        let assigns = cols
            .iter()
            .filter(|c| c.as_str() != pk)
            .map(|c| {
                let q = self.quote(c);
                format!("{q} = excluded.{q}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT ({}) DO UPDATE SET {assigns}",
            self.quote(table),
            cols.iter().map(|c| self.quote(c)).collect::<Vec<_>>().join(", "),
            (1..=cols.len()).map(|i| self.placeholder(i)).collect::<Vec<_>>().join(", "),
            self.quote(pk)
        )
    }

    fn bool_literal(&self, b: bool) -> String {
        if b { "1".into() } else { "0".into() }
    }

    fn col_type(&self, ty: ColType) -> String {
        // SQLite 是动态类型：列类型只是「亲和性」提示。时间存 RFC3339 文本、
        // 布尔存 1/0，都取 INTEGER/TEXT。
        match ty {
            ColType::I64 | ColType::I32 | ColType::Bool => "INTEGER".into(),
            ColType::F64 => "REAL".into(),
            ColType::Text | ColType::Json | ColType::Timestamp | ColType::Date => "TEXT".into(),
            ColType::Bytes => "BLOB".into(),
        }
    }

    fn table_exists_sql(&self, table: &str) -> String {
        format!("CREATE TABLE IF NOT EXISTS {}", self.quote(table))
    }

    fn autoincrement_ddl(&self, _ty: ColType) -> String {
        // SQLite 只允许 INTEGER PRIMARY KEY AUTOINCREMENT 这一种拼法 ——
        // 换成 BIGINT 就**不再是 rowid 别名**，自增静默失效。
        "INTEGER PRIMARY KEY AUTOINCREMENT".into()
    }

    fn max_params_per_stmt(&self) -> usize {
        // SQLITE_MAX_VARIABLE_NUMBER：3.32 之前是 999，之后是 32766。
        // 取保守值 999 —— 多切几块只是多几次往返，猜大了是运行期报错。
        999
    }
}
```

- [ ] **Step 6: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm dialect 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
```

期望：15 passed（mod.rs 4 + sqlite 11）。

- [ ] **Step 7: 提交**

```bash
git add ecat-orm/src/dialect ecat-orm/src/lib.rs
git commit -m "feat(ecat-orm): 方言层骨架 + Standard/SQLite"
```

### ⚠️ Task 9 必读：SQLite 的 `max_params_per_stmt` 取 999 而不是 32766

SQLite 3.32+ 的 `SQLITE_MAX_VARIABLE_NUMBER` 默认是 **32766**，而 sqlx 打包的 libsqlite3 版本远高于 3.32，所以 32766 是「实际生效」的值。**仍然取 999。**

理由：这是**分块上限**，不是容量上限。999 的错误方向是「多切几块」（多几次往返，慢一点）；32766 猜错的方向是「真库上报 SQLITE_ERROR: too many SQL variables」（功能直接不可用）。批次 1/2 一路在处理的都是「静默错答 / 运行期炸」这一族问题，这里取保守值。

**不要**为了「充分利用 SQLite 能力」改成 32766 —— 除非你能指出一个不依赖运行时探测就能确定该值的判据。

---

## Task 10: PostgreSQL / MySQL / SQL Server 方言

**Files:**
- Create: `ecat-orm/src/dialect/postgres.rs`
- Create: `ecat-orm/src/dialect/mysql.rs`
- Create: `ecat-orm/src/dialect/mssql.rs`
- Modify: `ecat-orm/src/dialect/mod.rs`（`mod` 声明 + `lookup` 换成真实现）

**本任务的分支全部来自 spec §6 的矩阵**，逐格对照实现。三个文件结构同构，但**每一格都要单独看** —— 同构是最容易产生「复制粘贴后忘了改」的地方。

- [ ] **Step 1: 写失败测试**

`ecat-orm/src/dialect/postgres.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::ColType;

    fn s() -> PostgresSpec { PostgresSpec }

    #[test]
    fn quotes_with_double_quotes() {
        assert_eq!(s().quote("user"), "\"user\"");
    }

    /// `$n` 是**编号**占位符 —— 下标必须有意义，与 `?` 类方言的关键区别。
    #[test]
    fn placeholder_is_numbered() {
        assert_eq!(s().placeholder(1), "$1");
        assert_eq!(s().placeholder(7), "$7");
    }

    #[test]
    fn limit_is_a_suffix() {
        assert_eq!(s().limit_clause(10, 0, false).suffix, " LIMIT 10");
        assert_eq!(s().limit_clause(10, 20, false).suffix, " LIMIT 10 OFFSET 20");
        assert_eq!(s().limit_clause(10, 0, false).prefix, "");
    }

    #[test]
    fn placeholders_are_sequential_in_insert() {
        let plan = s().insert_plan("users", &["a".into(), "b".into()], "id", 2);
        match plan {
            InsertPlan::Single { sql } => {
                assert_eq!(
                    sql,
                    "INSERT INTO \"users\" (\"a\", \"b\") VALUES ($1, $2) RETURNING \"id\""
                );
            }
            other => panic!("PG 应走一步式 RETURNING，得到 {other:?}"),
        }
    }

    #[test]
    fn upsert_uses_on_conflict() {
        let sql = s().upsert("users", &["id".into(), "name".into()], "id", 2);
        assert!(sql.contains("ON CONFLICT (\"id\")"), "got: {sql}");
        assert!(sql.contains("DO UPDATE SET \"name\" = excluded.\"name\""), "got: {sql}");
    }

    #[test]
    fn upsert_never_assigns_the_pk() {
        let sql = s().upsert("users", &["id".into(), "name".into()], "id", 2);
        assert!(!sql.contains("\"id\" = excluded"), "主键不该被更新: {sql}");
    }

    #[test]
    fn bool_literal_is_true_false() {
        assert_eq!(s().bool_literal(true), "TRUE");
        assert_eq!(s().bool_literal(false), "FALSE");
    }

    #[test]
    fn column_types_match_the_matrix() {
        assert_eq!(s().col_type(ColType::Timestamp), "TIMESTAMPTZ");
        assert_eq!(s().col_type(ColType::Text), "TEXT");
        assert_eq!(s().col_type(ColType::Date), "DATE");
        assert_eq!(s().col_type(ColType::Bytes), "BYTEA");
    }

    #[test]
    fn autoincrement_reuses_the_bigserial_identity() {
        assert!(s().autoincrement_ddl(ColType::I64).contains("BIGSERIAL")
             || s().autoincrement_ddl(ColType::I64).contains("IDENTITY"),
             "got: {}", s().autoincrement_ddl(ColType::I64));
    }

    #[test]
    fn max_params_is_the_pg_limit() {
        assert_eq!(s().max_params_per_stmt(), 65535);
    }
}
```

`ecat-orm/src/dialect/mysql.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::ColType;

    fn s() -> MySqlSpec { MySqlSpec }

    /// MySQL 用反引号，不是双引号 —— 这是三个方言里唯一不同的引号。
    #[test]
    fn quotes_with_backticks() {
        assert_eq!(s().quote("user"), "`user`");
    }

    #[test]
    fn placeholder_is_question_mark() {
        assert_eq!(s().placeholder(1), "?");
        assert_eq!(s().placeholder(9), "?");
    }

    /// **本测试是 Task 10 里最重要的一条。**
    /// MySQL 的 LAST_INSERT_ID() 是连接作用域的，池下两次取用可能落到不同连接
    /// → 静默取回别的会话的值。所以必须走两步式，且两条语句由调用方包进同一事务。
    #[test]
    fn insert_is_two_step_not_returning() {
        match s().insert_plan("users", &["name".into()], "id", 1) {
            InsertPlan::InsertThen { insert, fetch } => {
                assert_eq!(insert, "INSERT INTO `users` (`name`) VALUES (?)");
                assert!(
                    fetch.contains("LAST_INSERT_ID()"),
                    "fetch 必须是 LAST_INSERT_ID()，得到: {fetch}"
                );
            }
            InsertPlan::Single { sql } => panic!(
                "MySQL 不得走一步式 —— 池下会静默取回别的连接的 id。得到: {sql}"
            ),
        }
    }

    #[test]
    fn upsert_uses_on_duplicate_key() {
        let sql = s().upsert("users", &["id".into(), "name".into()], "id", 2);
        assert!(sql.contains("ON DUPLICATE KEY UPDATE"), "got: {sql}");
        assert!(sql.contains("`name` = VALUES(`name`)"), "got: {sql}");
    }

    #[test]
    fn bool_literal_is_1_and_0() {
        assert_eq!(s().bool_literal(true), "1");
        assert_eq!(s().bool_literal(false), "0");
    }

    #[test]
    fn column_types_match_the_matrix() {
        assert_eq!(s().col_type(ColType::Timestamp), "DATETIME");
        assert_eq!(s().col_type(ColType::I64), "BIGINT");
        assert_eq!(s().col_type(ColType::Text), "TEXT");
        assert_eq!(s().col_type(ColType::Bytes), "BLOB");
    }

    #[test]
    fn autoincrement_ddl_uses_auto_increment() {
        assert_eq!(s().autoincrement_ddl(ColType::I64), "BIGINT AUTO_INCREMENT");
    }

    #[test]
    fn table_exists_uses_if_not_exists() {
        assert_eq!(s().table_exists_sql("users"), "CREATE TABLE IF NOT EXISTS `users`");
    }
}
```

`ecat-orm/src/dialect/mssql.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::ColType;

    fn s() -> MssqlSpec { MssqlSpec }

    /// 方括号，不是双引号、不是反引号。
    #[test]
    fn quotes_with_brackets() {
        assert_eq!(s().quote("user"), "[user]");
    }

    #[test]
    fn placeholder_is_at_p_n() {
        assert_eq!(s().placeholder(1), "@P1");
        assert_eq!(s().placeholder(12), "@P12");
    }

    /// **无 OFFSET 时走 TOP（前缀）**，这是「裁决 C」存在的全部理由。
    #[test]
    fn limit_without_offset_uses_top_prefix() {
        let l = s().limit_clause(10, 0, false);
        assert_eq!(l.prefix, "TOP (10) ");
        assert_eq!(l.suffix, "");
    }

    /// 有 OFFSET 时走 OFFSET/FETCH（后缀）；无 ORDER BY 必须补一个，
    /// 否则 SQL Server 直接语法报错（ORDER BY 是 OFFSET/FETCH 的强制前提）。
    #[test]
    fn limit_with_offset_uses_offset_fetch_and_synthesizes_order_by() {
        let l = s().limit_clause(10, 20, false);
        assert_eq!(l.prefix, "");
        assert!(l.suffix.contains("ORDER BY (SELECT NULL)"), "got: {}", l.suffix);
        assert!(l.suffix.contains("OFFSET 20 ROWS"), "got: {}", l.suffix);
        assert!(l.suffix.contains("FETCH NEXT 10 ROWS ONLY"), "got: {}", l.suffix);
    }

    #[test]
    fn existing_order_by_is_not_duplicated() {
        let l = s().limit_clause(10, 20, true);
        assert!(
            !l.suffix.contains("ORDER BY (SELECT NULL)"),
            "用户已有 ORDER BY 时不得再补: {}",
            l.suffix
        );
    }

    /// INSERT ... OUTPUT INSERTED.pk 是一步式，无 LAST_INSERT_ID 的连接作用域问题。
    #[test]
    fn insert_uses_output_inserted() {
        match s().insert_plan("users", &["name".into()], "id", 1) {
            InsertPlan::Single { sql } => {
                assert!(sql.contains("OUTPUT INSERTED.[id]"), "got: {sql}");
            }
            other => panic!("MSSQL 应走一步式 OUTPUT，得到 {other:?}"),
        }
    }

    #[test]
    fn upsert_uses_merge() {
        let sql = s().upsert("users", &["id".into(), "name".into()], "id", 2);
        assert!(sql.contains("MERGE"), "got: {sql}");
    }

    #[test]
    fn bool_literal_is_1_and_0() {
        assert_eq!(s().bool_literal(true), "1");
        assert_eq!(s().bool_literal(false), "0");
    }

    #[test]
    fn column_types_match_the_matrix() {
        assert_eq!(s().col_type(ColType::Timestamp), "DATETIME2");
        assert_eq!(s().col_type(ColType::Text), "NVARCHAR(MAX)");
        assert_eq!(s().col_type(ColType::I64), "BIGINT");
        assert_eq!(s().col_type(ColType::Bytes), "VARBINARY(MAX)");
    }

    /// SQL Server 没有 CREATE TABLE IF NOT EXISTS —— 返回空串，
    /// 由 migrate::ddl 改为先查 INFORMATION_SCHEMA.TABLES。
    #[test]
    fn create_table_has_no_if_not_exists() {
        assert!(!s().table_exists_sql("users").contains("IF NOT EXISTS"));
    }

    #[test]
    fn autoincrement_uses_identity() {
        assert_eq!(s().autoincrement_ddl(ColType::I64), "BIGINT IDENTITY(1,1)");
    }

    /// 2100 是四个后端里最紧的 —— 写错成 65535 会让批量插入在真库上炸。
    #[test]
    fn max_params_is_2100() {
        assert_eq!(s().max_params_per_stmt(), 2100);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm dialect 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

- [ ] **Step 3: 实现 `postgres.rs` / `mysql.rs` / `mssql.rs`**

逐格照 spec §6 矩阵填。三处**必须单独想清楚**、不能靠复制：

1. **`mysql.rs` 的 `insert_plan` 返回 `InsertThen`**（连接作用域的 `LAST_INSERT_ID()`），其余三个返回 `Single`。
2. **`mssql.rs` 的 `limit_clause` 用 `prefix`**，其余三个用 `suffix`；且 OFFSET 路径在 `has_order == false` 时要补 `ORDER BY (SELECT NULL)`。
3. **`mssql.rs` 的 `table_exists_sql` 不含 `IF NOT EXISTS`**（该语法不存在）。

各文件的具体格值（照抄矩阵）：

| | Postgres | MySQL | MSSQL |
|---|---|---|---|
| `quote` | `"x"` | `` `x` `` | `[x]` |
| `placeholder` | `$n` | `?` | `@Pn` |
| `col_type(Text)` | `TEXT` | `TEXT` | `NVARCHAR(MAX)` |
| `col_type(Timestamp)` | `TIMESTAMPTZ` | `DATETIME` | `DATETIME2` |
| `col_type(Date)` | `DATE` | `DATE` | `DATE` |
| `col_type(Bytes)` | `BYTEA` | `BLOB` | `VARBINARY(MAX)` |
| `col_type(Bool)` | `BOOLEAN` | `TINYINT(1)` | `BIT` |
| `col_type(I64)` | `BIGINT` | `BIGINT` | `BIGINT` |
| `col_type(F64)` | `DOUBLE PRECISION` | `DOUBLE` | `FLOAT` |
| `autoincrement_ddl(I64)` | `BIGSERIAL` | `BIGINT AUTO_INCREMENT` | `BIGINT IDENTITY(1,1)` |
| `max_params_per_stmt` | 65535 | 65535 | 2100 |
| upsert | `ON CONFLICT (pk) DO UPDATE SET c = excluded.c` | `ON DUPLICATE KEY UPDATE c = VALUES(c)` | `MERGE` |

- [ ] **Step 4: 改 `dialect/mod.rs` 接线**

把 `mod sqlite; mod standard;` 扩成五行，并把 `lookup` 里 Task 9 留下的临时分支换掉：

```rust
mod mssql;
mod mysql;
mod postgres;
mod sqlite;
mod standard;

pub fn lookup(d: Dialect) -> &'static dyn DialectSpec {
    match d {
        Dialect::Standard => &standard::StandardSpec,
        Dialect::Sqlite => &sqlite::SqliteSpec,
        Dialect::Postgres => &postgres::PostgresSpec,
        Dialect::MySql => &mysql::MySqlSpec,
        Dialect::Mssql => &mssql::MssqlSpec,
    }
}
```

- [ ] **Step 5: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm dialect 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
```

期望：约 52 passed（mod 4 + sqlite 11 + PG 10 + MySQL 9 + MSSQL 12，具体数以实际为准）。

- [ ] **Step 6: 闸门 + 提交**

```bash
cd /home/wwwroot/e-cat
cargo fmt --check && cargo clippy -p ecat-orm --all-targets -- -D warnings; echo "rc=$?"
git add ecat-orm/src/dialect
git commit -m "feat(ecat-orm): PostgreSQL/MySQL/SQL Server 方言"
```

### ⚠️ Task 10 必读：三处「复制粘贴会错」的地方

写这三个文件时，**每个文件都从矩阵读一遍，不要从上一个文件复制**。矩阵里三列有差异的格子共 12 格，复制粘贴最容易漏掉的是：

1. **MySQL 漏改成 `InsertThen`** —— 这条最危险：测试会抓到（`insert_is_two_step_not_returning`），但只有在测试确实写了的前提下。**先写测试再写实现**，别倒过来。
2. **MSSQL 的 `col_type(Text)` 写成 `TEXT`** —— SQL Server 的 `TEXT` 是**已弃用**类型（且 `NVARCHAR` 才是 Unicode）。矩阵明确要 `NVARCHAR(MAX)`。
3. **PostgreSQL 的 `timestamp` 写成 `TIMESTAMP`** —— 必须是 `TIMESTAMPTZ`，否则存进去的 UTC 会被按本地时区解读（spec 的时间策略依赖所有时间都是 UTC 归一化的，`TIMESTAMP` 会破坏这个前提）。

**自检方法**：三个文件的测试模块里，`column_types_match_the_matrix` 是**同名不同值**的。跑测试时若看到某个方言的这条失败，先怀疑是复制粘贴漏改，而不是矩阵写错了。

---

## Task 11: 查询构建器（`query/filter.rs` + `query/mod.rs`）

**Files:**
- Create: `ecat-orm/src/query/filter.rs`
- Create: `ecat-orm/src/query/mod.rs`
- Modify: `ecat-orm/src/lib.rs`

**两条设计约束，本任务必须同时满足：**

1. **标识符白名单（spec §5.5a，安全约束）。** 列名是 `&str`，**值是绑定的、标识符不是**。所有收列名的 API 必须对照 `EntityMeta.columns` 校验，未命中返回 `OrmError::UnknownColumn`，**绝不拼进 SQL**。
2. **`delete_where` 必须在类型上就要求有过滤条件。** 否则 `User::query().delete_where(&db)` 会删全表。用类型状态 `Unfiltered → Filtered` 把它变成编译错误。

**白名单的边界（必须写进 rustdoc）**：白名单只覆盖**主实体自己的列**。`join()` 关联表的列不在 `EntityMeta.columns` 里 —— 因为我们没有关联实体的元数据（`join` 收的是表名字符串 + 原生 ON 条件）。**要按关联表的列过滤/排序，走 `filter_raw`**。这是个有意的边界，不是遗漏。

- [ ] **Step 1: 写失败测试**

`ecat-orm/src/query/mod.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::*;
    use serde_json::json;

    static COLS: [ColumnMeta; 3] = [
        ColumnMeta { name: "id", ty: ColType::I64, nullable: false, pk: true, auto_increment: true },
        ColumnMeta { name: "name", ty: ColType::Text, nullable: false, pk: false, auto_increment: false },
        ColumnMeta { name: "age", ty: ColType::I64, nullable: false, pk: false, auto_increment: false },
    ];
    static META: EntityMeta = EntityMeta {
        table: "users",
        pk: "id",
        columns: &COLS,
        relations: &[],
        flags: EntityFlags::NONE,
    };

    struct U;
    impl Entity for U {
        const TABLE: &'static str = "users";
        const PK: &'static str = "id";
        const META: &'static EntityMeta = &META;
        fn from_row(_r: &ecat_data::Row) -> Result<Self, OrmError> { Ok(U) }
        fn to_values(&self) -> Vec<(&'static str, serde_json::Value)> { vec![] }
        fn pk_value(&self) -> serde_json::Value { json!(0) }
    }

    // ---- 白名单 ----

    #[test]
    fn known_column_is_accepted() {
        assert!(U::query().filter("name", Op::Eq, "x").is_ok());
    }

    /// **安全约束**：未声明的列名必须被拒，且**不得出现在生成的 SQL 里**。
    #[test]
    fn unknown_column_is_rejected() {
        let e = U::query().filter("naem", Op::Eq, "x").unwrap_err();
        assert!(matches!(e, OrmError::UnknownColumn(ref c) if c == "naem"), "got: {e:?}");
    }

    /// 注入尝试必须被白名单拦下 —— 它不在 columns 里。
    #[test]
    fn injection_attempt_is_rejected_as_unknown_column() {
        let e = U::query()
            .filter("name\" = 'x' OR 1=1 --", Op::Eq, "x")
            .unwrap_err();
        assert!(matches!(e, OrmError::UnknownColumn(_)), "got: {e:?}");
    }

    #[test]
    fn unknown_order_by_column_is_rejected() {
        let e = U::query().order_by("nope", Order::Asc).unwrap_err();
        assert!(matches!(e, OrmError::UnknownColumn(_)), "got: {e:?}");
    }

    // ---- 类型状态 ----

    /// 未过滤的 Query **不能**调 `delete_where` / `update`。
    /// 这是编译期保证，因此用 `compile_fail` 钉住：
    ///
    /// ```compile_fail
    /// # use ecat_orm::query::Query;
    /// # fn f<E: ecat_orm::Entity>() {
    /// // 这行必须无法编译 —— 没有 filter 就 delete_where 等于删全表
    /// let _ = Query::<E, ecat_orm::query::Unfiltered>::new().delete_where();
    /// # }
    /// ```
    #[test]
    fn unfiltered_query_cannot_delete() {
        // 上面那个 doctest 是真正的断言；这里只是让它有个名字好定位失败。
    }

    #[test]
    fn filter_transitions_to_filtered() {
        let q: Query<U, Filtered> = U::query().filter("name", Op::Eq, "x").unwrap();
        assert_eq!(q.filter_count(), 1);
    }

    #[test]
    fn chaining_filters_accumulates() {
        let q = U::query()
            .filter("name", Op::Eq, "x").unwrap()
            .filter("age", Op::Gt, 18).unwrap();
        assert_eq!(q.filter_count(), 2);
    }

    // ---- 操作符 ----

    #[test]
    fn in_requires_an_array_value() {
        let ok = U::query().filter("id", Op::In, json!([1, 2, 3]));
        assert!(ok.is_ok());
        let bad = U::query().filter("id", Op::In, json!(1));
        assert!(matches!(bad, Err(OrmError::Rdbms(_))), "In 传非数组必须报错");
    }

    #[test]
    fn is_null_ignores_the_value() {
        let q = U::query().filter("name", Op::IsNull, json!("whatever")).unwrap();
        assert_eq!(q.filter_count(), 1);
    }

    // ---- 裁决 B：with_trashed 在两个状态上都可用 ----

    #[test]
    fn with_trashed_works_before_and_after_filter() {
        let a = U::query().with_trashed().filter("id", Op::Eq, 1).unwrap();
        let b = U::query().filter("id", Op::Eq, 1).unwrap().with_trashed();
        assert!(a.is_with_trashed() && b.is_with_trashed());
    }

    // ---- 逃生口 ----

    #[test]
    fn filter_raw_bypasses_the_whitelist_by_design() {
        // 关联表的列走这里。文档必须写明「输入必须可信」。
        let q = U::query().filter_raw("posts.published = 1").unwrap();
        assert_eq!(q.filter_count(), 1);
    }

    #[test]
    fn order_by_accepts_known_column_and_direction() {
        let q = U::query().order_by("id", Order::Desc).unwrap();
        assert_eq!(q.order_count(), 1);
    }
}
```

`ecat-orm/src/query/filter.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn op_maps_to_sql_operators() {
        assert_eq!(Op::Eq.as_sql(), "=");
        assert_eq!(Op::Ne.as_sql(), "<>");
        assert_eq!(Op::Lt.as_sql(), "<");
        assert_eq!(Op::Le.as_sql(), "<=");
        assert_eq!(Op::Gt.as_sql(), ">");
        assert_eq!(Op::Ge.as_sql(), ">=");
        assert_eq!(Op::Like.as_sql(), "LIKE");
    }

    /// `Ne` 必须是 `<>` 而不是 `!=` —— `!=` 不是标准 SQL，
    /// SQL Server 与部分 PG 配置下会直接语法错误。
    #[test]
    fn ne_is_ansi_not_bang_eq() {
        assert_ne!(Op::Ne.as_sql(), "!=");
    }

    #[test]
    fn op_reports_whether_it_takes_a_value() {
        assert!(Op::Eq.takes_value());
        assert!(!Op::IsNull.takes_value());
        assert!(!Op::NotNull.takes_value());
        assert!(Op::In.takes_value());
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm query 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

- [ ] **Step 3: 实现 `query/filter.rs`**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use serde_json::Value;

/// 比较操作符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Like,
    In,
    NotIn,
    IsNull,
    NotNull,
}

impl Op {
    /// SQL 运算符。**不用于 `In` / `NotIn` / `IsNull` / `NotNull`** ——
    /// 那四个的 SQL 形态不是中缀运算符（见 `takes_value`）。
    pub fn as_sql(self) -> &'static str {
        match self {
            // ANSI 用 `<>`。`!=` 在 SQL Server 上可用但在某些 PG 兼容模式下不是
            // 标准写法 —— 统一走 `<>`，各后端都认。
            Self::Eq => "=",
            Self::Ne => "<>",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Like => "LIKE",
            Self::In | Self::NotIn | Self::IsNull | Self::NotNull => {
                unreachable!("`{}` has no infix SQL form", self.as_str())
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "Eq",
            Self::Ne => "Ne",
            Self::Lt => "Lt",
            Self::Le => "Le",
            Self::Gt => "Gt",
            Self::Ge => "Ge",
            Self::Like => "Like",
            Self::In => "In",
            Self::NotIn => "NotIn",
            Self::IsNull => "IsNull",
            Self::NotNull => "NotNull",
        }
    }

    /// 是否需要一个绑定值。`IsNull` / `NotNull` 不需要。
    pub fn takes_value(self) -> bool {
        !matches!(self, Self::IsNull | Self::NotNull)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Asc,
    Desc,
}

impl Order {
    pub fn as_sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

/// WHERE 子句的一项。
///
/// **`Raw` 是逃生口**：关联表的列不在本实体的 `EntityMeta.columns` 里，
/// 白名单校验拦不住也不该拦（我们根本没有关联实体的元数据）。用它时
/// **输入必须可信** —— 它不做任何校验，直接拼进 SQL。
#[derive(Debug, Clone)]
pub enum Expr {
    Cmp {
        column: String,
        op: Op,
        value: Value,
    },
    In {
        column: String,
        values: Vec<Value>,
        negated: bool,
    },
    Null {
        column: String,
        negated: bool,
    },
    Raw(String),
}

/// ORDER BY 的一项。
#[derive(Debug, Clone)]
pub struct OrderBy {
    pub column: String,
    pub dir: Order,
}
```

- [ ] **Step 4: 实现 `query/mod.rs`**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! 查询构建器。类型状态 `Unfiltered → Filtered` 把「无过滤条件的删改」
//! 变成编译错误 —— `User::query().delete_where(&db)` 不该能编译。

pub mod filter;
mod sql;

use std::marker::PhantomData;

use serde_json::Value;

use crate::entity::Entity;
use crate::entity::EntityMeta;
use crate::error::OrmError;

pub use filter::Expr;
pub use filter::Op;
pub use filter::Order;
pub use filter::OrderBy;

/// 尚无过滤条件。
#[derive(Debug, Clone, Copy)]
pub struct Unfiltered;
/// 已有至少一个过滤条件 —— 只有这个状态能删改。
#[derive(Debug, Clone, Copy)]
pub struct Filtered;

pub struct Query<E, S> {
    meta: &'static EntityMeta,
    filters: Vec<Expr>,
    orders: Vec<OrderBy>,
    joins: Vec<(String, String)>,
    limit: Option<u64>,
    offset: Option<u64>,
    with_trashed: bool,
    _marker: PhantomData<(fn() -> E, S)>,
}

impl<E: Entity> Query<E, Unfiltered> {
    /// 从空查询开始。`User::query()` 是它的语法糖。
    pub fn new() -> Self {
        Self::from_meta()
    }
}

impl<E: Entity> Default for Query<E, Unfiltered> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Entity, S> Query<E, S> {
    fn from_meta() -> Self {
        Self {
            meta: E::META,
            filters: Vec::new(),
            orders: Vec::new(),
            joins: Vec::new(),
            limit: None,
            offset: None,
            with_trashed: false,
            _marker: PhantomData,
        }
    }

    /// 关掉软删除的自动过滤。见「裁决 B」—— 在**两个状态上**都可用。
    pub fn with_trashed(mut self) -> Self {
        self.with_trashed = true;
        self
    }

    /// 把值推进过滤器。`column` 必须已在 `EntityMeta.columns` 里。
    pub fn order_by(mut self, column: &str, dir: Order) -> Result<Self, OrmError> {
        self.check_column(column)?;
        self.orders.push(OrderBy {
            column: column.into(),
            dir,
        });
        Ok(self)
    }

    /// 列名白名单校验 —— **本模块的安全核心**。
    fn check_column(&self, column: &str) -> Result<(), OrmError> {
        if self.meta.column(column).is_some() {
            Ok(())
        } else {
            Err(OrmError::UnknownColumn(column.into()))
        }
    }

    pub fn limit(mut self, n: u64) -> Self {
        self.limit = Some(n);
        self
    }

    pub fn offset(mut self, n: u64) -> Self {
        self.offset = Some(n);
        self
    }

    /// 关联表上的原生 WHERE 片段。**输入必须可信** —— 不校验、直接拼。
    pub fn filter_raw(mut self, expr: &str) -> Result<Self, OrmError> {
        self.filters.push(Expr::Raw(expr.into()));
        Ok(self)
    }

    // ---- 以下仅供本 crate 的测试与 SQL 生成使用 ----

    #[doc(hidden)]
    pub fn filter_count(&self) -> usize {
        self.filters.len()
    }
    #[doc(hidden)]
    pub fn order_count(&self) -> usize {
        self.orders.len()
    }
    #[doc(hidden)]
    pub fn is_with_trashed(&self) -> bool {
        self.with_trashed
    }
}

impl<E: Entity, S> Query<E, S> {
    /// 加一个过滤条件。**返回新状态 `Filtered`** —— 这正是类型状态的作用。
    pub fn filter(
        self,
        column: &str,
        op: Op,
        value: impl Into<Value>,
    ) -> Result<Query<E, Filtered>, OrmError> {
        self.check_column(column)?;
        let value = value.into();
        let expr = match op {
            Op::IsNull => Expr::Null { column: column.into(), negated: false },
            Op::NotNull => Expr::Null { column: column.into(), negated: true },
            Op::In | Op::NotIn => {
                let arr = value.as_array().ok_or_else(|| {
                    OrmError::Rdbms(ecat_data::RdbmsError::Database(format!(
                        "`{}` requires an array value, got: {value}",
                        op.as_str()
                    )))
                })?;
                Expr::In {
                    column: column.into(),
                    values: arr.clone(),
                    negated: op == Op::NotIn,
                }
            }
            _ => Expr::Cmp { column: column.into(), op, value },
        };
        Ok(self.push_filter(expr))
    }

    fn push_filter(mut self, expr: Expr) -> Query<E, Filtered> {
        self.filters.push(expr);
        Query {
            meta: self.meta,
            filters: self.filters,
            orders: self.orders,
            joins: self.joins,
            limit: self.limit,
            offset: self.offset,
            with_trashed: self.with_trashed,
            _marker: PhantomData,
        }
    }
}
```

`Entity` 上补一个语法糖（加在 `entity.rs` 的 trait 定义里）：

```rust
    /// 开始一个查询。`User::query()` 比 `Query::<User, Unfiltered>::new()` 好读。
    fn query() -> crate::query::Query<Self, crate::query::Unfiltered>
    where
        Self: Sized,
    {
        crate::query::Query::new()
    }
```

- [ ] **Step 5: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm query 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
```

⚠️ `sql.rs` 在本任务里是空模块占位（`mod sql;` + 一个空文件）—— Task 12 填。若 `mod sql;` 指向不存在的文件，编译会失败；**先建一个空的 `query/sql.rs`**（只有版权注释）。

- [ ] **Step 6: 提交**

```bash
git add ecat-orm/src/query ecat-orm/src/entity.rs ecat-orm/src/lib.rs
git commit -m "feat(ecat-orm): 查询构建器（类型状态 + 标识符白名单）"
```

### ⚠️ Task 11 必读：`compile_fail` doctest 是唯一能钉住类型状态的测试

`Unfiltered` 上没有 `delete_where` 这件事，**无法用普通 `#[test]` 断言** —— 一段不该编译的代码，在测试里根本写不出来。

用 `rustdoc` 的 `compile_fail`：

````rust
/// ```compile_fail
/// // 没有 filter 就 delete_where —— 必须编译失败
/// ```
````

**注意**：`compile_fail` 对**任何**编译错误都算通过 —— 包括拼错方法名这种无关错误。所以 doctest 里的代码要尽量贴近真实用法，否则它会「因为错误的原因通过」。写完后**手动验证一次**：临时把 `delete_where` 挪到 `impl<E, S>`（两个状态都能调），确认这个 doctest 变成失败。这一步不做，你无法知道它到底在测什么。

`cargo test -p ecat-orm --doc` 才会跑 doctest（`--test` 不跑）。批次完成检查里要包含它。

---

## Task 12: SELECT / COUNT 生成（`query/sql.rs`）

**Files:**
- Modify: `ecat-orm/src/query/sql.rs`（Task 11 建的空文件）

**本任务的核心难点是「SQL 文本与参数向量必须同步生成」**：占位符编号（`$1` / `@P1`）依赖参数顺序，而参数的顺序又由 WHERE 子句的结构决定。**分两步做（先生成 SQL 再收集参数）必然错位** —— 那正是 `$1` 与 `@P1` 这类编号占位符方言上会静默取错值的 bug 形态。

所以：**一个函数同时产出 `sql` 和 `params`**。

- [ ] **Step 1: 写失败测试**

`ecat-orm/src/query/sql.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::*;
    use ecat_data::Dialect;
    use serde_json::json;

    static COLS: [ColumnMeta; 3] = [
        ColumnMeta { name: "id", ty: ColType::I64, nullable: false, pk: true, auto_increment: true },
        ColumnMeta { name: "name", ty: ColType::Text, nullable: false, pk: false, auto_increment: false },
        ColumnMeta { name: "deleted_at", ty: ColType::Timestamp, nullable: true, pk: false, auto_increment: false },
    ];
    static META: EntityMeta = EntityMeta {
        table: "users",
        pk: "id",
        columns: &COLS,
        relations: &[],
        flags: EntityFlags {
            created_at: None,
            updated_at: None,
            soft_delete: Some("deleted_at"),
            version: None,
        },
    };

    struct U;
    impl Entity for U {
        const TABLE: &'static str = "users";
        const PK: &'static str = "id";
        const META: &'static EntityMeta = &META;
        fn from_row(_r: &ecat_data::Row) -> Result<Self, OrmError> { Ok(U) }
        fn to_values(&self) -> Vec<(&'static str, serde_json::Value)> { vec![] }
        fn pk_value(&self) -> serde_json::Value { json!(0) }
    }

    fn sel<S>(q: &crate::query::Query<U, S>, d: Dialect) -> Built {
        build_select(q, d, false)
    }

    #[test]
    fn bare_select_lists_all_columns_quoted() {
        let b = sel(&U::query(), Dialect::Postgres);
        assert!(b.sql.contains("SELECT \"id\", \"name\", \"deleted_at\" FROM \"users\""), "got: {}", b.sql);
        assert!(b.params.is_empty());
    }

    /// **软删除自动过滤**：实体标了 soft_delete 且未 with_trashed 时，
    /// 查询必须自带 `WHERE deleted_at IS NULL`。
    #[test]
    fn soft_delete_filter_is_applied_by_default() {
        let b = sel(&U::query(), Dialect::Postgres);
        assert!(b.sql.contains("\"deleted_at\" IS NULL"), "got: {}", b.sql);
    }

    #[test]
    fn with_trashed_removes_the_soft_delete_filter() {
        let q = U::query().with_trashed();
        let b = sel(&q, Dialect::Postgres);
        assert!(!b.sql.contains("IS NULL"), "got: {}", b.sql);
    }

    #[test]
    fn filter_emits_placeholders_and_params_in_order() {
        let q = U::query().filter("name", Op::Eq, "alice").unwrap()
            .filter("id", Op::Gt, 5).unwrap();
        let b = sel(&q, Dialect::Postgres);
        // PG 是编号占位符：$1 对 name、$2 对 id —— 顺序由 filter 链决定
        assert!(b.sql.contains("\"name\" = $1"), "got: {}", b.sql);
        assert!(b.sql.contains("\"id\" > $2"), "got: {}", b.sql);
        assert_eq!(b.params, vec![json!("alice"), json!(5)]);
    }

    /// **同一份 Query 在 `?` 方言上必须生成 `?` 而不是 `$1`。**
    #[test]
    fn question_mark_dialects_get_question_marks() {
        let q = U::query().filter("name", Op::Eq, "alice").unwrap();
        let b = sel(&q, Dialect::Sqlite);
        assert!(b.sql.contains("\"name\" = ?"), "got: {}", b.sql);
        assert!(!b.sql.contains("$1"), "got: {}", b.sql);
    }

    /// SQL Server 是 `@Pn`。
    #[test]
    fn mssql_gets_at_p_n() {
        let q = U::query().filter("name", Op::Eq, "alice").unwrap();
        let b = sel(&q, Dialect::Mssql);
        assert!(b.sql.contains("[name] = @P1"), "got: {}", b.sql);
    }

    /// 软删除条件**不占参数位**（是字面量 IS NULL），所以用户参数仍从 1 开始编号。
    /// 这条最容易错：把 IS NULL 也分配一个占位符，会让所有后续参数错位一位。
    #[test]
    fn soft_delete_filter_does_not_consume_a_parameter_slot() {
        let q = U::query().filter("name", Op::Eq, "alice").unwrap();
        let b = sel(&q, Dialect::Postgres);
        assert!(b.sql.contains("\"name\" = $1"), "got: {}", b.sql);
        assert_eq!(b.params.len(), 1);
    }

    #[test]
    fn in_expands_to_one_placeholder_per_value() {
        let q = U::query().filter("id", Op::In, json!([1, 2, 3])).unwrap();
        let b = sel(&q, Dialect::Postgres);
        assert!(b.sql.contains("\"id\" IN ($1, $2, $3)"), "got: {}", b.sql);
        assert_eq!(b.params, vec![json!(1), json!(2), json!(3)]);
    }

    #[test]
    fn not_in_is_negated() {
        let q = U::query().filter("id", Op::NotIn, json!([1])).unwrap();
        let b = sel(&q, Dialect::Postgres);
        assert!(b.sql.contains("NOT IN"), "got: {}", b.sql);
    }

    #[test]
    fn empty_in_list_is_a_contradiction_not_a_syntax_error() {
        // `IN ()` 是语法错误。空列表语义上恒假，写成 1 = 0。
        let q = U::query().filter("id", Op::In, json!([])).unwrap();
        let b = sel(&q, Dialect::Postgres);
        assert!(b.sql.contains("1 = 0"), "got: {}", b.sql);
        assert!(b.params.is_empty(), "空 IN 不该产生参数");
    }

    #[test]
    fn is_null_emits_no_parameter() {
        let q = U::query().filter("name", Op::IsNull, json!(null)).unwrap();
        let b = sel(&q, Dialect::Postgres);
        assert!(b.sql.contains("\"name\" IS NULL"), "got: {}", b.sql);
        assert!(b.params.is_empty());
    }

    #[test]
    fn not_null_is_negated() {
        let q = U::query().filter("name", Op::NotNull, json!(null)).unwrap();
        let b = sel(&q, Dialect::Postgres);
        assert!(b.sql.contains("\"name\" IS NOT NULL"), "got: {}", b.sql);
    }

    #[test]
    fn raw_filter_goes_in_verbatim_with_no_parameter() {
        let q = U::query().filter_raw("posts.published = 1").unwrap();
        let b = sel(&q, Dialect::Postgres);
        assert!(b.sql.contains("posts.published = 1"), "got: {}", b.sql);
        assert!(b.params.is_empty());
    }

    #[test]
    fn order_by_emits_direction() {
        let q = U::query().order_by("id", Order::Desc).unwrap();
        let b = sel(&q, Dialect::Postgres);
        assert!(b.sql.contains("ORDER BY \"id\" DESC"), "got: {}", b.sql);
    }

    #[test]
    fn multi_order_preserves_declaration_order() {
        let q = U::query()
            .order_by("name", Order::Asc).unwrap()
            .order_by("id", Order::Desc).unwrap();
        let b = sel(&q, Dialect::Postgres);
        let i_name = b.sql.find("\"name\" ASC").expect("name ASC");
        let i_id = b.sql.find("\"id\" DESC").expect("id DESC");
        assert!(i_name < i_id, "ORDER BY 顺序必须与声明一致: {}", b.sql);
    }

    #[test]
    fn limit_and_offset_go_to_the_suffix_for_pg() {
        let q = U::query().limit(10).offset(20);
        let b = sel(&q, Dialect::Postgres);
        assert!(b.sql.ends_with(" LIMIT 10 OFFSET 20"), "got: {}", b.sql);
    }

    /// SQL Server 无 OFFSET 时走 TOP 前缀 —— 见「裁决 C」。
    #[test]
    fn mssql_limit_without_offset_uses_top_prefix() {
        let q = U::query().limit(10);
        let b = sel(&q, Dialect::Mssql);
        assert!(b.sql.starts_with("SELECT TOP (10) "), "got: {}", b.sql);
    }

    // ---- COUNT ----

    #[test]
    fn count_reuses_the_where_clause() {
        let q = U::query().filter("name", Op::Eq, "alice").unwrap();
        let b = build_select(&q, Dialect::Postgres, true);
        assert!(b.sql.starts_with("SELECT COUNT(*)"), "got: {}", b.sql);
        assert!(b.sql.contains("\"name\" = $1"), "got: {}", b.sql);
        assert_eq!(b.params, vec![json!("alice")]);
    }

    /// **COUNT 必须去掉 ORDER BY / LIMIT / OFFSET**：
    /// 留着 ORDER BY 让数据库白排一次序；留着 LIMIT 会**数出错误的行数**
    /// （total 变成「当前页的行数」—— 静默错值，分页 UI 直接算错页数）。
    #[test]
    fn count_drops_order_limit_and_offset() {
        let q = U::query()
            .filter("name", Op::Eq, "alice").unwrap()
            .order_by("id", Order::Desc).unwrap()
            .limit(10)
            .offset(20);
        let b = build_select(&q, Dialect::Postgres, true);
        assert!(!b.sql.contains("ORDER BY"), "COUNT 不得带 ORDER BY: {}", b.sql);
        assert!(!b.sql.contains("LIMIT"), "COUNT 不得带 LIMIT: {}", b.sql);
        assert!(!b.sql.contains("OFFSET"), "COUNT 不得带 OFFSET: {}", b.sql);
    }

    /// COUNT 也不该带上 TOP 前缀（MSSQL）。
    #[test]
    fn mssql_count_has_no_top_prefix() {
        let q = U::query().limit(10);
        let b = build_select(&q, Dialect::Mssql, true);
        assert!(!b.sql.contains("TOP"), "COUNT 不得带 TOP: {}", b.sql);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm query::sql 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

- [ ] **Step 3: 实现**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! SQL 文本与绑定参数**同时生成**。
//!
//! 为什么不分两步：`$1` / `@P1` 这类编号占位符的编号依赖参数**顺序**，
//! 而顺序又由 WHERE 子句的结构决定。先生成 SQL、再按同样顺序收集参数的写法，
//! 一旦某处漏收或多收（比如软删除条件被算进参数位），编号就会整体错位 ——
//! 在编号占位符方言上是**静默取错值**，不是报错。

use ecat_data::Dialect;
use serde_json::Value;

use super::Expr;
use super::Query;
use crate::dialect::InsertPlan;
use crate::dialect::dialect::lookup; // 按实际模块路径调整
use crate::entity::Entity;

/// 生成好的语句与它的参数。
#[derive(Debug, Clone, PartialEq)]
pub struct Built {
    pub sql: String,
    pub params: Vec<Value>,
}

/// 顺次发号，避免手写计数器。
struct Placeholders<'a> {
    spec: &'a dyn crate::dialect::DialectSpec,
    next: usize,
}

impl<'a> Placeholders<'a> {
    fn new(spec: &'a dyn crate::dialect::DialectSpec) -> Self {
        Self { spec, next: 0 }
    }
    fn take(&mut self) -> String {
        self.next += 1;
        self.spec.placeholder(self.next)
    }
    fn used(&self) -> usize {
        self.next
    }
}

/// `for_count` 为真时生成 COUNT 查询：**去掉 ORDER BY / LIMIT / OFFSET**。
pub(crate) fn build_select<E: Entity, S>(q: &Query<E, S>, d: Dialect, for_count: bool) -> Built {
    let spec = lookup(d);
    let meta = E::META;
    let mut ph = Placeholders::new(spec);
    let mut params: Vec<Value> = Vec::new();

    let cols = if for_count {
        "COUNT(*)".to_string()
    } else {
        meta.columns
            .iter()
            .map(|c| spec.quote(c.name))
            .collect::<Vec<_>>()
            .join(", ")
    };

    // 分页片段：COUNT 不带。见「裁决 C」——它是 prefix + suffix。
    let limit = if for_count {
        crate::dialect::Limit::none()
    } else {
        spec.limit_clause(
            q.limit.unwrap_or(u64::MAX),
            q.offset.unwrap_or(0),
            !q.orders.is_empty(),
        )
    };

    // LIMIT 未设置时不要生成分页片段：limit_clause(u64::MAX) 会产出
    // `LIMIT 18446744073709551615`，语义上等价但很难看，且 MSSQL 上
    // `TOP (18446744073709551615)` 会超出 int 范围。
    let limit = if q.limit.is_none() && q.offset.is_none() {
        crate::dialect::Limit::none()
    } else {
        limit
    };

    let mut sql = format!("SELECT {}{cols} FROM {}", limit.prefix, spec.quote(meta.table));

    for (table, on) in &q.joins {
        // 表名与 ON 条件都是调用方给的字符串 —— 信任边界在调用方（见 rustdoc）。
        sql.push_str(&format!(" JOIN {} ON {on}", spec.quote(table)));
    }

    // ---- WHERE ----
    let mut conds: Vec<String> = Vec::new();

    // 软删除闸门。**注意：它不分配占位符** —— 是字面量 IS NULL。
    if let Some(sd) = meta.flags.soft_delete
        && !q.with_trashed
    {
        conds.push(format!("{} IS NULL", spec.quote(sd)));
    }

    for f in &q.filters {
        match f {
            Expr::Cmp { column, op, value } => {
                let p = ph.take();
                conds.push(format!("{} {} {p}", spec.quote(column), op.as_sql()));
                params.push(value.clone());
            }
            Expr::In { column, values, negated } => {
                let q_col = spec.quote(column);
                if values.is_empty() {
                    // `IN ()` 是语法错误；空集合语义上恒假。
                    conds.push(if *negated { "1 = 1".into() } else { "1 = 0".into() });
                } else {
                    let list = values
                        .iter()
                        .map(|v| {
                            let p = ph.take();
                            params.push(v.clone());
                            p
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let not = if *negated { "NOT " } else { "" };
                    conds.push(format!("{q_col} {not}IN ({list})"));
                }
            }
            Expr::Null { column, negated } => {
                let q_col = spec.quote(column);
                conds.push(if *negated {
                    format!("{q_col} IS NOT NULL")
                } else {
                    format!("{q_col} IS NULL")
                });
            }
            Expr::Raw(expr) => conds.push(expr.clone()),
        }
    }

    if !conds.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conds.join(" AND "));
    }

    // ---- ORDER BY ----
    if !for_count && !q.orders.is_empty() {
        let parts = q
            .orders
            .iter()
            .map(|o| format!("{} {}", spec.quote(&o.column), o.dir.as_sql()))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" ORDER BY {parts}"));
    }

    sql.push_str(&limit.suffix);

    debug_assert_eq!(
        ph.used(),
        params.len(),
        "占位符数与参数数必须相等 —— 不等就是某处漏收/多收了参数"
    );

    Built { sql, params }
}
```

⚠️ `use crate::dialect::dialect::lookup;` 是**错的**，本 crate 的路径是 `crate::dialect::lookup`。写实现时用：

```rust
use crate::dialect::{DialectSpec, Limit, lookup};
```

同理 `InsertPlan` 在本文件不需要（它是 `crud.rs` 的事），别多 import —— `-D warnings` 会把 unused import 判死。

- [ ] **Step 4: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm query::sql 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
```

期望：18 passed。

- [ ] **Step 5: 闸门 + 提交**

```bash
cd /home/wwwroot/e-cat
cargo fmt --check && cargo clippy -p ecat-orm --all-targets -- -D warnings; echo "rc=$?"
git add ecat-orm/src/query/sql.rs
git commit -m "feat(ecat-orm): SELECT/COUNT 生成（SQL 与参数同步产出）"
```

### ⚠️ Task 12 必读：三条容易写出**静默错值**的地方

这三条都不会报错，只会给出错误结果 —— 是本次实施里最危险的形态：

1. **软删除条件分配了占位符。** `WHERE deleted_at IS NULL AND name = $1` 是对的；`WHERE deleted_at = $1 AND name = $2` 是错的（且 `$1` 会绑定到 NULL 上恒假，查询**静默返回空集**）。测试 `soft_delete_filter_does_not_consume_a_parameter_slot` 钉住这一条。

2. **COUNT 带上了 LIMIT。** `SELECT COUNT(*) ... LIMIT 10` 在多数后端返回 10（或更少），于是 `total` 变成「本页行数」—— 分页 UI 算出 1 页。测试 `count_drops_order_limit_and_offset` 钉住。

3. **`debug_assert_eq!(ph.used(), params.len())` 被当成多余的检查删掉。** 它是**唯一**能在开发期抓到「占位符与参数错位」的哨兵。保留它。它在 release 下不生效，代价是零。

**注意它为什么只是 `debug_assert`**：它验证的是「我们自己数的」与「我们自己收集的」相等 —— 是内部一致性，不是对数据库的断言。真正的验证在 SQLite 集成测试（Task 18）里。

---

## Task 13: CRUD（`crud.rs`）

**Files:**
- Create: `ecat-orm/src/crud.rs`
- Modify: `ecat-orm/src/entity.rs`（给 `Entity` 加 CRUD 默认方法）
- Modify: `ecat-orm/src/lib.rs`

**API 形态取自 spec §5.4**：`User::insert(&db, &user)`。这要求 CRUD 是 `Entity` 的**关联函数**（不是自由函数），所以实现放在 `crud.rs`、以 `Entity` 上的默认方法暴露。

**AFIT（async fn in trait）的 `Send` 问题**：`Entity` 的默认方法返回 `impl Future`。默认的 AFIT future **不保证 `Send`** —— 用户一旦在 `tokio::spawn` 里用它就会编译失败。因此返回类型显式写 `+ Send`，并依赖 `SqlExecutor: Send + Sync`（`ecat-data/src/rdbms.rs:172`）来满足它。

- [ ] **Step 1: 写失败测试**

`ecat-orm/src/crud.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::*;
    use async_trait::async_trait;
    use ecat_data::{Dialect, RdbmsError, Row, SqlExecutor};
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::Mutex;

    /// 记录收到的 (sql, params) 的假 executor。
    #[derive(Default, Clone)]
    struct Spy {
        calls: Arc<Mutex<Vec<(String, Vec<serde_json::Value>)>>>,
        rows: Arc<Mutex<Vec<Row>>>,
        affected: u64,
    }

    #[async_trait]
    impl SqlExecutor for Spy {
        async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
            self.calls.lock().unwrap().push((sql.into(), vec![]));
            Ok(self.affected)
        }
        async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
            self.calls.lock().unwrap().push((sql.into(), vec![]));
            Ok(self.rows.lock().unwrap().clone())
        }
        async fn execute_with(&self, sql: &str, p: &[serde_json::Value]) -> Result<u64, RdbmsError> {
            self.calls.lock().unwrap().push((sql.into(), p.to_vec()));
            Ok(self.affected)
        }
        async fn query_with(&self, sql: &str, p: &[serde_json::Value]) -> Result<Vec<Row>, RdbmsError> {
            self.calls.lock().unwrap().push((sql.into(), p.to_vec()));
            Ok(self.rows.lock().unwrap().clone())
        }
        fn dialect(&self) -> Dialect { Dialect::Sqlite }
    }

    // 实体：自增主键 + 一个普通列
    static COLS: [ColumnMeta; 2] = [
        ColumnMeta { name: "id", ty: ColType::I64, nullable: false, pk: true, auto_increment: true },
        ColumnMeta { name: "name", ty: ColType::Text, nullable: false, pk: false, auto_increment: false },
    ];
    static META: EntityMeta = EntityMeta {
        table: "users", pk: "id", columns: &COLS, relations: &[],
        flags: EntityFlags::NONE,
    };

    #[derive(Debug, Clone, PartialEq)]
    struct U { id: i64, name: String }

    impl Entity for U {
        const TABLE: &'static str = "users";
        const PK: &'static str = "id";
        const META: &'static EntityMeta = &META;
        fn from_row(r: &Row) -> Result<Self, OrmError> {
            Ok(U {
                id: crate::value::from_row_col::<i64>(r, "id")?,
                name: crate::value::from_row_col::<String>(r, "name")?,
            })
        }
        fn to_values(&self) -> Vec<(&'static str, serde_json::Value)> {
            vec![("id", json!(self.id)), ("name", json!(self.name))]
        }
        fn pk_value(&self) -> serde_json::Value { json!(self.id) }
    }

    fn last(spy: &Spy) -> (String, Vec<serde_json::Value>) {
        spy.calls.lock().unwrap().last().cloned().expect("no call recorded")
    }

    #[tokio::test]
    async fn insert_skips_the_auto_increment_pk() {
        let spy = Spy::default();
        U::insert(&spy, &U { id: 0, name: "alice".into() }).await.unwrap();
        let (sql, params) = last(&spy);
        assert!(!sql.contains("\"id\""), "自增主键不该出现在 INSERT 里: {sql}");
        assert!(sql.contains("\"name\""), "got: {sql}");
        assert_eq!(params, vec![json!("alice")], "id 不该被绑定");
    }

    #[tokio::test]
    async fn insert_returns_the_new_id() {
        // RETURNING 路径：假 executor 返回一行 [id=7]
        let spy = Spy::default();
        *spy.rows.lock().unwrap() =
            vec![Row::new(vec!["id".into()], vec![json!(7)])];
        let id = U::insert(&spy, &U { id: 0, name: "a".into() }).await.unwrap();
        assert_eq!(id, 7);
    }

    #[tokio::test]
    async fn find_by_id_returns_none_when_absent() {
        let spy = Spy::default(); // rows 为空
        assert_eq!(U::find_by_id(&spy, 1).await.unwrap(), None);
    }

    #[tokio::test]
    async fn find_by_id_maps_the_row() {
        let spy = Spy::default();
        *spy.rows.lock().unwrap() = vec![Row::new(
            vec!["id".into(), "name".into()],
            vec![json!(1), json!("alice")],
        )];
        assert_eq!(
            U::find_by_id(&spy, 1).await.unwrap(),
            Some(U { id: 1, name: "alice".into() })
        );
        let (sql, params) = last(&spy);
        assert!(sql.contains("WHERE \"id\" = ?"), "got: {sql}");
        assert_eq!(params, vec![json!(1)]);
    }

    #[tokio::test]
    async fn delete_by_pk_uses_the_pk_column() {
        let spy = Spy { affected: 1, ..Default::default() };
        let n = U::delete_by_id(&spy, 5).await.unwrap();
        assert_eq!(n, 1);
        let (sql, params) = last(&spy);
        assert!(sql.starts_with("DELETE FROM \"users\""), "got: {sql}");
        assert_eq!(params, vec![json!(5)]);
    }

    /// 找不到时返回 `NotFound` 而不是静默成功 —— 否则调用方以为删掉了。
    #[tokio::test]
    async fn delete_by_id_reports_missing_row() {
        let spy = Spy { affected: 0, ..Default::default() };
        let e = U::delete_by_id(&spy, 5).await.unwrap_err();
        assert!(matches!(e, OrmError::NotFound), "got: {e:?}");
    }

    #[tokio::test]
    async fn update_writes_only_non_pk_columns() {
        let spy = Spy { affected: 1, ..Default::default() };
        U::update(&spy, &U { id: 3, name: "bob".into() }).await.unwrap();
        let (sql, params) = last(&spy);
        assert!(sql.starts_with("UPDATE \"users\" SET"), "got: {sql}");
        assert!(!sql.contains("SET \"id\""), "主键不该出现在 SET 里: {sql}");
        assert!(sql.contains("\"name\" = ?"), "got: {sql}");
        assert!(sql.contains("WHERE \"id\" = ?"), "got: {sql}");
        assert_eq!(params, vec![json!("bob"), json!(3)], "SET 参数在前、WHERE 参数在后");
    }

    #[tokio::test]
    async fn update_reports_missing_row() {
        let spy = Spy { affected: 0, ..Default::default() };
        let e = U::update(&spy, &U { id: 99, name: "x".into() }).await.unwrap_err();
        assert!(matches!(e, OrmError::NotFound), "got: {e:?}");
    }

    /// `save` 对自增且主键为 0 的实体走 insert，否则走 update。
    #[tokio::test]
    async fn save_inserts_when_pk_is_unset() {
        let spy = Spy::default();
        *spy.rows.lock().unwrap() = vec![Row::new(vec!["id".into()], vec![json!(9)])];
        let id = U::save(&spy, &U { id: 0, name: "new".into() }).await.unwrap();
        assert_eq!(id, 9);
        assert!(last(&spy).0.starts_with("INSERT"), "got: {}", last(&spy).0);
    }

    #[tokio::test]
    async fn save_updates_when_pk_is_set() {
        let spy = Spy { affected: 1, ..Default::default() };
        U::save(&spy, &U { id: 4, name: "old".into() }).await.unwrap();
        assert!(last(&spy).0.starts_with("UPDATE"), "got: {}", last(&spy).0);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm crud 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

- [ ] **Step 3: 实现 `crud.rs`**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

use ecat_data::SqlExecutor;
use ecat_data::{Row, RdbmsError};
use serde_json::Value;

use crate::dialect::InsertPlan;
use crate::dialect::lookup;
use crate::entity::Entity;
use crate::error::OrmError;

/// 建 `INSERT` 的列清单与参数（**跳过自增主键**）。
fn insert_parts<E: Entity>(e: &E) -> (Vec<String>, Vec<Value>) {
    let all = e.to_values();
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    for (name, v) in all {
        let meta = E::META.column(name).expect("to_values returned an unknown column");
        if meta.pk && meta.auto_increment {
            continue;
        }
        cols.push(name.to_string());
        vals.push(v);
    }
    (cols, vals)
}

/// 建 `UPDATE` 的 SET 片段（**跳过主键**）。
fn update_parts<E: Entity>(e: &E) -> (Vec<String>, Vec<Value>) {
    let all = e.to_values();
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    for (name, v) in all {
        let meta = E::META.column(name).expect("to_values returned an unknown column");
        if meta.pk {
            continue;
        }
        cols.push(name.to_string());
        vals.push(v);
    }
    (cols, vals)
}

fn to_db(e: RdbmsError) -> OrmError {
    OrmError::Rdbms(e)
}

/// 从一行里取出主键回填值。
fn read_returned_pk<E: Entity>(rows: &[Row]) -> Result<i64, OrmError> {
    let row = rows.first().ok_or_else(|| {
        OrmError::Rdbms(RdbmsError::Database(
            "INSERT returned no row for the generated primary key".into(),
        ))
    })?;
    crate::value::from_row_col::<i64>(row, E::PK)
}

pub(crate) async fn insert<E, X>(db: &X, e: &E) -> Result<i64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let spec = lookup(db.dialect());
    let (cols, vals) = insert_parts(e);
    let plan = spec.insert_plan(E::TABLE, &cols, E::PK, vals.len());

    match plan {
        InsertPlan::Single { sql } => {
            // 一步式：RETURNING / OUTPUT INSERTED 都走 query_write —— 写路径需要
            // 返回结果，读写分离路由必须把它发到主库（`query_write` 的存在理由）。
            let rows = db.query_write(&sql, &vals).await.map_err(to_db)?;
            read_returned_pk::<E>(&rows)
        }
        InsertPlan::InsertThen { insert, fetch } => {
            // **两步式必须包事务**：LAST_INSERT_ID() 是连接作用域的，
            // 池下两次 query_write 可能落到不同连接，取回别的会话的值（静默错值）。
            // 见 spec:585-589。
            let tx = db.transaction().await.map_err(to_db)?;
            tx.execute_with(&insert, &vals).await.map_err(to_db)?;
            let rows = tx.query(&fetch).await.map_err(to_db)?;
            let id = read_returned_pk::<E>(&rows)?;
            tx.commit().await.map_err(to_db)?;
            Ok(id)
        }
    }
}

pub(crate) async fn find_by_id<E, X, K>(db: &X, pk: K) -> Result<Option<E>, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
    K: Into<Value>,
{
    let spec = lookup(db.dialect());
    let sql = format!(
        "SELECT {} FROM {} WHERE {} = {}",
        E::META.columns.iter().map(|c| spec.quote(c.name)).collect::<Vec<_>>().join(", "),
        spec.quote(E::TABLE),
        spec.quote(E::PK),
        spec.placeholder(1)
    );
    let rows = db.query_with(&sql, &[pk.into()]).await.map_err(to_db)?;
    rows.first().map(E::from_row).transpose()
}

pub(crate) async fn update<E, X>(db: &X, e: &E) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let spec = lookup(db.dialect());
    let (cols, mut vals) = update_parts(e);
    let set = cols
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{} = {}", spec.quote(c), spec.placeholder(i + 1)))
        .collect::<Vec<_>>()
        .join(", ");
    let pk_ph = spec.placeholder(vals.len() + 1);
    let sql = format!(
        "UPDATE {} SET {set} WHERE {} = {pk_ph}",
        spec.quote(E::TABLE),
        spec.quote(E::PK)
    );
    vals.push(e.pk_value());
    let n = db.execute_with(&sql, &vals).await.map_err(to_db)?;
    if n == 0 {
        return Err(OrmError::NotFound);
    }
    Ok(n)
}

pub(crate) async fn delete_by_id<E, X, K>(db: &X, pk: K) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
    K: Into<Value>,
{
    let spec = lookup(db.dialect());
    let sql = format!(
        "DELETE FROM {} WHERE {} = {}",
        spec.quote(E::TABLE),
        spec.quote(E::PK),
        spec.placeholder(1)
    );
    let n = db.execute_with(&sql, &[pk.into()]).await.map_err(to_db)?;
    if n == 0 {
        return Err(OrmError::NotFound);
    }
    Ok(n)
}
```

- [ ] **Step 4: 给 `Entity` 加默认方法**（加在 `entity.rs` 的 trait 里）

```rust
    /// 插入并返回新生成的主键。
    ///
    /// 自增主键不出现在列清单里（由数据库生成）。MySQL 路径会自动包事务 ——
    /// 见 `crud::insert` 里 `InsertPlan::InsertThen` 分支的注释。
    fn insert<X>(
        db: &X,
        entity: &Self,
    ) -> impl std::future::Future<Output = Result<i64, OrmError>> + Send
    where
        X: ecat_data::SqlExecutor + ?Sized,
        Self: Sync,
    {
        crate::crud::insert(db, entity)
    }

    fn find_by_id<X, K>(
        db: &X,
        pk: K,
    ) -> impl std::future::Future<Output = Result<Option<Self>, OrmError>> + Send
    where
        X: ecat_data::SqlExecutor + ?Sized,
        K: Into<serde_json::Value> + Send,
    {
        crate::crud::find_by_id(db, pk)
    }

    /// 按主键整行更新。影响 0 行时返回 [`OrmError::NotFound`]。
    fn update<X>(
        db: &X,
        entity: &Self,
    ) -> impl std::future::Future<Output = Result<u64, OrmError>> + Send
    where
        X: ecat_data::SqlExecutor + ?Sized,
        Self: Sync,
    {
        crate::crud::update(db, entity)
    }

    /// 按主键删除。影响 0 行时返回 [`OrmError::NotFound`]。
    fn delete_by_id<X, K>(
        db: &X,
        pk: K,
    ) -> impl std::future::Future<Output = Result<u64, OrmError>> + Send
    where
        X: ecat_data::SqlExecutor + ?Sized,
        K: Into<serde_json::Value> + Send,
    {
        crate::crud::delete_by_id(db, pk)
    }

    /// 主键为「未设置」时插入，否则更新。
    ///
    /// 「未设置」判定：主键字段的自增标志为真**且** `pk_value()` 等于 0。
    /// 非自增主键（如 UUID 字符串）永远走 update —— 它的主键从来不是 0。
    fn save<X>(
        db: &X,
        entity: &Self,
    ) -> impl std::future::Future<Output = Result<i64, OrmError>> + Send
    where
        X: ecat_data::SqlExecutor + ?Sized,
        Self: Sync,
    {
        crate::crud::save(db, entity)
    }
```

`crud.rs` 里补 `save`：

```rust
pub(crate) async fn save<E, X>(db: &X, e: &E) -> Result<i64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let pk_meta = E::META.column(E::PK).expect("PK must be a declared column");
    let unset = pk_meta.auto_increment && e.pk_value() == Value::from(0);
    if unset {
        insert(db, e).await
    } else {
        update(db, e).await?;
        // update 成功时主键值不变，把它原样返回，让 save 的返回类型
        // 在两条路径上一致（调用方不必分支）。
        e.pk_value().as_i64().ok_or_else(|| {
            OrmError::Rdbms(RdbmsError::Database(
                "primary key is not an integer; save() returns i64".into(),
            ))
        })
    }
}
```

`OrmError` 需补一个变体：

```rust
    /// 期望存在一行但影响行数为 0。
    #[error("row not found")]
    NotFound,
```

- [ ] **Step 5: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm crud 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
```

期望：11 passed。

- [ ] **Step 6: 闸门 + 提交**

```bash
cd /home/wwwroot/e-cat
cargo fmt --check && cargo clippy -p ecat-orm --all-targets -- -D warnings; echo "rc=$?"
git add ecat-orm/src/crud.rs ecat-orm/src/entity.rs ecat-orm/src/error.rs ecat-orm/src/lib.rs
git commit -m "feat(ecat-orm): CRUD（含 MySQL 两步式主键回填的事务包裹）"
```

### ⚠️ Task 13 必读：三条不能想当然的地方

1. **`InsertThen` 分支的事务不是「防御性编程」。** `LAST_INSERT_ID()` 是**连接**作用域的。没有事务，池下 `INSERT` 与 `SELECT LAST_INSERT_ID()` 是两次独立取用，可能落到不同连接 → 取回**别的会话刚插入的 id** → 静默错值。测试用假 executor 抓不到这个（假 executor 只有一条「连接」）；真正的验证在 Task 18 的 SQLite 多连接用例，或 spec 要求的「记录调用序列」用例。

2. **`update` 用 `execute_with` 而不是 `query_with`。** 不需要返回值，走 execute 让后端少一次结果集解析。**但读写分离后必须走主库** —— Task 17 的 `RdbmsRouting` 会把 `execute_with` 路由到 primary，这里不用特殊处理。

3. **`save` 的「未设置」判据是 `auto_increment && pk == 0`，不是 `pk == 0`。** 非自增主键（UUID 字符串、手工分配的整数）永远走 update —— 它的主键从来不是 0，用 `pk == 0` 判断会让手工分配的整数主键实体的 `save` **每次都变成 insert**（主键冲突或重复行）。

### ⚠️ Task 13 必读（二）：`impl Future + Send` 写法的代价

`Entity` 的默认方法用 `impl Future<Output = ...> + Send` 而不是 `async fn`，是为了让返回的 future 是 `Send`（否则用户在 `tokio::spawn` 里用不了）。

**代价**：每个默认方法多一个 `Self: Sync` / `K: Send` 约束，且编译器对「为什么这个 future 不是 Send」的报错会比较绕。

**若实施时遇到 Send 报错**，检查顺序：
1. `X: SqlExecutor` 是否带上了（`SqlExecutor: Send + Sync` 是它的 supertrait 约束）
2. `Self: Sync` 是否缺 —— `&Self` 要跨 await 点
3. `K: Send`（`find_by_id` / `delete_by_id`）
4. 最后才怀疑 `crud.rs` 内部有非 Send 的值跨了 await

**不要**为了消掉报错把 `+ Send` 去掉 —— 那会让 `tokio::spawn` 里的 ORM 调用全部编译失败，是把问题推给用户。

---

## Task 14: 自动行为（时间戳 / 软删除 / 乐观锁）

**Files:**
- Modify: `ecat-orm/src/crud.rs`
- Modify: `ecat-orm/src/error.rs`（已有 `OptimisticLockConflict`，本任务开始真正产生它）

**三条行为都由 `EntityMeta.flags` 驱动，且只在写路径生效**（spec:521-527）。

### ⚠️ spec 在 `created_at` 与 `updated_at` 上**故意不对称**，不要「改匀」

spec:522-523 的原文：

> - `created_at`：insert 时填当前时间（**字段为 `None` 才填，显式值优先**）
> - `updated_at`：insert 与 update 时**均填**

`created_at` 尊重显式值，`updated_at` 无条件覆盖。这不是疏漏，是有意的：
`created_at` 是**事实记录**（数据导入时要保留原始创建时间），`updated_at` 是**变更追踪**
（任何一次写入都让「最后修改时间」变成此刻，显式传旧值没有意义）。

**实施者不要因为「不对称看着别扭」把它改成一致。** 若你认为该改，先在报告里提出，
不要静默改掉 —— 静默改掉会让数据导入场景丢时间。

- [ ] **Step 1: 写失败测试**

追加到 `ecat-orm/src/crud.rs` 的测试模块：

```rust
    // ---- 自动行为 ----

    static AUTO_COLS: [ColumnMeta; 6] = [
        ColumnMeta { name: "id", ty: ColType::I64, nullable: false, pk: true, auto_increment: true },
        ColumnMeta { name: "name", ty: ColType::Text, nullable: false, pk: false, auto_increment: false },
        ColumnMeta { name: "created_at", ty: ColType::Timestamp, nullable: true, pk: false, auto_increment: false },
        ColumnMeta { name: "updated_at", ty: ColType::Timestamp, nullable: true, pk: false, auto_increment: false },
        ColumnMeta { name: "deleted_at", ty: ColType::Timestamp, nullable: true, pk: false, auto_increment: false },
        ColumnMeta { name: "version", ty: ColType::I64, nullable: false, pk: false, auto_increment: false },
    ];
    static AUTO_META: EntityMeta = EntityMeta {
        table: "docs",
        pk: "id",
        columns: &AUTO_COLS,
        relations: &[],
        flags: EntityFlags {
            created_at: Some("created_at"),
            updated_at: Some("updated_at"),
            soft_delete: Some("deleted_at"),
            version: Some("version"),
        },
    };

    #[derive(Debug, Clone)]
    struct D {
        id: i64,
        name: String,
        created_at: Option<time::OffsetDateTime>,
        updated_at: Option<time::OffsetDateTime>,
        deleted_at: Option<time::OffsetDateTime>,
        version: i64,
    }

    impl Entity for D {
        const TABLE: &'static str = "docs";
        const PK: &'static str = "id";
        const META: &'static EntityMeta = &AUTO_META;
        fn from_row(_r: &Row) -> Result<Self, OrmError> { todo!("本任务不测 from_row") }
        fn to_values(&self) -> Vec<(&'static str, serde_json::Value)> {
            vec![
                ("id", json!(self.id)),
                ("name", json!(self.name)),
                ("created_at", crate::value::ColumnValue::to_json(&self.created_at)),
                ("updated_at", crate::value::ColumnValue::to_json(&self.updated_at)),
                ("deleted_at", crate::value::ColumnValue::to_json(&self.deleted_at)),
                ("version", json!(self.version)),
            ]
        }
        fn pk_value(&self) -> serde_json::Value { json!(self.id) }
    }

    fn blank() -> D {
        D {
            id: 0,
            name: "x".into(),
            created_at: None,
            updated_at: None,
            deleted_at: None,
            version: 0,
        }
    }

    fn param_for(spy: &Spy, col: &str) -> serde_json::Value {
        let (sql, params) = last(spy);
        // 从 SQL 里找出列名，再取其位置对应的参数
        let cols: Vec<&str> = sql
            .split(['(', ')', ','])
            .map(str::trim)
            .filter(|s| s.starts_with('"') && s.ends_with('"'))
            .collect();
        let idx = cols
            .iter()
            .position(|c| c.trim_matches('"') == col)
            .unwrap_or_else(|| panic!("column {col} not in {sql}"));
        params[idx].clone()
    }

    #[tokio::test]
    async fn insert_fills_created_at_when_none() {
        let spy = Spy::default();
        *spy.rows.lock().unwrap() = vec![Row::new(vec!["id".into()], vec![json!(1)])];
        D::insert(&spy, &blank()).await.unwrap();
        let v = param_for(&spy, "created_at");
        assert!(v.is_string(), "created_at 应被填成 RFC3339 字符串，得到 {v}");
    }

    /// **显式值优先** —— created_at 是事实记录，导入时要保留原值。
    #[tokio::test]
    async fn insert_respects_an_explicit_created_at() {
        let spy = Spy::default();
        *spy.rows.lock().unwrap() = vec![Row::new(vec!["id".into()], vec![json!(1)])];
        let mut d = blank();
        d.created_at = Some(time::macros::datetime!(2020-01-01 00:00:00 UTC));
        D::insert(&spy, &d).await.unwrap();
        assert_eq!(param_for(&spy, "created_at"), json!("2020-01-01T00:00:00Z"));
    }

    /// **与上面相反**：updated_at 无条件覆盖（spec:523「均填」）。
    #[tokio::test]
    async fn insert_overwrites_an_explicit_updated_at() {
        let spy = Spy::default();
        *spy.rows.lock().unwrap() = vec![Row::new(vec!["id".into()], vec![json!(1)])];
        let mut d = blank();
        d.updated_at = Some(time::macros::datetime!(2020-01-01 00:00:00 UTC));
        D::insert(&spy, &d).await.unwrap();
        assert_ne!(
            param_for(&spy, "updated_at"),
            json!("2020-01-01T00:00:00Z"),
            "updated_at 在 insert 时应被覆盖为此刻（spec:523）"
        );
    }

    #[tokio::test]
    async fn update_always_refreshes_updated_at() {
        let spy = Spy { affected: 1, ..Default::default() };
        let mut d = blank();
        d.id = 1;
        d.updated_at = Some(time::macros::datetime!(2020-01-01 00:00:00 UTC));
        D::update(&spy, &d).await.unwrap();
        assert_ne!(param_for(&spy, "updated_at"), json!("2020-01-01T00:00:00Z"));
    }

    // ---- 乐观锁 ----

    #[tokio::test]
    async fn update_guards_on_version_and_bumps_it() {
        let spy = Spy { affected: 1, ..Default::default() };
        let mut d = blank();
        d.id = 1;
        d.version = 5;
        D::update(&spy, &d).await.unwrap();
        let (sql, params) = last(&spy);
        // WHERE 里有 version 条件
        assert!(sql.contains("\"version\" = ?"), "got: {sql}");
        // 且 version 被写成 6
        assert!(params.contains(&json!(6)), "version 应被 +1，params={params:?}");
        assert!(params.contains(&json!(5)), "WHERE 里应是旧值 5，params={params:?}");
    }

    /// 影响 0 行 = 版本不匹配（别人先改了）→ **必须是 OptimisticLockConflict**，
    /// 不能退化成 NotFound —— 两者对调用方的处理完全不同（重试 vs 报错）。
    #[tokio::test]
    async fn version_mismatch_reports_conflict_not_not_found() {
        let spy = Spy { affected: 0, ..Default::default() };
        let mut d = blank();
        d.id = 1;
        d.version = 5;
        let e = D::update(&spy, &d).await.unwrap_err();
        assert!(matches!(e, OrmError::OptimisticLockConflict), "got: {e:?}");
    }

    // ---- 软删除 ----

    #[tokio::test]
    async fn delete_is_a_soft_update_not_a_delete_statement() {
        let spy = Spy { affected: 1, ..Default::default() };
        D::delete_by_id(&spy, 1).await.unwrap();
        let (sql, params) = last(&spy);
        assert!(sql.starts_with("UPDATE \"docs\" SET"), "软删除必须是 UPDATE: {sql}");
        assert!(!sql.starts_with("DELETE"), "软删除不得发 DELETE: {sql}");
        assert!(sql.contains("\"deleted_at\" = ?"), "got: {sql}");
        assert!(sql.contains("\"deleted_at\" IS NULL"), "已软删的行不该被再删一次: {sql}");
        assert_eq!(params.len(), 2, "deleted_at 与 pk");
    }

    #[tokio::test]
    async fn hard_delete_is_still_available_explicitly() {
        let spy = Spy { affected: 1, ..Default::default() };
        D::hard_delete_by_id(&spy, 1).await.unwrap();
        let (sql, _) = last(&spy);
        assert!(sql.starts_with("DELETE FROM"), "got: {sql}");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm crud 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

- [ ] **Step 3: 实现**

在 `crud.rs` 顶部加时间源：

```rust
/// 当前时间。**不做可注入的全局时钟** —— 那是个进程级 `static`，
/// 一个测试设了固定时间会污染同二进制的其它测试（并行跑时行为随机）。
/// 需要的可测性由「断言性质而非精确值」提供：见本任务的
/// `insert_overwrites_an_explicit_updated_at` 用 `assert_ne!` 而非 `assert_eq!`。
fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}
```

`insert_parts` 改为按 flags 自动填充。**填充发生在构造列清单时，不修改实体本身** ——
`insert` 取 `&E`，没有可变性可用，也**不该**有（让 `insert` 偷偷改动调用方的实体是意外副作用）：

```rust
/// 按 `flags` 填入自动时间戳，返回该列的最终值。
///
/// `is_insert` 为真时 `created_at` 尊重显式值（字段为 `None` 才填）；
/// `updated_at` 无论 insert 还是 update 都无条件覆盖（spec:522-523 的不对称）。
fn auto_timestamp(
    flags_value: Option<&'static str>,
    column: &str,
    current: &Value,
    always_overwrite: bool,
) -> Option<Value> {
    if flags_value != Some(column) {
        return None;
    }
    if always_overwrite || current.is_null() {
        Some(crate::value::ColumnValue::to_json(&now()))
    } else {
        None // 保留显式值
    }
}
```

然后在 `insert_parts` 的循环里：

```rust
    for (name, mut v) in all {
        let meta = E::META.column(name).expect("to_values returned an unknown column");
        if meta.pk && meta.auto_increment {
            continue;
        }
        if let Some(filled) =
            auto_timestamp(E::META.flags.created_at, name, &v, false)
        {
            v = filled;
        }
        if let Some(filled) =
            auto_timestamp(E::META.flags.updated_at, name, &v, true)
        {
            v = filled;
        }
        cols.push(name.to_string());
        vals.push(v);
    }
```

`update_parts` 里对 `updated_at` 同样处理（`always_overwrite = true`），并处理乐观锁：

```rust
pub(crate) async fn update<E, X>(db: &X, e: &E) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let spec = lookup(db.dialect());
    let (cols, mut vals) = update_parts(e);
    let version_col = E::META.flags.version;

    let set = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            // version 列写的是**新值**（旧值 + 1）。旧值从实体上读，
            // 便于调用方在冲突后重新加载再试。
            let v = if Some(c.as_str()) == version_col {
                let old = vals[i].as_i64().unwrap_or(0);
                crate::value::ColumnValue::to_json(&(old + 1))
            } else {
                vals[i].clone()
            };
            let _ = v;
            format!("{} = {}", spec.quote(c), spec.placeholder(i + 1))
        })
        .collect::<Vec<_>>()
        .join(", ");

    // 把 version 的 SET 参数替换成 +1 后的值
    if let Some(vc) = version_col
        && let Some(pos) = cols.iter().position(|c| c.as_str() == vc)
    {
        let old = vals[pos].as_i64().unwrap_or(0);
        vals[pos] = crate::value::ColumnValue::to_json(&(old + 1));
    }

    let mut where_parts = vec![format!("{} = {}", spec.quote(E::PK), spec.placeholder(vals.len() + 1))];
    vals.push(e.pk_value());

    // 乐观锁：WHERE 里带上旧版本号。
    if let Some(vc) = version_col
        && let Some(pos) = cols.iter().position(|c| c.as_str() == vc)
    {
        // 注意：此时 vals[pos] 已被改成 +1，旧值要从实体重新取。
        let _ = pos;
        let old = e
            .to_values()
            .into_iter()
            .find(|(n, _)| *n == vc)
            .map(|(_, v)| v)
            .unwrap_or(Value::from(0));
        where_parts.push(format!("{} = {}", spec.quote(vc), spec.placeholder(vals.len() + 1)));
        vals.push(old);
    }

    let sql = format!(
        "UPDATE {} SET {set} WHERE {}",
        spec.quote(E::TABLE),
        where_parts.join(" AND ")
    );

    let n = db.execute_with(&sql, &vals).await.map_err(to_db)?;
    if n == 0 {
        // 带版本列的实体：0 行几乎总是版本不匹配（行被别人改过），
        // 而不是行不存在 —— 报 OptimisticLockConflict 让调用方知道该重试。
        return Err(if version_col.is_some() {
            OrmError::OptimisticLockConflict
        } else {
            OrmError::NotFound
        });
    }
    Ok(n)
}
```

⚠️ 上面 `update` 的草稿里有一处**多余的脚手架**（那个 `let v = …; let _ = v;` 的 `set` 闭包）—— 是我写岔了。**实现时按下面的干净版本写**：`set` 只负责生成 `col = placeholder`，version 的新值在**收集参数时**决定：

```rust
    let set = cols
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{} = {}", spec.quote(c), spec.placeholder(i + 1)))
        .collect::<Vec<_>>()
        .join(", ");

    // 参数顺序必须与 set 的列顺序严格一致。
    // version 列写入旧值 +1；其余原样。
    for (i, c) in cols.iter().enumerate() {
        if Some(c.as_str()) == version_col {
            let old = vals[i].as_i64().unwrap_or(0);
            vals[i] = Value::from(old + 1);
        }
    }
```

`delete_by_id` 改为软删除感知：

```rust
pub(crate) async fn delete_by_id<E, X, K>(db: &X, pk: K) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
    K: Into<Value>,
{
    let spec = lookup(db.dialect());
    let pk: Value = pk.into();

    let sql = match E::META.flags.soft_delete {
        Some(sd) => {
            // 软删除：UPDATE ... AND deleted_at IS NULL。
            // 加 IS NULL 条件是为了让「重复删除同一行」返回 0 行 → NotFound，
            // 而不是把 deleted_at 覆盖成新的时刻（那会让「何时删的」失真）。
            format!(
                "UPDATE {} SET {} = {} WHERE {} = {} AND {} IS NULL",
                spec.quote(E::TABLE),
                spec.quote(sd),
                spec.placeholder(1),
                spec.quote(E::PK),
                spec.placeholder(2),
                spec.quote(sd)
            )
        }
        None => format!(
            "DELETE FROM {} WHERE {} = {}",
            spec.quote(E::TABLE),
            spec.quote(E::PK),
            spec.placeholder(1)
        ),
    };

    let params = match E::META.flags.soft_delete {
        Some(_) => vec![crate::value::ColumnValue::to_json(&now()), pk],
        None => vec![pk],
    };

    let n = db.execute_with(&sql, &params).await.map_err(to_db)?;
    if n == 0 {
        return Err(OrmError::NotFound);
    }
    Ok(n)
}

/// 绕过软删除，真的发 `DELETE`。软删除实体有时确实需要物理删除
/// （合规要求、垃圾回收）—— 不提供这个口子会逼用户去用 `filter_raw` 拼 SQL。
pub(crate) async fn hard_delete_by_id<E, X, K>(db: &X, pk: K) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
    K: Into<Value>,
{
    let spec = lookup(db.dialect());
    let sql = format!(
        "DELETE FROM {} WHERE {} = {}",
        spec.quote(E::TABLE),
        spec.quote(E::PK),
        spec.placeholder(1)
    );
    let n = db.execute_with(&sql, &[pk.into()]).await.map_err(to_db)?;
    if n == 0 {
        return Err(OrmError::NotFound);
    }
    Ok(n)
}
```

再给 `Entity` 加 `hard_delete_by_id` 默认方法，签名与 `delete_by_id` 相同，转发到 `crud::hard_delete_by_id`。

- [ ] **Step 4: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm crud 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
```

期望：20 passed（Task 13 的 11 + 本任务 9）。

- [ ] **Step 5: 闸门 + 提交**

```bash
cd /home/wwwroot/e-cat
cargo fmt --check && cargo clippy -p ecat-orm --all-targets -- -D warnings; echo "rc=$?"
git add ecat-orm/src/crud.rs ecat-orm/src/entity.rs
git commit -m "feat(ecat-orm): 自动时间戳、软删除与乐观锁"
```

### ⚠️ Task 14 必读：为什么不做「可注入的时钟」

写这一节时我第一版写的是 `static NOW_FN: OnceLock<fn() -> OffsetDateTime>` + `set_now_fn()`，**然后撤掉了**。理由：

那是个**进程级可变全局**。`cargo test` 默认在一个二进制内并行跑所有测试，任何一个测试调 `set_now_fn` 之后，同二进制里**其它所有**用 `now()` 的测试都会拿到固定时间 —— 包括那些断言「时间应该接近此刻」的。失败会是随机的、和线程调度相关的。

批次 1 在 clickhouse 的 TTL 测试上已经吃过同族问题（时间依赖导致 ~0.67% 假失败，最后靠**消除时间依赖**而非放宽断言修掉，提交 `e2c2af6`）。

**本任务的可测性来自断言的选择**：`assert_ne!(value, 2020-01-01)` 这类**性质断言**既能证明「被覆盖了」，又不依赖具体时刻。**要写精确的自动填充时间断言时，改用「解析回来与此刻相差 < 1 分钟」的形式**，同样不需要注入时钟。

若将来确实需要冻结时钟（例如测试 `updated_at > created_at` 的严格序），正确做法是**把时间作为参数传进内部函数**（`fn insert_parts(e: &E, now: OffsetDateTime)`），让测试直接调内部函数 —— 而不是让全局状态变。**

---

## Task 15: 批量分块与分页（`batch.rs` + `page.rs`）

**Files:**
- Create: `ecat-orm/src/batch.rs`
- Create: `ecat-orm/src/page.rs`
- Modify: `ecat-orm/src/query/mod.rs`（加 `fetch` / `paginate`）
- Modify: `ecat-orm/src/entity.rs`（加 `insert_many` / `upsert` 默认方法）
- Modify: `ecat-orm/src/lib.rs`

**本任务的两条硬约束都来自 spec §5.5b、§5.6：**

1. **批量必须按方言参数上限分块。** 各后端单语句参数上限差异极大（SQL Server **2100**、SQLite 999、PG/MySQL 65535）。**不分块 = 几千行批量插入必然报错。** 分块大小 = `max_params_per_stmt / 每行参数数`（**向下取整，且至少为 1** —— 每行参数数超过上限时不能变成 0）。
2. **COUNT 查询必须复用同一 WHERE/JOIN 且去掉 ORDER BY / LIMIT / OFFSET。** Task 12 的 `build_select(..., for_count = true)` 已经做了，本任务负责调用它。

**`insert_many` 返回受影响行数，不返回 id 列表**（spec:504-506）：

> MySQL 的 `LAST_INSERT_ID()` 只给批量首行、SQLite 给末行，跨后端语义不可靠。

- [ ] **Step 1: 写失败测试**

`ecat-orm/src/batch.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// **分块数学**：上限 2100、每行 3 个参数 → 每块 700 行。
    #[test]
    fn chunk_size_divides_the_limit_by_columns_per_row() {
        assert_eq!(chunk_size(2100, 3), 700);
        assert_eq!(chunk_size(999, 2), 499);
    }

    /// 向下取整：700.33 行 → 700 行（**不能进位** —— 进位就超限了）。
    #[test]
    fn chunk_size_rounds_down() {
        assert_eq!(chunk_size(10, 3), 3); // 3.33 → 3
    }

    /// **每行参数数超过上限时至少为 1**：否则 chunk_size 返回 0，
    /// 分块循环要么死循环、要么一行都不插（静默丢数据）。
    #[test]
    fn chunk_size_never_returns_zero() {
        assert_eq!(chunk_size(2, 5), 1);
        assert_eq!(chunk_size(1, 100), 1);
    }

    #[test]
    fn splitting_covers_every_row_exactly_once() {
        let chunks = split_chunks(10, 3);
        assert_eq!(chunks, vec![3, 3, 3, 1]);
        assert_eq!(chunks.iter().sum::<usize>(), 10);
    }

    #[test]
    fn splitting_handles_an_exact_multiple() {
        let chunks = split_chunks(9, 3);
        assert_eq!(chunks, vec![3, 3, 3]);
    }

    #[test]
    fn splitting_nothing_yields_no_chunks() {
        assert!(split_chunks(0, 100).is_empty());
    }

    #[test]
    fn chunk_boundaries_align_with_the_param_limit() {
        // 2100 上限、每行 3 参 → 700 行/块。3 块 = 2100 行。
        let chunks = split_chunks(2100, chunk_size(2100, 3));
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.iter().sum::<usize>(), 2100);
    }
}
```

`ecat-orm/src/page.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_carries_items_and_totals() {
        let p = Page { items: vec![1, 2, 3], total: Some(42), page: 2, per_page: 3 };
        assert_eq!(p.items.len(), 3);
        assert_eq!(p.total, Some(42));
        assert_eq!(p.page, 2);
        assert_eq!(p.per_page, 3);
    }

    #[test]
    fn total_is_optional_for_count_less_paging() {
        let p: Page<i32> = Page { items: vec![], total: None, page: 1, per_page: 20 };
        assert!(p.total.is_none());
    }

    /// 总页数用**向上取整**：42 条 / 每页 20 → 3 页。
    /// 用整除会得到 2 页，最后 2 条无处可去。
    #[test]
    fn total_pages_rounds_up() {
        let p = Page { items: vec![1], total: Some(42), page: 1, per_page: 20 };
        assert_eq!(p.total_pages(), Some(3));
    }

    #[test]
    fn total_pages_is_exact_on_a_multiple() {
        let p = Page { items: vec![1], total: Some(40), page: 1, per_page: 20 };
        assert_eq!(p.total_pages(), Some(2));
    }

    /// 0 条 → 0 页，不是 1 页。空结果的页数是 0。
    #[test]
    fn zero_rows_yields_zero_pages() {
        let p = Page { items: vec![], total: Some(0), page: 1, per_page: 20 };
        assert_eq!(p.total_pages(), Some(0));
    }

    /// 没有 total 时也算不出总页数 —— 返回 None 而不是猜。
    #[test]
    fn total_pages_is_none_without_total() {
        let p: Page<i32> = Page { items: vec![], total: None, page: 1, per_page: 20 };
        assert!(p.total_pages().is_none());
    }

    /// `per_page == 0` 不能让 `total_pages` 除零 panic。
    #[test]
    fn zero_per_page_does_not_panic() {
        let p = Page { items: vec![], total: Some(10), page: 1, per_page: 0 };
        assert_eq!(p.total_pages(), None);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm 'batch::' 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
cargo test -p ecat-orm 'page::' 2>&1 | tail -15; echo "rc=${PIPESTATUS[0]}"
```

- [ ] **Step 3: 实现 `batch.rs` 的分块数学（纯函数，先测它）**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

/// 每块能放多少行。
///
/// **必须至少为 1**：每行参数数超过方言上限时（例如 SQL Server 的 2100 而
/// 实体有 3000 列 —— 极端但合法），向下取整会得到 0，分块循环就再也前进不了。
/// 那时单行本身就会超限并报错，**那是数据库该给的信息**，不该被我们的
/// 分块逻辑掩盖成死循环或空插入。
pub(crate) fn chunk_size(max_params: usize, params_per_row: usize) -> usize {
    if params_per_row == 0 {
        return 1.max(max_params);
    }
    (max_params / params_per_row).max(1)
}

/// 把 `total` 行切成每块最多 `per_chunk` 行的尺寸列表。
///
/// 返回**尺寸**而不是索引区间：调用方拿它去 `chunks()` 更直接，
/// 也便于测试直接断言尺寸序列。
pub(crate) fn split_chunks(total: usize, per_chunk: usize) -> Vec<usize> {
    if total == 0 {
        return Vec::new();
    }
    let per_chunk = per_chunk.max(1);
    let mut out = Vec::new();
    let mut left = total;
    while left > 0 {
        let take = left.min(per_chunk);
        out.push(take);
        left -= take;
    }
    out
}
```

- [ ] **Step 4: 实现 `insert_many` / `upsert`（`batch.rs` 其余部分）**

```rust
pub(crate) async fn insert_many<E, X>(db: &X, entities: &[E]) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    if entities.is_empty() {
        return Ok(0); // 空输入不发语句 —— 生成 `VALUES ` 是语法错误
    }
    let spec = lookup(db.dialect());
    // 每行参数数 = 可插列数。用第一行推得，同一实体的各行列数一致。
    let params_per_row = insert_parts(&entities[0]).0.len();
    if params_per_row == 0 {
        return Err(OrmError::Rdbms(RdbmsError::Database(
            "entity has no insertable columns".into(),
        )));
    }
    let per_chunk = chunk_size(spec.max_params_per_stmt(), params_per_row);

    let mut total = 0u64;
    let mut start = 0usize;
    for size in split_chunks(entities.len(), per_chunk) {
        let slice = &entities[start..start + size];
        total += insert_chunk::<E, X>(db, slice, spec).await?;
        start += size;
    }
    Ok(total)
}

async fn insert_chunk<E, X>(
    db: &X,
    rows: &[E],
    spec: &dyn DialectSpec,
) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let (cols, _) = insert_parts(&rows[0]);
    let mut params = Vec::with_capacity(cols.len() * rows.len());
    let mut tuples = Vec::with_capacity(rows.len());
    let mut n = 0usize;
    for r in rows {
        let (_, vals) = insert_parts(r);
        let ph = vals
            .iter()
            .map(|v| {
                n += 1;
                params.push(v.clone());
                spec.placeholder(n)
            })
            .collect::<Vec<_>>()
            .join(", ");
        tuples.push(format!("({ph})"));
    }
    let sql = format!(
        "INSERT INTO {} ({}) VALUES {}",
        spec.quote(E::TABLE),
        cols.iter().map(|c| spec.quote(c)).collect::<Vec<_>>().join(", "),
        tuples.join(", ")
    );
    db.execute_with(&sql, &params).await.map_err(to_db)
}
```

`upsert` 单行版：

```rust
pub(crate) async fn upsert<E, X>(db: &X, e: &E) -> Result<u64, OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let spec = lookup(db.dialect());
    let (cols, vals) = insert_parts(e);
    let sql = spec.upsert(E::TABLE, &cols, E::PK, vals.len());
    db.execute_with(&sql, &vals).await.map_err(to_db)
}
```

- [ ] **Step 5: 实现 `page.rs`**

```rust
// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

/// 一页数据。
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// `paginate_without_count` 时为 `None`（省掉一次 COUNT 全表扫描）。
    pub total: Option<u64>,
    /// 页码，**从 1 开始**。
    pub page: u64,
    pub per_page: u64,
}

impl<T> Page<T> {
    /// 总页数。`total` 为 `None` 或 `per_page == 0` 时返回 `None`
    /// —— 算不出来就如实说算不出来，不猜。
    pub fn total_pages(&self) -> Option<u64> {
        let total = self.total?;
        if self.per_page == 0 {
            return None;
        }
        // 向上取整：40 条 / 每页 20 = 2 页；42 条 = 3 页（整除会丢最后 2 条）。
        Some(total.div_ceil(self.per_page))
    }

    pub fn has_next(&self) -> bool {
        self.total_pages().is_some_and(|t| self.page < t)
    }
}
```

- [ ] **Step 6: 给 `Query` 加 `fetch` / `paginate` / `paginate_without_count`**

```rust
impl<E: Entity, S> Query<E, S> {
    /// 执行查询。
    pub async fn fetch<X>(&self, db: &X) -> Result<Vec<E>, OrmError>
    where
        X: SqlExecutor + ?Sized,
    {
        let built = sql::build_select(self, db.dialect(), false);
        let rows = db.query_with(&built.sql, &built.params).await.map_err(OrmError::Rdbms)?;
        rows.iter().map(E::from_row).collect()
    }

    /// 取一页，并额外发一条 COUNT 得到 `total`（共 2 次查询）。
    pub async fn paginate<X>(&self, db: &X, page: u64, per_page: u64) -> Result<Page<E>, OrmError>
    where
        X: SqlExecutor + ?Sized,
    {
        let count = sql::build_select(self, db.dialect(), true);
        let rows = db
            .query_with(&count.sql, &count.params)
            .await
            .map_err(OrmError::Rdbms)?;
        let total = rows
            .first()
            .map(|r| crate::value::from_row_col::<i64>(r, "COUNT(*)"))
            .transpose()?
            .unwrap_or(0)
            .max(0) as u64;

        let items = self.page_items(db, page, per_page).await?;
        Ok(Page { items, total: Some(total), page, per_page })
    }

    /// 取一页但不数总数（1 次查询）。大表深层翻页时 `COUNT(*)` 全表扫描
    /// 的代价高于取一页数据本身。
    pub async fn paginate_without_count<X>(
        &self,
        db: &X,
        page: u64,
        per_page: u64,
    ) -> Result<Page<E>, OrmError>
    where
        X: SqlExecutor + ?Sized,
    {
        let items = self.page_items(db, page, per_page).await?;
        Ok(Page { items, total: None, page, per_page })
    }

    async fn page_items<X>(&self, db: &X, page: u64, per_page: u64) -> Result<Vec<E>, OrmError>
    where
        X: SqlExecutor + ?Sized,
    {
        // 页码从 1 开始；`page == 0` 当作第 1 页而不是 offset 溢出。
        let page = page.max(1);
        let offset = (page - 1).saturating_mul(per_page);
        let q = self.clone_with_limit(per_page, offset);
        q.fetch(db).await
    }
}
```

⚠️ `Page<T>` 需要 `T: Clone` 吗？不需要 —— `page_items` 返回 `Vec<E>` 直接装进 `Page`。
但 `self.clone_with_limit(...)` 需要 `Query` 可克隆 —— 加 `#[derive(Clone)]` 到 `Query`
（`PhantomData` 与 `&'static` 都可克隆，`Expr` 已是 `Clone`）。**注意 `E` 不必 `Clone`** ——
用 `PhantomData<fn() -> E>`（Task 11 已这么写）就不要求 `E: Clone`。

`clone_with_limit` 写成：

```rust
impl<E: Entity, S> Query<E, S> {
    fn clone_with_limit(&self, limit: u64, offset: u64) -> Self {
        Self {
            meta: self.meta,
            filters: self.filters.clone(),
            orders: self.orders.clone(),
            joins: self.joins.clone(),
            limit: Some(limit),
            offset: Some(offset),
            with_trashed: self.with_trashed,
            _marker: PhantomData,
        }
    }
}
```

- [ ] **Step 7: 给 `Entity` 加 `insert_many` / `upsert` 默认方法**

```rust
    /// 批量插入，返回受影响行数。
    ///
    /// **不返回 id 列表** —— MySQL 的 `LAST_INSERT_ID()` 只给批量首行、
    /// SQLite 给末行，跨后端语义不可靠（spec:504-506）。需要逐行 id 时
    /// 循环调 `insert`（或包在一个事务里）。
    fn insert_many<X>(
        db: &X,
        entities: &[Self],
    ) -> impl std::future::Future<Output = Result<u64, OrmError>> + Send
    where
        X: ecat_data::SqlExecutor + ?Sized,
        Self: Sync,
    {
        crate::batch::insert_many(db, entities)
    }

    /// 按主键 upsert（方言各自用 `ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE`）。
    fn upsert<X>(
        db: &X,
        entity: &Self,
    ) -> impl std::future::Future<Output = Result<u64, OrmError>> + Send
    where
        X: ecat_data::SqlExecutor + ?Sized,
        Self: Sync,
    {
        crate::batch::upsert(db, entity)
    }
```

- [ ] **Step 8: 跑测试确认通过**

```bash
cd /home/wwwroot/e-cat && cargo test -p ecat-orm 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
```

- [ ] **Step 9: 闸门 + 提交**

```bash
cd /home/wwwroot/e-cat
cargo fmt --check && cargo clippy -p ecat-orm --all-targets -- -D warnings; echo "rc=$?"
git add ecat-orm/src/batch.rs ecat-orm/src/page.rs ecat-orm/src/query/mod.rs ecat-orm/src/entity.rs ecat-orm/src/lib.rs
git commit -m "feat(ecat-orm): 批量分块与分页"
```

### ⚠️ Task 15 必读：`chunk_size` 返回 0 会让批量插入**静默丢数据**

`max_params / params_per_row` 在 `params_per_row > max_params` 时是 0。若 `split_chunks` 拿到 `per_chunk = 0`：

- 写成 `while left > 0 { take = left.min(0) = 0; left -= 0 }` → **死循环**
- 写成 `entities.chunks(0)` → **`chunks()` 对 0 会 panic**
- 写成 `if take == 0 { break }` → **一行都不插，返回 0，调用方以为成功**

三种都糟。`.max(1)` 把这种情况变成「每块一行」——单行超限时会得到数据库的参数过多报错，**那是正确且可诊断的行为**。

测试 `chunk_size_never_returns_zero` 钉住这一条。**不要**因为「实际不会有 2100 列的实体」就删掉它 —— 列数是用户定义的，`#[derive(Entity)]` 不限制。

---

## Task 16: 关联预加载（`relation.rs`）

**Files:**
- Modify: `ecat-orm/src/relation.rs`
- Modify: `ecat-orm/src/query/mod.rs`（加 `with`）

**目标（spec:444）**：一次 `IN` 查询取回全部关联，**杜绝 N+1**。

**算法**（`with(&[UserRelation::Posts])`）：

1. 先取本页的主体行（已有 `fetch` 的结果）。
2. 对每个被请求的关联：
   - 从 `EntityMeta.relation(name)` 取 `kind` / `target_table` / `foreign_key` / `local_key`
   - 收集本页主体的 `local_key` 值集合
   - **按目标方言的参数上限分块**（复用 Task 15 的 `chunk_size` / `split_chunks`），
     发 `SELECT * FROM <target_table> WHERE <foreign_key> IN (…)`
   - 按 `foreign_key` 把结果分组，回填到每个主体

**三种关联都做**（HasMany / HasOne / BelongsTo）。查询形态统一是
`SELECT * FROM <目标表> WHERE <目标列> IN (…)`，靠 `from_row` **按列名取值**
来解析 —— 因此**不需要目标表的列元数据**，`RelationMeta` 里的
`target_table` + `foreign_key` + `local_key` 就够。

用 `SELECT *` 而不是显式列清单，是本设计里**唯一**一处放宽列清单纪律的地方：
主体查询始终列全列（Task 12），关联查询用 `*`。理由是这里的目标表列清单
在编译期拿不到（`RelationMeta` 只有表名），而 `from_row` 按名字取值，
多出来的列会被忽略、缺列会响亮报错 —— 安全性不依赖列清单。

**写回靠派生宏生成的 `set_relation`**（见 Task 8 补充）—— `Entity` trait 无法
泛型地访问「`posts` 字段」，只有宏知道每个关联对应哪个字段、以及目标类型。

- [ ] **Step 1: 写失败测试**

追加到 `ecat-orm/src/relation.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_buckets_rows_by_key() {
        let rows = vec![
            (Value::from(1), "a".to_string()),
            (Value::from(1), "b".to_string()),
            (Value::from(2), "c".to_string()),
        ];
        let g = group_by_key(rows);
        assert_eq!(g.get(&Value::from(1)).map(Vec::len), Some(2));
        assert_eq!(g.get(&Value::from(2)).map(Vec::len), Some(1));
        assert_eq!(g.get(&Value::from(3)), None);
    }

    /// 主体列表里的重复 local_key 只该查一次 —— 否则 100 行同属一个用户时
    /// 会往 IN 列表里塞 100 个相同的 id。
    #[test]
    fn distinct_keys_are_deduplicated() {
        let keys = distinct_keys(&[Value::from(1), Value::from(1), Value::from(2)]);
        assert_eq!(keys.len(), 2);
    }

    /// NULL 的 local_key 不参与 IN 查询：`IN (NULL)` 恒为 UNKNOWN，
    /// 查不出任何行，只是白跑一趟。
    #[test]
    fn null_keys_are_skipped() {
        let keys = distinct_keys(&[Value::from(1), Value::Null]);
        assert_eq!(keys, vec![Value::from(1)]);
    }

    #[test]
    fn empty_subject_list_issues_no_query() {
        assert!(distinct_keys(&[]).is_empty());
    }

    /// **HasOne / BelongsTo 只取第一行。**
    #[test]
    fn single_valued_relations_take_only_the_first_row() {
        assert!(is_single_valued(RelationKind::HasOne));
        assert!(is_single_valued(RelationKind::BelongsTo));
        assert!(!is_single_valued(RelationKind::HasMany));
    }

    /// **三个方向各自的「两边是哪一列」必须分清** —— 写反了会查出一堆无关行
    /// （不报错，只是结果错）。
    #[test]
    fn join_sides_are_derived_per_kind() {
        // HasMany: 本表 local_key ← 目标表 foreign_key
        let (mine, theirs) = join_sides(RelationKind::HasMany);
        assert_eq!(mine, Side::Subject);
        assert_eq!(theirs, Side::Target);
        // HasOne 同 HasMany
        let (mine, theirs) = join_sides(RelationKind::HasOne);
        assert_eq!(mine, Side::Subject);
        assert_eq!(theirs, Side::Target);
        // BelongsTo: 反过来 —— 本表 foreign_key ← 目标表 local_key
        let (mine, theirs) = join_sides(RelationKind::BelongsTo);
        assert_eq!(mine, Side::Target);
        assert_eq!(theirs, Side::Subject);
    }

    /// `chunk_size` 为 1 时（列数超过方言上限）IN 列表也要逐块发，
    /// 不能一口气全塞进去。
    #[test]
    fn in_list_is_chunked_by_the_dialect_limit() {
        // SQL Server 2100，每行 1 个参数 → 2100 个 key 一块
        let chunks = crate::batch::split_chunks(5000, crate::batch::chunk_size(2100, 1));
        assert_eq!(chunks.sum::<usize>(), 5000);
        assert!(chunks.len() >= 3);
    }
}
```

- [ ] **Step 2: 实现**

三个纯函数（可单测），与执行部分分开：

```rust
/// 关联的方向。决定 `local_key` / `foreign_key` 哪一列属于谁。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    /// 主体表（发起查询的那张）。
    Subject,
    /// 被查的目标表。
    Target,
}

/// 返回 `(取值的一侧, 匹配的一侧)`：
/// 从 `取值的一侧` 收集值，去 `匹配的一侧` 的列上找。
///
/// - HasMany / HasOne：主体在 `local_key` 上有值，去目标的 `foreign_key` 里找
/// - BelongsTo：主体在 `foreign_key` 上有值，去目标的 `local_key` 里找
///
/// **写反了不会报错，只会查出一堆无关行。**
pub(crate) fn join_sides(kind: RelationKind) -> (Side, Side) {
    match kind {
        RelationKind::HasMany | RelationKind::HasOne => (Side::Subject, Side::Target),
        RelationKind::BelongsTo => (Side::Target, Side::Subject),
    }
}

/// 单值关联（取第一行）还是多值（全部）。
pub(crate) fn is_single_valued(kind: RelationKind) -> bool {
    matches!(kind, RelationKind::HasOne | RelationKind::BelongsTo)
}

/// 主体侧键值的去重集合。**跳过 NULL**（`IN (NULL)` 恒 UNKNOWN）。
pub(crate) fn distinct_keys(keys: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for k in keys {
        if k.is_null() {
            continue;
        }
        if !out.contains(k) {
            out.push(k.clone());
        }
    }
    out
}

/// 按关联键分组。
pub(crate) fn group_by_key<K>(rows: Vec<(Value, K)>) -> std::collections::HashMap<Value, Vec<K>> {
    let mut g: std::collections::HashMap<Value, Vec<K>> = std::collections::HashMap::new();
    for (k, v) in rows {
        g.entry(k).or_default().push(v);
    }
    g
}
```

执行部分：

```rust
/// 为一个关联发**一条**（或分块后少数几条）查询，把结果按主体分组写回。
///
/// 写回走 `Entity::set_relation` —— `RelationMeta` 里只有表名，
/// 只有派生宏知道「`posts` 这个关联对应 `posts` 字段、目标类型是 `Post`」。
pub(crate) async fn load_relation<E, X>(
    db: &X,
    subjects: &mut [E],
    name: &str,
) -> Result<(), OrmError>
where
    E: Entity,
    X: SqlExecutor + ?Sized,
{
    let meta = E::META
        .relation(name)
        .ok_or_else(|| OrmError::UnknownColumn(format!("unknown relation `{name}`")))?;

    let (value_side, match_side) = join_sides(meta.kind);
    let (subject_col, target_col) = match value_side {
        // Subject 侧作为取值来源
        Side::Subject => (meta.local_key, meta.foreign_key),
        // BelongsTo：主体用 foreign_key 取值，去目标的 local_key 匹配
        Side::Target => (meta.foreign_key, meta.local_key),
    };
    let _ = match_side; // 只在上面分支里用，保留名以便阅读

    // 1) 收集主体侧的键
    let keys: Vec<Value> = if subject_col == E::PK {
        subjects.iter().map(E::pk_value).collect()
    } else {
        // 非主键的 local_key（如 `comments` 关联用 post_id）—— 没有通用访问器，
        // 用 to_values 按列名取。
        subjects
            .iter()
            .map(|s| {
                s.to_values()
                    .into_iter()
                    .find(|(n, _)| *n == subject_col)
                    .map(|(_, v)| v)
                    .unwrap_or(Value::Null)
            })
            .collect()
    };
    let keys = distinct_keys(&keys);
    if keys.is_empty() {
        // 没有任何主体有值可查 —— 不发语句（`IN ()` 是语法错误）。
        return Ok(());
    }

    let spec = lookup(db.dialect());
    let per_chunk = chunk_size(spec.max_params_per_stmt(), 1);

    // 2) 分块发 IN 查询，累积「键 → 目标行」
    let mut buckets: std::collections::HashMap<Value, Vec<Row>> = std::collections::HashMap::new();
    let mut start = 0usize;
    for size in crate::batch::split_chunks(keys.len(), per_chunk) {
        let slice = &keys[start..start + size];
        start += size;

        let placeholders = (1..=slice.len())
            .map(|i| spec.placeholder(i))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT * FROM {} WHERE {} IN ({placeholders})",
            spec.quote(meta.target_table),
            spec.quote(target_col)
        );
        let rows = db.query_with(&sql, slice).await.map_err(OrmError::Rdbms)?;

        // 按 target_col 分桶
        for r in rows {
            let k = match r.get(target_col) {
                Some(v) => v.clone(),
                None => {
                    return Err(OrmError::UnknownColumn(format!(
                        "relation `{name}`: target column `{target_col}` missing from the result set; \
                         the relation's foreign_key/local_key may be swapped"
                    )));
                }
            };
            buckets.entry(k).or_default().push(r);
        }
    }

    // 3) 写回。**每个主体都调用** set_relation —— 即使该主体没有关联行，
    //    也要调用传入空 Vec/None，否则 set_relation 里的默认值（前一次加载
    //    留下的旧数据）不会被清掉。
    for s in subjects.iter_mut() {
        let key = if subject_col == E::PK {
            s.pk_value()
        } else {
            s.to_values()
                .into_iter()
                .find(|(n, _)| *n == subject_col)
                .map(|(_, v)| v)
                .unwrap_or(Value::Null)
        };
        let rows = buckets.remove(&key).unwrap_or_default();
        s.set_relation(name, rows)?;
    }
    Ok(())
}
```

- [ ] **Step 3: `Query::with`**

```rust
impl<E: Entity, S> Query<E, S> {
    /// 声明要预加载的关联。
    pub fn with(mut self, relations: &[&dyn RelationSelector]) -> Self {
        self.relations.extend(relations.iter().map(|r| r.name().to_string()));
        self
    }
}
```

`fetch` 在取到 `Vec<E>` 之后：

```rust
        let mut items: Vec<E> = rows.iter().map(E::from_row).collect::<Result<_, _>>()?;
        for name in &self.relations {
            crate::relation::load_relation(db, &mut items, name).await?;
        }
        Ok(items)
```

`Query` 结构体加字段 `relations: Vec<String>`（Task 11 的字段列表与 `clone_with_limit` 都要同步加）。

- [ ] **Step 3: 跑测试 + 提交**

```bash
cd /home/wwwroot/e-cat
cargo test -p ecat-orm relation 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
cargo fmt --check && cargo clippy -p ecat-orm --all-targets -- -D warnings; echo "rc=$?"
git add ecat-orm/src/relation.rs ecat-orm/src/query/mod.rs
git commit -m "feat(ecat-orm): 关联预加载（IN 分块，杜绝 N+1）"
```

### ⚠️ Task 16 必读：预加载的完整验证在 Task 18，不在本任务的单测里

本任务的单测覆盖**分组的纯函数**（去重、跳 NULL、分桶）。但「真的杜绝了 N+1」这件事，
单测证明不了 —— 需要一个**记录 SQL 条数**的假 executor，断言「取 3 个用户 + 他们的 posts
只发了 2 条 SQL，不是 4 条」。

**这条断言必须写进 Task 18 的集成测试**（用 SQLite 真跑，或用一个计数的中间层）。
若 Task 16 结束时没有这条断言，本任务的核心目标（杜绝 N+1）就**没有被验证过**。

---

## Task 17: 迁移系统（`migrate/`）

**Files:**
- Create: `ecat-orm/src/migrate/mod.rs`
- Create: `ecat-orm/src/migrate/ddl.rs`
- Create: `ecat-orm/src/migrate/version.rs`
- Modify: `ecat-orm/src/lib.rs`

**执行位置在用户代码**（spec §7）—— `ecat-cli` 不链接用户代码，看不到实体定义，
无从知道建什么表。这与 diesel / sea-orm 的 CLI 不同（它们靠扫描源码目录）。

```rust
let db = SqlxClient::connect(&url).await?;
let m = Migrator::new(&db)
    .add("001_users", create_table::<User>())
    .add("002_posts", create_table::<Post>());
m.status().await?;
m.run().await?;
```

- [ ] **Step 1: 写失败测试**

`ecat-orm/src/migrate/ddl.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::*;
    use ecat_data::Dialect;

    static COLS: [ColumnMeta; 3] = [
        ColumnMeta { name: "id", ty: ColType::I64, nullable: false, pk: true, auto_increment: true },
        ColumnMeta { name: "name", ty: ColType::Text, nullable: false, pk: false, auto_increment: false },
        ColumnMeta { name: "bio", ty: ColType::Text, nullable: true, pk: false, auto_increment: false },
    ];
    static META: EntityMeta = EntityMeta {
        table: "users", pk: "id", columns: &COLS, relations: &[], flags: EntityFlags::NONE,
    };

    #[test]
    fn create_table_lists_columns_with_types() {
        let sql = create_table_sql(&META, Dialect::Postgres);
        assert!(sql.starts_with("CREATE TABLE IF NOT EXISTS \"users\""), "got: {sql}");
        assert!(sql.contains("\"name\" TEXT NOT NULL"), "got: {sql}");
        assert!(sql.contains("\"bio\" TEXT"), "got: {sql}");
    }

    /// **可空列写 `NULL`、非空列写 `NOT NULL`** —— 漏掉 NOT NULL 会让
    /// 「本不该为空的列」在数据库层没有约束，ORM 的 `UnexpectedNull` 报错
    /// 就成了唯一防线（而它只在读路径上）。
    #[test]
    fn nullability_is_emitted_explicitly() {
        let sql = create_table_sql(&META, Dialect::Postgres);
        assert!(!sql.contains("\"bio\" TEXT NOT NULL"), "bio 可空: {sql}");
        assert!(sql.contains("\"name\" TEXT NOT NULL"), "name 非空: {sql}");
    }

    #[test]
    fn autoincrement_pk_uses_the_dialect_spelling() {
        let pg = create_table_sql(&META, Dialect::Postgres);
        assert!(pg.contains("\"id\" BIGSERIAL PRIMARY KEY"), "got: {pg}");
        let my = create_table_sql(&META, Dialect::MySql);
        assert!(my.contains("`id` BIGINT AUTO_INCREMENT PRIMARY KEY"), "got: {my}");
        let ms = create_table_sql(&META, Dialect::Mssql);
        assert!(ms.contains("[id] BIGINT IDENTITY(1,1) PRIMARY KEY"), "got: {ms}");
        let lite = create_table_sql(&META, Dialect::Sqlite);
        assert!(lite.contains("\"id\" INTEGER PRIMARY KEY AUTOINCREMENT"), "got: {lite}");
    }

    /// **非自增主键也要写 PRIMARY KEY** —— 否则表建出来没有主键，
    /// 后续的 upsert / 乐观锁全部失去依据。
    #[test]
    fn non_autoincrement_pk_still_gets_a_primary_key_clause() {
        static COLS2: [ColumnMeta; 1] = [
            ColumnMeta { name: "id", ty: ColType::Text, nullable: false, pk: true, auto_increment: false },
        ];
        static META2: EntityMeta = EntityMeta {
            table: "k", pk: "id", columns: &COLS2, relations: &[], flags: EntityFlags::NONE,
        };
        let sql = create_table_sql(&META2, Dialect::Postgres);
        assert!(sql.contains("PRIMARY KEY"), "got: {sql}");
        assert!(!sql.contains("BIGSERIAL"), "非自增不该有 BIGSERIAL: {sql}");
    }

    /// MSSQL 没有 `CREATE TABLE IF NOT EXISTS` —— 由调用方先查
    /// INFORMATION_SCHEMA。本函数生成的 SQL **不含** IF NOT EXISTS。
    #[test]
    fn mssql_create_table_has_no_if_not_exists() {
        let sql = create_table_sql(&META, Dialect::Mssql);
        assert!(!sql.contains("IF NOT EXISTS"), "got: {sql}");
    }

    #[test]
    fn drop_table_is_dialect_quoted() {
        assert_eq!(drop_table_sql(&META, Dialect::MySql), "DROP TABLE IF EXISTS `users`");
        assert_eq!(drop_table_sql(&META, Dialect::Mssql), "DROP TABLE IF EXISTS [users]");
    }
}
```

`ecat-orm/src/migrate/mod.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classifies_applied_and_pending() {
        let s = classify(&[1, 2], &[1, 2, 3, 4]);
        assert_eq!(s.applied, vec![1, 2]);
        assert_eq!(s.pending, vec![3, 4]);
    }

    /// 已应用的版本里有、但迁移列表里没有的（例如分支回滚后遗留）——
    /// 必须**报出来**而不是静默忽略：那说明数据库状态和代码不一致。
    #[test]
    fn unknown_applied_versions_are_surfaced() {
        let s = classify(&[1, 2, 9], &[1, 2]);
        assert_eq!(s.unknown, vec![9]);
    }

    /// 待应用项**按版本号排序**，不按声明顺序 —— 声明顺序在多次编辑后
    /// 很容易与实际版本号不符。
    #[test]
    fn pending_is_sorted_by_version_not_declaration_order() {
        let s = classify(&[], &[3, 1, 2]);
        assert_eq!(s.pending, vec![1, 2, 3]);
    }

    #[test]
    fn down_without_reverse_sql_is_irreversible() {
        let m = Migration::new(1, "001_users".into(), "CREATE TABLE t (a int)".into());
        assert!(m.reverse_sql.is_none());
        let e = m.reverse().unwrap_err();
        assert!(matches!(e, OrmError::MigrationIrreversible(_)), "got: {e:?}");
    }

    #[test]
    fn down_with_reverse_sql_is_allowed() {
        let mut m = Migration::new(1, "001_users".into(), "CREATE TABLE t (a int)".into());
        m.reverse_sql = Some("DROP TABLE t".into());
        assert_eq!(m.reverse().unwrap(), "DROP TABLE t");
    }
}
```

- [ ] **Step 2: 实现**（三处需要**单独想清楚**的地方）

**(a) `ddl.rs` 的类型映射**直接复用 `DialectSpec::col_type` / `autoincrement_ddl` —— **不要再写一份映射表**。非空列附 `NOT NULL`；主键列附 `PRIMARY KEY`；自增主键用 `autoincrement_ddl`（它已含类型）。

**(b) MSSQL 的建表存在性检查**：`DialectSpec::table_exists_sql` 在 MSSQL 上返回的串不含 `IF NOT EXISTS`（Task 10 已定）。调用方（`Migrator::run`）必须**先**发
`SELECT 1 FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_NAME = @P1`，查到就跳过建表。
**表名要参数化**，不要拼进 SQL。

**(c) 版本表** `_ecat_migrations` 的 DDL 由同一套 `DialectSpec` 生成（列：
`version BIGINT PRIMARY KEY` / `name <文本类型>` / `applied_at <时间类型>`），
时间存 RFC3339 UTC 文本。**`run()` 的失败语义是「中止」**：单个迁移失败即返回错误，
不继续后面的（半应用的迁移集比不应用更难排查）。

- [ ] **Step 3: 跑测试 + 闸门 + 提交**

```bash
cd /home/wwwroot/e-cat
cargo test -p ecat-orm migrate 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
cargo fmt --check && cargo clippy -p ecat-orm --all-targets -- -D warnings; echo "rc=$?"
git add ecat-orm/src/migrate ecat-orm/src/lib.rs
git commit -m "feat(ecat-orm): 迁移系统（DDL 生成 + 版本表）"
```

---

## Task 18: SQLite 全链路集成测试 + 文档 + 批次收尾

**Files:**
- Create: `ecat-orm/tests/sqlite_e2e.rs`
- Modify: `docs/api.md`、`README.md`、`README.en.md`、`docs/i18n/{12}/api.md`、`docs/i18n/{12}/README.md`、`config/databases.example.yaml`

**这是本批唯一能证明「整套东西真的能用」的任务。** Tasks 1–17 的单测都是字符串断言与假 executor —— 它们证明「生成了我期望的 SQL」，**不证明那些 SQL 在任何数据库上能跑通**。批次 1/2 的教训是这个差别很大（真库抓到了 2 个 Critical）。

- [ ] **Step 1: 写集成测试**

`ecat-orm/tests/sqlite_e2e.rs` 必须覆盖 spec §12 第 4 条列的全部环节：

```
实体定义 → 迁移建表 → CRUD → 关联预加载 → 分页 → 事务 → 软删除 → 乐观锁冲突
```

用 `sqlite::memory:`（spec:743-744）。**关键：多连接场景**。

- [ ] **Step 2: 必须包含的三条「单测证明不了」的断言**

1. **N+1 杜绝**（Task 16 的核心目标）：
   取 3 个用户 + 他们的 posts，**总计只发 2 条 SELECT**（1 条主体 + 1 条 IN），
   不是 4 条。做法：包一层计数 executor，或直接断言 `posts` 内容正确 +
   `distinct_keys` 的调用次数。
   **没有这条，Task 16 的目标就没被验证过。**

2. **MySQL 两步式的事务包裹**（Task 13 的核心风险）：
   用假 executor 记录调用序列，断言 `InsertThen` 路径下
   `transaction()` → `execute_with(INSERT)` → `query(LAST_INSERT_ID)` → `commit()`
   **的顺序与归属**。这条在 SQLite 上跑不出真问题（SQLite 走一步式），
   所以用**调用序列断言**代替。

3. **时间列无 CAST 往返**（spec §12 第 5 条）：
   写一个带 `+08:00` 偏移的 `OffsetDateTime`，读回来断言是同一时刻且为 UTC。
   这条钉住「写入前归一化 UTC」这条规则**真的生效了**。

- [ ] **Step 3: 文档（spec §9.1，**与代码同批**）**

| 文档 | 文件 | 更新内容 |
|---|---|---|
| API 参考 | `docs/api.md` + `docs/i18n/{12}/api.md` | ORM 公开 API：`Entity` trait、`#[entity(...)]` 属性文法、CRUD、查询构建器、分块与分页、关联预加载、迁移 |
| README | `README.md` / `README.en.md` + `docs/i18n/{12}/README.md` | 目录树补 `ecat-orm` / `ecat-orm-derive`；新增 ORM 用法段 |
| 配置示例 | `config/databases.example.yaml` | 无新增配置项（ORM 不需要配置）—— **不要为了「有改动」而加** |

**属性文法那张表直接抄 Task 7 的**（它是规范）。

- [ ] **Step 4: 批次收尾检查**

```bash
cd /home/wwwroot/e-cat
cargo test --workspace --doc 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
cargo test --workspace 2>&1 | tail -20; echo "rc=${PIPESTATUS[0]}"
cargo fmt --check; echo "rc=$?"
cargo clippy --workspace --all-targets -- -D warnings; echo "rc=$?"
cargo audit --deny warnings; echo "rc=$?"
wc -l ecat-orm/src/*.rs ecat-orm/src/**/*.rs ecat-orm-derive/src/*.rs | tail -3
```

逐项确认：

- [ ] `cargo test --workspace` 全绿，**测试数不低于批次前的 771**
- [ ] `--doc` 也跑（`compile_fail` 断言在 doctest 里）
- [ ] `cargo fmt --check` 与 `clippy -D warnings` 全绿（此前 **11 个 crate 共 52 条 `double_must_use`** 与本仓唯一一处 fmt 差异已单独清理 —— 前者根因是 `async-trait` 0.1.91 注入属性，升 0.1.92 根治，**这两条现在应是真闸门**）
- [ ] `cargo audit --deny warnings` 通过
- [ ] **每个源文件 < 500 行**（`wc -l` 超出就拆）
- [ ] 文档与代码同批落地，未提前写「已实现」

- [ ] **Step 5: 提交**

```bash
git add ecat-orm/tests docs README.md README.en.md config
git commit -m "test(ecat-orm): SQLite 全链路集成测试；docs: ORM API 与 README ×14"
```

---

## 批次完成判据（spec §12 逐条对照）

| # | 判据 | 验证方式 |
|---|---|---|
| 1 | `cargo test --workspace` 全绿（含新增方言单测与 SQLite ORM 集成测试） | Task 18 Step 4 |
| 2 | `clippy -D warnings` 与 `fmt --check` 通过 | Task 18 Step 4（**两项债务已单独清理，这两条现在是真闸门**） |
| 3 | `cargo audit --deny warnings` 通过 | Task 18 Step 4（本批**不新增依赖**，除 syn/quote/proc-macro2/base64 —— 都是已被本仓其它 crate 验证过的） |
| 4 | SQLite 上跑通：实体 → 迁移建表 → CRUD → 关联预加载 → 分页 → 事务 → 软删除 → 乐观锁冲突 | Task 18 Step 1 |
| 5 | 时间列无需 CAST 即可正确读写 | Task 18 Step 2 第 3 条 |
| 6 | MSSQL 客户端可编译可配置（批次 2 已交付） | 本批不涉及 |
| 7 | 池增强逐项可验证 | 批次 1/2 已交付 |
| 8 | 会话初始化生效可验证 | 批次 1/2 已交付 |
| 9 | `ecat-circuit-breaker` 既有 12 个测试全绿 | 批次 4 |

**本批新增的、spec 未列但必须满足的三条**（本次实施中发现）：

| 判据 | 出自 |
|---|---|
| N+1 杜绝：3 主体 + 关联 = 2 条 SELECT | Task 16 的核心目标，单测证明不了 |
| MySQL `InsertThen` 的调用序列断言 | Task 13 的核心风险，SQLite 上跑不出 |
| 关联字段在 `from_row` 后为空、`set_relation` 空入参清空残留 | Task 8 补充，防「静默给过时数据」 |


