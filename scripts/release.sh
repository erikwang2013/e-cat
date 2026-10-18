#!/usr/bin/env bash
# 发布规则：在 main 上以 workspace 版本与最新 tag 的较大者为基准 bump，
# 同步所有版本串 → 提交并推送 → 打 tag 并推送。
# 用法：scripts/release.sh [patch|minor|major|X.Y.Z]   （默认 patch）
# 不做任何打包（cargo package / docker build / crates.io publish 一律跳过）。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

branch=$(git symbolic-ref --short HEAD 2>/dev/null || true)
if [[ "$branch" != "main" ]]; then
  echo "!! 发布必须在 main 分支上进行（当前：${branch:-detached}）" >&2
  exit 1
fi
if [[ -n "$(git status --porcelain)" ]]; then
  echo "!! 工作树不干净，先提交或还原：" >&2
  git status --short >&2
  exit 1
fi

latest_tag=$(git tag --sort=-v:refname | head -1 || true)
if [[ -z "$latest_tag" ]]; then
  echo "!! 没有现存 tag，首次发布请手动指定版本（X.Y.Z）。" >&2
  exit 1
fi

current=$(grep -m1 '^version = ' Cargo.toml | grep -oP '\d+\.\d+\.\d+')
tag_ver=${latest_tag#v}

# 以 Cargo.toml 与最新 tag 的较大者作为 bump 基准
base=$(printf '%s\n%s\n' "$current" "$tag_ver" | sort -V | tail -1)

bump="${1:-patch}"
if [[ "$bump" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  new_ver="$bump"
else
  IFS=. read -r major minor patch <<< "$base"
  case "$bump" in
    patch) new_ver="$major.$minor.$((patch + 1))" ;;
    minor) new_ver="$major.$((minor + 1)).0" ;;
    major) new_ver="$((major + 1)).0.0" ;;
    *)
      echo "!! 用法：$0 [patch|minor|major|X.Y.Z]" >&2
      exit 1
      ;;
  esac
fi
new_tag="v$new_ver"

if [[ "$base" == "$new_ver" || "$(printf '%s\n%s\n' "$base" "$new_ver" | sort -V | tail -1)" != "$new_ver" ]]; then
  echo "!! 新版本 $new_ver 必须大于基准 $base" >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/$new_tag" >/dev/null; then
  echo "!! tag $new_tag 已存在" >&2
  exit 1
fi
if ! grep -q "^## \[$new_ver\]" CHANGELOG.md; then
  echo "!! CHANGELOG.md 缺少 [$new_ver] 段，先补齐再发布。" >&2
  exit 1
fi

echo "== 最新 tag: $latest_tag | 当前版本: $current | 基准: $base | 新版本: $new_ver"

# 同步所有版本串：workspace/成员 Cargo.toml、14 份 README、配置示例。
# 只改「version = "old"」且行以 version 开头或含 ecat 的行，避免误伤外部依赖。
counts=$(python3 - "$base" "$new_ver" <<'PY'
import os
import re
import sys
from pathlib import Path

old, new = sys.argv[1], sys.argv[2]
cargo_refs = readme_refs = config_refs = lock_refs = 0
member_names = set()

# 不用 rglob：target/ 可达数十万文件，遍历会拖到分钟级。
cargo_files = []
for root, dirs, files in os.walk("."):
    dirs[:] = [
        d
        for d in dirs
        if d not in ("target", ".git", ".claude", ".claude-flow", ".swarm", ".superpowers")
    ]
    if "Cargo.toml" in files:
        cargo_files.append(Path(root) / "Cargo.toml")

for path in sorted(cargo_files):
    text = path.read_text(encoding="utf-8")
    pkg = re.search(r"^\[package\]\s*$", text, re.M)
    if pkg:
        name = re.search(r'^name = "([^"]+)"', text[pkg.end():], re.M)
        if name:
            member_names.add(name.group(1))

    lines = text.splitlines(keepends=True)
    out = []
    changed = False
    for line in lines:
        if f'version = "{old}"' in line and (
            line.lstrip().startswith("version =") or "ecat" in line
        ):
            line = line.replace(f'version = "{old}"', f'version = "{new}"')
            cargo_refs += 1
            changed = True
        out.append(line)
    if changed:
        path.write_text("".join(out), encoding="utf-8")

# 只改 workspace 成员的 Cargo.lock 版本，不走 cargo check（整仓编译太慢，
# 也避开 /home 写盘抖动）；随后用 cargo metadata --locked 校验锁文件。
lock_path = Path("Cargo.lock")
current = None
out = []
for line in lock_path.read_text(encoding="utf-8").splitlines(keepends=True):
    name = re.match(r'name = "([^"]+)"', line)
    if name:
        current = name.group(1)
    if current in member_names and line.startswith(f'version = "{old}"'):
        line = line.replace(f'version = "{old}"', f'version = "{new}"', 1)
        lock_refs += 1
        current = None
    out.append(line)
lock_path.write_text("".join(out), encoding="utf-8")

readmes = [Path("README.md"), Path("README.en.md")]
readmes += sorted(Path("docs/i18n").glob("*/README.md"))
for path in readmes:
    text = path.read_text(encoding="utf-8")
    n = text.count(old)
    if n:
        path.write_text(text.replace(old, new), encoding="utf-8")
        readme_refs += n

config = Path("config/databases.example.yaml")
text = config.read_text(encoding="utf-8")
n = text.count(old)
if n:
    config.write_text(text.replace(old, new), encoding="utf-8")
    config_refs += n

print(f"{cargo_refs} {readme_refs} {config_refs} {lock_refs}")
PY
)
read -r cargo_refs readme_refs config_refs lock_refs <<< "$counts"

echo "== 同步引用：Cargo.toml $cargo_refs 处 | README $readme_refs 处 | 配置 $config_refs 处 | Cargo.lock $lock_refs 个成员"
if (( cargo_refs == 0 || readme_refs == 0 || lock_refs == 0 )); then
  echo "!! 版本串替换数量异常（Cargo=$cargo_refs, README=$readme_refs, lock=$lock_refs），中止。" >&2
  exit 1
fi

if ! timeout 120 cargo metadata --no-deps --locked --offline --quiet >/dev/null; then
  echo "!! Cargo.lock 校验失败（cargo metadata --locked）" >&2
  exit 1
fi
echo "== Cargo.lock 校验通过"

changed=$(git diff --name-only)
if [[ -z "$changed" ]]; then
  echo "!! 没有产生文件变更，中止。" >&2
  exit 1
fi
echo "== 即将提交："
printf '   %s\n' $changed

if [[ "${DRY_RUN:-0}" == "1" ]]; then
  echo "== DRY RUN：以上为将要提交的变更（未提交、未打 tag）"
  git diff --stat
  exit 0
fi

mapfile -t files <<< "$changed"
# 不先 git add：--only 直接提交这些路径的工作树内容，少一次 index.lock 竞争。
git commit --only -m "chore(release): $base → $new_ver" \
  -m "Lockstep version sync: $cargo_refs Cargo.toml refs, $readme_refs README refs, $config_refs config ref; Cargo.lock refreshed." \
  -- "${files[@]}"
git push

git tag "$new_tag"
git push origin "$new_tag"

echo "== 完成：$new_tag 已推送（未打包）"
