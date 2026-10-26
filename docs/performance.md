# Ecat 性能基准（全链路）

> 版本：7.3.0。状态：2026-10-26；性能提交 `dcd8523`（harness）与 `979d25f`（文档）。范围：本机 loopback，HTTP + gRPC + MySQL 9 + Redis 7；读 SLO 为每轮 max < 10ms，写路径单独报告、不在 SLO 内。数字为共享笔记本上的 best-effort，不是生产保证。

## 结论

| 场景 | 最终运行 | 5 轮 max | ≥10ms 样本 | 错误 |
|---|---|---:|---:|---:|
| HTTP 读（10k/轮 × 5） | `20261026-131632-2541047` | 5917–9193µs | 0 / 50,000 | 0 |
| gRPC 读（10k/轮 × 5） | 同上（同一合并运行） | 6026–8422µs | 0 / 50,000 | 0 |

- 最终合并运行同时压两个传输，读样本合计 **0 / 100,000 ≥10ms**，gate 通过；env 因最高温度 91°C 标记 `noisy=true`（loadavg 7.12、PSI IO avg10 16.69%）。
- 读路径 = 一条主键 `SELECT` + 一条 Redis `GET`。此前分开运行的 `20261026-123115` / `20261026-063056` 仅作历史对照，不再作为主数据。
- 写路径（`UPDATE` + Redis `SET`，含 InnoDB 落盘）p99 12–26ms、max 23–49ms，见“写路径”，不纳入 10ms 读 SLO。
- 中间件是 opt-in 且仅 HTTP；最终运行未通过 10ms gate（14/15,000 ≥10ms），见“中间件”。

![全链路读延迟：每轮 max（柱）与 p99（点），对照 10ms SLO](performance.svg)

## 环境与拓扑

| 项 | 值 |
|---|---|
| 主机 | Intel i7-8550U，4C/8T（`nproc=8`），governor `powersave` |
| 内核 / 工具链 | 6.6.155；rustc/cargo 1.99.0；`RUSTFLAGS=-C target-cpu=native` |
| 运行时 | `TOKIO_WORKER_THREADS=2`（服务端与压测客户端） |
| CPU 绑定 | 服务端 0–1，客户端 2–3，MySQL 4，Redis 5 |
| 网络 | `127.0.0.1` loopback，无 TLS/WAN；容器端口绑定 `127.0.0.1` |
| MySQL | `mysql:9`，`127.0.0.1:3307`，`--performance-schema=OFF --skip-name-resolve --innodb-buffer-pool-size=512M`；10k 行种子数据 |
| Redis | `redis:7-alpine`，`127.0.0.1:6379`，`--save "" --appendonly no`（无持久化） |
| 容器 | 每次运行 `ecat-perf-mysql-$$` / `ecat-perf-redis-$$`，只清理本进程创建的容器 |
| 构建 | release，独立 target dir `/var/tmp/ecat-perf-target`，`cargo build --offline` |

`probe_timeout` 与本次结果无关：它只约束 tiering readiness 的冷层探针与归档滞后查询，本基准未启用 tiering。

## 方法

- 每轮固定 8 并发、10,000 个计时请求；每轮先 2,000 预热，再用 Barrier 同时开始计时；每个传输 5 轮。最终运行在一次 `serve` 中同时压 HTTP 与 gRPC。
- 延迟样本为客户端端到端耗时（含连接复用、协议解析、服务端处理）。每轮输出 p50 / p99 / p99.9 / max、`>=10ms` 计数、错误数、吞吐。
- 服务端处理时间：HTTP 通过响应头 `x-server-us`、gRPC 通过响应字段 `server_us` 回传，客户端记录该轮最大值 `serverMaxµs`；最终 harness 只统计序号 >= warmup 的请求并每轮清零。
  HTTP 的 `server_us` 在 JSON 序列化之后打点，gRPC 的在 tonic 编码之前打点；两者不能逐微秒互相比较。
- 压测客户端每个 worker 使用独立 HTTP 连接池（`pool_max_idle_per_host=1`，`tcp_nodelay`）或独立 gRPC Channel（`tcp_nodelay`），避免共享连接池锁与单条 HTTP/2 连接多路复用的干扰；静态/读写请求都会轮换 client。
- 静态基线：HTTP `GET /static`（返回 `OK`），gRPC `Ping`，不访问 DB/Redis。
- 组件探针：独立进程按同样的并发/请求数/轮次分别压 MySQL `SELECT` 与 Redis `GET`；探针先后串行运行，不能与全链路数字相加，只用于归因。
- 脚本 gate 校验预期 series、轮数与每轮请求数，缺数据、错误或读轮 `>= threshold` 均失败（C1）；`--middleware` 会强制 `--transport http`，因为 tonic 侧没有对应中间件集成。
- `require_port_free` 检查 3307/6379/8000/50051，容器以 `$$` 命名且只清理本进程创建成功的容器（C2）；每次运行的 loadavg、PSI、温度、governor、commit 与 dirty 数写入 `env.txt`。

## 读路径（最终合并运行）

`/var/tmp/ecat-perf-target/results/20261026-131632-2541047/`

### HTTP

| round | p50µs | p99µs | p99.9µs | maxµs | serverMaxµs | ≥10ms | err | qps |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 915 | 4389 | 6925 | 7637 | 7124 | 0 | 0 | 7290 |
| 2 | 894 | 3078 | 5043 | 6129 | 5079 | 0 | 0 | 7805 |
| 3 | 919 | 3302 | 7511 | 9193 | 6550 | 0 | 0 | 7466 |
| 4 | 909 | 2819 | 4943 | 5917 | 4936 | 0 | 0 | 7786 |
| 5 | 929 | 3339 | 5146 | 7818 | 5314 | 0 | 0 | 7331 |

### gRPC

| round | p50µs | p99µs | p99.9µs | maxµs | serverMaxµs | ≥10ms | err | qps |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 949 | 3567 | 4884 | 6180 | 4661 | 0 | 0 | 7251 |
| 2 | 966 | 3280 | 4678 | 6936 | 5709 | 0 | 0 | 7184 |
| 3 | 961 | 3154 | 5338 | 8422 | 6888 | 0 | 0 | 7290 |
| 4 | 965 | 2976 | 4859 | 6414 | 5271 | 0 | 0 | 7259 |
| 5 | 985 | 3218 | 5252 | 6026 | 5450 | 0 | 0 | 7160 |

两个传输各 0 错误、0/50,000 ≥10ms，合计 0/100,000；gate 通过。该轮 `noisy=true` 仅由最高温度 91°C 触发，读路径仍全部 <10ms。

## 开销归因（同一最终运行）

| 基线/探针 | p50µs | p99µs | maxµs |
|---|---:|---:|---:|
| HTTP 静态（无 DB/Redis） | 218–235 | 461–595 | 973–4240 |
| gRPC 静态（Ping） | 213–224 | 424–475 | 622–3661 |
| MySQL 探针（单条 `SELECT`） | 595–611 | 1692–1858 | 4002–13555 |
| Redis 探针（单条 `GET`） | 104–120 | 241–268 | 1017–2473 |

- 静态协议基线 p50 约 0.21–0.24ms，不是主要成本；全链路读 p50 为 0.89–0.99ms，比 MySQL 与 Redis 探针 p50 之和（0.70–0.73ms）高约 0.16–0.29ms。探针串行执行两条 I/O，只能作粗略归因。
- 同一最终运行的 MySQL 探针仍有 2/50,000 ≥10ms（max 13555µs），而读 series 为 0；探针与读轮窗口不同，不能把探针尾数当成全链路读的尾部。

## 写路径（独立口径，不在 10ms SLO 内）

`/var/tmp/ecat-perf-target/results/20261026-132004-2544813/`

| 传输 | p50µs | p99µs | p99.9µs | maxµs | serverMaxµs | ≥10ms / 50,000 | qps |
|---|---:|---:|---:|---:|---:|---:|---:|
| HTTP write | 1102–7681 | 13287–26013 | 19536–34483 | 22645–48589 | 不可用 | 3427 | 988–3018 |
| gRPC write | 1150–1311 | 12011–25382 | 18227–34261 | 24004–42979 | 23786–40609 | 1742 | 2635–3124 |

- HTTP 第 1 轮 p50 7681µs、1617/10000 ≥10ms；其余 4 轮 p50 1102–1312µs、317–546 个越界。原始数据保留，不剔除首轮。
- HTTP write 的 `serverMaxµs` 为 0 是因为该次运行的客户端没有解析该响应头，**不能解读为服务端处理时间为零**；HTTP 写路径没有服务端归因。gRPC 写 `serverMax` 表明尾部主要来自服务端持久化。
- 写路径包含 `UPDATE`、Redis `SET` 与 InnoDB 默认落盘/fsync；0 错误，但不承诺 10ms。该轮 `noisy=true`（判定时 load1 8.30、最高温度 97°C）。

## 中间件（opt-in，仅 HTTP）

基线是裸 router，框架不会自动注入中间件。`--middleware` 才在 HTTP 路由上安装 `MetricsLayer` + `TracingLayer` + `LoggingLayer`，并为 `LoggingLayer` 安装同步 fmt INFO stdout subscriber；脚本会强制 HTTP（tonic 侧没有对应中间件集成）。最终运行 `/var/tmp/ecat-perf-target/results/20261026-132411-2551098/`：8 并发、5,000/轮、1,000 预热、3 轮。

| round | p50µs | p99µs | p99.9µs | maxµs | serverMaxµs | ≥10ms | err | qps |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 1578 | 5288 | 7147 | 8400 | 6490 | 0 | 0 | 4322 |
| 2 | 1586 | 7618 | 10766 | 11579 | 6895 | 10 | 0 | 3867 |
| 3 | 1512 | 5119 | 7791 | 12085 | 6305 | 4 | 0 | 4576 |

- 合计 **14/15,000 ≥10ms，gate FAILED**；`noisy=true`（判定时 load1 8.03、最高温度 98°C），`server.log` 约 19.7MB，符合每请求同步写日志的行为。
- 服务端 handler 自报 `serverMax` 最大 6895µs <10ms；>10ms 是客户端观测，包含 handler 计时之外的中间件与日志写出。
- 结论：opt-in Metrics + Tracing + Logging 在**同步 fmt stdout 每请求日志**下没有达到 max<10ms。更早的 middleware 运行曾出现 0 越界，但窗口与采样不同，不构成受控对照；裸基线（无中间件）才是 10ms 数据集。建议 p100 10ms 目标使用非阻塞/采样日志，或不安装 `LoggingLayer`。

## TCP_NODELAY

改动（均为服务端）：

- `ecat-transport-http`：`axum::serve` 不会自动设置 `TCP_NODELAY`；明文 listener 通过 `ListenerExt::tap_io` 对每条连接设置，TLS accept loop 同步设置。
- `ecat-transport-grpc`：tonic 的 `Server` 默认 `tcp_nodelay=false`，改为 `Server::builder().tcp_nodelay(true)`；tonic 客户端 `Endpoint` 默认已开启。

变更前后 smoke（原始工件，非受控 A/B；运行期间有其他构建，loadavg 约 18→3.7）：

| 阶段 | run | 配置 | HTTP 读 maxµs（≥10ms/请求） | gRPC 读 maxµs（≥10ms/请求） |
|---|---|---|---:|---:|
| HTTP 改动前（gRPC 也尚未改） | 060617 | 2000×1 | 29625（71/2000） | 11648（8/2000） |
| HTTP 改动后 | 061005/061321/061558 | 2000×1 | 5392–15466（0–42/2000） | 4257–8393（0/2000） |
| gRPC 改动前 | 062208 | 10000×5 | — | 8742–11268（6/50000） |
| gRPC 改动后 | 062536 | 10000×5 | — | 6216–10851（1/50000） |
| 两者改动后（干净） | 063056 | 10000×5 | — | 5613–7310（0/50000） |

- 前后归属按源文件 mtime 与运行开始时间对应；`env.txt` 只记录 commit 与 dirty 数量，工件本身无法区分版本。
- 早期 smoke 的静态/`serverMax` 数字受旧客户端缺陷影响（未轮换 client、预热样本计入 `serverMax`），最终 harness 已修复；本文主数据全部来自最终运行。TLS 分支同步设置，但没有 TLS 性能数据。

## 噪声与变异

- 最终读运行 `131632` 仅因温度 91°C 被判 `noisy=true`，读 gate 仍通过；最终 middleware 运行 `132411` 为 load1 8.03 + 98°C，出现 14/15,000 越界且 gate 失败。
- 历史噪声样例：`122711` HTTP 读第 3 轮 max 13201µs（14/10000 越界）；`124538` gRPC 读 max 21050µs（第 1 轮 178/10000 越界），HTTP 读 max 25837µs（18 越界）；`123439` gRPC 读第 3 轮 max 57052µs（102 越界）。
- 当天观测到 loadavg 最高约 18、PSI IO avg10 最高约 77.5%、温度 97–98°C。噪声运行全部保留，未静默丢弃；`max`/p100 在共享机器上不可保证，结果不是生产承诺。

## 复现

```bash
# 默认：8 并发，10k/轮，2k 预热，5 轮，HTTP+gRPC 读+写+静态
bash scripts/perf/fullchain.sh

# 只跑读路径；中间件（仅 HTTP）
bash scripts/perf/fullchain.sh --op read
bash scripts/perf/fullchain.sh --middleware

# 常用选项
#   --rounds N  --concurrency N  --total N  --warmup N  --threshold-us N
#   --transport both|http|grpc   --op both|read|write|static
#   --middleware   --skip-build   --keep
```

- 依赖已缓存的 `mysql:9` 与 `redis:7-alpine`，不需要联网；3307/6379/8000/50051 任一被占用脚本即拒绝启动，容器端口绑定 `127.0.0.1`。
- 默认 target dir `/var/tmp/ecat-perf-target`（可用 `ECAT_PERF_TARGET` 覆盖）；产物在 `results/<timestamp>-<pid>/`：`env.txt`、`<transport>-<op>.jsonl`、`probe-*.jsonl`、`summary.md`。
- gate 校验预期 series、轮数、每轮请求数与错误数，任一读轮 `>= threshold` 即失败；缺数据同样失败。
- 取数窗口建议：load1 < `nproc`、PSI IO avg10 < 25%、最高温度 < 85°C；脚本完成时会按这三个阈值在 `env.txt` 追加 `noisy=` 标记。

## 已知限制

- 只测 MySQL 9 + Redis 7（README 列出的 19 个后端中的 2 个）；未覆盖其余 17 个后端，也未覆盖分片、分层、Saga、2PC 等数据层能力。
- 只有 loopback，无 TLS、无真实 WAN RTT、无跨机网络/负载均衡；TLS listener 虽已设置 `TCP_NODELAY`，但没有 TLS 性能数据。
- 只测读、写、静态三种操作；未测冷启动、连接风暴、长稳/内存与故障注入。
- 写路径包含 InnoDB 落盘/fsync，且 HTTP 写路径没有服务端归因；不承诺 10ms。
- 中间件只覆盖 HTTP，且同步 fmt stdout 日志未达到 max<10ms；不能据此评价 gRPC 中间件或非阻塞日志方案。
- 单机 4C/8T、`powersave` governor、共享构建负载；`max`/p100 不可保证，结果不能外推到生产环境。
