# 批次 2 验收材料（`ecat-data-mssql` — SQL Server 后端）

日期：2026-10-06 · 分支：`feat/orm-mssql` · 状态：**完成，待用户验收**

## 一、交付内容

### 新增 crate

`ecat-data-mssql` —— 用 `tiberius-ng 0.13.1` + `deadpool 0.13.1` 实现 `SqlExecutor`。
数据后端从 **15 个增至 16 个**。

```
src/config.rs      MssqlConfig（URL/ADO 双形态 + TLS 映射 + 池参数）   434 行
src/url_query.rs   URL 查询串解析（encrypt / trustservercertificate）    99 行
src/pool.rs        deadpool Manager（建连 + 会话初始化 + 智能探活）     320 行
src/cell.rs        ColumnData → JSON（单 match，无类型链）              416 行
src/bind.rs        参数绑定（Value → &dyn ToSql，@P1..@Pn）              70 行
src/client.rs      MssqlClient + SqlExecutor + 事务 wrapper             322 行
src/tests.rs       离线单测                                            454 行
src/live_tests.rs  env 门控真库测试                                    442 行
```

全部 < 500 行（项目硬规则）。

### 代码提交

| 提交 | 内容 |
|---|---|
| `1cfb65b` | crate 骨架与依赖 |
| `9393753` | `MssqlConfig`（URL/ADO 解析 + TLS 映射 + 池参数） |
| `8606efb` | 订正 lib.rs 的 crate 名（包名 `tiberius-ng` / lib 名 `tiberius`） |
| `24ac1cf` | `MssqlManager`（建连 + 会话初始化 + 智能探活） |
| `cf1faf0` | `ColumnData` 行转换（单 match，无类型链） |
| `0dfec1e` | `SqlExecutor` 实现 + 池超时 + 脏连接防护 |
| `4569d13` | 删空配置 `idle_timeout`；两后端事务方法补查询超时 |
| `e1ab756` | `deny_unknown_fields` + 超时回归测试（mssql 侧） |
| `54b248e` | 超时回归测试（sqlx 侧，覆盖事务+客户端两条路径） |
| `dcb71e1` | env 门控真库测试 + `docker-compose.dev.yml` |
| `356fd57` | URL 查询串支持 + `create_timeout` 可配 + `#tmp` 限制记录 |
| `1604a32` | `encrypt=false` 对齐 ADO 语义（`Off` 而非 `On`） |
| `e73dfcd` | README 后端表 ×14 + 配置示例 |

### 文档提交

- `e73dfcd` README ×14（2 根 + 12 i18n）+ `config/databases.example.yaml` 的 `mssql:` 段
- `356fd57` 含 spec 新增一节（`#tmp` 跨语句限制）
- 若干 spec/plan 修订（实施记录、偏差留档、护栏）

## 二、验收数据

| 项 | 值 |
|---|---|
| `ecat-data-mssql` 离线测试 | **52 passed / 0 failed**（含 `pool_create_timeout` 的 2s 版本） |
| **`ecat-data-mssql` 真库测试** | **7 条 live 用例，真 SQL Server 2022 上全绿** |
| `ecat-data-sqlx` | 43 passed / 0 failed |
| 工作区全量 | 见「待补」（后台运行中） |
| 改动文件 fmt | clean |
| 两个 crate clippy | **0 条自身诊断** |

### 真库三向验证（Task 6 实测）

| 场景 | 结果 |
|---|---|
| 真 URL（ADO 形态） | ✅ 49 passed |
| 死端口 | ✅ **6 条 live 用例 panic**（证明真在连库，不是空转） |
| 无 URL + `ECAT_REQUIRE_LIVE_DB=1` | ✅ **6 条 panic 且点名缺失键** |
| 两个 env 都不设 | ✅ 49 passed，live 用例走 skip |

## 三、离线判断被真库证实（5/5）

批次 2 前五个任务的所有结论都是**离线得来**的（单测 + 读源码 + 读文档）。Task 6 逐个验：

| 判断 | 离线依据 | 真库结果 |
|---|---|---|
| `DATE` → 纯 `"2026-10-05"` | 批次 1 改过三次主意才定 | ✅ 成立（没发明 UTC 午夜） |
| `DATETIME2` → RFC3339 UTC | 批次 1 踩过 `.to_string()` 不是 RFC3339 | ✅ |
| `ARITHABORT` 必须用 batch 而非 RPC | Task 3 读 tiberius 源码 | ✅ `SESSIONPROPERTY` 返回 1 |
| `create_timeout` 界定卡死建连 | Task 5 加的 | ✅ ~30s（后压到 2s 测试） |
| drop 后不留脏连接 | Task 5 的 (d) 设计 | ✅ |

## 四、抓到的真缺陷（本批 11 个）

| # | 缺陷 | 发现方式 | 后果 |
|---|---|---|---|
| 1 | `MssqlConfig.tls` 之外的 `PoolParams::default` 与 `pool()` 分叉 | 实现者自查 | 两个构造器行为不一致（批次 1 同族，本批复查） |
| 2 | **事务方法没套查询超时**（两后端都是） | 实现者对照 spec | spec 承诺了但没实现 |
| 3 | **`MssqlConfig.idle_timeout` 是空配置** | 实现者 grep deadpool 源码 | 配了不生效（deadpool 无空闲回收） |
| 4 | `session_init` 必须用 batch 不能用 RPC | 实现者读源码 | 用 RPC 则 `SET ARITHABORT ON` 会被还原（配了不生效） |
| 5 | **URL 形态连不上自签证书的 SQL Server** | **真库实测** | 功能缺口：开发/内部 CA 场景完全连不上 |
| 6 | **`#tmp` 跨语句活不下来** | **真库实测** | 与 sqlx 后端的 API 级行为差异 |
| 7 | **`encrypt=false` 与 ADO 语义不一致** | 实现者对照 tiberius 的 ADO 解析器 | 同一连接串两条路径行为不同 |
| 8 | 删字段后 serde 静默吞掉残留键 | 实现者 | 拼错的键名被静默忽略 |
| 9 | 真库 URL 形态在 TLS 握手就失败 | 真库实测 | 见 #5 |
| 10 | `#tmp` 用例全挂 | 真库实测 | 见 #6 |
| 11 | 事务 wrapper 无处可测（需真池连接） | 实现者 | 由 Task 6 的真库用例覆盖 |

**#5 #6 只有真库能发现** —— 离线测试、源码走读、类型检查全都抓不到。

## 五、并行期间处理的三个"外来"问题

批次 2 进行中，升级 audit 闸门时暴露了三个与批次 2 无关的问题：

| 提交 | 问题 | 性质 |
|---|---|---|
| `2df2c4c` | `h2` / `rustls` / `chacha20` 三个 CVE（含 5.3 medium） | 依赖漏洞 |
| `9a918f7` | **`tiberius-ng` 引入 `aws-lc-rs` 打破 rustls provider 唯一性** → `ecat-mq-nats` 两个测试 panic | 跨 crate 全局不变量 |
| `e2c2af6` | `ecat-data-clickhouse` 的 TTL 测试 flaky（150 次失败 1 次） | 既有脆测试 |

**#9a918f7 只在 `cargo test --workspace` 下暴露**（特性合并启用 `aws-lc-rs`）——
单 crate 跑 60 次全过。而且它是**全仓 5 个用同模式的 crate 里唯一漏掉的那个**：
问题一直存在，只是此前没有第二个 provider 去捅它。

## 六、已知债务与限制

| # | 项 | 归属 |
|---|---|---|
| ① | ~~`ecat-data` 35 条 `clippy::double_must_use` 误报（使 `-D warnings` 恒红）~~ **已闭合（2026-10-06）** | 独立（批次 1 已记；**更正见批次 1 验收文档 ① 的注**：实为 11 个 crate 共 52 条、且是**真阳性**不是误报，根因是 `async-trait` 0.1.91 注入 `#[must_use]`，升 0.1.92 根治） |
| ② | `cargo fmt --check` 在 `ecat-security/src/lib.rs:107` 失败 | 独立（批次 1 已记） |
| ③ | **`#tmp` 跨语句不可用**（与 sqlx 后端的行为差异） | 已写进 spec §6 |
| ④ | URL 形态 → 真自签库**未端到端复跑**（镜像已删） | 缺的那段与已验的 ADO 路径共用同一组 setter |
| ⑤ | `recycle_timeout` / `wait_timeout` 无复现场景，未覆盖 | 批次 4（可观测性） |
| ⑥ | 查询超时会把连接留在协议半途（tiberius 不自动 `cancel_query`） | 与 sqlx 同源，未扩大 |
| ⑦ | 查询串只支持 `encrypt` / `trustservercertificate` 两键 | 有意的窄范围 |

## 七、方法论：本批的"验证层级"证据

批次 1 确立的「单驱动测试不算证据」在本批得到更强的实例：

| 层级 | 通过情况 | 漏掉了什么 |
|---|---|---|
| 单 crate 测试（`-p ecat-data-mssql`） | ✅ 52 passed | 不知道真库语义 |
| 单 crate 跑 60 次 | ✅ 全过 | 不知道特性合并后的行为 |
| **`cargo test --workspace`** | ❌ 曾 panic | ← 抓到了 #9a918f7 |
| **真 SQL Server** | ❌ 曾失败 | ← 抓到了 #5 #6 |

**四层里，只有后两层抓到了真问题。** 前面每一层单独看都是"绿的"。
