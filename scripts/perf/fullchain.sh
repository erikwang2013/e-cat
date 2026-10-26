#!/usr/bin/env bash
# 全链路延迟基准：HTTP/gRPC + MySQL + Redis。
#
# 目标：并发 8、每轮 1 万请求、5 轮，记录 p50/p99/p99.9/max/>=10ms/错误数。
# 默认在独立 target dir、独立 Docker 容器和 CPU 亲和下运行；数字只在安静
# 窗口内可作为验收依据，脚本会记录环境并标记 noisy。
#
# 用法：
#   scripts/perf/fullchain.sh [--rounds 5] [--concurrency 8] [--total 10000]
#     [--warmup 2000] [--threshold-us 10000] [--middleware] [--skip-build]
#     [--keep] [--transport both|http|grpc] [--op both|read|write|static]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

ROUNDS=5
CONCURRENCY=8
TOTAL=10000
WARMUP=2000
THRESHOLD_US=10000
MIDDLEWARE=0
SKIP_BUILD=0
KEEP=0
TRANSPORT=both
OP=both

while (($#)); do
  case "$1" in
    --rounds) ROUNDS="$2"; shift 2 ;;
    --concurrency) CONCURRENCY="$2"; shift 2 ;;
    --total) TOTAL="$2"; shift 2 ;;
    --warmup) WARMUP="$2"; shift 2 ;;
    --threshold-us) THRESHOLD_US="$2"; shift 2 ;;
    --middleware) MIDDLEWARE=1; shift ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    --keep) KEEP=1; shift ;;
    --transport) TRANSPORT="$2"; shift 2 ;;
    --op) OP="$2"; shift 2 ;;
    -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if ! [[ "$ROUNDS" =~ ^[1-9][0-9]*$ && "$CONCURRENCY" =~ ^[1-9][0-9]*$ && "$TOTAL" =~ ^[1-9][0-9]*$ && "$WARMUP" =~ ^[0-9]+$ && "$THRESHOLD_US" =~ ^[1-9][0-9]*$ ]]; then
  echo "!! numeric options must be positive (warmup may be 0)" >&2; exit 2
fi
case "$TRANSPORT" in http|grpc|both) ;; *) echo "!! bad --transport $TRANSPORT" >&2; exit 2 ;; esac
case "$OP" in read|write|static|both) ;; *) echo "!! bad --op $OP" >&2; exit 2 ;; esac
if (( $(nproc) < 6 )); then
  echo "!! this harness pins 6 logical CPUs (server 0-1, client 2-3, MySQL 4, Redis 5); nproc=$(nproc)" >&2
  exit 2
fi
if (( MIDDLEWARE == 1 )) && [[ "$TRANSPORT" != "http" ]]; then
  echo "!! middleware is HTTP-only (no tonic integration); forcing --transport http" >&2
  TRANSPORT=http
fi

export PATH="/home/erik/.local/bin:$PATH"
export CARGO_HOME="${CARGO_HOME:-/var/tmp/b5b-cargo-home}"
export CARGO_TARGET_DIR="${ECAT_PERF_TARGET:-/var/tmp/ecat-perf-target}"
export RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native}"
export TOKIO_WORKER_THREADS=2
BIN="$CARGO_TARGET_DIR/release/fullchain-bench"
OUT="$CARGO_TARGET_DIR/results/$(date +%Y%m%d-%H%M%S)-$$"
MYSQL_NAME="ecat-perf-mysql-$$"
REDIS_NAME="ecat-perf-redis-$$"
MYSQL_PORT=3307
REDIS_PORT=6379
MYSQL_URL="mysql://root:root@127.0.0.1:${MYSQL_PORT}/ecat?ssl-mode=DISABLED"
REDIS_URL="redis://127.0.0.1:${REDIS_PORT}"

mkdir -p "$OUT"
SERVER_PID=""
CREATED_MYSQL=0
CREATED_REDIS=0
cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if (( KEEP == 0 )); then
    (( CREATED_MYSQL == 1 )) && docker rm -f "$MYSQL_NAME" >/dev/null 2>&1 || true
    (( CREATED_REDIS == 1 )) && docker rm -f "$REDIS_NAME" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT
# 只清理本次创建成功的容器；并发调用使用 $$ 后缀，名字不会互撞。


require_port_free() {
  local port="$1"
  if ss -ltn "sport = :$port" 2>/dev/null | grep -q LISTEN; then
    echo "!! port $port is already in use; refusing to kill an unrelated service" >&2
    exit 1
  fi
}

echo "== environment =="
{
  echo "timestamp=$(date -Is)"
  echo "host=$(hostname)"
  echo "kernel=$(uname -r)"
  echo "cpu=$(lscpu | sed -n 's/^Model name:[[:space:]]*//p' | head -1)"
  echo "nproc=$(nproc)"
  echo "git_commit=$(git rev-parse HEAD)"
  echo "git_dirty=$(git status --porcelain | wc -l)"
  echo "rustc=$(rustc --version)"
  echo "cargo=$(cargo --version)"
  echo "rustflags=$RUSTFLAGS"
  echo "target_dir=$CARGO_TARGET_DIR"
  echo "mysql_url=mysql://root:***@127.0.0.1:${MYSQL_PORT}/ecat?ssl-mode=DISABLED"
  echo "redis_url=$REDIS_URL"
  echo "concurrency=$CONCURRENCY total=$TOTAL warmup=$WARMUP rounds=$ROUNDS threshold_us=$THRESHOLD_US middleware=$MIDDLEWARE"
  echo "loadavg=$(cat /proc/loadavg)"
  echo "governor=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unknown)"
  echo "psi_cpu=$(grep '^some' /proc/pressure/cpu 2>/dev/null || true)"
  echo "psi_io=$(grep '^some' /proc/pressure/io 2>/dev/null || true)"
  echo "temps=$(for f in /sys/class/thermal/thermal_zone*/temp; do cat "$f" 2>/dev/null; done | tr '\n' ',' )"
} | tee "$OUT/env.txt"

if (( SKIP_BUILD == 0 )); then
  echo "== build release (target-cpu=native) =="
  cargo build --release -p fullchain-bench --offline
fi
[[ -x "$BIN" ]] || { echo "!! missing binary $BIN" >&2; exit 1; }

if ! docker image inspect mysql:9 >/dev/null 2>&1; then
  echo "!! docker image mysql:9 not cached; no network-safe fallback" >&2
  exit 1
fi
if ! docker image inspect redis:7-alpine >/dev/null 2>&1; then
  echo "!! docker image redis:7-alpine not cached" >&2
  exit 1
fi

require_port_free "$MYSQL_PORT"
require_port_free "$REDIS_PORT"
require_port_free 8000
require_port_free 50051

echo "== start mysql/redis =="
docker run -d --name "$MYSQL_NAME" --cpuset-cpus=4 \
  -e MYSQL_ROOT_PASSWORD=root -e MYSQL_DATABASE=ecat \
  -p "127.0.0.1:${MYSQL_PORT}:3306" mysql:9 \
  --performance-schema=OFF --skip-name-resolve --innodb-buffer-pool-size=512M >/dev/null
CREATED_MYSQL=1
docker run -d --name "$REDIS_NAME" --cpuset-cpus=5 \
  -p "127.0.0.1:${REDIS_PORT}:6379" redis:7-alpine \
  redis-server --save "" --appendonly no >/dev/null
CREATED_REDIS=1

for _ in $(seq 1 90); do
  if docker exec "$MYSQL_NAME" mysqladmin ping -uroot -proot -h127.0.0.1 --silent >/dev/null 2>&1; then
    break
  fi
  sleep 2
done
docker exec "$MYSQL_NAME" mysqladmin ping -uroot -proot -h127.0.0.1 --silent >/dev/null
for _ in $(seq 1 60); do
  docker exec "$REDIS_NAME" redis-cli ping 2>/dev/null | grep -q PONG && break
  sleep 1
done
docker exec "$REDIS_NAME" redis-cli ping 2>/dev/null | grep -q PONG

echo "== start server (cores 0-1) =="
taskset -c 0-1 env \
  ECAT_BENCH_MYSQL_URL="$MYSQL_URL" \
  ECAT_BENCH_REDIS_URL="$REDIS_URL" \
  TOKIO_WORKER_THREADS=2 \
  "$BIN" serve "middleware=$MIDDLEWARE" >"$OUT/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 240); do
  grep -q '^READY ' "$OUT/server.log" && break
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    tail -40 "$OUT/server.log" >&2
    echo "!! server exited before READY" >&2
    exit 1
  fi
  sleep 1
done
grep -q '^READY ' "$OUT/server.log" || { tail -40 "$OUT/server.log" >&2; exit 1; }
head -1 "$OUT/server.log"

run_load() {
  local transport="$1" op="$2"
  local jsonl="$OUT/${transport}-${op}.jsonl"
  local err="$OUT/${transport}-${op}.err"
  echo "== $transport/$op =="
  taskset -c 2-3 env \
    ECAT_BENCH_HTTP="http://127.0.0.1:8000" \
    ECAT_BENCH_GRPC="http://127.0.0.1:50051" \
    TOKIO_WORKER_THREADS=2 \
    timeout 900 "$BIN" load "transport=$transport" "op=$op" \
      "concurrency=$CONCURRENCY" "total=$TOTAL" "warmup=$WARMUP" \
      "rounds=$ROUNDS" "threshold_us=$THRESHOLD_US" \
      >"$jsonl" 2>"$err" || true
}

run_probe() {
  local component="$1"
  local jsonl="$OUT/probe-${component}.jsonl"
  echo "== probe/$component =="
  taskset -c 2-3 env \
    ECAT_BENCH_MYSQL_URL="$MYSQL_URL" \
    ECAT_BENCH_REDIS_URL="$REDIS_URL" \
    TOKIO_WORKER_THREADS=2 \
    timeout 900 "$BIN" probe "component=$component" \
      "concurrency=$CONCURRENCY" "total=$TOTAL" "warmup=$WARMUP" \
      "rounds=$ROUNDS" "threshold_us=$THRESHOLD_US" \
      >"$jsonl" 2>"$OUT/probe-${component}.err" || true
}

if [[ "$OP" == "both" || "$OP" == "read" ]]; then
  run_probe mysql
  run_probe redis
fi
if [[ "$TRANSPORT" == "both" || "$TRANSPORT" == "http" ]]; then
  [[ "$OP" == "both" || "$OP" == "read" ]] && run_load http read
  [[ "$OP" == "both" || "$OP" == "write" ]] && run_load http write
  [[ "$OP" == "both" || "$OP" == "read" || "$OP" == "static" ]] && run_load http static
fi
if [[ "$TRANSPORT" == "both" || "$TRANSPORT" == "grpc" ]]; then
  [[ "$OP" == "both" || "$OP" == "read" ]] && run_load grpc read
  [[ "$OP" == "both" || "$OP" == "write" ]] && run_load grpc write
  [[ "$OP" == "both" || "$OP" == "read" || "$OP" == "static" ]] && run_load grpc static
fi

echo "== summarize =="
load1=$(awk '{print $1}' /proc/loadavg)
psi_io_avg10=$(sed -n 's/^psi_io=.*avg10=\([0-9.]*\).*/\1/p' "$OUT/env.txt" | head -1)
max_temp=$(sed -n 's/^temps=//p' "$OUT/env.txt" | tr ',' '\n' | sort -n | tail -1)
noisy=false
reasons=""
if awk -v l="$load1" -v c="$(nproc)" 'BEGIN {exit !(l > c)}'; then
  noisy=true; reasons="$reasons load1=$load1>nproc=$(nproc)"
fi
if [[ -n "$psi_io_avg10" ]] && awk -v p="$psi_io_avg10" 'BEGIN {exit !(p > 25)}'; then
  noisy=true; reasons="$reasons psi_io_avg10=$psi_io_avg10>25"
fi
if [[ -n "$max_temp" ]] && (( max_temp > 85000 )); then
  noisy=true; reasons="$reasons max_temp=${max_temp}mC>85C"
fi
if [[ -n "$reasons" ]]; then
  echo "noisy=true ($reasons )" | tee -a "$OUT/env.txt"
else
  echo "noisy=false (load1=$load1, psi_io_avg10=${psi_io_avg10:-?}, max_temp=${max_temp:-?}mC)" | tee -a "$OUT/env.txt"
fi

python3 - "$OUT" "$TRANSPORT" "$OP" "$ROUNDS" "$TOTAL" "$MIDDLEWARE" <<'PY'
import json, pathlib, sys
out = pathlib.Path(sys.argv[1])
transport = sys.argv[2]
op = sys.argv[3]
rounds = int(sys.argv[4])
total = int(sys.argv[5])
middleware = sys.argv[6] == "1"

rows = []
for path in sorted(out.glob('*.jsonl')):
    for line in path.read_text().splitlines():
        line = line.strip()
        if line:
            rows.append(json.loads(line))

threshold = int(rows[0]['threshold_us']) if rows else 10000

def row_md(r):
    return (f"| {r['transport']} | {r['op']} | {r['round']} | {r['requests']} | {r['errors']} | "
            f"{r['p50_us']:.0f} | {r['p99_us']:.0f} | {r['p999_us']:.0f} | {r['max_us']:.0f} | "
            f"{r.get('server_max_us', 0)} | {r['over_threshold']} | {r['throughput_rps']:.0f} |")

header = (f"| transport | op | round | req | err | p50µs | p99µs | p99.9µs | maxµs | "
          f"serverMaxµs | >= {threshold}µs | qps |")
sep = "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
print()
print(header)
print(sep)
for row in rows:
    print(row_md(row))
(out / 'summary.md').write_text('\n'.join([header, sep] + [row_md(r) for r in rows]) + '\n')

expected = []
if op in ("read", "both"):
    expected += [("probe", "mysql"), ("probe", "redis")]
if transport in ("http", "both"):
    if op in ("read", "both"):
        expected.append(("http", "read"))
    if op in ("write", "both"):
        expected.append(("http", "write"))
    if op in ("read", "both", "static"):
        expected.append(("http", "static"))
if transport in ("grpc", "both") and not middleware:
    if op in ("read", "both"):
        expected.append(("grpc", "read"))
    if op in ("write", "both"):
        expected.append(("grpc", "write"))
    if op in ("read", "both", "static"):
        expected.append(("grpc", "static"))

def rows_for(name, component):
    if name == "probe":
        path = out / f"probe-{component}.jsonl"
        key = ("probe", component)
    else:
        path = out / f"{name}-{component}.jsonl"
        key = (name, component)
    if not path.exists():
        return key, []
    return key, [json.loads(line) for line in path.read_text().splitlines() if line.strip()]

problems = []
for name, component in expected:
    key, got = rows_for(name, component)
    if len(got) != rounds:
        problems.append(f"{key}: expected {rounds} rounds, got {len(got)}")
        continue
    if sorted(row["round"] for row in got) != list(range(1, rounds + 1)):
        problems.append(f"{key}: round numbers are not 1..{rounds}")
    for row in got:
        if row["requests"] != total:
            problems.append(f"{key} round {row['round']}: requests {row['requests']} != {total}")
        if row["errors"] != 0:
            problems.append(f"{key} round {row['round']}: {row['errors']} errors")
        if component == "read" and row["over_threshold"] != 0:
            problems.append(f"{key} round {row['round']}: {row['over_threshold']} samples >= {threshold}us")

if problems:
    print("\nGATE FAILED:")
    for problem in problems:
        print(f"  - {problem}")
    sys.exit(1)
print(f"\nGATE PASSED: {len(expected)} expected series, {rounds} rounds each, {total} requests/round, 0 errors, 0 read samples >= {threshold}us")
PY
