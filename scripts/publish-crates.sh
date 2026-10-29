#!/usr/bin/env bash
# 把 workspace 内所有 crate 逐个发布到 crates.io。
#
# 为什么不是 `cargo publish --workspace`：crates.io 对新 crate 名限速
# （约 5 个突发，之后约每 10 分钟 1 个），一次发 52 个必然撞 429 而中断。
# 本脚本的处理：
#   - 撞限流   -> 退避 INTERVAL 秒后重试同一个 crate
#   - 依赖未就绪 -> 跳过本轮，等依赖发完下一轮再试
#   - 已发布   -> 自动跳过（按 crates.io API 查证）
#   - 其他错误 -> 记 FAIL 并跳过，不再重试
# 中断后直接重跑即可续上，不会重复发布。
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

INTERVAL="${INTERVAL:-620}"     # 限流退避秒数（crates.io 新 crate 窗口约 10 分钟）
MAX_STALLS="${MAX_STALLS:-3}"   # 连续多少轮毫无进展就放弃
LOG="${LOG:-$ROOT/target/publish.log}"
UA='e-cat-release (https://github.com/erikwang2013/e-cat)'

mkdir -p "$(dirname "$LOG")"
log() { printf '%s %s\n' "$(date '+%F %T')" "$*" | tee -a "$LOG"; }

declare -A ver done
names=()
# 按 workspace 依赖拓扑排序发布：先把被依赖的 crate 发出去，避免
# `cargo publish` 为未发布的依赖反复解析索引而空转。
while IFS=$'\t' read -r name version; do
  [[ -n "$name" ]] || continue
  ver["$name"]="$version"
  names+=("$name")
done < <(cargo metadata --no-deps --format-version 1 | python3 -c '
import json, sys
d = json.load(sys.stdin)
pkgs = {p["name"]: p for p in d["packages"] if p["publish"] != []}
deps = {n: set() for n in pkgs}
for n, p in pkgs.items():
    for dep in p.get("dependencies", []):
        # 普通依赖 + build 依赖；dev-dependencies 不约束 crates.io 发布顺序。
        if dep.get("kind") in (None, "build") and dep["name"] in pkgs:
            deps[n].add(dep["name"])
order = []
ready = [n for n in pkgs if not deps[n]]
while ready:
    n = ready.pop(0)
    order.append(n)
    for m in pkgs:
        if n in deps[m]:
            deps[m].remove(n)
            if not deps[m] and m not in order and m not in ready:
                ready.append(m)
for n in pkgs:
    if n not in order:
        order.append(n)
for n in order:
    print(n + "\t" + pkgs[n]["version"])
')

total=${#names[@]}
if (( total == 0 )); then log "没有待发布的 crate。"; exit 0; fi
log "== 待发布 $total 个 crate | 日志 $LOG | 退避 ${INTERVAL}s"

already_published() { # name version
  curl -sf --max-time 20 -H "User-Agent: $UA" \
    "https://crates.io/api/v1/crates/$1/$2" >/dev/null 2>&1
}

# 429 响应里 crates.io 会给出确切的重试时刻，例如：
#   Please try again after Thu, 24 Sep 2026 13:36:12 GMT and see https://...
# 按它等待比固定睡 INTERVAL 快得多（实测约 6 分钟 vs 10 分钟）。
# 解析失败则回退到 INTERVAL。
retry_after() { # <cargo 输出> -> 秒数
  local ts epoch
  ts=$(printf '%s\n' "$1" | grep -oP 'try again after \K.*?\d{2}:\d{2}:\d{2} GMT' | head -1)
  [[ -z "$ts" ]] && return 1
  epoch=$(date -d "$ts" +%s 2>/dev/null) || return 1
  local wait=$(( epoch - $(date +%s) + 15 ))   # +15s 余量
  (( wait < 30 )) && wait=30
  printf '%s' "$wait"
}

remaining=$total
stalls=0
wait_seconds=$INTERVAL

while (( remaining > 0 )); do
  progressed=0

  for name in "${names[@]}"; do
    [[ -n "${done[$name]:-}" ]] && continue
    version="${ver[$name]}"

    if already_published "$name" "$version"; then
      log "skip    $name $version（crates.io 上已存在）"
      done[$name]=1; ((remaining--)); continue
    fi

    # 限流和网络抖动都在原地重试，不跳出重扫整轮：重扫要为每个剩余 crate
    # 跑一次 cargo 依赖解析（~3s），43 个约 110s/轮，叠在 ~500s 的限流等待上
    # 白多花近 20% 时间。
    while :; do
      log "publish $name $version ..."
      out="$(cargo publish -p "$name" 2>&1)"
      rc=$?
      (( rc == 0 )) && break

      case "$out" in
        *"status 429"*|*"Too Many Requests"*)
          wait_seconds=$(retry_after "$out" || printf '%s' "$INTERVAL")
          log "429     $name —— 限流，等待 ${wait_seconds}s（至 $(date -d "+${wait_seconds} seconds" '+%H:%M:%S')）"
          sleep "$wait_seconds" ;;
        *"failed to update registry"*|*"network failure"*|*"timed out"*|*"connection refused"*|*"connection reset"*)
          # crates.io 索引拉取偶发失败，不是发布本身的问题，重试即可
          log "net     $name —— 网络抖动，60s 后重试"
          sleep 60 ;;
        *) break ;;
      esac
    done

    if (( rc == 0 )); then
      log "ok      $name $version"
      done[$name]=1; ((remaining--)); progressed=1
      sleep 20   # 等索引同步，后续依赖它的 crate 才解析得到
      continue
    fi

    case "$out" in
      *"already uploaded"*|*"already exists on crates.io"*)
        log "skip    $name $version（已上传）"
        done[$name]=1; ((remaining--)); continue ;;
      *"no matching package"*|*"failed to select a version"*|*"not found in registry"*)
        # 依赖尚未发布，本轮跳过，等下一轮
        log "wait    $name（依赖尚未发布）"
        continue ;;
      *)
        log "FAIL    $name $version"
        printf '%s\n' "$out" | tail -12 | tee -a "$LOG"
        done[$name]=1; ((remaining--)) ;;   # 真实错误，不再重试
    esac
  done

  (( remaining == 0 )) && break

  if (( progressed == 0 )); then
    ((stalls++))
    if (( stalls >= MAX_STALLS )); then
      log "== 连续 $stalls 轮无进展，中止。剩余未发布："
      for name in "${names[@]}"; do
        [[ -z "${done[$name]:-}" ]] && log "   - $name ${ver[$name]}"
      done
      exit 1
    fi
    log "-- 本轮无进展（$stalls/$MAX_STALLS），等待 ${INTERVAL}s"
    sleep "$INTERVAL"
  else
    stalls=0
  fi
done

log "== 全部完成。日志：$LOG"
