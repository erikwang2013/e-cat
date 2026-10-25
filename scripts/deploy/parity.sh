#!/usr/bin/env bash
# Deployment parity gate: the same CRUD/ORM SQL must produce identical output on
# standalone (single MySQL) and distributed (MySQL behind ShardingSphere-Proxy).
# --check is static: it validates the two compose profiles and the Helm values and
# needs neither Docker nor Helm. --run needs ECAT_STANDALONE_DSN/ECAT_DISTRIBUTED_DSN
# and is skipped when they are unset; ECAT_REQUIRE_LIVE_DB=1 turns that skip into a
# failure so CI can demand a live run.
#
# The distributed side must be the ShardingSphere-Proxy endpoint: a direct MySQL DSN
# is not a distributed parity run. Set ECAT_PROXY_DSN to that proxy endpoint and the
# run first executes scripts/sharding/proxy-auth-check.sh against it; with
# ECAT_REQUIRE_PROXY=1 an unset ECAT_PROXY_DSN is a failure instead of a warning.
#
# Usage:
#   scripts/deploy/parity.sh --check
#   ECAT_STANDALONE_DSN=mysql://user:pass@host:3306/db \
#   ECAT_PROXY_DSN=mysql://user:pass@proxy:3307/db scripts/deploy/parity.sh --run
set -euo pipefail

ROOT=$(cd -- "$(dirname "$0")/../.." && pwd)
COMPOSE=$ROOT/docker-compose.dev.yml
VALUES=$ROOT/ecat-deploy/helm/values.yaml

usage() {
  cat <<'EOF'
usage: parity.sh --check
       parity.sh --run

  --check   static check of docker-compose profiles and Helm values (no DB needed)
  --run     run one CRUD/ORM SQL script on both topologies and diff the output;
            needs ECAT_STANDALONE_DSN and ECAT_DISTRIBUTED_DSN (mysql://...)
            ECAT_PROXY_DSN (recommended) must be the ShardingSphere-Proxy endpoint;
            it is verified with scripts/sharding/proxy-auth-check.sh.
            ECAT_MYSQL_VERSION=8.4|9 (default 8.4) is the backend line claimed by the
            proxy check. ECAT_REQUIRE_PROXY=1 makes ECAT_PROXY_DSN mandatory.
EOF
}

fail() {
  echo "deploy-parity: $*" >&2
  exit 1
}

usage_error() {
  echo "deploy-parity: $*" >&2
  usage >&2
  exit 2
}

# Services of one compose profile, one name per line. Unprofiled services are not
# listed. Services are the only keys at two-space indent, profiles are four.
compose_service_profiles() {
  awk '
    /^  [A-Za-z0-9_.-]+:[[:space:]]*$/ {
      if (svc != "") print svc "\t" prof
      svc = $1
      sub(/:$/, "", svc)
      prof = ""
      next
    }
    /^    profiles:[[:space:]]*\[/ {
      line = $0
      sub(/^[^[]*\[/, "", line)
      sub(/\].*$/, "", line)
      gsub(/[[:space:]"]/, "", line)
      prof = line
    }
    END { if (svc != "") print svc "\t" prof }
  ' "$COMPOSE"
}

compose_services() {
  compose_service_profiles | awk -F'\t' -v want="$1" '
    {
      n = split($2, names, ",")
      for (i = 1; i <= n; i++) {
        if (names[i] == want) print $1
      }
    }
  '
}

# One service's YAML block (header + body) on stdout; empty when absent.
compose_service_block() {
  awk -v want="$1" '
    /^  [A-Za-z0-9_.-]+:[[:space:]]*$/ {
      name = $1
      sub(/:$/, "", name)
      on = (name == want)
      if (on) print
      next
    }
    on { print }
  ' "$COMPOSE"
}

# Exact service sets: dev is the old trio, standalone the single-node shape,
# distributed the full topology. Any extra or missing service breaks isolation.
require_profile_set() {
  local profile=$1 expected=$2 actual
  actual=$(compose_services "$profile" | LC_ALL=C sort | tr '\n' ' ')
  [[ "$actual" == "$expected" ]] \
    || fail "compose profile '$profile' services are '${actual:-<none>}' (want '$expected')"
}

check_compose() {
  [[ -f "$COMPOSE" ]] || fail "missing $COMPOSE"
  require_profile_set dev 'mssql mysql postgres '
  require_profile_set standalone 'ecat-mysql ecat-redis '
  require_profile_set distributed 'app-1 app-2 clickhouse ecat-mysql ecat-redis shardingsphere-proxy '
  # Unprofiled services would co-start with every profile.
  local unprofiled
  unprofiled=$(compose_service_profiles | awk -F'\t' '$2 == "" { printf "%s ", $1 }')
  [[ -z "$unprofiled" ]] \
    || fail "compose services without a profile co-start with every profile: $unprofiled"
  # Scope the env assertions to each app's own block: a file-wide grep would let a
  # comment (or the other app) satisfy the check.
  local node block
  for node in app-1 app-2; do
    block=$(compose_service_block "$node")
    [[ -n "$block" ]] || fail "compose service '$node' not found"
    grep -qxF -- '      ECAT_DEPLOYMENT_MODE: distributed' <<<"$block" \
      || fail "compose service '$node' has no ECAT_DEPLOYMENT_MODE: distributed env"
    grep -qxF -- "      ECAT_NODE_ID: $node" <<<"$block" \
      || fail "compose service '$node' has no ECAT_NODE_ID: $node env"
  done
}

check_helm_values() {
  [[ -f "$VALUES" ]] || fail "missing $VALUES"
  local key mode
  for key in 'deploymentMode:' 'replicaCount:' 'nodeId:' 'fieldPath: metadata.name' \
             'readinessProbe:' 'livenessProbe:' 'existingSecret:'; do
    grep -qF -- "$key" "$VALUES" || fail "helm values missing '$key'"
  done
  # mysql.image.tag 是可切换项：8.4 与 9 都合法。
  grep -Eq '^[[:space:]]*tag:[[:space:]]*"(8\.4|9)"' "$VALUES" \
    || fail 'helm values mysql.image.tag must be "8.4" or "9"'
  # 只认 deploymentMode 的取值；注释里出现 distributed 不算启用分布式。
  mode=$(sed -n 's/^deploymentMode:[[:space:]]*//p' "$VALUES" | head -n1 \
    | sed 's/[[:space:]]*#.*//; s/^"//; s/"$//; s/[[:space:]]*$//')
  [[ "$mode" == standalone || "$mode" == distributed ]] \
    || fail "helm values deploymentMode must be standalone or distributed (got '${mode:-<missing>}')"
  check_no_secret_literals
}

# Any key ending in password/passwd must carry a Secret reference or a placeholder,
# never a literal. Catches mysql_root_password/rootPassword, which a prefix regex misses.
check_no_secret_literals() {
  local key value leaks=()
  while IFS=$'\t' read -r key value; do
    [[ -n "$key" ]] || continue
    value=$(printf '%s\n' "$value" | sed 's/[[:space:]]#.*//')
    value=${value//[[:space:]]/}
    value=${value//\"/}
    value=${value//\'/}
    case "${value^^}" in
      '' | '~' | NULL | REPLACE_ME | CHANGEME) continue ;;
    esac
    if [[ "$value" =~ ^\$\{.*\}$ || "$value" =~ ^\{\{.*\}\}$ || "$value" =~ ^\<.*\>$ ]]; then
      continue
    fi
    leaks+=("$key")
  done < <(awk '
    BEGIN { IGNORECASE = 1 }
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*[A-Za-z0-9_.-]*(password|passwd)[[:space:]]*:/ {
      line = $0
      key = line
      sub(/^[[:space:]]*/, "", key)
      sub(/[[:space:]]*:.*/, "", key)
      value = line
      sub(/^[^:]*:[[:space:]]*/, "", value)
      print key "\t" value
    }
  ' "$VALUES")
  ((${#leaks[@]} == 0)) \
    || fail "helm values contain password literal(s): ${leaks[*]} (use existingSecret)"
}

# The chart has no default templates: without these, every value above is inert.
check_helm_templates() {
  local dir=$ROOT/ecat-deploy/helm/templates key
  for key in deployment.yaml service.yaml mysql.yaml; do
    [[ -f "$dir/$key" ]] || fail "missing helm template ecat-deploy/helm/templates/$key"
  done
  for key in '.Values.deploymentMode' '.Values.replicaCount' '.Values.nodeId' \
             '.Values.existingSecret' '.Values.readinessProbe' '.Values.livenessProbe'; do
    grep -qF -- "$key" "$dir/deployment.yaml" \
      || fail "helm deployment template does not consume $key"
  done
  grep -qF -- '.Values.mysql.image' "$dir/mysql.yaml" \
    || fail "helm mysql template does not consume .Values.mysql.image"
  grep -qF -- '.Values.service' "$dir/service.yaml" \
    || fail "helm service template does not consume .Values.service"
}

check_all() {
  check_compose
  check_helm_values
  check_helm_templates
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
    fail "invalid DSN (want mysql://user[:pass]@host[:port]/db): $(redact_dsn "$1")"
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
    || fail "invalid DSN (want mysql://user[:pass]@host[:port]/db): $(redact_dsn "$1")"
}

# Run the shared SQL file for one side on stdout; password stays out of argv/ps.
run_side() {
  parse_dsn "$1"
  if [[ -n "$DSN_PASS" ]]; then export MYSQL_PWD="$DSN_PASS"; else unset MYSQL_PWD; fi
  mysql --batch --raw --skip-column-names --default-character-set=utf8mb4 \
    --protocol=TCP --connect-timeout=10 \
    --host="$DSN_HOST" --port="$DSN_PORT" --user="$DSN_USER" --database="$DSN_DB"
}

mode=
case "${1:-}" in
  --check) mode=check ;;
  --run) mode=run ;;
  -h | --help) usage; exit 0 ;;
  '') usage_error "missing mode" ;;
  *) usage_error "unknown argument: $(redact_arg "$1")" ;;
esac
(($# == 1)) || usage_error "unexpected extra argument: $(redact_arg "$2")"

check_all

if [[ "$mode" == check ]]; then
  echo "deploy-parity: ok"
  exit 0
fi

standalone_dsn=${ECAT_STANDALONE_DSN:-}
proxy_dsn=${ECAT_PROXY_DSN:-}
# When ECAT_PROXY_DSN is given it is both the verified and the compared endpoint.
distributed_dsn=${proxy_dsn:-${ECAT_DISTRIBUTED_DSN:-}}
if [[ -z "$standalone_dsn" || -z "$distributed_dsn" ]]; then
  if [[ "${ECAT_REQUIRE_LIVE_DB:-0}" == 1 ]]; then
    fail "ECAT_STANDALONE_DSN and ECAT_DISTRIBUTED_DSN/ECAT_PROXY_DSN are required when ECAT_REQUIRE_LIVE_DB=1"
  fi
  echo "deploy-parity: skipped live comparison (set ECAT_STANDALONE_DSN and ECAT_DISTRIBUTED_DSN/ECAT_PROXY_DSN)"
  exit 0
fi
command -v mysql >/dev/null || fail "mysql client not found"

# A direct MySQL DSN on the distributed side would turn this into a tautology.
if [[ -n "$proxy_dsn" ]]; then
  proxy_check=$ROOT/scripts/sharding/proxy-auth-check.sh
  [[ -x "$proxy_check" ]] || fail "missing executable $proxy_check"
  MYSQL_VERSION=${ECAT_MYSQL_VERSION:-8.4} PROXY_DSN="$proxy_dsn" "$proxy_check" \
    || fail "proxy-auth-check failed for ECAT_PROXY_DSN (distributed side is not a usable proxy endpoint)"
elif [[ "${ECAT_REQUIRE_PROXY:-0}" == 1 ]]; then
  fail "ECAT_REQUIRE_PROXY=1 requires ECAT_PROXY_DSN"
else
  echo "deploy-parity: WARN ECAT_PROXY_DSN unset: assuming ECAT_DISTRIBUTED_DSN is ShardingSphere-Proxy; direct MySQL is not a distributed parity run" >&2
fi

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

# One script for both topologies: upsert, update, delete and an ordered read-back.
# Output is compared byte for byte, so every statement prints a labelled row set.
cat >"$tmpdir/parity.sql" <<'SQL'
DROP TABLE IF EXISTS ecat_parity_probe;
CREATE TABLE ecat_parity_probe (
  id BIGINT NOT NULL PRIMARY KEY,
  name VARCHAR(64) NOT NULL,
  amount DECIMAL(10,2) NOT NULL,
  payload JSON NULL,
  updated_at DATETIME NOT NULL
);
INSERT INTO ecat_parity_probe (id, name, amount, payload, updated_at) VALUES
  (1, 'alpha', 10.50, '{"k":"v"}', '2026-01-01 00:00:00'),
  (2, 'beta', 0.00, NULL, '2026-01-01 00:00:01');
INSERT INTO ecat_parity_probe (id, name, amount, payload, updated_at) VALUES
  (2, 'beta', 2.25, '{"k":"v2"}', '2026-01-01 00:00:02')
ON DUPLICATE KEY UPDATE name = VALUES(name), amount = VALUES(amount),
  payload = VALUES(payload), updated_at = VALUES(updated_at);
SELECT 'after_upsert', id, name, amount, payload FROM ecat_parity_probe ORDER BY id;
UPDATE ecat_parity_probe SET name = 'alpha-2', amount = amount + 1.25 WHERE id = 1;
DELETE FROM ecat_parity_probe WHERE id = 2;
SELECT 'final', id, name, amount, payload FROM ecat_parity_probe ORDER BY id;
SELECT 'count', COUNT(*), COALESCE(SUM(amount), 0) FROM ecat_parity_probe;
DROP TABLE IF EXISTS ecat_parity_probe;
SQL

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
[[ -n "$DIFF" ]] || fail "no working diff found (PATH diff reports no difference)"

run_side "$standalone_dsn" <"$tmpdir/parity.sql" >"$tmpdir/standalone"
run_side "$distributed_dsn" <"$tmpdir/parity.sql" >"$tmpdir/distributed"

# mysql exits non-zero on errors, but an empty result would make the diff trivially green.
[[ -s "$tmpdir/standalone" ]] || fail "standalone produced no output (SQL failed silently)"
[[ -s "$tmpdir/distributed" ]] || fail "distributed produced no output (SQL failed silently)"

rc=0
"$DIFF" -u "$tmpdir/standalone" "$tmpdir/distributed" || rc=$?
case "$rc" in
  0) echo "deploy-parity: ok (standalone == distributed)" ;;
  1) fail "standalone and distributed outputs differ" ;;
  *) fail "diff failed with rc=$rc (tool error, not a data mismatch)" ;;
esac
