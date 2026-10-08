# डेटाबेस कॉन्फ़िगरेशन ट्यूटोरियल

**संस्करण:** 2.4.2 · **दिनांक:** 2026-08-01

e-cat के 14 डेटा बैकएंड सभी कॉन्फ़िगरेशन फ़ाइल से कनेक्शन जानकारी लोड करने का समर्थन करते हैं, कोड में हार्डकोडिंग की आवश्यकता नहीं। `username` / `password` दोनों वैकल्पिक फ़ील्ड हैं, छोड़ने पर प्रमाणीकरण छोड़ दिया जाता है।

---

## त्वरित आरंभ

### 1. कॉन्फ़िगरेशन फ़ाइल बनाएं

उदाहरण टेम्पलेट कॉपी करें और वास्तविक वातावरण के अनुसार संशोधित करें:

```bash
cp config/databases.example.yaml databases.yaml
```

`databases.yaml` संपादित करें, वास्तविक कनेक्शन जानकारी भरें:

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

### 2. निर्भरताएँ जोड़ें

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
ecat-data-sqlx = { path = "../ecat-data-sqlx" }
ecat-data-redis = { path = "../ecat-data-redis" }
ecat-data-clickhouse = { path = "../ecat-data-clickhouse" }
```

### 3. लोड करें और उपयोग करें

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
    // YAML कॉन्फ़िगरेशन लोड करें
    let yaml = std::fs::read_to_string("databases.yaml")?;
    let cfg: AppConfig = serde_yaml::from_str(&yaml)?;

    // डेटाबेस क्लाइंट बनाएं — कोई हार्डकोडेड कनेक्शन जानकारी नहीं
    let db = SqlxClient::from_config(cfg.sql).await?;
    let cache = RedisCache::from_config(cfg.redis).await?;
    let ch = ClickhouseClient::from_config(cfg.clickhouse);

    // उपयोग
    let rows = db.query("SELECT id, name FROM users LIMIT 10").await?;
    cache.set("health", b"ok", std::time::Duration::from_secs(30)).await?;

    Ok(())
}
```

---

## पूर्ण कॉन्फ़िगरेशन संदर्भ

### टॉप-लेवल कॉन्फ़िगरेशन स्ट्रक्चर परिभाषित करें

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

### YAML पूर्ण उदाहरण

`config/databases.example.yaml` देखें।

---

## प्रत्येक बैकएंड Config फ़ील्ड त्वरित संदर्भ

### RDBMS — SqlxConfig

#### नेटिव पूल (PostgreSQL / MySQL / SQLite)

URL scheme से नेटिव ड्राइवर स्वतः चुना जाता है, किसी अतिरिक्त कॉन्फ़िगरेशन की ज़रूरत नहीं:

| scheme | ड्राइवर |
|---|---|
| `postgres://` / `postgresql://` | PostgreSQL |
| `mysql://` / `mariadb://` | MySQL |
| `sqlite:` | SQLite |

- **समय प्रकारों का नेटिव समर्थन**: पुराने sqlx `AnyPool` पथ में समय प्रकार समर्थित नहीं थे, स्वयं `CAST` करके टेक्स्ट बनाना पड़ता था; अब समय कॉलम सीधे पढ़े जा सकते हैं।
- scheme का **केस और आगे-पीछे का खाली स्थान दोनों सहन किए जाते हैं** (`"POSTGRES://…"` और `" postgres://…"` दोनों चलते हैं); अपरिचित scheme पर त्रुटि आती है और **कौन-सा scheme है यह नाम लेकर बताया जाता है**।
- `mssql://` **इस बैकएंड द्वारा प्रदान नहीं किया जाता** (`ecat-data-mssql` इस्तेमाल करें); यहाँ देने पर स्पष्ट रूप से अस्वीकार होगा।

#### कॉन्फ़िगरेशन उदाहरण

```yaml
sql:
  url: "postgres://host:5432/dbname"
  # username: "app_user"    # वैकल्पिक
  # password: "secret"      # वैकल्पिक
```

| फ़ील्ड | प्रकार | डिफ़ॉल्ट मान | स्पष्टीकरण |
|------|------|--------|------|
| `url` | `String` | — | sqlx कनेक्शन स्ट्रिंग, SQLite/PG/MySQL/TiDB का समर्थन |
| `username` | `Option<String>` | `None` | वैकल्पिक: URL में एम्बेडेड प्रमाणीकरण (password के साथ) |
| `password` | `Option<String>` | `None` | वैकल्पिक: URL में एम्बेडेड प्रमाणीकरण (username के साथ) |
| `max_connections` | `u32` | `10` | पूल में अधिकतम कनेक्शन |
| `min_connections` | `u32` | `0` | बनाए रखे जाने वाले न्यूनतम कनेक्शन; ≤ `max_connections` तक सीमित |
| `acquire_timeout_secs` | `u64` | `30` | कनेक्शन के इंतज़ार का समय; **`0` = तुरंत टाइमआउट** (कोई कनेक्शन खाली न हो तो विफल), जो `query_timeout_secs` के `0` = अक्षम के उलट है |
| `idle_timeout_secs` | `u64` | `600` | निष्क्रिय कनेक्शन की वापसी |
| `max_lifetime_secs` | `u64` | `1800` | कनेक्शन का अधिकतम जीवनकाल |
| `query_timeout_secs` | `u64` | `30` | प्रति क्वेरी टाइमआउट; **0 = अक्षम** |
| `slow_query_ms` | `u64` | `1000` | स्लो क्वेरी चेतावनी सीमा (मिलीसेकंड); सेट न हो = 1000, **0 = बंद** (केवल `tracing` feature) |
| `test_before_acquire` | `bool` | `false` | कनेक्शन देने से पहले ping करना है या नहीं |
| `session_init` | `string[]` | डायलेक्ट अनुसार | हर नए कनेक्शन के लिए सेशन इनिशियलाइज़ेशन कथन |

#### `session_init` (सेशन इनिशियलाइज़ेशन)

हर नया कनेक्शन बनने के बाद ये कथन क्रम से चलाए जाते हैं:

| डायलेक्ट | डिफ़ॉल्ट `session_init` |
|---|---|
| PostgreSQL | `SET TIME ZONE 'UTC'`, `SET application_name = 'ecat'` |
| MySQL | `SET time_zone = '+00:00'` |
| SQLite | सेशन की अवधारणा नहीं — डिफ़ॉल्ट रूप से खाली |

उद्देश्य यह है कि **डेटाबेस पक्ष सीधे UTC लौटाए**, जो फ़्रेमवर्क के इस नियम से मेल खाता है कि सभी समय एकरूप रूप से RFC3339 UTC में प्रस्तुत किए जाएँ।

- **स्पष्ट खाली ऐरे `[]` का अर्थ "जानबूझकर बंद"** है और यह डायलेक्ट डिफ़ॉल्ट को ओवरराइड करता है (`query_timeout_secs: 0` के "अक्षम" होने जैसा ही स्पष्ट-ओवरराइड नियम)।
- कोई भी कथन विफल → कनेक्शन बनना विफल, **चुपचाप कमज़ोर पड़ना नहीं**।

```yaml
sql:
  url: "mysql://host:3306/dbname"
  session_init:
    - "SET time_zone = '+00:00'"
    - "SET NAMES utf8mb4"
```

#### `warm_up()` — वॉर्म-अप

शुरू होते समय सक्रिय रूप से `min_connections` कनेक्शन बनाकर लौटा देता है, ताकि सेवा उठते ही तैयार स्थिति में हो:

```rust
let db = SqlxClient::from_config(cfg).await?;
db.warm_up().await?;   // शुरू होते समय एक बार कॉल करें
```

क्यों ज़रूरी है: sqlx `min_connections` को **बैकग्राउंड टास्क द्वारा असिंक्रोनस रूप से** बनाए रखता है, इसलिए `connect()` लौटते समय यह गारंटी नहीं कि पूल भर चुका है — शुरू के बाद अनुरोधों की पहली लहर बैकग्राउंड टास्क से होड़ करेगी।

#### अवलोकनीयता feature (`metrics` / `health` / `tracing`)

तीनों feature **डिफ़ॉल्ट रूप से बंद** हैं (ताकि axum — जो `ecat-metrics` / `ecat-health` की निर्भरता है — कोर निर्भरता ट्री में न आए); आवश्यकता अनुसार चालू करें। `ecat-data-mssql` भी वही तीनों देता है।

```toml
ecat-data-sqlx = { path = "../ecat-data-sqlx", features = ["metrics", "health", "tracing"] }
```

```rust
use std::sync::Arc;
use ecat_data_sqlx::{RdbmsHealthCheck, SqlxClient, SqlxConfig, register_pool_metrics};
use ecat_health::HealthRegistry;

let db = SqlxClient::from_config(cfg).await?;

// metrics: रजिस्टर करने पर /metrics एंडपॉइंट में चार मेट्रिक्स जुड़ जाते हैं —
// ecat_rdbms_pool_connections (gauge, state="idle"/"active" के साथ),
// ecat_rdbms_pool_timeouts_total, ecat_rdbms_query_timeout_total,
// ecat_rdbms_transactions_leaked_total (सभी counter, backend लेबल सहित)
register_pool_metrics("primary", db.pool());

// health: SELECT 1 कनेक्टिविटी प्रोब, /health के readyz में पंजीकृत
let registry = HealthRegistry::new()
    .with_check(RdbmsHealthCheck::new("sql", Arc::new(db)));
```

`tracing` feature क्वेरी के `slow_query_ms` से अधिक होने पर warn लिखता है (लगा समय + पहले 200 अक्षरों तक काटा गया SQL)। यह फ़ील्ड केवल यही feature पढ़ता है — बंद रहने पर भी पार्स होता है, पर प्रभाव नहीं डालता।

#### समय और दिनांक: एकरूप RFC3339 UTC

**sqlite का समय/दिनांक टेक्स्ट RFC3339 UTC में बदल दिया जाता है**:

- `2026-10-05 12:34:56` जैसा टेक्स्ट → `"2026-10-05T12:34:56Z"`
- `2026-10-05` जैसा टेक्स्ट → `"2026-10-05T00:00:00Z"` (UTC आधी रात का क्षण)

कारण: sqlite में टाइप सिस्टम नहीं है, दिनांक/समय जैसा टेक्स्ट स्वभावतः अस्पष्ट होता है, इसलिए फ़्रेमवर्क उसे एकरूप रूप से समय मानता है।

**दुष्प्रभाव**: वर्ज़न नंबर, बिज़नेस कोड जैसे "संयोग से दिनांक जैसे दिखने वाले" टेक्स्ट कॉलम भी बदल दिए जाएँगे। यह व्यवहार न चाहिए तो उस कॉलम को स्पष्ट रूप से गैर-दिनांक आकार में CAST करें, या कोई और टाइप इस्तेमाल करें।

PG / MySQL के वास्तविक `DATE` / `TIMESTAMP` कॉलम भी RFC3339 UTC स्ट्रिंग के रूप में प्रस्तुत होते हैं, और **शुद्ध दिनांक पर भी `T00:00:00Z` लगता है** — क्योंकि `"2026-10-05"` वैध RFC3339 नहीं है, और फ़्रेमवर्क भीतर एक ही प्रारूप रखता है ताकि ऊपरी परतें उसे आसानी से पार्स कर सकें।

### Redis — RedisConfig

```yaml
redis:
  url: "redis://host:6379"
  # password: "auth_token"  # वैकल्पिक
  # query_timeout_secs: 30     # वैकल्पिक: प्रति कमांड टाइमआउट, 0 = अक्षम
  # breaker: {}                # वैकल्पिक: breaker कॉन्फ़िग, छोड़ा गया = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
```

| फ़ील्ड | प्रकार | डिफ़ॉल्ट मान | स्पष्टीकरण |
|------|------|--------|------|
| `url` | `String` | — | Redis कनेक्शन URL |
| `password` | `Option<String>` | `None` | वैकल्पिक: Redis AUTH पासवर्ड |
| `query_timeout_secs` | `Option<u64>` | `30` | प्रति कमांड टाइमआउट (सेकंड में); **`0` = अक्षम** |
| `breaker` | `Option<BreakerConfig>` | रूढ़िवादी डिफ़ॉल्ट | breaker की सीमाएँ और विंडो; फ़ील्ड छोड़े जा सकते हैं, `breaker: {}` = सब डिफ़ॉल्ट |

**क्षमता सीमा**: यह client `MultiplexedConnection` इस्तेमाल करता है (एक TCP कनेक्शन सारी समवर्तीता संभालता है), **कनेक्शन पूल नहीं** — कैश लोड के लिए यह पूल से बेहतर है: कम कनेक्शन और कम राउंड-ट्रिप। कीमत यह है कि **स्टेटफ़ुल कमांड अनुक्रम इससे नहीं चल सकते**: `MULTI`/`EXEC` ट्रांज़ैक्शन, `WATCH`, `SUBSCRIBE` और ब्लॉकिंग कमांड को समर्पित कनेक्शन चाहिए — मल्टीप्लेक्सिंग में वे दूसरे कमांड के साथ जुड़ जाएँगे। ज़रूरत पड़े तो `redis::Client::get_async_connection()` से अलग कनेक्शन खोलें।

**टाइमआउट**: कॉन्फ़िग में `query_timeout_secs: 0` का अर्थ **अक्षम** है («0 सेकंड में टाइमआउट» नहीं); 30 सेकंड का डिफ़ॉल्ट तभी लागू होता है जब फ़ील्ड छोड़ी गई हो। लाइब्रेरी स्तर पर `run_with_timeout(kind, Some(Duration::ZERO), fut)` उल्टा है — वह **तुरंत टाइमआउट** है (tokio पहले भीतरी future को poll करता है, इसलिए पहले से तैयार future फिर भी सफल होता है)। दोनों «0» के अर्थ अलग हैं; लाइब्रेरी फ़ंक्शन सीधे कॉल करते समय कॉन्फ़िग का मान न दोहराएँ।

**breaker डिफ़ॉल्ट रूप से चालू है** — रूढ़िवादी सीमाओं (विफलता अनुपात 0.5, विंडो 30 सेकंड, half-open प्रोब 3, खुला 10 सेकंड) के साथ यह केवल लगातार विफलता पर खुलता है। **अभी कोई मास्टर स्विच नहीं है**: `BreakerConfig` में केवल ये चार सीमा-फ़ील्ड हैं, कोई `enabled` नहीं; `{"enabled": false}` लिखने पर केवल डिसेरियलाइज़ेशन त्रुटि मिलेगी। सचमुच बंद करना हो तो सीमाएँ पहुँच से बाहर रखें (जैसे `failure_ratio: 1.1`)।

### Memcached — MemcachedConfig

```yaml
memcached:
  # username: "memcache"    # वैकल्पिक: आरक्षित फ़ील्ड (वर्तमान में मेमोरी कार्यान्वयन)
  # password: "secret"      # वैकल्पिक: आरक्षित फ़ील्ड
  {}
```

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `username` | `Option<String>` | वैकल्पिक: आरक्षित फ़ील्ड |
| `password` | `Option<String>` | वैकल्पिक: आरक्षित फ़ील्ड |

वर्तमान में मेमोरी कार्यान्वयन है, प्रमाणीकरण फ़ील्ड आरक्षित हैं।

### ClickHouse — ClickhouseConfig

```yaml
clickhouse:
  base_url: "http://host:8123"
  database: "default"
  # username: "default"   # वैकल्पिक
  # password: "secret"    # वैकल्पिक
  # query_timeout_secs: 30  # वैकल्पिक: प्रति कॉल टाइमआउट, 0 = अक्षम
  # breaker: {}             # वैकल्पिक: breaker कॉन्फ़िग, छोड़ा गया = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
  # max_concurrency: 32     # वैकल्पिक: समवर्तीता सीमा (इसी crate का सीमाफोर)
```

| फ़ील्ड | प्रकार | डिफ़ॉल्ट मान | स्पष्टीकरण |
|------|------|--------|------|
| `base_url` | `String` | — | HTTP इंटरफ़ेस पता |
| `database` | `String` | `"default"` | डेटाबेस नाम |
| `username` | `Option<String>` | `None` | वैकल्पिक: HTTP Basic Auth उपयोगकर्ता नाम |
| `password` | `Option<String>` | `None` | वैकल्पिक: HTTP Basic Auth पासवर्ड |
| `query_timeout_secs` | `Option<u64>` | `30` | प्रति कॉल टाइमआउट (सेकंड में); **`0` = अक्षम** (Redis जैसा) |
| `breaker` | `Option<BreakerConfig>` | रूढ़िवादी डिफ़ॉल्ट | breaker की सीमाएँ और विंडो; इसमें भी `enabled` मास्टर स्विच नहीं |
| `max_concurrency` | `Option<usize>` | `32` | समवर्तीता सीमा; **इसी crate का सीमाफोर (semaphore)**, reqwest का नियंत्रक नहीं (reqwest में केवल `pool_max_idle_per_host` है — निष्क्रिय रखे जाने वाले कनेक्शन, कोई ऊपरी सीमा नहीं) |

**दो परतों वाला टाइमआउट**: `from_config` जो `reqwest::Client` बनाता है (`ecat-tls`) उसमें अपना 5 सेकंड कनेक्शन + 30 सेकंड कुल टाइमआउट है; `query_timeout_secs` **बाहरी** बजट है — दोनों सक्रिय हों तो **जो पहले पूरा हो वही लागू**; भीतरी परत पूरी होने पर त्रुटि `RdbmsError::Database` होती है और `ecat_outbound_timeouts_total` में **नहीं** गिनी जाती (बाहरी टाइमआउट का counter, `metrics` feature)। `new` / `with_auth` नंगे `reqwest::Client::new()` से चलते हैं, भीतरी टाइमआउट नहीं।

### QuestDB — QuestdbConfig

```yaml
questdb:
  base_url: "http://host:9000"
  # username: "admin"     # वैकल्पिक
  # password: "quest"     # वैकल्पिक
  # query_timeout_secs: 30   # वैकल्पिक: प्रति कॉल टाइमआउट, 0 = अक्षम
  # breaker: {}              # वैकल्पिक: breaker कॉन्फ़िग, छोड़ी गई = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
  # max_concurrency: 32      # वैकल्पिक: समवर्तीता सीमा (इसी crate का सीमाफोर)
```

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `base_url` | `String` | HTTP API पता |
| `username` | `Option<String>` | वैकल्पिक: HTTP Basic Auth उपयोगकर्ता नाम |
| `password` | `Option<String>` | वैकल्पिक: HTTP Basic Auth पासवर्ड |
| `query_timeout_secs` | `Option<u64>` | प्रति कॉल टाइमआउट (सेकंड में); छोड़ी गई = `30`, **`0` = अक्षम** |
| `breaker` | `Option<BreakerConfig>` | breaker की सीमाएँ और विंडो; `enabled` मास्टर स्विच नहीं |
| `max_concurrency` | `Option<usize>` | समवर्तीता सीमा (डिफ़ॉल्ट `32`); इसी crate का अपना सीमाफोर |

**त्रुटि का प्रकार**: QuestDB `SqlExecutor` (RDBMS परिवार) से जाता है —— टाइमआउट `RdbmsError::Timeout` है,
और breaker की अस्वीकृति `RdbmsError::Connection("circuit breaker is open")`; बाक़ी सभी HTTP बैकएंड
एकसमान `ecat_errors::Error` लौटाते हैं (`code = DeadlineExceeded` / `Unavailable`, `reason` = बैकएंड का नाम)।

### Elasticsearch — ElasticsearchConfig

```yaml
elasticsearch:
  base_url: "http://host:9200"
  # username: "elastic"   # वैकल्पिक
  # password: "secret"    # वैकल्पिक
  # query_timeout_secs: 30   # वैकल्पिक: प्रति कॉल टाइमआउट, 0 = अक्षम
  # breaker: {}              # वैकल्पिक: breaker कॉन्फ़िग, छोड़ी गई = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
  # max_concurrency: 32      # वैकल्पिक: समवर्तीता सीमा (इसी crate का सीमाफोर)
```

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `base_url` | `String` | REST API पता |
| `username` | `Option<String>` | वैकल्पिक: HTTP Basic Auth उपयोगकर्ता नाम |
| `password` | `Option<String>` | वैकल्पिक: HTTP Basic Auth पासवर्ड |
| `query_timeout_secs` | `Option<u64>` | प्रति कॉल टाइमआउट (सेकंड में); छोड़ी गई = `30`, **`0` = अक्षम** |
| `breaker` | `Option<BreakerConfig>` | breaker की सीमाएँ और विंडो; `enabled` मास्टर स्विच नहीं |
| `max_concurrency` | `Option<usize>` | समवर्तीता सीमा (डिफ़ॉल्ट `32`); इसी crate का अपना सीमाफोर |

### OpenSearch — OpenSearchConfig

```yaml
opensearch:
  base_url: "http://host:9200"
  # username: "admin"     # वैकल्पिक
  # password: "secret"    # वैकल्पिक
  # query_timeout_secs: 30   # वैकल्पिक: प्रति कॉल टाइमआउट, 0 = अक्षम
  # breaker: {}              # वैकल्पिक: breaker कॉन्फ़िग, छोड़ी गई = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
  # max_concurrency: 32      # वैकल्पिक: समवर्तीता सीमा (इसी crate का सीमाफोर)
```

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `base_url` | `String` | REST API पता |
| `username` | `Option<String>` | वैकल्पिक: HTTP Basic Auth उपयोगकर्ता नाम |
| `password` | `Option<String>` | वैकल्पिक: HTTP Basic Auth पासवर्ड |
| `query_timeout_secs` | `Option<u64>` | प्रति कॉल टाइमआउट (सेकंड में); छोड़ी गई = `30`, **`0` = अक्षम** |
| `breaker` | `Option<BreakerConfig>` | breaker की सीमाएँ और विंडो; `enabled` मास्टर स्विच नहीं |
| `max_concurrency` | `Option<usize>` | समवर्तीता सीमा (डिफ़ॉल्ट `32`); इसी crate का अपना सीमाफोर |

### InfluxDB — InfluxConfig

```yaml
influxdb:
  base_url: "http://host:8086"
  org: "myorg"
  bucket: "mybucket"
  token: "my-token"
  # query_timeout_secs: 30   # वैकल्पिक: प्रति कॉल टाइमआउट, 0 = अक्षम
  # breaker: {}              # वैकल्पिक: breaker कॉन्फ़िग, छोड़ी गई = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
  # max_concurrency: 32      # वैकल्पिक: समवर्तीता सीमा (इसी crate का सीमाफोर)
```

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `base_url` | `String` | InfluxDB 2.x API पता |
| `org` | `String` | संगठन नाम |
| `bucket` | `String` | बकेट नाम |
| `token` | `String` | प्रमाणीकरण टोकन |
| `query_timeout_secs` | `Option<u64>` | प्रति कॉल टाइमआउट (सेकंड में); छोड़ी गई = `30`, **`0` = अक्षम** |
| `breaker` | `Option<BreakerConfig>` | breaker की सीमाएँ और विंडो; `enabled` मास्टर स्विच नहीं |
| `max_concurrency` | `Option<usize>` | समवर्तीता सीमा (डिफ़ॉल्ट `32`); इसी crate का अपना सीमाफोर |

### Neo4j — Neo4jConfig

```yaml
neo4j:
  base_url: "http://host:7474"
  username: "neo4j"
  password: "secret"
  # query_timeout_secs: 30   # वैकल्पिक: प्रति कॉल टाइमआउट, 0 = अक्षम
  # breaker: {}              # वैकल्पिक: breaker कॉन्फ़िग, छोड़ी गई = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
  # max_concurrency: 32      # वैकल्पिक: समवर्तीता सीमा (इसी crate का सीमाफोर)
```

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `base_url` | `String` | REST API पता |
| `username` | `String` | उपयोगकर्ता नाम |
| `password` | `String` | पासवर्ड |
| `query_timeout_secs` | `Option<u64>` | प्रति कॉल टाइमआउट (सेकंड में); छोड़ी गई = `30`, **`0` = अक्षम** |
| `breaker` | `Option<BreakerConfig>` | breaker की सीमाएँ और विंडो; `enabled` मास्टर स्विच नहीं |
| `max_concurrency` | `Option<usize>` | समवर्तीता सीमा (डिफ़ॉल्ट `32`); इसी crate का अपना सीमाफोर |

### NebulaGraph — NebulaGraphConfig

```yaml
nebulagraph:
  base_url: "http://host:19669"
  space: "my_space"
  # username: "root"      # वैकल्पिक
  # password: "nebula"    # वैकल्पिक
  # query_timeout_secs: 30   # वैकल्पिक: प्रति कॉल टाइमआउट, 0 = अक्षम
  # breaker: {}              # वैकल्पिक: breaker कॉन्फ़िग, छोड़ी गई = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
  # max_concurrency: 32      # वैकल्पिक: समवर्तीता सीमा (इसी crate का सीमाफोर)
```

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `base_url` | `String` | API पता |
| `space` | `String` | ग्राफ स्पेस नाम |
| `username` | `Option<String>` | वैकल्पिक: HTTP Basic Auth उपयोगकर्ता नाम |
| `password` | `Option<String>` | वैकल्पिक: HTTP Basic Auth पासवर्ड |
| `query_timeout_secs` | `Option<u64>` | प्रति कॉल टाइमआउट (सेकंड में); छोड़ी गई = `30`, **`0` = अक्षम** |
| `breaker` | `Option<BreakerConfig>` | breaker की सीमाएँ और विंडो; `enabled` मास्टर स्विच नहीं |
| `max_concurrency` | `Option<usize>` | समवर्तीता सीमा (डिफ़ॉल्ट `32`); इसी crate का अपना सीमाफोर |

### ArangoDB — ArangoConfig

```yaml
arangodb:
  base_url: "http://host:8529"
  db: "mydb"
  username: "root"
  password: "secret"
  # query_timeout_secs: 30   # वैकल्पिक: प्रति कॉल टाइमआउट, 0 = अक्षम
  # breaker: {}              # वैकल्पिक: breaker कॉन्फ़िग, छोड़ी गई = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
  # max_concurrency: 32      # वैकल्पिक: समवर्तीता सीमा (इसी crate का सीमाफोर)
```

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `base_url` | `String` | API पता |
| `db` | `String` | डेटाबेस नाम |
| `username` | `String` | उपयोगकर्ता नाम |
| `password` | `String` | पासवर्ड |
| `query_timeout_secs` | `Option<u64>` | प्रति कॉल टाइमआउट (सेकंड में); छोड़ी गई = `30`, **`0` = अक्षम** |
| `breaker` | `Option<BreakerConfig>` | breaker की सीमाएँ और विंडो; `enabled` मास्टर स्विच नहीं |
| `max_concurrency` | `Option<usize>` | समवर्तीता सीमा (डिफ़ॉल्ट `32`); इसी crate का अपना सीमाफोर |

### IoTDB — IotdbConfig

```yaml
iotdb:
  base_url: "http://host:18080"
  username: "root"
  password: "root"
  # query_timeout_secs: 30   # वैकल्पिक: प्रति कॉल टाइमआउट, 0 = अक्षम
  # breaker: {}              # वैकल्पिक: breaker कॉन्फ़िग, छोड़ी गई = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
  # max_concurrency: 32      # वैकल्पिक: समवर्तीता सीमा (इसी crate का सीमाफोर)
```

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `base_url` | `String` | REST API पता |
| `username` | `String` | उपयोगकर्ता नाम |
| `password` | `String` | पासवर्ड |
| `query_timeout_secs` | `Option<u64>` | प्रति कॉल टाइमआउट (सेकंड में); छोड़ी गई = `30`, **`0` = अक्षम** |
| `breaker` | `Option<BreakerConfig>` | breaker की सीमाएँ और विंडो; `enabled` मास्टर स्विच नहीं |
| `max_concurrency` | `Option<usize>` | समवर्तीता सीमा (डिफ़ॉल्ट `32`); इसी crate का अपना सीमाफोर |

### TDengine — TdengineConfig

```yaml
tdengine:
  base_url: "http://host:6041"
  username: "root"
  password: "taosdata"
  # database: "my_db"        # वैकल्पिक: न दें तो REST पथ का डिफ़ॉल्ट डेटाबेस इस्तेमाल होता है
  # query_timeout_secs: 30   # वैकल्पिक: प्रति कॉल टाइमआउट, 0 = अक्षम
  # breaker: {}              # वैकल्पिक: breaker कॉन्फ़िग, छोड़ी गई = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
  # max_concurrency: 32      # वैकल्पिक: समवर्तीता सीमा (इसी crate का सीमाफोर)
```

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `base_url` | `String` | REST इंटरफ़ेस का पता (taosAdapter, डिफ़ॉल्ट पोर्ट 6041) |
| `username` | `String` | उपयोगकर्ता नाम |
| `password` | `String` | पासवर्ड |
| `database` | `Option<String>` | वैकल्पिक: डिफ़ॉल्ट डेटाबेस का नाम (REST पथ में जुड़ता है) |
| `query_timeout_secs` | `Option<u64>` | प्रति कॉल टाइमआउट (सेकंड में); छोड़ी गई = `30`, **`0` = अक्षम** |
| `breaker` | `Option<BreakerConfig>` | breaker की सीमाएँ और विंडो; `enabled` मास्टर स्विच नहीं |
| `max_concurrency` | `Option<usize>` | समवर्तीता सीमा (डिफ़ॉल्ट `32`); इसी crate का अपना सीमाफोर |

**पूरे कॉल के लिए एक बजट**: `write()` डेटा-बिंदुओं के एक बैच को कई HTTP अनुरोधों में बाँटता है, और `query_timeout_secs` **पूरे कॉल** (सभी हिस्सों) को कवर करता है, हर हिस्से का अलग बजट नहीं।

### MongoDB — MongoConfig

```yaml
mongodb:
  url: "mongodb://host:27017"
  database: "app"
  # max_pool_size: 10        # वैकल्पिक: कनेक्शन पूल की ऊपरी सीमा, छोड़ी गई = ड्राइवर डिफ़ॉल्ट (**10**)
  # min_pool_size: 0         # वैकल्पिक: कनेक्शन पूल की निचली सीमा (पृष्ठभूमि में बनाए रखे कनेक्शन)
  # query_timeout_secs: 30   # वैकल्पिक: प्रति कमांड टाइमआउट, 0 = अक्षम
  # breaker: {}              # वैकल्पिक: breaker कॉन्फ़िग, छोड़ी गई = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
```

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `url` | `String` | कनेक्शन URI (प्रमाणीकरण, रेप्लिका सेट और TLS विकल्प सब URI में ही रहते हैं) |
| `database` | `String` | डेटाबेस का नाम |
| `max_pool_size` | `Option<u32>` | कनेक्शन पूल की ऊपरी सीमा; छोड़ी गई = ड्राइवर डिफ़ॉल्ट **10** (`mongodb` 3.8.0 पर मापा गया, 100 नहीं) |
| `min_pool_size` | `Option<u32>` | कनेक्शन पूल की निचली सीमा; छोड़ी गई = ड्राइवर डिफ़ॉल्ट `0` |
| `query_timeout_secs` | `Option<u64>` | प्रति कमांड टाइमआउट (सेकंड में); छोड़ी गई = `30`, **`0` = अक्षम** |
| `breaker` | `Option<BreakerConfig>` | breaker की सीमाएँ और विंडो; `enabled` मास्टर स्विच नहीं |

**समवर्तीता का बैकप्रेशर ड्राइवर पूल से जाता है**: इस crate में `max_concurrency` **नहीं** है (और यह HTTP भी नहीं है) —— ड्राइवर अपना पूल लाता है; ज़्यादा समवर्तीता चाहिए तो `max_pool_size` स्पष्ट रूप से सेट करें।

### S3 / MinIO — S3Config

```yaml
s3:
  endpoint: "http://host:9000"
  region: "us-east-1"
  access_key: "minioadmin"
  secret_key: "minioadmin"
  # query_timeout_secs: 30   # वैकल्पिक: प्रति कॉल टाइमआउट, 0 = अक्षम
  # breaker: {}              # वैकल्पिक: breaker कॉन्फ़िग, छोड़ी गई = रूढ़िवादी डिफ़ॉल्ट (0.5 / 30s / खुला 10s)
  # max_concurrency: 32      # वैकल्पिक: समवर्तीता सीमा (इसी crate का सीमाफोर)
```

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `endpoint` | `String` | S3-संगत सेवा का पता (MinIO / स्वयं-होस्टेड गेटवे) |
| `region` | `String` | हस्ताक्षर के लिए क्षेत्र; MinIO को मान से फ़र्क़ नहीं पड़ता, `us-east-1` काफ़ी है |
| `access_key` | `String` | Access Key |
| `secret_key` | `String` | Secret Key |
| `query_timeout_secs` | `Option<u64>` | प्रति कॉल टाइमआउट (सेकंड में); छोड़ी गई = `30`, **`0` = अक्षम** |
| `breaker` | `Option<BreakerConfig>` | breaker की सीमाएँ और विंडो; `enabled` मास्टर स्विच नहीं |
| `max_concurrency` | `Option<usize>` | समवर्तीता सीमा (डिफ़ॉल्ट `32`); इसी crate का अपना सीमाफोर |

**पूरे कॉल के लिए एक बजट**: `list()` continuation token का पीछा करते हुए एक कॉल में कई GET भेजता है; टाइमआउट **पूरी पेजिंग** को कवर करता है।

> **आउटबाउंड रेज़िलिएंस की साझा बातें** (इस खंड के सभी HTTP बैकएंड): क्रम **परमिट → breaker → टाइमआउट** है; टाइमआउट त्रुटि `code = DeadlineExceeded`, breaker की अस्वीकृति `code = Unavailable` के साथ `message = "circuit breaker is open"`; `metrics` feature के साथ ये `ecat_outbound_timeouts_total{backend="<कॉन्फ़िग खंड का नाम>"}` में गिने जाते हैं —— लेबल **कॉन्फ़िग खंड का नाम** है (`"arangodb"` / `"mongodb"` / …), trait श्रेणी का नाम नहीं।

---

## प्रोग्रामेटिक निर्माण

### प्रमाणीकरण के बिना

```rust
let es = ElasticsearchClient::new("http://localhost:9200");
let ch = ClickhouseClient::new("http://localhost:8123", "default");
```

### प्रमाणीकरण के साथ

```rust
let es = ElasticsearchClient::with_auth("http://es:9200", "elastic", "secret");
let ch = ClickhouseClient::with_auth("http://ch:8123", "default", "admin", "pass");
let qdb = QuestdbClient::with_auth("http://qdb:9000", "admin", "quest");
let ng = NebulaGraphClient::with_auth("http://ng:19669", "space1", "root", "nebula");
```

---

---

## TLS प्रमाणपत्र कॉन्फ़िगरेशन

डेटा बैकएंड आम तौर पर वैकल्पिक TLS क्लाइंट प्रमाणीकरण (`tls` फ़ील्ड) का समर्थन करते हैं, पर **दो अपवाद** हैं: `ecat-data-sqlx` इस फ़ील्ड को समर्थित नहीं करता — सेट करने पर शुरुआत विफल हो जाएगी (इसका TLS URL पैरामीटर से जाता है); `ecat-data-memcached` का यह फ़ील्ड **चुपचाप निष्प्रभावी** है — घोषित है, पर crate में कहीं पढ़ा नहीं जाता।

### कॉन्फ़िगरेशन उदाहरण

```yaml
clickhouse:
  base_url: "https://ch.internal:8443"
  tls:
    ca_cert: "/etc/ecat/ca.pem"
    client_cert: "/etc/ecat/client.pem"
    client_key: "/etc/ecat/client-key.pem"
    # skip_verify: true  # केवल परीक्षण वातावरण
```

### प्रमाणपत्र स्वतः जनरेशन (ecat-tls)

```rust
use ecat_tls::{generate_ca, generate_server_cert, generate_client_cert};

// 1. CA जनरेट करें
let ca = generate_ca("MyOrg")?;
std::fs::write("ca.pem", &ca.cert_pem)?;
std::fs::write("ca-key.pem", &ca.key_pem)?;

// 2. सर्वर प्रमाणपत्र जनरेट करें
let srv = generate_server_cert("db.example.com")?;
std::fs::write("server.pem", &srv.cert_pem)?;
std::fs::write("server-key.pem", &srv.key_pem)?;

// 3. क्लाइंट प्रमाणपत्र जनरेट करें (mTLS)
let client = generate_client_cert("myapp")?;
std::fs::write("client.pem", &client.cert_pem)?;
std::fs::write("client-key.pem", &client.key_pem)?;
```

### मैन्युअल जनरेशन (OpenSSL)

```bash
# CA
openssl req -x509 -newkey rsa:4096 -keyout ca-key.pem -out ca.pem -days 3650 -nodes

# सर्वर प्रमाणपत्र
openssl req -new -newkey rsa:4096 -keyout server-key.pem -out server.csr -nodes -subj "/CN=db.example.com"
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -out server.pem -days 365

# क्लाइंट प्रमाणपत्र (mTLS)
openssl req -new -newkey rsa:4096 -keyout client-key.pem -out client.csr -nodes -subj "/CN=myapp"
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca-key.pem -out client.pem -days 365
```

### TLS फ़ील्ड स्पष्टीकरण

| फ़ील्ड | प्रकार | स्पष्टीकरण |
|------|------|------|
| `ca_cert` | `Option<String>` | CA प्रमाणपत्र PEM पथ (सर्वर सत्यापन) |
| `client_cert` | `Option<String>` | क्लाइंट प्रमाणपत्र PEM पथ (mTLS) |
| `client_key` | `Option<String>` | क्लाइंट निजी कुंजी PEM पथ (mTLS) |
| `skip_verify` | `Option<bool>` | प्रमाणपत्र सत्यापन छोड़ें (केवल परीक्षण) |

---

## उन्नत उपयोग

### पर्यावरण चर ओवरराइड

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

### ecat-config फ्रेमवर्क के साथ संयोजन

```rust
use ecat_config::{Config, FileSource};

let mut app_config = Config::new();
app_config.load(&FileSource::new("databases.yaml")).await?;

let redis_cfg: RedisConfig = serde_json::from_value(
    app_config.get::<serde_json::Value>("redis").unwrap()
)?;
let cache = RedisCache::from_config(redis_cfg).await?;
```

### आवश्यकता अनुसार कॉन्फ़िगरेशन

अनुपयोगी डेटाबेस YAML में छोड़ दें, Rust स्ट्रक्चर में `Option` से चिह्नित करें:

```rust
#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: Option<RedisConfig>,
    clickhouse: Option<ClickhouseConfig>,
}
```

---

## संबंधित दस्तावेज़

- [ऑडिट रिपोर्ट r5](audit-report-2026-08-01-r5.md)
- [TLS प्रमाणपत्र प्रमाणीकरण ट्यूटोरियल](tls-certificate-tutorial.md)
- [उदाहरण कॉन्फ़िगरेशन फ़ाइल](../../../config/databases.example.yaml)
