# ecat-orm 与 SQL Server 实施计划 — 总览

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 e-cat 补齐完整 ORM（`ecat-orm`）与 SQL Server 数据后端（`ecat-data-mssql`），并顺带修正地基上的四处结构性缺口。

**Spec:** [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../specs/2026-10-05-orm-and-mssql-design.md)

**分支:** `feat/orm-mssql` · **版本:** 3.0.3 → 4.0.0（仅在批次 4 后 bump）

---

## 批次划分

每批结束时 `cargo test --workspace` 必须全绿（CI 闸门），可独立提交与回滚。
文档与代码**同批落地**。批次 1–2 的计划已写至任务级；批次 3–4 在各自开工前展开
（批次 1 会实际校正 sqlx 原生池的细节，提前写死批次 3 的代码会失效）。

| 批次 | 计划文件 | 范围 |
|---|---|---|
| 1 地基 | [batch1-foundation](2026-10-05-orm-mssql-batch1-foundation.md) | `SqlExecutor` 拆分、`Dialect`、事务可执行、超时助手、`ecat-data-sqlx` 原生池重写 |
| 2 驱动 | 开工前展开 | `ecat-data-mssql`（tiberius-ng + deadpool）、预热、智能 recycle、README ×14 |
| 3 ORM | 开工前展开 | `ecat-orm` + `ecat-orm-derive`、api.md ×13、config 示例 |
| 4 收尾 | 开工前展开 | 熔断/路由包装器、可观测性三 feature、聚合 crate、CHANGELOG、规划文档、docker-compose |

## 批次 2–4 任务概览（供排期参考，细节待展开）

**批次 2 — `ecat-data-mssql`**
1. crate 骨架 + `MssqlConfig`（URL 解析：`mssql://user:pass@host:1433/db?encrypt=...`）
2. `MssqlManager`（deadpool `Manager`：`create` 建 TCP + `Client::connect`，`recycle` 按 idle 时长决定是否 `SELECT 1`）
3. 参数绑定（`Value` → `Bind` 枚举 → `&dyn ToSql`，占位符 `@P1..@Pn`）
4. 行转换（`ColumnData` 匹配，时间 → RFC3339 UTC，`Bytes` → base64）
5. `SqlExecutor` 实现 + `with_timeout` 接入
6. `warm_up()` + `pool_status()`
7. env 门控集成测试（`ECAT_TEST_MSSQL_URL`）
8. `docker-compose.dev.yml`（sqlserver 2022 + postgres + mysql）
9. 文档：README ×14（后端表第 16 行、目录树、依赖行）

**批次 3 — `ecat-orm` + `ecat-orm-derive`**
1. `Entity` trait / `EntityMeta` / `ColumnMeta`（`&'static [..]`，const 可构造）
2. `#[derive(Entity)]`：表名、列、主键、标志位、`from_row`、`to_values`
3. 方言层 `DialectSpec` ×5 + 纯字符串断言单测（含 `max_params_per_stmt`）
4. `Query` 构建器 + 标识符白名单 + SQL 生成
5. CRUD（含 MySQL `InsertThen` 包事务路径）
6. 批量（分块）+ 分页（COUNT / `paginate_without_count`）
7. 关联 `XxxRelation` 枚举 + 预加载（`IN` 分块）
8. 软删除 / 时间戳自动填充 / 乐观锁
9. 迁移 `Migrator` + DDL 生成 + 版本表
10. SQLite in-memory 全链路集成测试
11. 文档：api.md ×13、README ORM 段 ×14、config 示例

**批次 4 — 池增强与收尾**
1. `ecat-circuit-breaker` 抽公开 `Breaker`（tower 层改为调用它，12 个既有测试保持全绿）
2. `CircuitBreakerExecutor`
3. `RdbmsRouting`（含跳过已熔断端点、`fallback_to_primary`）
4. 可观测性三 feature：`metrics` / `health` / `tracing`
5. `ecat` 聚合 feature `orm` / `mssql`
6. 版本 bump 4.0.0 + CHANGELOG
7. 文档：ecosystem-plan ×13、tls 教程 ×13

## 贯穿约束

- 每个源文件 < 500 行（项目规则）
- 每批提交前跑：`cargo test --workspace && cargo fmt --check && cargo clippy --workspace -- -D warnings`
- 新增依赖必须过 `cargo audit --deny warnings`
- 文档与代码同一次提交落地，不留下与代码不符的描述
