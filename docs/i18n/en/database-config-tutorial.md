# Database Configuration Tutorial

**Version:** 2.4.2 · **Date:** 2026-08-01

All 14 data backends of e-cat support loading connection information from config files, so no hardcoding in code is needed. `username` / `password` are both optional fields; authentication is skipped when omitted.

---

## Quick Start

### 1. Create a Config File

Copy the example template and modify it for your environment:

```bash
cp config/databases.example.yaml databases.yaml
```

Edit `databases.yaml` and fill in the real connection information:

```yaml
# databases.yaml
sql:
  url: "postgres://myapp:secret@db.internal:5432/myapp"

redis:
  url: "redis://cache.internal:6379"

clickhouse:
  base_url: "http://ch.internal:8123"
  database: "analytics"
```

### 2. Add Dependencies

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
ecat-data-sqlx = { path = "../ecat-data-sqlx" }
ecat-data-redis = { path = "../ecat-data-redis" }
ecat-data-clickhouse = { path = "../ecat-data-clickhouse" }
```

### 3. Load and Use

```rust
use ecat_data_redis::{RedisCache, RedisConfig};
use ecat_data_sqlx::{SqlxClient, SqlxConfig};
use ecat_data_clickhouse::{ClickhouseClient, ClickhouseConfig};
use serde::Deserialize;

#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: RedisConfig,
    clickhouse: ClickhouseConfig,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load the YAML configuration
    let yaml = std::fs::read_to_string("databases.yaml")?;
    let cfg: AppConfig = serde_yaml::from_str(&yaml)?;

    // Create the database clients — no hard-coded connection info
    let db = SqlxClient::from_config(cfg.sql).await?;
    let cache = RedisCache::from_config(cfg.redis).await?;
    let ch = ClickhouseClient::from_config(cfg.clickhouse);

    // Usage
    let rows = db.query("SELECT id, name FROM users LIMIT 10").await?;
    cache.set("health", b"ok", std::time::Duration::from_secs(30)).await?;

    Ok(())
}
```

---

## Full Configuration Reference

### Define the Top-level Config Struct

```rust
use serde::Deserialize;

#[derive(Deserialize)]
pub struct DatabasesConfig {
    pub sql: ecat_data_sqlx::SqlxConfig,
    pub redis: ecat_data_redis::RedisConfig,
    pub memcached: ecat_data_memcached::MemcachedConfig,
    pub clickhouse: ecat_data_clickhouse::ClickhouseConfig,
    pub questdb: ecat_data_questdb::QuestdbConfig,
    pub elasticsearch: ecat_data_elasticsearch::ElasticsearchConfig,
    pub opensearch: ecat_data_opensearch::OpenSearchConfig,
    pub neo4j: ecat_data_neo4j::Neo4jConfig,
    pub nebulagraph: ecat_data_nebulagraph::NebulaGraphConfig,
    pub arangodb: ecat_data_arangodb::ArangoConfig,
    pub influxdb: ecat_data_influxdb::InfluxConfig,
    pub iotdb: ecat_data_iotdb::IotdbConfig,
}
```

### Full YAML Example

See `config/databases.example.yaml`.

---

## Config Field Quick Reference per Backend

### RDBMS — SqlxConfig

#### Native pools (PostgreSQL / MySQL / SQLite)

The native driver is selected automatically from the URL scheme — no extra configuration needed:

| scheme | Driver |
|---|---|
| `postgres://` / `postgresql://` | PostgreSQL |
| `mysql://` / `mariadb://` | MySQL |
| `sqlite:` | SQLite |

- **Native time types**: the old sqlx `AnyPool` path did not support time types, so you had to `CAST` them to text yourself; time columns are now read directly.
- Both letter case and surrounding whitespace in the scheme are **tolerated** (`"POSTGRES://…"` and `" postgres://…"` work); an unrecognized scheme raises an error that **names** the offending scheme.
- `mssql://` is **not served by this backend** (use `ecat-data-mssql`); passing it here is explicitly rejected.

#### Configuration example

```yaml
sql:
  url: "postgres://host:5432/dbname"
  # username: "app_user"    # optional
  # password: "secret"      # optional
```

| Field | Type | Default | Notes |
|------|------|--------|------|
| `url` | `String` | — | sqlx connection string, supports SQLite/PG/MySQL/TiDB |
| `username` | `Option<String>` | `None` | Optional: URL-embedded authentication (paired with password) |
| `password` | `Option<String>` | `None` | Optional: URL-embedded authentication (paired with username) |
| `max_connections` | `u32` | `10` | Maximum number of connections in the pool |
| `min_connections` | `u32` | `0` | Idle-floor connection count; clamped to ≤ `max_connections` |
| `acquire_timeout_secs` | `u64` | `30` | Timeout spent waiting for a connection; **`0` = time out immediately** (fails if no connection is free), the opposite of `0` = disabled in `query_timeout_secs` |
| `idle_timeout_secs` | `u64` | `600` | Reclaim idle connections after this |
| `max_lifetime_secs` | `u64` | `1800` | Maximum lifetime of a connection |
| `query_timeout_secs` | `u64` | `30` | Per-query timeout; **0 = disabled** |
| `slow_query_ms` | `u64` | `1000` | Slow-query warn threshold (ms); unset = 1000, **0 = off** (`tracing` feature only) |
| `test_before_acquire` | `bool` | `false` | Ping the connection before handing it out |
| `session_init` | `string[]` | Per dialect | Session initialization statements for each new connection |

#### `session_init` (session initialization)

After each new connection is established, these statements are executed in order:

| Dialect | Default `session_init` |
|---|---|
| PostgreSQL | `SET TIME ZONE 'UTC'`, `SET application_name = 'ecat'` |
| MySQL | `SET time_zone = '+00:00'` |
| SQLite | No session concept — empty by default |

The purpose is to make the **database return UTC directly**, matching the framework convention that all times are presented as RFC3339 UTC.

- An **explicit empty array `[]` means "turned off"** and overrides the dialect default (the same explicit-override convention as `query_timeout_secs: 0` disabling the timeout).
- If any statement fails → connection creation fails, with **no silent fallback**.

```yaml
sql:
  url: "mysql://host:3306/dbname"
  session_init:
    - "SET time_zone = '+00:00'"
    - "SET NAMES utf8mb4"
```

#### `warm_up()` — Warm-up

Proactively opens `min_connections` connections at startup and returns them, so the service is ready the moment it comes up:

```rust
let db = SqlxClient::from_config(cfg).await?;
db.warm_up().await?;   // call once at startup
```

Why this is needed: sqlx maintains `min_connections` **asynchronously in a background task**, so `connect()` returning does not guarantee the pool is filled — the first wave of requests would race the background task.

#### Observability features (`metrics` / `health` / `tracing`)

All three features are **off by default** (keeping axum — a dependency of `ecat-metrics` / `ecat-health` — out of the core dependency tree); enable them as needed. `ecat-data-mssql` offers the same three.

```toml
ecat-data-sqlx = { path = "../ecat-data-sqlx", features = ["metrics", "health", "tracing"] }
```

```rust
use std::sync::Arc;
use ecat_data_sqlx::{RdbmsHealthCheck, SqlxClient, SqlxConfig, register_pool_metrics};
use ecat_health::HealthRegistry;

let db = SqlxClient::from_config(cfg).await?;

// metrics: once registered, the /metrics endpoint gains four metrics —
// ecat_rdbms_pool_connections (gauge, with state="idle"/"active"),
// ecat_rdbms_pool_timeouts_total, ecat_rdbms_query_timeout_total,
// ecat_rdbms_transactions_leaked_total (counters, labelled by backend)
register_pool_metrics("primary", db.pool());

// health: a SELECT 1 connectivity probe, registered on the /health readyz
let registry = HealthRegistry::new()
    .with_check(RdbmsHealthCheck::new("sql", Arc::new(db)));
```

The `tracing` feature logs a warn when a query exceeds `slow_query_ms` (elapsed time + the SQL truncated to its first 200 characters). That field is read only by this feature — it is still parsed when the feature is off, but has no effect.

#### Time and date: RFC3339 UTC throughout

**sqlite time/date text is rewritten to RFC3339 UTC**:

- text shaped like `2026-10-05 12:34:56` → `"2026-10-05T12:34:56Z"`
- text shaped like `2026-10-05` → `"2026-10-05T00:00:00Z"` (the UTC-midnight instant)

Reason: sqlite has no type system, so text shaped like a date or time is inherently ambiguous; the framework treats it uniformly as a time.

**Side effect**: Text columns that merely happen to look like a date — version numbers, business codes — are rewritten too. If you do not want this, explicitly CAST the column into a non-date shape, or use a different type.

Real `DATE` / `TIMESTAMP` columns in PG / MySQL are likewise presented as RFC3339 UTC strings, and **a pure date still carries `T00:00:00Z`** — because `"2026-10-05"` is not valid RFC3339, and the framework uses one uniform format internally so upper layers can parse it.

### Redis — RedisConfig

```yaml
redis:
  url: "redis://host:6379"
  # password: "auth_token"  # optional
  # query_timeout_secs: 30     # optional: per-command timeout, 0 = disabled
  # breaker: {}                # optional: breaker config, omitted = conservative defaults (0.5 / 30s / open 10s)
```

| Field | Type | Default | Notes |
|------|------|--------|------|
| `url` | `String` | — | Redis connection URL |
| `password` | `Option<String>` | `None` | Optional: Redis AUTH password |
| `query_timeout_secs` | `Option<u64>` | `30` | Per-command timeout in seconds; **`0` = disabled** |
| `breaker` | `Option<BreakerConfig>` | conservative defaults | Breaker thresholds and window; fields may be omitted, `breaker: {}` means all defaults |

**Capability boundary**: this client uses a `MultiplexedConnection` (one TCP connection serving all concurrency), **not a connection pool** — better than a pool for cache workloads: fewer connections and fewer round trips. The cost is that **stateful command sequences cannot use it**: `MULTI`/`EXEC` transactions, `WATCH`, `SUBSCRIBE` and blocking commands need an exclusive connection — under multiplexing they would interleave with other commands. When you need one, open a dedicated connection with `redis::Client::get_async_connection()`.

**Timeouts**: `query_timeout_secs: 0` in the config means **disabled** (not "time out after 0 seconds"); the 30-second default applies only when the field is omitted. At the library level, `run_with_timeout(kind, Some(Duration::ZERO), fut)` is the opposite — that **times out immediately** (tokio polls the inner future first, so an already-ready future still succeeds). The two "0"s mean different things; don't copy the config value when calling the library function directly.

**The breaker is on by default** — with conservative thresholds (failure ratio 0.5, window 30 s, half-open probes 3, open 10 s) it opens only under sustained failure. **There is currently no master switch**: `BreakerConfig` has only those four threshold fields and no `enabled`; writing `{"enabled": false}` just yields a deserialization error. To really disable it, push the thresholds beyond reach (e.g. `failure_ratio: 1.1`).

### Memcached — MemcachedConfig

```yaml
memcached:
  # username: "memcache"    # optional: reserved field (currently an in-memory implementation)
  # password: "secret"      # optional: reserved field
  {}
```

| Field | Type | Notes |
|------|------|------|
| `username` | `Option<String>` | Optional: reserved field |
| `password` | `Option<String>` | Optional: reserved field |

Currently an in-memory implementation; the authentication fields are reserved.

### ClickHouse — ClickhouseConfig

```yaml
clickhouse:
  base_url: "http://host:8123"
  database: "default"
  # username: "default"   # optional
  # password: "secret"    # optional
  # query_timeout_secs: 30  # optional: per-call timeout, 0 = disabled
  # breaker: {}             # optional: breaker config, omitted = conservative defaults (0.5 / 30s / open 10s)
  # max_concurrency: 32     # optional: concurrency limit (this crate's semaphore)
```

| Field | Type | Default | Notes |
|------|------|--------|------|
| `base_url` | `String` | — | HTTP interface address |
| `database` | `String` | `"default"` | Database name |
| `username` | `Option<String>` | `None` | Optional: HTTP Basic Auth username |
| `password` | `Option<String>` | `None` | Optional: HTTP Basic Auth password |
| `query_timeout_secs` | `Option<u64>` | `30` | Per-call timeout in seconds; **`0` = disabled** (same as Redis) |
| `breaker` | `Option<BreakerConfig>` | conservative defaults | Breaker thresholds and window; likewise no `enabled` master switch |
| `max_concurrency` | `Option<usize>` | `32` | Concurrency limit; **this crate's own semaphore**, not a reqwest knob (reqwest only has `pool_max_idle_per_host` — connections kept idle, not a cap) |

**Two layers of timeout**: the `reqwest::Client` built by `from_config` (`ecat-tls`) carries its own 5 s connect timeout + 30 s total timeout, and `query_timeout_secs` is the **outer** budget — when both are present, **whichever fires first wins**; when the inner one fires, the error is `RdbmsError::Database` and is **not counted** in `ecat_outbound_timeouts_total` (the counter for outer timeouts, `metrics` feature). `new` / `with_auth` use a bare `reqwest::Client::new()` with no inner timeout.

### QuestDB — QuestdbConfig

```yaml
questdb:
  base_url: "http://host:9000"
  # username: "admin"     # optional
  # password: "quest"     # optional
  # query_timeout_secs: 30   # Optional: per-call timeout, 0 = disabled
  # breaker: {}              # Optional: breaker config, omitted = conservative defaults (0.5 / 30s / opens for 10s)
  # max_concurrency: 32      # Optional: concurrency cap (this crate's semaphore)
```

| Field | Type | Notes |
|------|------|------|
| `base_url` | `String` | HTTP API address |
| `username` | `Option<String>` | Optional: HTTP Basic Auth username |
| `password` | `Option<String>` | Optional: HTTP Basic Auth password |
| `query_timeout_secs` | `Option<u64>` | Per-call timeout in seconds; omitted = `30`, **`0` = disabled** |
| `breaker` | `Option<BreakerConfig>` | Breaker thresholds and window; no `enabled` master switch |
| `max_concurrency` | `Option<usize>` | Concurrency cap (default `32`); this crate's own semaphore |

**Error type**: QuestDB goes through `SqlExecutor` (the RDBMS family) — a timeout is `RdbmsError::Timeout`,
a breaker rejection is `RdbmsError::Connection("circuit breaker is open")`; every other HTTP backend uniformly
returns `ecat_errors::Error` (`code = DeadlineExceeded` / `Unavailable`, `reason` = backend name).

### Elasticsearch — ElasticsearchConfig

```yaml
elasticsearch:
  base_url: "http://host:9200"
  # username: "elastic"   # optional
  # password: "secret"    # optional
  # query_timeout_secs: 30   # Optional: per-call timeout, 0 = disabled
  # breaker: {}              # Optional: breaker config, omitted = conservative defaults (0.5 / 30s / opens for 10s)
  # max_concurrency: 32      # Optional: concurrency cap (this crate's semaphore)
```

| Field | Type | Notes |
|------|------|------|
| `base_url` | `String` | REST API address |
| `username` | `Option<String>` | Optional: HTTP Basic Auth username |
| `password` | `Option<String>` | Optional: HTTP Basic Auth password |
| `query_timeout_secs` | `Option<u64>` | Per-call timeout in seconds; omitted = `30`, **`0` = disabled** |
| `breaker` | `Option<BreakerConfig>` | Breaker thresholds and window; no `enabled` master switch |
| `max_concurrency` | `Option<usize>` | Concurrency cap (default `32`); this crate's own semaphore |

### OpenSearch — OpenSearchConfig

```yaml
opensearch:
  base_url: "http://host:9200"
  # username: "admin"     # optional
  # password: "secret"    # optional
  # query_timeout_secs: 30   # Optional: per-call timeout, 0 = disabled
  # breaker: {}              # Optional: breaker config, omitted = conservative defaults (0.5 / 30s / opens for 10s)
  # max_concurrency: 32      # Optional: concurrency cap (this crate's semaphore)
```

| Field | Type | Notes |
|------|------|------|
| `base_url` | `String` | REST API address |
| `username` | `Option<String>` | Optional: HTTP Basic Auth username |
| `password` | `Option<String>` | Optional: HTTP Basic Auth password |
| `query_timeout_secs` | `Option<u64>` | Per-call timeout in seconds; omitted = `30`, **`0` = disabled** |
| `breaker` | `Option<BreakerConfig>` | Breaker thresholds and window; no `enabled` master switch |
| `max_concurrency` | `Option<usize>` | Concurrency cap (default `32`); this crate's own semaphore |

### InfluxDB — InfluxConfig

```yaml
influxdb:
  base_url: "http://host:8086"
  org: "myorg"
  bucket: "mybucket"
  token: "my-token"
  # query_timeout_secs: 30   # Optional: per-call timeout, 0 = disabled
  # breaker: {}              # Optional: breaker config, omitted = conservative defaults (0.5 / 30s / opens for 10s)
  # max_concurrency: 32      # Optional: concurrency cap (this crate's semaphore)
```

| Field | Type | Notes |
|------|------|------|
| `base_url` | `String` | InfluxDB 2.x API address |
| `org` | `String` | Organization name |
| `bucket` | `String` | Bucket name |
| `token` | `String` | Authentication token |
| `query_timeout_secs` | `Option<u64>` | Per-call timeout in seconds; omitted = `30`, **`0` = disabled** |
| `breaker` | `Option<BreakerConfig>` | Breaker thresholds and window; no `enabled` master switch |
| `max_concurrency` | `Option<usize>` | Concurrency cap (default `32`); this crate's own semaphore |

### Neo4j — Neo4jConfig

```yaml
neo4j:
  base_url: "http://host:7474"
  username: "neo4j"
  password: "secret"
  # query_timeout_secs: 30   # Optional: per-call timeout, 0 = disabled
  # breaker: {}              # Optional: breaker config, omitted = conservative defaults (0.5 / 30s / opens for 10s)
  # max_concurrency: 32      # Optional: concurrency cap (this crate's semaphore)
```

| Field | Type | Notes |
|------|------|------|
| `base_url` | `String` | REST API address |
| `username` | `String` | Username |
| `password` | `String` | Password |
| `query_timeout_secs` | `Option<u64>` | Per-call timeout in seconds; omitted = `30`, **`0` = disabled** |
| `breaker` | `Option<BreakerConfig>` | Breaker thresholds and window; no `enabled` master switch |
| `max_concurrency` | `Option<usize>` | Concurrency cap (default `32`); this crate's own semaphore |

### NebulaGraph — NebulaGraphConfig

```yaml
nebulagraph:
  base_url: "http://host:19669"
  space: "my_space"
  # username: "root"      # optional
  # password: "nebula"    # optional
  # query_timeout_secs: 30   # Optional: per-call timeout, 0 = disabled
  # breaker: {}              # Optional: breaker config, omitted = conservative defaults (0.5 / 30s / opens for 10s)
  # max_concurrency: 32      # Optional: concurrency cap (this crate's semaphore)
```

| Field | Type | Notes |
|------|------|------|
| `base_url` | `String` | API address |
| `space` | `String` | Graph space name |
| `username` | `Option<String>` | Optional: HTTP Basic Auth username |
| `password` | `Option<String>` | Optional: HTTP Basic Auth password |
| `query_timeout_secs` | `Option<u64>` | Per-call timeout in seconds; omitted = `30`, **`0` = disabled** |
| `breaker` | `Option<BreakerConfig>` | Breaker thresholds and window; no `enabled` master switch |
| `max_concurrency` | `Option<usize>` | Concurrency cap (default `32`); this crate's own semaphore |

### ArangoDB — ArangoConfig

```yaml
arangodb:
  base_url: "http://host:8529"
  db: "mydb"
  username: "root"
  password: "secret"
  # query_timeout_secs: 30   # Optional: per-call timeout, 0 = disabled
  # breaker: {}              # Optional: breaker config, omitted = conservative defaults (0.5 / 30s / opens for 10s)
  # max_concurrency: 32      # Optional: concurrency cap (this crate's semaphore)
```

| Field | Type | Notes |
|------|------|------|
| `base_url` | `String` | API address |
| `db` | `String` | Database name |
| `username` | `String` | Username |
| `password` | `String` | Password |
| `query_timeout_secs` | `Option<u64>` | Per-call timeout in seconds; omitted = `30`, **`0` = disabled** |
| `breaker` | `Option<BreakerConfig>` | Breaker thresholds and window; no `enabled` master switch |
| `max_concurrency` | `Option<usize>` | Concurrency cap (default `32`); this crate's own semaphore |

### IoTDB — IotdbConfig

```yaml
iotdb:
  base_url: "http://host:18080"
  username: "root"
  password: "root"
  # query_timeout_secs: 30   # Optional: per-call timeout, 0 = disabled
  # breaker: {}              # Optional: breaker config, omitted = conservative defaults (0.5 / 30s / opens for 10s)
  # max_concurrency: 32      # Optional: concurrency cap (this crate's semaphore)
```

| Field | Type | Notes |
|------|------|------|
| `base_url` | `String` | REST API address |
| `username` | `String` | Username |
| `password` | `String` | Password |
| `query_timeout_secs` | `Option<u64>` | Per-call timeout in seconds; omitted = `30`, **`0` = disabled** |
| `breaker` | `Option<BreakerConfig>` | Breaker thresholds and window; no `enabled` master switch |
| `max_concurrency` | `Option<usize>` | Concurrency cap (default `32`); this crate's own semaphore |

### TDengine — TdengineConfig

```yaml
tdengine:
  base_url: "http://host:6041"
  username: "root"
  password: "taosdata"
  # database: "my_db"        # Optional: without it the default database in the REST path is used
  # query_timeout_secs: 30   # Optional: per-call timeout, 0 = disabled
  # breaker: {}              # Optional: breaker config, omitted = conservative defaults (0.5 / 30s / opens for 10s)
  # max_concurrency: 32      # Optional: concurrency cap (this crate's semaphore)
```

| Field | Type | Notes |
|------|------|------|
| `base_url` | `String` | REST endpoint (taosAdapter, default port 6041) |
| `username` | `String` | Username |
| `password` | `String` | Password |
| `database` | `Option<String>` | Optional: default database name (appended to the REST path) |
| `query_timeout_secs` | `Option<u64>` | Per-call timeout in seconds; omitted = `30`, **`0` = disabled** |
| `breaker` | `Option<BreakerConfig>` | Breaker thresholds and window; no `enabled` master switch |
| `max_concurrency` | `Option<usize>` | Concurrency cap (default `32`); this crate's own semaphore |

**One budget for the whole call**: `write()` splits a batch of data points into several HTTP requests, and `query_timeout_secs` covers the **whole call** (all chunks), not one budget per chunk.

### MongoDB — MongoConfig

```yaml
mongodb:
  url: "mongodb://host:27017"
  database: "app"
  # max_pool_size: 10        # Optional: connection pool cap, omitted = driver default (**10**)
  # min_pool_size: 0         # Optional: connection pool floor (connections kept alive in the background)
  # query_timeout_secs: 30   # Optional: per-command timeout, 0 = disabled
  # breaker: {}              # Optional: breaker config, omitted = conservative defaults (0.5 / 30s / opens for 10s)
```

| Field | Type | Notes |
|------|------|------|
| `url` | `String` | Connection URI (auth, replica set and TLS options all live in the URI) |
| `database` | `String` | Database name |
| `max_pool_size` | `Option<u32>` | Connection pool cap; omitted = driver default **10** (measured on `mongodb` 3.8.0, not 100) |
| `min_pool_size` | `Option<u32>` | Connection pool floor; omitted = driver default `0` |
| `query_timeout_secs` | `Option<u64>` | Per-command timeout in seconds; omitted = `30`, **`0` = disabled** |
| `breaker` | `Option<BreakerConfig>` | Breaker thresholds and window; no `enabled` master switch |

**Concurrency backpressure goes through the driver's pool**: this crate has **no** `max_concurrency` (and is not HTTP either) — the driver carries its own pool, so set `max_pool_size` explicitly when you need more concurrency.

### S3 / MinIO — S3Config

```yaml
s3:
  endpoint: "http://host:9000"
  region: "us-east-1"
  access_key: "minioadmin"
  secret_key: "minioadmin"
  # query_timeout_secs: 30   # Optional: per-call timeout, 0 = disabled
  # breaker: {}              # Optional: breaker config, omitted = conservative defaults (0.5 / 30s / opens for 10s)
  # max_concurrency: 32      # Optional: concurrency cap (this crate's semaphore)
```

| Field | Type | Notes |
|------|------|------|
| `endpoint` | `String` | S3-compatible service address (MinIO / self-hosted gateway) |
| `region` | `String` | Region used for signing; MinIO is indifferent to the value, so `us-east-1` is fine |
| `access_key` | `String` | Access Key |
| `secret_key` | `String` | Secret Key |
| `query_timeout_secs` | `Option<u64>` | Per-call timeout in seconds; omitted = `30`, **`0` = disabled** |
| `breaker` | `Option<BreakerConfig>` | Breaker thresholds and window; no `enabled` master switch |
| `max_concurrency` | `Option<usize>` | Concurrency cap (default `32`); this crate's own semaphore |

**One budget for the whole call**: `list()` follows continuation tokens and issues several GETs in one call; the timeout covers the **whole pagination**.

> **What outbound resilience has in common** (every HTTP backend in this section): the order is **permit → breaker → timeout**; timeout errors carry `code = DeadlineExceeded`, breaker rejections `code = Unavailable` with `message = "circuit breaker is open"`; with the `metrics` feature they are counted under `ecat_outbound_timeouts_total{backend="<config section name>"}` — the label is the **config section name** (`"arangodb"` / `"mongodb"` / …), not the trait category name.

---

## Programmatic Creation

### Without Authentication

```rust
let es = ElasticsearchClient::new("http://localhost:9200");
let ch = ClickhouseClient::new("http://localhost:8123", "default");
```

### With Authentication

```rust
let es = ElasticsearchClient::with_auth("http://es:9200", "elastic", "secret");
let ch = ClickhouseClient::with_auth("http://ch:8123", "default", "admin", "pass");
let qdb = QuestdbClient::with_auth("http://qdb:9000", "admin", "quest");
let ng = NebulaGraphClient::with_auth("http://ng:19669", "space1", "root", "nebula");
```

---

---

## TLS Certificate Configuration

All data backends generally support optional TLS client authentication (the `tls` field), with **two exceptions**: `ecat-data-sqlx` does not support the field — setting it makes startup fail (its TLS goes through URL parameters); `ecat-data-memcached`'s field is **silently inert** — declared, but never read anywhere in the crate.

### Config Example

```yaml
clickhouse:
  base_url: "https://ch.internal:8443"
  tls:
    ca_cert: "/etc/ecat/ca.pem"
    client_cert: "/etc/ecat/client.pem"
    client_key: "/etc/ecat/client-key.pem"
    # skip_verify: true  # test environment only
```

### Automatic Certificate Generation (ecat-tls)

```rust
use ecat_tls::{generate_ca, generate_server_cert, generate_client_cert};

// 1. Generate the CA
let ca = generate_ca("MyOrg")?;
std::fs::write("ca.pem", &ca.cert_pem)?;
std::fs::write("ca-key.pem", &ca.key_pem)?;

// 2. Generate the server certificate
let srv = generate_server_cert("db.example.com")?;
std::fs::write("server.pem", &srv.cert_pem)?;
std::fs::write("server-key.pem", &srv.key_pem)?;

// 3. Generate the client certificate (mTLS)
let client = generate_client_cert("myapp")?;
std::fs::write("client.pem", &client.cert_pem)?;
std::fs::write("client-key.pem", &client.key_pem)?;
```

### Manual Generation (OpenSSL)

```bash
# CA
openssl req -x509 -newkey rsa:4096 -keyout ca-key.pem -out ca.pem -days 3650 -nodes

# Server certificate
openssl req -new -newkey rsa:4096 -keyout server-key.pem -out server.csr -nodes -subj "/CN=db.example.com"
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -out server.pem -days 365

# Client certificate (mTLS)
openssl req -new -newkey rsa:4096 -keyout client-key.pem -out client.csr -nodes -subj "/CN=myapp"
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca-key.pem -out client.pem -days 365
```

### TLS Field Reference

| Field | Type | Notes |
|------|------|------|
| `ca_cert` | `Option<String>` | CA certificate PEM path (verifies the server) |
| `client_cert` | `Option<String>` | Client certificate PEM path (mTLS) |
| `client_key` | `Option<String>` | Client private key PEM path (mTLS) |
| `skip_verify` | `Option<bool>` | Skip certificate verification (testing only) |

---

## Advanced Usage

### Environment Variable Override

```rust
use std::env;

fn load_config() -> Result<SqlxConfig, Box<dyn std::error::Error>> {
    let mut cfg: SqlxConfig = serde_yaml::from_str(
        &std::fs::read_to_string("databases.yaml")?
    )?;
    if let Ok(url) = env::var("DATABASE_URL") {
        cfg.url = url;
    }
    Ok(cfg)
}
```

### Combining with the ecat-config Framework

```rust
use ecat_config::{Config, FileSource};

let mut app_config = Config::new();
app_config.load(&FileSource::new("databases.yaml")).await?;

let redis_cfg: RedisConfig = serde_json::from_value(
    app_config.get::<serde_json::Value>("redis").unwrap()
)?;
let cache = RedisCache::from_config(redis_cfg).await?;
```

### Configure as Needed

Omit unused databases in YAML and mark them with `Option` in the Rust struct:

```rust
#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: Option<RedisConfig>,
    clickhouse: Option<ClickhouseConfig>,
}
```

---

## Related Documents

- [Audit report r5](audit-report-2026-08-01-r5.md)
- [TLS certificate authentication tutorial](tls-certificate-tutorial.md)
- [Example config files](../../../config/databases.example.yaml)
