#!/usr/bin/env bash
# Sharding migration verifier: compare row counts, primary-key-range checksums and
# foreign-key orphan counts of one table between a source and a target MySQL.
# Read-only. Exit 0 when both sides produce identical output; otherwise print the
# diff and exit 1.
#
# Usage:
#   scripts/sharding/verify.sh --check
#   scripts/sharding/verify.sh --source-dsn mysql://user:pass@host:3306/db \
#                              --target-dsn mysql://user:pass@host:3306/db --table <name>
set -euo pipefail

SQL="$(dirname "$0")/checksum.sql"
TABLE_RE='^[A-Za-z_][A-Za-z0-9_]*$'
SECTIONS=(ROW_COUNTS PK_RANGES CHECKSUMS ORPHANS)

usage() {
  cat <<'EOF'
usage: verify.sh --check
       verify.sh --source-dsn <dsn> --target-dsn <dsn> --table <name>

  --check      verify checksum.sql contains the four section markers
  comparison   run checksum.sql on both sides and diff the output
               dsn: mysql://user[:pass]@host[:port]/db
EOF
}

# --check runs without a MySQL server; it only inspects checksum.sql.
check_sections() {
  [[ -f "$SQL" ]] || { echo "missing SQL file: $SQL" >&2; exit 1; }
  local section
  for section in "${SECTIONS[@]}"; do
    grep -q -- "-- SECTION: $section" "$SQL" || { echo "missing section: $section" >&2; exit 1; }
  done
}

# Never echo a DSN's raw input: a malformed DSN can carry the password anywhere.
# Only the fixed mysql:// prefix and the host part of that scheme survive redaction.
redact_dsn() {
  if [[ "$1" != mysql://* ]]; then
    printf '<redacted DSN>'
  elif [[ "$1" == *@* ]]; then
    printf 'mysql://***@%s' "${1##*@}"
  else
    printf 'mysql://***'
  fi
}

# Unknown arguments can themselves be pasted DSNs; redact anything DSN-shaped.
redact_arg() {
  if [[ "$1" == *://* || "$1" == *@* ]]; then
    redact_dsn "$1"
  else
    printf '%s' "$1"
  fi
}

# mysql://user[:pass]@host[:port]/db -> DSN_USER/DSN_PASS/DSN_HOST/DSN_PORT/DSN_DB.
# shortcut: no IPv6 literals or DSN query parameters (e.g. ?ssl-mode=); user/password must be URL-encoded.
parse_dsn() {
  local dsn=${1#mysql://}
  if [[ "$1" != mysql://* || "$dsn" != *@* || "${dsn#*@}" != */* ]]; then
    echo "invalid DSN (want mysql://user[:pass]@host[:port]/db): $(redact_dsn "$1")" >&2
    exit 1
  fi
  local userinfo=${dsn%%@*}
  local hostdb=${dsn#*@}
  local hostport=${hostdb%%/*}
  DSN_DB=${hostdb#*/}
  DSN_HOST=${hostport%%:*}
  DSN_PORT=3306
  [[ "$hostport" == *:* ]] && DSN_PORT=${hostport##*:}
  DSN_USER=${userinfo%%:*}
  DSN_PASS=
  [[ "$userinfo" == *:* ]] && DSN_PASS=${userinfo#*:}
  urldecode() { printf '%b' "${1//%/\\x}"; }
  DSN_USER=$(urldecode "$DSN_USER")
  DSN_PASS=$(urldecode "$DSN_PASS")
  [[ -n "$DSN_USER" && -n "$DSN_HOST" && -n "$DSN_DB" ]] \
    || { echo "invalid DSN (want mysql://user[:pass]@host[:port]/db): $(redact_dsn "$1")" >&2; exit 1; }
}

# Run the rendered SQL for one side on stdout; password stays out of argv/ps.
run_side() {
  parse_dsn "$1"
  if [[ -n "$DSN_PASS" ]]; then export MYSQL_PWD="$DSN_PASS"; else unset MYSQL_PWD; fi
  mysql --batch --raw --skip-column-names --protocol=TCP --connect-timeout=10 \
    --host="$DSN_HOST" --port="$DSN_PORT" --user="$DSN_USER" --database="$DSN_DB"
}

check_or_compare=compare
source_dsn=
target_dsn=
table=
while (($#)); do
  case "$1" in
    --check) check_or_compare=check ;;
    --source-dsn) source_dsn=${2:-}; shift ;;
    --target-dsn) target_dsn=${2:-}; shift ;;
    --table) table=${2:-}; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $(redact_arg "$1")" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

if [[ "$check_or_compare" == check ]]; then
  check_sections
  echo "sharding-checksum: ok"
  exit 0
fi

# Validate before touching a DSN or a server: the identifier is interpolated into SQL.
[[ "$table" =~ $TABLE_RE ]] || { echo "invalid table identifier: $table" >&2; exit 1; }
[[ -n "$source_dsn" ]] || { echo "--source-dsn is required" >&2; exit 2; }
[[ -n "$target_dsn" ]] || { echo "--target-dsn is required" >&2; exit 2; }

check_sections

tmpdir=$(mktemp -d)
src="$tmpdir/source"
tgt="$tmpdir/target"
trap 'rm -rf "$tmpdir"' EXIT

# Some hosts put a broken diff first in PATH (it exits 0 even for different files),
# which would turn every comparison green. A candidate qualifies only if it reports
# identical files as equal AND different files as different (a tool that always fails
# would otherwise look like a detector).
printf 'a\n' >"$tmpdir/probe-a"
printf 'b\n' >"$tmpdir/probe-b"
DIFF=
for candidate in /usr/bin/diff "$(command -v diff || true)"; do
  [[ -n "$candidate" && -x "$candidate" ]] || continue
  if "$candidate" -u "$tmpdir/probe-a" "$tmpdir/probe-a" >/dev/null 2>&1 \
     && ! "$candidate" -u "$tmpdir/probe-a" "$tmpdir/probe-b" >/dev/null 2>&1; then
    DIFF=$candidate
    break
  fi
done
[[ -n "$DIFF" ]] || { echo "no working diff found (PATH diff reports no difference)" >&2; exit 1; }

sed "s/{{TABLE}}/$table/g" "$SQL" | run_side "$source_dsn" >"$src"
sed "s/{{TABLE}}/$table/g" "$SQL" | run_side "$target_dsn" >"$tgt"

# mysql exits non-zero on errors, but a gutted checksum.sql could still exit 0 with no
# output; refuse to compare two empty results.
[[ -s "$src" ]] || { echo "sharding-checksum: no output from source (checksum.sql produced nothing)" >&2; exit 1; }
[[ -s "$tgt" ]] || { echo "sharding-checksum: no output from target (checksum.sql produced nothing)" >&2; exit 1; }

rc=0
"$DIFF" -u "$src" "$tgt" || rc=$?
case "$rc" in
  0) echo "sharding-checksum: ok (table $table)" ;;
  1) echo "sharding-checksum: MISMATCH (table $table)" >&2; exit 1 ;;
  *) echo "sharding-checksum: diff failed with rc=$rc (tool error, not a data mismatch)" >&2; exit 2 ;;
esac
