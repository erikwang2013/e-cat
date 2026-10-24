#!/usr/bin/env bash
# Gate: ShardingSphere-Proxy must authenticate to its MySQL backend with
# caching_sha2_password. MySQL 9.x removed the legacy native plugin and 8.4 disables
# it by default, so a proxy that cannot do caching_sha2_password forces the A phase
# (direct client connections) for that version.
#
# The gate does not provision servers. Prepare the pair first:
#   1. Start the backend image selected by MYSQL_VERSION.
#   2. Create the backend account the proxy connects with (see usage()).
#   3. Point the proxy at that backend and expose the proxy endpoint as PROXY_DSN.
#
# Usage:
#   scripts/sharding/proxy-auth-check.sh --check
#   MYSQL_VERSION=8.4 PROXY_DSN=mysql://user:pass@proxy-host:3307/db \
#     scripts/sharding/proxy-auth-check.sh
set -euo pipefail

# Assembled from two pieces so this guard does not match its own source: the file
# must never contain the legacy plugin name (checked by --check).
FORBIDDEN_PLUGIN="mysql_native""_password"
SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"

usage() {
  cat <<'EOF'
usage: proxy-auth-check.sh --check
       MYSQL_VERSION=8.4|9 PROXY_DSN=<dsn> proxy-auth-check.sh

  --check         static check: both backend image branches and the
                  caching_sha2_password account creation exist, and the legacy
                  auth plugin is not referenced; needs no server
  MYSQL_VERSION   8.4 or 9; selects the backend image the proxy must reach
  PROXY_DSN       proxy endpoint: mysql://user[:pass]@host[:port]/db

  Backend account (run on the MySQL backend, minimum rights for this gate):
    CREATE USER 'ecat_proxy'@'%' IDENTIFIED WITH caching_sha2_password BY '<secret>';
    GRANT SELECT ON <db>.* TO 'ecat_proxy'@'%';

  Live gate: connect through the proxy, run SELECT 1, record proxy and MySQL
  versions. Exit 0 only when SELECT 1 returns 1 and the MySQL version matches
  MYSQL_VERSION (8.4.x for 8.4, 9.x for 9); exit 1 otherwise.
EOF
}

fail() { echo "proxy-auth-check: $*" >&2; exit 1; }

# MYSQL_VERSION -> backend image. Kept as a function so --check exercises the
# mapping instead of grepping for the tags in its own source.
backend_image() {
  case "$1" in
    8.4) printf 'mysql:8.4\n' ;;
    9) printf 'mysql:9\n' ;;
    *) return 1 ;;
  esac
}

check_static() {
  [[ "$(backend_image 8.4 || true)" == "mysql:8.4" ]] || fail "missing mysql:8.4 branch"
  [[ "$(backend_image 9 || true)" == "mysql:9" ]] || fail "missing mysql:9 branch"

  # Anchored at line start so a commented-out recipe does not count.
  grep -Eq '^[[:space:]]*CREATE USER .*IDENTIFIED WITH caching_sha2_password' "$SELF" \
    || fail "missing backend account creation with caching_sha2_password"

  local plugins
  plugins=$(grep -Eo 'IDENTIFIED WITH [A-Za-z0-9_]+' "$SELF" | awk '{print $3}' | sort -u || true)
  [[ -n "$plugins" ]] || fail "missing account creation"
  [[ "$plugins" == caching_sha2_password ]] || fail "unexpected auth plugin(s): ${plugins//$'\n'/, }"

  if grep -Fq -- "$FORBIDDEN_PLUGIN" "$SELF"; then
    fail "legacy auth plugin referenced in $SELF"
  fi

  echo "proxy-auth-check: ok"
}

# Never echo a DSN's raw input: a malformed DSN can carry the password anywhere.
# Only the fixed mysql:// prefix and the part after the last @, cut before any
# query string, survive redaction.
redact_dsn() {
  if [[ "$1" != mysql://* ]]; then
    printf '<redacted DSN>'
  elif [[ "$1" == *@* ]]; then
    local tail=${1##*@}
    printf 'mysql://***@%s' "${tail%%\?*}"
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

# Strict %HH decoder for DSN userinfo. A bare % or a malformed escape is rejected;
# printf %b would instead eat raw backslash sequences and mangle the credential.
urldecode() {
  local in=$1 out= byte
  while [[ "$in" == *%* ]]; do
    out+=${in%%\%*}
    in=${in#*%}
    if [[ ! "$in" =~ ^[0-9A-Fa-f]{2} ]]; then
      echo "invalid percent-encoding (user and password must be URL-encoded)" >&2
      return 1
    fi
    printf -v byte '%b' "\\x${in:0:2}"
    out+=$byte
    in=${in:2}
  done
  printf '%s' "$out$in"
}

# mysql://user[:pass]@host[:port]/db -> DSN_USER/DSN_PASS/DSN_HOST/DSN_PORT/DSN_DB.
# shortcut: no IPv6 literals or DSN query parameters (e.g. ?ssl-mode=); user/password must be URL-encoded.
parse_dsn() {
  local dsn=${1#mysql://}
  if [[ "$1" != mysql://* || "$dsn" != *@* || "${dsn#*@}" != */* ]]; then
    echo "invalid DSN (want mysql://user[:pass]@host[:port]/db): $(redact_dsn "$1")" >&2
    exit 2
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
  DSN_USER=$(urldecode "$DSN_USER") || exit 2
  DSN_PASS=$(urldecode "$DSN_PASS") || exit 2
  [[ -n "$DSN_USER" && -n "$DSN_HOST" && -n "$DSN_DB" ]] \
    || { echo "invalid DSN (want mysql://user[:pass]@host[:port]/db): $(redact_dsn "$1")" >&2; exit 2; }
}

# Password stays out of argv/ps; stderr is left to the caller.
mysql_proxy() {
  mysql --batch --raw --skip-column-names --protocol=TCP --connect-timeout=10 \
    --host="$DSN_HOST" --port="$DSN_PORT" --user="$DSN_USER" --database="$DSN_DB" "$@"
}

# MYSQL_VERSION is a claim about the backend line: 8.4 -> 8.4.x, 9 -> 9.x.
# Uses run_gate's locals (bash dynamic scoping).
check_backend_version() {
  local pattern
  case "$MYSQL_VERSION" in
    8.4) pattern='^8[.]4[.]' ;;
    9) pattern='^9[.]' ;;
  esac
  if [[ "$mysql_version" == unavailable ]]; then
    fail "backend version unavailable through the proxy (proxy version: $proxy_version); cannot verify MYSQL_VERSION=$MYSQL_VERSION"
  fi
  [[ "$mysql_version" =~ $pattern ]] \
    || fail "backend version mismatch: MYSQL_VERSION=$MYSQL_VERSION requires ${MYSQL_VERSION}.x, but SELECT VERSION() returned '$mysql_version' (proxy version: $proxy_version)"
}

run_gate() {
  [[ -n "${PROXY_DSN:-}" ]] || { echo "proxy-auth-check: PROXY_DSN is required" >&2; usage >&2; exit 2; }
  [[ -n "${MYSQL_VERSION:-}" ]] \
    || { echo "proxy-auth-check: MYSQL_VERSION is required (8.4 or 9)" >&2; usage >&2; exit 2; }

  local image
  if ! image=$(backend_image "$MYSQL_VERSION"); then
    echo "proxy-auth-check: MYSQL_VERSION must be 8.4 or 9, got: $MYSQL_VERSION" >&2
    exit 2
  fi

  command -v mysql >/dev/null \
    || { echo "proxy-auth-check: mysql client not found in PATH" >&2; exit 2; }
  parse_dsn "$PROXY_DSN"
  if [[ -n "$DSN_PASS" ]]; then export MYSQL_PWD="$DSN_PASS"; else unset MYSQL_PWD; fi

  local rc=0 out=
  if out=$(mysql_proxy -e 'SELECT 1'); then rc=0; else rc=$?; fi

  # The proxy version is the handshake string it advertises; the MySQL version is
  # what the backend reports through the proxy. Both are diagnostics only.
  local proxy_version mysql_version
  proxy_version=$(mysql_proxy -e 'status' 2>/dev/null \
    | awk '/^Server version:/ { sub(/^Server version:[[:space:]]*/, ""); print; exit }' || true)
  mysql_version=$(mysql_proxy -e 'SELECT VERSION()' 2>/dev/null | head -n 1 || true)
  [[ -n "$proxy_version" ]] || proxy_version=unavailable
  [[ -n "$mysql_version" ]] || mysql_version=unavailable

  if ((rc != 0)) || [[ "$out" != 1 ]]; then
    echo "proxy-auth-check: FAILED: SELECT 1 through the proxy failed (rc=$rc, output=${out:-<empty>})" >&2
    echo "backend image: $image" >&2
    echo "proxy version: $proxy_version" >&2
    echo "mysql version: $mysql_version" >&2
    exit 1
  fi

  check_backend_version

  echo "proxy-auth-check: ok"
  echo "backend image: $image"
  echo "proxy version: $proxy_version"
  echo "mysql version: $mysql_version"
}

case "${1:-}" in
  --check)
    [[ $# -eq 1 ]] || { echo "proxy-auth-check: --check takes no arguments" >&2; exit 2; }
    check_static
    ;;
  -h|--help) usage ;;
  '') run_gate ;;
  *) echo "proxy-auth-check: unknown argument: $(redact_arg "$1")" >&2; usage >&2; exit 2 ;;
esac
