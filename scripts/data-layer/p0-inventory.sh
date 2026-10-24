#!/usr/bin/env bash
# P0 baseline inventory: run p0-inventory.sql against MYSQL_DSN, one section at a time
# so a denied or unavailable section (mysql.user, performance_schema, replication) does
# not stop the rest.
# Usage: MYSQL_DSN=mysql://user:pass@host:3306/db scripts/data-layer/p0-inventory.sh [--check|--dry-run]
set -euo pipefail

SQL="$(dirname "$0")/p0-inventory.sql"
SECTIONS=(VERSION AUTH_PLUGINS TABLES INDEXES SLOW_QUERIES LOCKS REPLICATION PARTITIONS)

[[ -f "$SQL" ]] || { echo "missing SQL file: $SQL" >&2; exit 1; }
for s in "${SECTIONS[@]}"; do
  grep -q -- "-- SECTION: $s" "$SQL" || { echo "missing section: $s" >&2; exit 1; }
done

case "${1:-}" in
  --check)
    [[ -x "$0" ]] || { echo "p0-inventory: not executable: $0" >&2; exit 1; }
    echo "p0-inventory: ok"
    exit 0
    ;;
  --dry-run)
    cat "$SQL"
    exit 0
    ;;
  '') ;;
  *) echo "usage: $0 [--check|--dry-run] (or set MYSQL_DSN)" >&2; exit 2 ;;
esac

: "${MYSQL_DSN:?set MYSQL_DSN}"

# The mysql client cannot parse URIs, so split mysql://user[:pass]@host[:port]/db here.
# shortcut: no IPv6 literals or DSN query parameters (e.g. ?ssl-mode=); user/password must be URL-encoded.
dsn=${MYSQL_DSN#mysql://}
if [[ "$dsn" == "$MYSQL_DSN" || "$dsn" != *@* || "${dsn#*@}" != */* ]]; then
  echo "MYSQL_DSN must look like mysql://user:pass@host:3306/db" >&2
  exit 1
fi
userinfo=${dsn%%@*}
hostdb=${dsn#*@}
user=${userinfo%%:*}
hostport=${hostdb%%/*}
db=${hostdb#*/}
host=${hostport%%:*}
port=3306
[[ "$hostport" == *:* ]] && port=${hostport##*:}
[[ -n "$user" && -n "$host" && -n "$db" ]] || { echo "MYSQL_DSN must look like mysql://user:pass@host:3306/db" >&2; exit 1; }

# Percent-decode user/password; MYSQL_PWD keeps the password out of argv/ps.
urldecode() { printf '%b' "${1//%/\\x}"; }
user=$(urldecode "$user")
if [[ "$userinfo" == *:* ]]; then export MYSQL_PWD="$(urldecode "${userinfo#*:}")"; fi

mysql_args=(--batch --raw --protocol=TCP --host="$host" --port="$port" --user="$user" --database="$db")

section_sql() {
  awk -v name="$1" '
    $0 == "-- SECTION: " name { on = 1; next }
    on && /^-- SECTION: / { exit }
    on { print }
  ' "$SQL"
}

failed=0
for s in "${SECTIONS[@]}"; do
  printf -- '-- SECTION: %s\n' "$s"
  if ! mysql "${mysql_args[@]}" <<<"$(section_sql "$s")"; then
    failed=1
    if [[ "$s" == AUTH_PLUGINS ]]; then
      printf 'unknown\tunknown\tunknown\n'
    fi
  fi
done
exit "$failed"
