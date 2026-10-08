# ডেটাবেস কনফিগ টিউটোরিয়াল

**ভার্সন:** 2.4.2 · **তারিখ:** 2026-08-01

e-cat-এর 14টি ডেটা ব্যাকএন্ডই কনফিগ ফাইল থেকে সংযোগ তথ্য লোড করতে সমর্থন করে, কোডে হার্ডকোড করার প্রয়োজন নেই। `username` / `password` দুটোই ঐচ্ছিক ফিল্ড, বাদ দিলে অথেনটিকেশন স্কিপ হয়।

---

## দ্রুত শুরু

### 1. কনফিগ ফাইল তৈরি করুন

উদাহরণ টেমপ্লেট কপি করে বাস্তব পরিবেশ অনুযায়ী পরিবর্তন করুন:

```bash
cp config/databases.example.yaml databases.yaml
```

`databases.yaml` এডিট করে আসল সংযোগ তথ্য দিন:

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

### 2. ডিপেন্ডেন্সি যোগ করুন

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
ecat-data-sqlx = { path = "../ecat-data-sqlx" }
ecat-data-redis = { path = "../ecat-data-redis" }
ecat-data-clickhouse = { path = "../ecat-data-clickhouse" }
```

### 3. লোড ও ব্যবহার

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
    // YAML কনফিগ লোড
    let yaml = std::fs::read_to_string("databases.yaml")?;
    let cfg: AppConfig = serde_yaml::from_str(&yaml)?;

    // ডেটাবেস ক্লায়েন্ট তৈরি — কোনো হার্ডকোডেড সংযোগ তথ্য নেই
    let db = SqlxClient::from_config(cfg.sql).await?;
    let cache = RedisCache::from_config(cfg.redis).await?;
    let ch = ClickhouseClient::from_config(cfg.clickhouse);

    // ব্যবহার
    let rows = db.query("SELECT id, name FROM users LIMIT 10").await?;
    cache.set("health", b"ok", std::time::Duration::from_secs(30)).await?;

    Ok(())
}
```

---

## সম্পূর্ণ কনফিগ রেফারেন্স

### টপ-লেভেল কনফিগ স্ট্রাক্ট সংজ্ঞায়িত করা

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

### YAML সম্পূর্ণ উদাহরণ

`config/databases.example.yaml` দেখুন।

---

## প্রতিটি ব্যাকএন্ডের Config ফিল্ড দ্রুত রেফারেন্স

### RDBMS — SqlxConfig

#### নেটিভ পুল (PostgreSQL / MySQL / SQLite)

URL scheme থেকে নেটিভ ড্রাইভার স্বয়ংক্রিয়ভাবে নির্বাচিত হয়, অতিরিক্ত কনফিগারেশনের দরকার নেই:

| scheme | ড্রাইভার |
|---|---|
| `postgres://` / `postgresql://` | PostgreSQL |
| `mysql://` / `mariadb://` | MySQL |
| `sqlite:` | SQLite |

- **সময় টাইপের নেটিভ সমর্থন**: পুরোনো sqlx `AnyPool` পথে সময় টাইপ সমর্থিত ছিল না, নিজে `CAST` করে টেক্সট বানাতে হতো; এখন সময় কলাম সরাসরি পড়া যায়।
- scheme-এর **বড়/ছোট হাতের অক্ষর এবং আগে-পরে ফাঁকা জায়গা দুটোই সহনীয়** (`"POSTGRES://…"` ও `" postgres://…"` দুটোই চলে); অচেনা scheme হলে ত্রুটি হয় এবং **কোন scheme তা নাম ধরে বলা হয়**।
- `mssql://` **এই ব্যাকএন্ডে দেওয়া হয় না** (`ecat-data-mssql` ব্যবহার করুন); এখানে দিলে স্পষ্টভাবে প্রত্যাখ্যাত হবে।

#### কনফিগারেশন উদাহরণ

```yaml
sql:
  url: "postgres://host:5432/dbname"
  # username: "app_user"    # ঐচ্ছিক
  # password: "secret"      # ঐচ্ছিক
```

| ফিল্ড | টাইপ | ডিফল্ট | ব্যাখ্যা |
|------|------|--------|------|
| `url` | `String` | — | sqlx সংযোগ স্ট্রিং, SQLite/PG/MySQL/TiDB সমর্থন করে |
| `username` | `Option<String>` | `None` | ঐচ্ছিক: URL-এ এমবেডেড অথেনটিকেশন (password-এর সাথে) |
| `password` | `Option<String>` | `None` | ঐচ্ছিক: URL-এ এমবেডেড অথেনটিকেশন (username-এর সাথে) |
| `max_connections` | `u32` | `10` | পুলে সর্বোচ্চ সংযোগ সংখ্যা |
| `min_connections` | `u32` | `0` | ন্যূনতম সংযোগ ধরে রাখা হয়; ≤ `max_connections` এ সীমাবদ্ধ |
| `acquire_timeout_secs` | `u64` | `30` | সংযোগের জন্য অপেক্ষার সময়সীমা; **`0` = সঙ্গে সঙ্গে টাইমআউট** (খালি সংযোগ না থাকলে ব্যর্থ), যা `query_timeout_secs`-এর `0` = নিষ্ক্রিয়-এর উল্টো |
| `idle_timeout_secs` | `u64` | `600` | নিষ্ক্রিয় সংযোগ ফিরিয়ে নেওয়া |
| `max_lifetime_secs` | `u64` | `1800` | সংযোগের সর্বোচ্চ আয়ু |
| `query_timeout_secs` | `u64` | `30` | প্রতি কুয়েরিতে টাইমআউট; **0 = নিষ্ক্রিয়** |
| `slow_query_ms` | `u64` | `1000` | স্লো কুয়েরি সতর্কতার থ্রেশহোল্ড (মিলিসেকেন্ড); সেট না থাকলে = 1000, **0 = বন্ধ** (শুধু `tracing` feature) |
| `test_before_acquire` | `bool` | `false` | সংযোগ দেওয়ার আগে ping করা হবে কি না |
| `session_init` | `string[]` | ডায়ালেক্ট অনুযায়ী | প্রতিটি নতুন সংযোগের সেশন ইনিশিয়ালাইজেশন স্টেটমেন্ট |

#### `session_init` (সেশন ইনিশিয়ালাইজেশন)

প্রতিটি নতুন সংযোগ প্রতিষ্ঠার পর এই স্টেটমেন্টগুলো ক্রমানুসারে চালানো হয়:

| ডায়ালেক্ট | ডিফল্ট `session_init` |
|---|---|
| PostgreSQL | `SET TIME ZONE 'UTC'`, `SET application_name = 'ecat'` |
| MySQL | `SET time_zone = '+00:00'` |
| SQLite | সেশনের ধারণা নেই — ডিফল্টে খালি |

উদ্দেশ্য হলো **ডেটাবেস পক্ষ সরাসরি UTC ফেরত দেবে**, যা ফ্রেমওয়ার্কের "সব সময় একরূপে RFC3339 UTC-তে উপস্থাপন" নীতির সাথে মেলে।

- **স্পষ্ট খালি অ্যারে `[]` মানে "ইচ্ছাকৃতভাবে বন্ধ"** এবং এটি ডায়ালেক্ট ডিফল্টকে ওভাররাইড করে (`query_timeout_secs: 0` দিয়ে নিষ্ক্রিয় করার মতোই স্পষ্ট-ওভাররাইড নীতি)।
- যেকোনো স্টেটমেন্ট ব্যর্থ → সংযোগ তৈরি ব্যর্থ হয়, **নীরবে নিম্নস্তরে পড়ে না**।

```yaml
sql:
  url: "mysql://host:3306/dbname"
  session_init:
    - "SET time_zone = '+00:00'"
    - "SET NAMES utf8mb4"
```

#### `warm_up()` — ওয়ার্ম-আপ

শুরুর সময় সক্রিয়ভাবে `min_connections` টি সংযোগ তৈরি করে ফেরত দেয়, যাতে সেবা উঠতেই প্রস্তুত অবস্থায় থাকে:

```rust
let db = SqlxClient::from_config(cfg).await?;
db.warm_up().await?;   // শুরুর সময় একবার কল করুন
```

কেন দরকার: sqlx `min_connections` **ব্যাকগ্রাউন্ড টাস্ক দিয়ে অ্যাসিঙ্ক্রোনাসভাবে** রক্ষণাবেক্ষণ করে, তাই `connect()` ফেরার সময় পুল ভরা আছে তার নিশ্চয়তা নেই — শুরুর পর অনুরোধের প্রথম ঢেউ ব্যাকগ্রাউন্ড টাস্কের সাথে দৌড়ে যাবে।

#### অবজারভেবিলিটি feature (`metrics` / `health` / `tracing`)

তিনটি feature **ডিফল্টে বন্ধ** (যাতে axum — `ecat-metrics` / `ecat-health`-এর নির্ভরতা — কোর নির্ভরতা ট্রিতে না ঢোকে); প্রয়োজনমতো চালু করুন। `ecat-data-mssql`-ও একই তিনটি দেয়।

```toml
ecat-data-sqlx = { path = "../ecat-data-sqlx", features = ["metrics", "health", "tracing"] }
```

```rust
use std::sync::Arc;
use ecat_data_sqlx::{RdbmsHealthCheck, SqlxClient, SqlxConfig, register_pool_metrics};
use ecat_health::HealthRegistry;

let db = SqlxClient::from_config(cfg).await?;

// metrics: নিবন্ধন করলে /metrics এন্ডপয়েন্টে চারটি মেট্রিক যোগ হয় —
// ecat_rdbms_pool_connections (gauge, state="idle"/"active" সহ),
// ecat_rdbms_pool_timeouts_total, ecat_rdbms_query_timeout_total,
// ecat_rdbms_transactions_leaked_total (সবই counter, backend লেবেলসহ)
register_pool_metrics("primary", db.pool());

// health: SELECT 1 সংযোগ পরীক্ষা, /health-এর readyz-এ নিবন্ধিত
let registry = HealthRegistry::new()
    .with_check(RdbmsHealthCheck::new("sql", Arc::new(db)));
```

`tracing` feature কুয়েরি `slow_query_ms` ছাড়ালে warn লেখে (ব্যয়িত সময় + প্রথম ২০০ অক্ষরে কাটা SQL)। এই ফিল্ড কেবল এই feature-ই পড়ে — বন্ধ থাকলেও পার্স হয়, তবে কার্যকর নয়।

#### সময় ও তারিখ: একরূপে RFC3339 UTC

**sqlite-এর সময়/তারিখ টেক্সট RFC3339 UTC-তে পুনর্লিখিত হয়**:

- `2026-10-05 12:34:56` আকৃতির টেক্সট → `"2026-10-05T12:34:56Z"`
- `2026-10-05` আকৃতির টেক্সট → `"2026-10-05T00:00:00Z"` (UTC মধরাত্রির মুহূর্ত)

কারণ: sqlite-এ টাইপ সিস্টেম নেই, তারিখ/সময় আকৃতির টেক্সট স্বভাবতই দ্ব্যর্থক, তাই ফ্রেমওয়ার্ক এটিকে একরূপে সময় হিসেবে ধরে।

**পার্শ্বপ্রতিক্রিয়া**: ভার্সন নম্বর, বিজনেস কোড—এমন "কাকতালীয়ভাবে তারিখের মতো দেখতে" টেক্সট কলামও পুনর্লিখিত হবে। এই আচরণ না চাইলে কলামটি স্পষ্টভাবে তারিখ-বহির্ভূত আকৃতিতে CAST করুন, বা অন্য টাইপ ব্যবহার করুন।

PG / MySQL-এর প্রকৃত `DATE` / `TIMESTAMP` কলামও একইভাবে RFC3339 UTC স্ট্রিং হিসেবে উপস্থাপিত হয়, এবং **শুধু তারিখেও `T00:00:00Z` থাকে** — কারণ `"2026-10-05"` বৈধ RFC3339 নয়, আর ফ্রেমওয়ার্ক ভেতরে একটি অভিন্ন ফরম্যাট রাখে যাতে উপরের স্তর সহজে পার্স করতে পারে।

### Redis — RedisConfig

```yaml
redis:
  url: "redis://host:6379"
  # password: "auth_token"  # ঐচ্ছিক
  # query_timeout_secs: 30     # ঐচ্ছিক: প্রতি কমান্ডে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}                # ঐচ্ছিক: breaker কনফিগ, বাদ দিলে = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
```

| ফিল্ড | টাইপ | ডিফল্ট | ব্যাখ্যা |
|------|------|--------|------|
| `url` | `String` | — | Redis সংযোগ URL |
| `password` | `Option<String>` | `None` | ঐচ্ছিক: Redis AUTH পাসওয়ার্ড |
| `query_timeout_secs` | `Option<u64>` | `30` | প্রতি কমান্ডে টাইমআউট (সেকেন্ডে); **`0` = নিষ্ক্রিয়** |
| `breaker` | `Option<BreakerConfig>` | রক্ষণশীল ডিফল্ট | breaker-এর থ্রেশহোল্ড ও উইন্ডো; ফিল্ড বাদ দেওয়া যায়, `breaker: {}` মানে সব ডিফল্ট |

**সামর্থ্যের সীমা**: এই client `MultiplexedConnection` ব্যবহার করে (একটি TCP সংযোগেই সব সমবর্তিতা), **কানেকশন পুল নয়** — ক্যাশ লোডে এটি পুলের চেয়ে ভালো: কম সংযোগ, কম রাউন্ড-ট্রিপ। মূল্য হলো **স্টেটপূর্ণ কমান্ড সিকোয়েন্স এটি দিয়ে চলবে না**: `MULTI`/`EXEC` ট্রানজ্যাকশন, `WATCH`, `SUBSCRIBE` ও ব্লকিং কমান্ডের জন্য একচেটিয়া সংযোগ দরকার — মাল্টিপ্লেক্সিংয়ে সেগুলো অন্য কমান্ডের সাথে জড়িয়ে যাবে। দরকার হলে `redis::Client::get_async_connection()` দিয়ে আলাদা সংযোগ খুলুন।

**টাইমআউট**: কনফিগে `query_timeout_secs: 0` মানে **নিষ্ক্রিয়** («০ সেকেন্ডে টাইমআউট» নয়); ৩০ সেকেন্ডের ডিফল্ট কেবল ফিল্ড বাদ দিলে প্রযোজ্য। লাইব্রেরি স্তরে `run_with_timeout(kind, Some(Duration::ZERO), fut)` উল্টো — সেটি **সঙ্গে সঙ্গে টাইমআউট** (tokio আগে ভেতরের future poll করে, তাই আগেই প্রস্তুত future তবুও সফল হয়)। দুটি «0»-এর অর্থ আলাদা; লাইব্রেরি ফাংশন সরাসরি ডাকলে কনফিগের মান নকল করবেন না।

**breaker ডিফল্টে চালু** — রক্ষণশীল থ্রেশহোল্ডে (ব্যর্থতার অনুপাত ০.৫, উইন্ডো ৩০ সেকেন্ড, half-open প্রোব ৩, খোলা ১০ সেকেন্ড) কেবল ক্রমাগত ব্যর্থতায় খোলে। **এখন কোনো মাস্টার সুইচ নেই**: `BreakerConfig`-এ কেবল এই চারটি থ্রেশহোল্ড ফিল্ড, `enabled` নেই; `{"enabled": false}` লিখলে কেবল ডিসেরিয়ালাইজেশন ত্রুটি মেলে। সত্যিই বন্ধ করতে চাইলে থ্রেশহোল্ড নাগালের বাইরে নিয়ে যান (যেমন `failure_ratio: 1.1`)।

### Memcached — MemcachedConfig

```yaml
memcached:
  # username: "memcache"    # ঐচ্ছিক: রিজার্ভড ফিল্ড (বর্তমানে মেমরি-ভিত্তিক)
  # password: "secret"      # ঐচ্ছিক: রিজার্ভড ফিল্ড
  {}
```

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `username` | `Option<String>` | ঐচ্ছিক: রিজার্ভড ফিল্ড |
| `password` | `Option<String>` | ঐচ্ছিক: রিজার্ভড ফিল্ড |

বর্তমানে মেমরি-ভিত্তিক ইমপ্লিমেন্টেশন, অথেনটিকেশন ফিল্ড রিজার্ভড।

### ClickHouse — ClickhouseConfig

```yaml
clickhouse:
  base_url: "http://host:8123"
  database: "default"
  # username: "default"   # ঐচ্ছিক
  # password: "secret"    # ঐচ্ছিক
  # query_timeout_secs: 30  # ঐচ্ছিক: প্রতি কলে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}             # ঐচ্ছিক: breaker কনফিগ, বাদ দিলে = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
  # max_concurrency: 32     # ঐচ্ছিক: সমবর্তিতার সীমা (এই crate-এর সেমাফোর)
```

| ফিল্ড | টাইপ | ডিফল্ট | ব্যাখ্যা |
|------|------|--------|------|
| `base_url` | `String` | — | HTTP ইন্টারফেস ঠিকানা |
| `database` | `String` | `"default"` | ডেটাবেস নাম |
| `username` | `Option<String>` | `None` | ঐচ্ছিক: HTTP Basic Auth ইউজারনেম |
| `password` | `Option<String>` | `None` | ঐচ্ছিক: HTTP Basic Auth পাসওয়ার্ড |
| `query_timeout_secs` | `Option<u64>` | `30` | প্রতি কলে টাইমআউট (সেকেন্ডে); **`0` = নিষ্ক্রিয়** (Redis-এর মতো) |
| `breaker` | `Option<BreakerConfig>` | রক্ষণশীল ডিফল্ট | breaker-এর থ্রেশহোল্ড ও উইন্ডো; এখানেও `enabled` মাস্টার সুইচ নেই |
| `max_concurrency` | `Option<usize>` | `32` | সমবর্তিতার সীমা; **এই crate-এর নিজস্ব সেমাফোর**, reqwest-এর নব নয় (reqwest-এ কেবল `pool_max_idle_per_host` আছে — নিষ্ক্রিয় সংযোগ ধরে রাখার সংখ্যা, কোনো ঊর্ধ্বসীমা নয়) |

**দুই স্তরের টাইমআউট**: `from_config` যে `reqwest::Client` বানায় (`ecat-tls`) তার নিজের ৫ সেকেন্ড সংযোগ + ৩০ সেকেন্ড মোট টাইমআউট আছে; `query_timeout_secs` হলো **বাইরের** বাজেট — দুটোই থাকলে **যেটি আগে শেষ হয় সেটিই কার্যকর**; ভেতরেরটি শেষ হলে ত্রুটি `RdbmsError::Database` এবং `ecat_outbound_timeouts_total`-এ **গণনা হয় না** (বাইরের টাইমআউটের counter, `metrics` feature)। `new` / `with_auth` খালি `reqwest::Client::new()` ব্যবহার করে, ভেতরের টাইমআউট নেই।

### QuestDB — QuestdbConfig

```yaml
questdb:
  base_url: "http://host:9000"
  # username: "admin"     # ঐচ্ছিক
  # password: "quest"     # ঐচ্ছিক
  # query_timeout_secs: 30   # ঐচ্ছিক: প্রতি কলে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}              # ঐচ্ছিক: breaker কনফিগ, বাদ দেওয়া = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
  # max_concurrency: 32      # ঐচ্ছিক: সমবর্তিতার সীমা (এই crate-এর সেমাফোর)
```

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `base_url` | `String` | HTTP API ঠিকানা |
| `username` | `Option<String>` | ঐচ্ছিক: HTTP Basic Auth ইউজারনেম |
| `password` | `Option<String>` | ঐচ্ছিক: HTTP Basic Auth পাসওয়ার্ড |
| `query_timeout_secs` | `Option<u64>` | প্রতি কলে টাইমআউট (সেকেন্ডে); বাদ দেওয়া = `30`, **`0` = নিষ্ক্রিয়** |
| `breaker` | `Option<BreakerConfig>` | breaker-এর থ্রেশহোল্ড ও উইন্ডো; `enabled` মাস্টার সুইচ নেই |
| `max_concurrency` | `Option<usize>` | সমবর্তিতার সীমা (ডিফল্ট `32`); এই crate-এর নিজস্ব সেমাফোর |

**ত্রুটির ধরন**: QuestDB `SqlExecutor` (RDBMS পরিবার) দিয়ে যায় —— টাইমআউট হলো `RdbmsError::Timeout`,
আর breaker-এর প্রত্যাখ্যান `RdbmsError::Connection("circuit breaker is open")`; বাকি HTTP ব্যাকএন্ড সবই
`ecat_errors::Error` (`code = DeadlineExceeded` / `Unavailable`, `reason` = ব্যাকএন্ডের নাম)।

### Elasticsearch — ElasticsearchConfig

```yaml
elasticsearch:
  base_url: "http://host:9200"
  # username: "elastic"   # ঐচ্ছিক
  # password: "secret"    # ঐচ্ছিক
  # query_timeout_secs: 30   # ঐচ্ছিক: প্রতি কলে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}              # ঐচ্ছিক: breaker কনফিগ, বাদ দেওয়া = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
  # max_concurrency: 32      # ঐচ্ছিক: সমবর্তিতার সীমা (এই crate-এর সেমাফোর)
```

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `base_url` | `String` | REST API ঠিকানা |
| `username` | `Option<String>` | ঐচ্ছিক: HTTP Basic Auth ইউজারনেম |
| `password` | `Option<String>` | ঐচ্ছিক: HTTP Basic Auth পাসওয়ার্ড |
| `query_timeout_secs` | `Option<u64>` | প্রতি কলে টাইমআউট (সেকেন্ডে); বাদ দেওয়া = `30`, **`0` = নিষ্ক্রিয়** |
| `breaker` | `Option<BreakerConfig>` | breaker-এর থ্রেশহোল্ড ও উইন্ডো; `enabled` মাস্টার সুইচ নেই |
| `max_concurrency` | `Option<usize>` | সমবর্তিতার সীমা (ডিফল্ট `32`); এই crate-এর নিজস্ব সেমাফোর |

### OpenSearch — OpenSearchConfig

```yaml
opensearch:
  base_url: "http://host:9200"
  # username: "admin"     # ঐচ্ছিক
  # password: "secret"    # ঐচ্ছিক
  # query_timeout_secs: 30   # ঐচ্ছিক: প্রতি কলে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}              # ঐচ্ছিক: breaker কনফিগ, বাদ দেওয়া = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
  # max_concurrency: 32      # ঐচ্ছিক: সমবর্তিতার সীমা (এই crate-এর সেমাফোর)
```

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `base_url` | `String` | REST API ঠিকানা |
| `username` | `Option<String>` | ঐচ্ছিক: HTTP Basic Auth ইউজারনেম |
| `password` | `Option<String>` | ঐচ্ছিক: HTTP Basic Auth পাসওয়ার্ড |
| `query_timeout_secs` | `Option<u64>` | প্রতি কলে টাইমআউট (সেকেন্ডে); বাদ দেওয়া = `30`, **`0` = নিষ্ক্রিয়** |
| `breaker` | `Option<BreakerConfig>` | breaker-এর থ্রেশহোল্ড ও উইন্ডো; `enabled` মাস্টার সুইচ নেই |
| `max_concurrency` | `Option<usize>` | সমবর্তিতার সীমা (ডিফল্ট `32`); এই crate-এর নিজস্ব সেমাফোর |

### InfluxDB — InfluxConfig

```yaml
influxdb:
  base_url: "http://host:8086"
  org: "myorg"
  bucket: "mybucket"
  token: "my-token"
  # query_timeout_secs: 30   # ঐচ্ছিক: প্রতি কলে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}              # ঐচ্ছিক: breaker কনফিগ, বাদ দেওয়া = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
  # max_concurrency: 32      # ঐচ্ছিক: সমবর্তিতার সীমা (এই crate-এর সেমাফোর)
```

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `base_url` | `String` | InfluxDB 2.x API ঠিকানা |
| `org` | `String` | সংস্থার নাম |
| `bucket` | `String` | বাকেটের নাম |
| `token` | `String` | অথেনটিকেশন টোকেন |
| `query_timeout_secs` | `Option<u64>` | প্রতি কলে টাইমআউট (সেকেন্ডে); বাদ দেওয়া = `30`, **`0` = নিষ্ক্রিয়** |
| `breaker` | `Option<BreakerConfig>` | breaker-এর থ্রেশহোল্ড ও উইন্ডো; `enabled` মাস্টার সুইচ নেই |
| `max_concurrency` | `Option<usize>` | সমবর্তিতার সীমা (ডিফল্ট `32`); এই crate-এর নিজস্ব সেমাফোর |

### Neo4j — Neo4jConfig

```yaml
neo4j:
  base_url: "http://host:7474"
  username: "neo4j"
  password: "secret"
  # query_timeout_secs: 30   # ঐচ্ছিক: প্রতি কলে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}              # ঐচ্ছিক: breaker কনফিগ, বাদ দেওয়া = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
  # max_concurrency: 32      # ঐচ্ছিক: সমবর্তিতার সীমা (এই crate-এর সেমাফোর)
```

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `base_url` | `String` | REST API ঠিকানা |
| `username` | `String` | ইউজারনেম |
| `password` | `String` | পাসওয়ার্ড |
| `query_timeout_secs` | `Option<u64>` | প্রতি কলে টাইমআউট (সেকেন্ডে); বাদ দেওয়া = `30`, **`0` = নিষ্ক্রিয়** |
| `breaker` | `Option<BreakerConfig>` | breaker-এর থ্রেশহোল্ড ও উইন্ডো; `enabled` মাস্টার সুইচ নেই |
| `max_concurrency` | `Option<usize>` | সমবর্তিতার সীমা (ডিফল্ট `32`); এই crate-এর নিজস্ব সেমাফোর |

### NebulaGraph — NebulaGraphConfig

```yaml
nebulagraph:
  base_url: "http://host:19669"
  space: "my_space"
  # username: "root"      # ঐচ্ছিক
  # password: "nebula"    # ঐচ্ছিক
  # query_timeout_secs: 30   # ঐচ্ছিক: প্রতি কলে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}              # ঐচ্ছিক: breaker কনফিগ, বাদ দেওয়া = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
  # max_concurrency: 32      # ঐচ্ছিক: সমবর্তিতার সীমা (এই crate-এর সেমাফোর)
```

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `base_url` | `String` | API ঠিকানা |
| `space` | `String` | গ্রাফ স্পেস নাম |
| `username` | `Option<String>` | ঐচ্ছিক: HTTP Basic Auth ইউজারনেম |
| `password` | `Option<String>` | ঐচ্ছিক: HTTP Basic Auth পাসওয়ার্ড |
| `query_timeout_secs` | `Option<u64>` | প্রতি কলে টাইমআউট (সেকেন্ডে); বাদ দেওয়া = `30`, **`0` = নিষ্ক্রিয়** |
| `breaker` | `Option<BreakerConfig>` | breaker-এর থ্রেশহোল্ড ও উইন্ডো; `enabled` মাস্টার সুইচ নেই |
| `max_concurrency` | `Option<usize>` | সমবর্তিতার সীমা (ডিফল্ট `32`); এই crate-এর নিজস্ব সেমাফোর |

### ArangoDB — ArangoConfig

```yaml
arangodb:
  base_url: "http://host:8529"
  db: "mydb"
  username: "root"
  password: "secret"
  # query_timeout_secs: 30   # ঐচ্ছিক: প্রতি কলে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}              # ঐচ্ছিক: breaker কনফিগ, বাদ দেওয়া = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
  # max_concurrency: 32      # ঐচ্ছিক: সমবর্তিতার সীমা (এই crate-এর সেমাফোর)
```

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `base_url` | `String` | API ঠিকানা |
| `db` | `String` | ডেটাবেস নাম |
| `username` | `String` | ইউজারনেম |
| `password` | `String` | পাসওয়ার্ড |
| `query_timeout_secs` | `Option<u64>` | প্রতি কলে টাইমআউট (সেকেন্ডে); বাদ দেওয়া = `30`, **`0` = নিষ্ক্রিয়** |
| `breaker` | `Option<BreakerConfig>` | breaker-এর থ্রেশহোল্ড ও উইন্ডো; `enabled` মাস্টার সুইচ নেই |
| `max_concurrency` | `Option<usize>` | সমবর্তিতার সীমা (ডিফল্ট `32`); এই crate-এর নিজস্ব সেমাফোর |

### IoTDB — IotdbConfig

```yaml
iotdb:
  base_url: "http://host:18080"
  username: "root"
  password: "root"
  # query_timeout_secs: 30   # ঐচ্ছিক: প্রতি কলে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}              # ঐচ্ছিক: breaker কনফিগ, বাদ দেওয়া = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
  # max_concurrency: 32      # ঐচ্ছিক: সমবর্তিতার সীমা (এই crate-এর সেমাফোর)
```

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `base_url` | `String` | REST API ঠিকানা |
| `username` | `String` | ইউজারনেম |
| `password` | `String` | পাসওয়ার্ড |
| `query_timeout_secs` | `Option<u64>` | প্রতি কলে টাইমআউট (সেকেন্ডে); বাদ দেওয়া = `30`, **`0` = নিষ্ক্রিয়** |
| `breaker` | `Option<BreakerConfig>` | breaker-এর থ্রেশহোল্ড ও উইন্ডো; `enabled` মাস্টার সুইচ নেই |
| `max_concurrency` | `Option<usize>` | সমবর্তিতার সীমা (ডিফল্ট `32`); এই crate-এর নিজস্ব সেমাফোর |

### TDengine — TdengineConfig

```yaml
tdengine:
  base_url: "http://host:6041"
  username: "root"
  password: "taosdata"
  # database: "my_db"        # ঐচ্ছিক: না দিলে REST পাথের ডিফল্ট ডেটাবেস ব্যবহৃত হয়
  # query_timeout_secs: 30   # ঐচ্ছিক: প্রতি কলে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}              # ঐচ্ছিক: breaker কনফিগ, বাদ দেওয়া = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
  # max_concurrency: 32      # ঐচ্ছিক: সমবর্তিতার সীমা (এই crate-এর সেমাফোর)
```

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `base_url` | `String` | REST ইন্টারফেসের ঠিকানা (taosAdapter, ডিফল্ট পোর্ট 6041) |
| `username` | `String` | ব্যবহারকারীর নাম |
| `password` | `String` | পাসওয়ার্ড |
| `database` | `Option<String>` | ঐচ্ছিক: ডিফল্ট ডেটাবেসের নাম (REST পাথে যুক্ত হয়) |
| `query_timeout_secs` | `Option<u64>` | প্রতি কলে টাইমআউট (সেকেন্ডে); বাদ দেওয়া = `30`, **`0` = নিষ্ক্রিয়** |
| `breaker` | `Option<BreakerConfig>` | breaker-এর থ্রেশহোল্ড ও উইন্ডো; `enabled` মাস্টার সুইচ নেই |
| `max_concurrency` | `Option<usize>` | সমবর্তিতার সীমা (ডিফল্ট `32`); এই crate-এর নিজস্ব সেমাফোর |

**গোটা কলের জন্য এক বাজেট**: `write()` এক ব্যাচ ডেটাপয়েন্টকে কয়েকটি HTTP রিকোয়েস্টে ভাগ করে, আর `query_timeout_secs` **গোটা কল** (সব ভাগ) ঢাকে, প্রতি ভাগে আলাদা বাজেট নয়।

### MongoDB — MongoConfig

```yaml
mongodb:
  url: "mongodb://host:27017"
  database: "app"
  # max_pool_size: 10        # ঐচ্ছিক: কানেকশন পুলের ঊর্ধ্বসীমা, বাদ দেওয়া = ড্রাইভারের ডিফল্ট (**10**)
  # min_pool_size: 0         # ঐচ্ছিক: কানেকশন পুলের নিম্নসীমা (ব্যাকগ্রাউন্ডে রাখা সংযোগ)
  # query_timeout_secs: 30   # ঐচ্ছিক: প্রতি কমান্ডে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}              # ঐচ্ছিক: breaker কনফিগ, বাদ দেওয়া = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
```

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `url` | `String` | সংযোগ URI (প্রমাণীকরণ, রেপ্লিকা সেট ও TLS অপশন সবই URI-তে) |
| `database` | `String` | ডেটাবেসের নাম |
| `max_pool_size` | `Option<u32>` | কানেকশন পুলের ঊর্ধ্বসীমা; বাদ দেওয়া = ড্রাইভারের ডিফল্ট **10** (`mongodb` 3.8.0-তে মাপা, 100 নয়) |
| `min_pool_size` | `Option<u32>` | কানেকশন পুলের নিম্নসীমা; বাদ দেওয়া = ড্রাইভারের ডিফল্ট `0` |
| `query_timeout_secs` | `Option<u64>` | প্রতি কমান্ডে টাইমআউট (সেকেন্ডে); বাদ দেওয়া = `30`, **`0` = নিষ্ক্রিয়** |
| `breaker` | `Option<BreakerConfig>` | breaker-এর থ্রেশহোল্ড ও উইন্ডো; `enabled` মাস্টার সুইচ নেই |

**সমবর্তিতার ব্যাকপ্রেশার চলে ড্রাইভারের কানেকশন পুলে**: এই crate-এ **নেই** `max_concurrency` (HTTP-ও নয়) —— ড্রাইভার নিজের পুল বহন করে; বেশি সমবর্তিতা চাইলে `max_pool_size` স্পষ্টভাবে সেট করুন।

### S3 / MinIO — S3Config

```yaml
s3:
  endpoint: "http://host:9000"
  region: "us-east-1"
  access_key: "minioadmin"
  secret_key: "minioadmin"
  # query_timeout_secs: 30   # ঐচ্ছিক: প্রতি কলে টাইমআউট, 0 = নিষ্ক্রিয়
  # breaker: {}              # ঐচ্ছিক: breaker কনফিগ, বাদ দেওয়া = রক্ষণশীল ডিফল্ট (0.5 / 30s / খোলা 10s)
  # max_concurrency: 32      # ঐচ্ছিক: সমবর্তিতার সীমা (এই crate-এর সেমাফোর)
```

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `endpoint` | `String` | S3-সামঞ্জস্যপূর্ণ সেবার ঠিকানা (MinIO / নিজস্ব গেটওয়ে) |
| `region` | `String` | সাইনিংয়ের রিজিয়ন; MinIO মান নিয়ে সংবেদনশীল নয়, `us-east-1` দিলেই চলে |
| `access_key` | `String` | Access Key |
| `secret_key` | `String` | Secret Key |
| `query_timeout_secs` | `Option<u64>` | প্রতি কলে টাইমআউট (সেকেন্ডে); বাদ দেওয়া = `30`, **`0` = নিষ্ক্রিয়** |
| `breaker` | `Option<BreakerConfig>` | breaker-এর থ্রেশহোল্ড ও উইন্ডো; `enabled` মাস্টার সুইচ নেই |
| `max_concurrency` | `Option<usize>` | সমবর্তিতার সীমা (ডিফল্ট `32`); এই crate-এর নিজস্ব সেমাফোর |

**গোটা কলের জন্য এক বাজেট**: `list()` continuation token অনুসরণ করে এক কলে কয়েকটি GET পাঠায়, আর টাইমআউট **গোটা পেজিং** ঢাকে।

> **আউটবাউন্ড রেজিলিয়েন্সের সাধারণ দিক** (এই বিভাগের সব HTTP ব্যাকএন্ড): ক্রম **পারমিট → breaker → টাইমআউট**; টাইমআউট ত্রুটি `code = DeadlineExceeded`, breaker-এর প্রত্যাখ্যান `code = Unavailable` ও `message = "circuit breaker is open"`; `metrics` feature থাকলে `ecat_outbound_timeouts_total{backend="<কনফিগ বিভাগের নাম>"}`-এ গোনা হয় —— লেবেলটি **কনফিগ বিভাগের নাম** (`"arangodb"` / `"mongodb"` / …), trait শ্রেণির নাম নয়।

---

## প্রোগ্রাম্যাটিকভাবে তৈরি

### অথেনটিকেশন ছাড়া

```rust
let es = ElasticsearchClient::new("http://localhost:9200");
let ch = ClickhouseClient::new("http://localhost:8123", "default");
```

### অথেনটিকেশন সহ

```rust
let es = ElasticsearchClient::with_auth("http://es:9200", "elastic", "secret");
let ch = ClickhouseClient::with_auth("http://ch:8123", "default", "admin", "pass");
let qdb = QuestdbClient::with_auth("http://qdb:9000", "admin", "quest");
let ng = NebulaGraphClient::with_auth("http://ng:19669", "space1", "root", "nebula");
```

---

---

## TLS সার্টিফিকেট কনফিগ

ডেটা ব্যাকএন্ড সাধারণত ঐচ্ছিক TLS ক্লায়েন্ট অথেনটিকেশন (`tls` ফিল্ড) সমর্থন করে, তবে **দুটি ব্যতিক্রম** আছে: `ecat-data-sqlx` ফিল্ডটি সমর্থন করে না — দিলে চালু হওয়ার সময়ই ত্রুটি হবে (এর TLS যায় URL প্যারামিটার দিয়ে); `ecat-data-memcached`-এর ফিল্ডটি **চুপচাপ নিষ্ক্রিয়** — ঘোষিত, কিন্তু crate-এ কোথাও পড়া হয় না।

### কনফিগ উদাহরণ

```yaml
clickhouse:
  base_url: "https://ch.internal:8443"
  tls:
    ca_cert: "/etc/ecat/ca.pem"
    client_cert: "/etc/ecat/client.pem"
    client_key: "/etc/ecat/client-key.pem"
    # skip_verify: true  # শুধুমাত্র টেস্ট পরিবেশ
```

### সার্টিফিকেট অটো-জেনারেশন (ecat-tls)

```rust
use ecat_tls::{generate_ca, generate_server_cert, generate_client_cert};

// 1. CA তৈরি
let ca = generate_ca("MyOrg")?;
std::fs::write("ca.pem", &ca.cert_pem)?;
std::fs::write("ca-key.pem", &ca.key_pem)?;

// 2. সার্ভার সার্টিফিকেট তৈরি
let srv = generate_server_cert("db.example.com")?;
std::fs::write("server.pem", &srv.cert_pem)?;
std::fs::write("server-key.pem", &srv.key_pem)?;

// 3. ক্লায়েন্ট সার্টিফিকেট তৈরি (mTLS)
let client = generate_client_cert("myapp")?;
std::fs::write("client.pem", &client.cert_pem)?;
std::fs::write("client-key.pem", &client.key_pem)?;
```

### ম্যানুয়াল জেনারেশন (OpenSSL)

```bash
# CA
openssl req -x509 -newkey rsa:4096 -keyout ca-key.pem -out ca.pem -days 3650 -nodes

# সার্ভার সার্টিফিকেট
openssl req -new -newkey rsa:4096 -keyout server-key.pem -out server.csr -nodes -subj "/CN=db.example.com"
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -out server.pem -days 365

# ক্লায়েন্ট সার্টিফিকেট (mTLS)
openssl req -new -newkey rsa:4096 -keyout client-key.pem -out client.csr -nodes -subj "/CN=myapp"
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca-key.pem -out client.pem -days 365
```

### TLS ফিল্ড ব্যাখ্যা

| ফিল্ড | টাইপ | ব্যাখ্যা |
|------|------|------|
| `ca_cert` | `Option<String>` | CA সার্টিফিকেট PEM পাথ (সার্ভার যাচাই) |
| `client_cert` | `Option<String>` | ক্লায়েন্ট সার্টিফিকেট PEM পাথ (mTLS) |
| `client_key` | `Option<String>` | ক্লায়েন্ট প্রাইভেট কী PEM পাথ (mTLS) |
| `skip_verify` | `Option<bool>` | সার্টিফিকেট যাচাই স্কিপ (শুধুমাত্র টেস্ট) |

---

## অ্যাডভান্সড ব্যবহার

### এনভায়রনমেন্ট ভেরিয়েবল ওভাররাইড

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

### ecat-config ফ্রেমওয়ার্কের সাথে

```rust
use ecat_config::{Config, FileSource};

let mut app_config = Config::new();
app_config.load(&FileSource::new("databases.yaml")).await?;

let redis_cfg: RedisConfig = serde_json::from_value(
    app_config.get::<serde_json::Value>("redis").unwrap()
)?;
let cache = RedisCache::from_config(redis_cfg).await?;
```

### প্রয়োজনে কনফিগ

অব্যবহৃত ডেটাবেস YAML-এ বাদ দিন, Rust স্ট্রাক্টে `Option` দিয়ে চিহ্নিত করুন:

```rust
#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: Option<RedisConfig>,
    clickhouse: Option<ClickhouseConfig>,
}
```

---

## সম্পর্কিত ডকুমেন্ট

- [অডিট রিপোর্ট r5](audit-report-2026-08-01-r5.md)
- [TLS সার্টিফিকেট অথেনটিকেশন টিউটোরিয়াল](tls-certificate-tutorial.md)
- [কনফিগ উদাহরণ ফাইল](../../../config/databases.example.yaml)
