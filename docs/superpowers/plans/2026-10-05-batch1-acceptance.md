# 批次 1 验收材料（地基 + 原生池）

日期：2026-10-06 · 分支：`feat/orm-mssql` · 状态：**完成，待用户验收**

## 一、交付内容

### 代码提交（14 个）

| 提交 | 内容 |
|---|---|
| `8b3e57c` | `Dialect` 枚举 + `from_url` |
| `5aff691` | 大小写不敏感（审查发现） |
| `facbce7` | 容忍首尾空白（审查发现） |
| `7ea8555` | trim 谓词对齐 url crate 的 C0 范围（实现者实测发现） |
| `8f7839e` | **`SqlExecutor` supertrait 拆分**（破坏性） |
| `d3e390d` | **事务内可执行 SQL**（破坏性） |
| `592eb55` | 空事务执行 SQL 报错（审查发现静默丢数据） |
| `c1439c1` | 查询超时助手 + 超时/泄漏计数 |
| `9ea4f33` | `SqlxConfig` 独立模块 + 8 个池参数 |
| `facca70` | `connect`/`from_config` 会话默认一致（实现者发现静默分叉） |
| `c5190e5` | 池默认值单一来源（实现者发现两处定义） |
| `c9eb365` | **弃用 `AnyPool`，三路原生池**（破坏性） |
| `40b8381` | 补 `i16`/`u64`/`f32`/日期分支 + 链尾改报错（审查发现 2 个 Critical） |
| `8fcb302` | `Date` 输出纯 `YYYY-MM-DD`（不伪造时刻/时区） |
| `9e122c9` | 真库用例拆到 `live_tests.rs`（`tests.rs` 一度到 515 行，破 500 行规则，实现者自查后拆出） |

### 文档提交

- `56db6c6` 数据库配置教程 ×13（根 + 12 语言）
- `09915b7` trait 抽象补 `SqlExecutor` ×27（README ×14 + api.md ×13）——
  批次 1 把执行能力拆到 `SqlExecutor`，原列表里没有它
- `4be196c` 生态规划 v4.0 段状态更新 ×13（含给 12 个 i18n 副本补译该段——
  此前只有根文件有，是 `e6059c7` 漏同步副本造成的漂移）
- 30+ 个 spec/plan 修订提交（评审轮次、实施记录、决策留档）

### 规模

```
35 files changed, 6867 insertions(+), 492 deletions(-)
```

## 二、验收数据

| 项 | 值 | 来源 |
|---|---|---|
| `ecat-data` 测试 | 28 passed / 0 failed | 实测 |
| `ecat-data-sqlx` 测试（无真库） | 39 passed / 0 failed | 实测 |
| `ecat-data-sqlx` 测试（真 PG16 + MySQL8） | **41 passed / 0 failed** | 审查者实测 |
| **工作区全量测试** | **718 passed / 0 failed**，`cargo_rc=0`，113 个 test binary，0 error 行 | 机器恢复后实测（**正确捕获退出码**） |

> **关于中途出现过的 `611`**：那是我自己的管道命令
> （`cargo test --workspace 2>&1 | grep … | awk …`）造成的假象 —— 管道的退出码是
> **`awk` 的**，cargo 的失败被吞掉；而在 I/O 挂起的机器上部分 test binary 根本没跑出来，
> 总数被静默削减，命令却报 exit 0。**机器恢复后重测为 718/0，binary 数与完整跑一致。**
> 这条记在这里是因为它是个典型案例：**一个看起来在验证、实际上在掩盖失败的命令**。
| 改动文件 fmt | clean | 实测 |
| `ecat-data-sqlx` clippy | **0 条自身诊断** | 审查者实测 |
| `ecat-data` clippy | 35 条，**全为 `async_trait` 的已知误报** | 实测，见债务 ① |
| `ecat-data-sqlx` 行数 | lib 453 / pool 220 / cell 173+ / config 225 / tests 372 | 实测 |

**破坏性变更**（需 4.0.0）：

1. `RdbmsClient` 拆出 `SqlExecutor` supertrait（4 个实现者已同步）
2. `TransactionInner` 签名扩张
3. `SqlxClient::from_pool(AnyPool, Dialect)` → `from_pool(Pool)`
4. `run_with_timeout` 签名（批次 5 会再改一次）

## 三、过程中抓到的真缺陷（本批次最有价值的部分）

**14 个缺陷，全部属「失败却看起来成功」族** —— 没有一个能靠读代码发现：

| # | 缺陷 | 发现方式 | 后果 |
|---|---|---|---|
| 1 | `from_url` 大小写敏感 | 代码审查 | 大写 scheme 能连通却判成 `Standard` |
| 2 | `from_url` 不容首尾空白 | 代码审查 | 同上 |
| 3 | trim 谓词不含 C0 控制字符 | **实现者实测** | NUL 前缀的连接串静默判错 |
| 4 | 空事务 `Ok(0)` 与合法 0 行无法区分 | 代码审查 | **写操作静默丢失** |
| 5 | `PoolParams::default` 与 `pool()` 会话默认分叉 | **实现者自查** | 两个构造器行为不一致 |
| 6 | 池默认值两处定义 | **实现者自查** | 改一处另一处静默不变 |
| 7 | flaky 测试断言（进程级计数器） | **实现者 400 次实测** | 1.25% 概率假失败 |
| 8 | 类型链 `bool` 抢整数 | **实现者实测** | sqlite 任意整数变 `true` |
| 9 | NULL 无闸门 | **实现者实测** | sqlite NULL 静默变 `false` |
| 10 | `OffsetDateTime::Display` 非 RFC3339 | **实现者核源码** | 时间格式错误 |
| 11 | **MySQL `UNSIGNED` 被 bool 吃掉** | **审查者真库 A/B** | **最常见的自增主键**值在 `false`/`true`/`Null` 间跳 |
| 12 | **PG `smallint` 返回 Null** | **审查者真库 A/B** | 整型列静默变 null |
| 13 | 未支持类型从「响亮报错」变「静默 null」 | 审查者 | 金额/uuid 列一路 null 进 ORM |
| 14 | `SqlxConfig.tls` 静默无效 | **Task 7 实现者核对源码** | 用户配了 TLS 却无效 |

**#11 与 #12 是分水岭**：它们**在 710 个测试全绿的情况下依然存在** —— 因为那 710 个测试里涉及本 crate 的全跑在**内存 SQLite** 上，而 SQLite 的 `compatible` 恰好是三个驱动里最宽松的。**必须自建真 MySQL 8.0 与 PG16、做 A/B 对比才暴露。**

由此确立的判据（已写进 spec）：

> **`ECAT_TEST_PG_URL` / `ECAT_TEST_MYSQL_URL` / `ECAT_TEST_MSSQL_URL` 那套 env 门控
> 集成测试不是可选增值，而是覆盖非 SQLite 路径的唯一手段。缺了它们时只能声明
> 「未验证」，不能算通过。**（`ECAT_REQUIRE_LIVE_DB=1` 让 CI 里「跳过即失败」）

## 四、已知债务（5 项，均已定位、均未擅自处理）

| # | 债务 | 归属 | 建议 |
|---|---|---|---|
| ① | `ecat-data` 35 条 `clippy::double_must_use` 误报，使 `-D warnings` 闸门恒红 | 独立 | 加一条带理由的 crate 级 `[lints.clippy] allow`，一次清掉 |
| ② | `cargo fmt --check` 在 `ecat-security/src/lib.rs:107` 失败（base 即红） | 独立 | 跑一次 `cargo fmt -p ecat-security` |
| ③ | `ecat-data/src/rdbms.rs` 499 行（距上限 1 行） | 批次 4 | 拆 `Transaction` 到 `src/transaction.rs` |
| ④ | `ecat-data-sqlx/src/lib.rs` 453 行（已达标）；`tests.rs` 372 行 | — | 无需动作 |
| ⑤ | 7 个 i18n 副本的 yaml 注释仍是中文 | 独立小清理 | 一行 sed，但改动前已有 |
| ⑥ | **f32 最短表示只覆盖 PG**，MySQL `FLOAT` 仍返回加宽值（`0.1` → `0.10000000149011612`） | 独立立项 | **不要靠「f32 挪到 f64 前」修** —— 会让 `DOUBLE` 列 `1e300`→`inf` 真丢数据。详见计划文件的已知限制一节 |

| ③ | **`cargo audit --deny warnings` 失败**：`h2 0.4.15`（RUSTSEC-2026-0258）、`rustls 0.23.43`（RUSTSEC-2026-0285，**5.3 medium**）、`chacha20 0.10.1`（yanked） | 独立 | 升 `h2`/`rustls`，或按 `.cargo/audit.toml` 既有方针加 ignore + 补 CVE 评估表 |

① ② ③ 是**既有红灯**，与本次改动无关，但在用当前工具链时会让 CI 失败。**CI 的三道闸门在 `main` 上都是红的。**

③ 的验证方式（批次 2 Task 1 实测）：导出 HEAD 的 lockfile 单跑 CI 钉的同一 audit 二进制
（v0.22.2 musl），得到**同样三条、版本一字不差**；两条 advisory 的发布日期
（2026-08-17 / 2026-09-14）晚于仓库上次审计报告（2026-08-14）→ 是 advisory 库更新所致。
`chacha20` 的 yank 来自 `mongodb → hickory → rand`。**`tiberius-ng` / `deadpool` 零告警。**

## 五、待你决策（2 项）

1. **`SqlxConfig.tls` 静默无效** —— 有 `#[serde(default)]` 但代码从不读它，
   用户配了 TLS 不生效且无提示。三个选项见
   `docs/superpowers/plans/2026-10-05-orm-mssql-batch1-foundation.md`（推荐「设了就报错」，约 5 行）。
2. **容器清理** —— `sudo docker rm -f revt2-pg ecat-t6-pg`（容器内进程僵死，
   需提权；未代为执行）。

## 六、环境问题（影响后续所有批次）

本机处于**存储层 I/O 挂起**，非单纯高负载：

```
15 个 D 状态（不可中断 I/O）进程：
  kworker/u16:7+flush-8:0         ← writeback 卡住
  kworker/*+inode_switch_wbs ×3   ← writeback cgroup 切换卡住
  sync（不返回）、umount ×2、container-init、postgres ×2
磁盘空间与 inode 均正常（根 61%/13%，/home 77%/6%）→ 不是耗尽
```

**症状**：cargo 单次编译 10+ 分钟、docker 写操作（`run`/`create`/`rm`）一律挂到超时。
本会话全程的卡顿都源于此。**建议你从系统层面查一下**（writeback 卡死可能是 I/O 饱和，
也可能是控制器/磁盘问题）。

另有 3 个 peer session 在别的仓库上并发跑 cargo/docker，共享同一磁盘。

## 七、未完成

**无。** 批次 1 全部验证项已闭环。

（过程记录：工作区全量测试一度因本机 I/O 挂起 + 内存不足而无法完成，
机器恢复后补测为 **718 passed / 0 failed / cargo_rc=0**，113 个 test binary 与完整跑一致。）
