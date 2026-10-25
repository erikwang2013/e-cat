#!/usr/bin/env bash
# Per-shard backup + binlog position + restore/checksum drill.
#
# --check is static (no DB): it validates the marker list below.
# --run needs SHARD_DSNS (comma-separated mysql:// DSNs) and RESTORE_DSN.
#
# Secrets: DSNs stay in the environment (visible in /proc/<pid>/environ, same
# caveat as MYSQL_PWD); the mysql/mysqldump commands only see a 0600 defaults
# file via --defaults-extra-file, never a password in argv. Literal passwords
# are supported; percent-encoded DSN passwords are not decoded by this script.
#
# MySQL 8.0: SHOW MASTER STATUS. MySQL 8.4/9: SHOW BINARY LOG STATUS.
# This script never uses the legacy replication status command or the removed
# authentication plugin.
set -euo pipefail

SELF=$0

usage() {
  cat <<'EOF'
usage: sharded-backup-drill.sh --check
       SHARD_DSNS=mysql://u:p@h1:3306/db,mysql://u:p@h2:3306/db \
       RESTORE_DSN=mysql://u:p@h:3306/scratch sharded-backup-drill.sh --run

  --check   static marker check, no database access
  --run     per-shard mysqldump + binlog position, restore into a scratch
            schema and compare CHECKSUM TABLE output
EOF
}

fail() {
  echo "sharded-backup-drill: $*" >&2
  exit 1
}

usage_error() {
  echo "sharded-backup-drill: $*" >&2
  usage >&2
  exit 2
}

require_text() {
  local needle=$1 description=$2
  grep -Fq -- "$needle" "$SELF" || fail "missing $description"
}

check() {
  require_text 'mysqldump --defaults-extra-file' "per-shard mysqldump step"
  require_text 'BINLOG_SQL="SHOW BINARY LOG STATUS"' "8.4/9 binlog position capture"
  require_text 'BINLOG_SQL="SHOW MASTER STATUS"' "8.0 binlog position capture"
  require_text 'if is_mysql_80 "$version"; then' "8.0-only binlog branch"
  require_text 'CREATE DATABASE IF NOT EXISTS' "scratch schema restore"
  require_text 'CHECKSUM TABLE' "checksum verification"
  require_text 'validate_ident()' "identifier validation"
  require_text '--defaults-extra-file' "defaults-file credentials"
  require_text 'mktemp' "temporary defaults file"
  require_text 'chmod 600' "defaults file permissions"
  require_text 'SHARD_DSNS' "SHARD_DSNS gate"
  require_text 'RESTORE_DSN' "RESTORE_DSN gate"

  local forbidden_slave forbidden_auth
  forbidden_slave=$(printf 'SHOW %s STATUS' SLAVE)
  forbidden_auth=$(printf 'mysql_%s_password' native)
  ! grep -Fq -- "$forbidden_slave" "$SELF" \
    || fail "legacy replication status command is forbidden"
  ! grep -Fq -- "$forbidden_auth" "$SELF" \
    || fail "removed authentication plugin is forbidden"

  echo "sharded-backup-drill: ok"
}

validate_ident() {
  local value=$1 label=$2
  [[ $value =~ ^[A-Za-z0-9_]+$ ]] || fail "invalid $label: $value"
}

# Parse mysql://user:pass@host[:port]/db. Sets DSN_* globals.
parse_dsn() {
  local dsn=$1
  [[ $dsn == mysql://* ]] || fail "DSN must start with mysql://"
  local rest=${dsn#mysql://}
  [[ $rest == *@* ]] || fail "DSN has no @host"
  local userinfo=${rest%@*}
  rest=${rest##*@}
  local hostport=${rest%%/*}
  [[ $hostport != "$rest" ]] || fail "DSN has no /database"
  local db=${rest#*/}
  db=${db%%\?*}
  DSN_USER=${userinfo%%:*}
  if [[ $userinfo == *:* ]]; then
    DSN_PASS=${userinfo#*:}
  else
    DSN_PASS=
  fi
  DSN_HOST=${hostport%:*}
  if [[ $hostport == *:* ]]; then
    DSN_PORT=${hostport##*:}
  else
    DSN_PORT=3306
  fi
  DSN_DB=$db
  [[ -n $DSN_HOST && -n $DSN_DB ]] || fail "DSN host/database must not be empty"
  [[ $DSN_PASS != *$'\n'* && $DSN_PASS != *$'\r'* ]] \
    || fail "DSN password must not contain newlines"
  validate_ident "$DSN_DB" "database"
}

# Write a 0600 defaults file for the already-parsed DSN_* globals.
# Sets DEFAULTS_FILE and appends it to the cleanup list.
make_defaults() {
  local file
  file=$(mktemp) || fail "mktemp failed"
  chmod 600 "$file"
  {
    printf '[client]\n'
    printf 'user=%s\n' "$DSN_USER"
    printf 'password=%s\n' "$DSN_PASS"
    printf 'host=%s\n' "$DSN_HOST"
    printf 'port=%s\n' "$DSN_PORT"
  } > "$file"
  DEFAULTS_FILE=$file
  DEFAULTS_FILES+=("$file")
}

is_mysql_80() {
  [[ $1 == 8.0.* ]]
}

normalize_checksum() {
  awk -F'\t' '{ name=$1; sub(/^.*\./, "", name); print name "\t" $2 }' | LC_ALL=C sort
}

DEFAULTS_FILES=()
cleanup() {
  local file
  if (( ${#DEFAULTS_FILES[@]} > 0 )); then
    for file in "${DEFAULTS_FILES[@]}"; do
      rm -f -- "$file"
    done
  fi
}

run() {
  [[ -n ${SHARD_DSNS:-} ]] || fail "SHARD_DSNS is required for --run"
  [[ -n ${RESTORE_DSN:-} ]] || fail "RESTORE_DSN is required for --run"
  command -v mysql >/dev/null 2>&1 || fail "mysql client not found"
  command -v mysqldump >/dev/null 2>&1 || fail "mysqldump not found"

  local backup_dir=${BACKUP_DIR:-backup-drill-$(date -u +%Y%m%dT%H%M%SZ)}
  mkdir -p -- "$backup_dir"
  trap cleanup EXIT

  parse_dsn "$RESTORE_DSN"
  local restore_base=$DSN_DB
  make_defaults
  local restore_defaults=$DEFAULTS_FILE

  local IFS=','
  local -a shard_dsns
  read -r -a shard_dsns <<< "$SHARD_DSNS"
  [[ ${#shard_dsns[@]} -gt 0 ]] || fail "SHARD_DSNS is empty"

  local index dsn defaults version binlog_sql dump scratch checksum_list tables table
  for index in "${!shard_dsns[@]}"; do
    dsn=${shard_dsns[$index]}
    parse_dsn "$dsn"
    make_defaults
    local defaults=$DEFAULTS_FILE
    local tag=$((index + 1))
    dump="$backup_dir/shard-$tag.sql"

    version=$(mysql --defaults-extra-file="$defaults" -N -B -e 'SELECT VERSION()')
    if is_mysql_80 "$version"; then
      BINLOG_SQL="SHOW MASTER STATUS"
    else
      BINLOG_SQL="SHOW BINARY LOG STATUS"
    fi
    binlog_sql=$BINLOG_SQL
    mysql --defaults-extra-file="$defaults" -N -B -e "$binlog_sql" \
      > "$backup_dir/shard-$tag.binlog"
    [[ -s "$backup_dir/shard-$tag.binlog" ]] \
      || fail "no binlog position for shard $tag (binary logging disabled?)"

    mysqldump --defaults-extra-file="$defaults" --single-transaction \
      --source-data=2 --set-gtid-purged=OFF --routines --triggers --events \
      "$DSN_DB" > "$dump"
    [[ -s "$dump" ]] || fail "empty dump for shard $tag"

    scratch="${restore_base}_shard${tag}"
    validate_ident "$scratch" "scratch schema"
    (( ${#scratch} <= 64 )) || fail "scratch schema name too long: $scratch"
    mysql --defaults-extra-file="$restore_defaults" \
      -e "CREATE DATABASE IF NOT EXISTS \`$scratch\`"
    mysql --defaults-extra-file="$restore_defaults" "$scratch" < "$dump"

    tables=$(mysql --defaults-extra-file="$defaults" -N -B "$DSN_DB" -e \
      "SELECT table_name FROM information_schema.tables \
       WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE' \
       ORDER BY table_name")
    if [[ -z $tables ]]; then
      echo "shard $tag: no base tables; restore/checksum skipped"
      continue
    fi
    checksum_list=
    while IFS= read -r table; do
      validate_ident "$table" "table"
      checksum_list+="\`$table\`,"
    done <<< "$tables"
    checksum_list=${checksum_list%,}

    local source_sum restore_sum
    source_sum=$(mysql --defaults-extra-file="$defaults" -N -B "$DSN_DB" -e \
      "CHECKSUM TABLE $checksum_list" | normalize_checksum)
    restore_sum=$(mysql --defaults-extra-file="$restore_defaults" -N -B "$scratch" -e \
      "CHECKSUM TABLE $checksum_list" | normalize_checksum)
    [[ "$source_sum" == "$restore_sum" ]] \
      || fail "checksum mismatch for shard $tag ($DSN_DB -> $scratch)"

    echo "shard $tag: dumped, binlog captured, restored to $scratch, checksum ok"
  done

  echo "sharded-backup-drill: ok (backups in $backup_dir)"
}

case "${1:-}" in
  --check) check ;;
  --run) run ;;
  -h|--help) usage ;;
  *) usage_error "unknown argument: ${1:-<none>}" ;;
esac
