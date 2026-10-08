# برنامج تعليمي لإعداد قاعدة البيانات

**الإصدار:** 2.4.2 · **التاريخ:** 2026-08-01

تدعم خلفيات البيانات الأربعة عشر في e-cat تحميل معلومات الاتصال من ملفات الإعدادات، دون الحاجة إلى ترميزها في الكود. `username` / `password` حقلان اختياريان، ويُتخطى التحقق عند حذفهما.

---

## البدء السريع

### ١. إنشاء ملف الإعدادات

انسخ القالب النموذجي وعدّله حسب بيئتك الفعلية:

```bash
cp config/databases.example.yaml databases.yaml
```

عدّل `databases.yaml` وأدخل معلومات الاتصال الحقيقية:

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

### ٢. إضافة التبعيات

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
ecat-data-sqlx = { path = "../ecat-data-sqlx" }
ecat-data-redis = { path = "../ecat-data-redis" }
ecat-data-clickhouse = { path = "../ecat-data-clickhouse" }
```

### ٣. التحميل والاستخدام

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
    // تحميل إعدادات YAML
    let yaml = std::fs::read_to_string("databases.yaml")?;
    let cfg: AppConfig = serde_yaml::from_str(&yaml)?;

    // إنشاء عملاء قواعد البيانات — بدون ترميز معلومات الاتصال في الكود
    let db = SqlxClient::from_config(cfg.sql).await?;
    let cache = RedisCache::from_config(cfg.redis).await?;
    let ch = ClickhouseClient::from_config(cfg.clickhouse);

    // الاستخدام
    let rows = db.query("SELECT id, name FROM users LIMIT 10").await?;
    cache.set("health", b"ok", std::time::Duration::from_secs(30)).await?;

    Ok(())
}
```

---

## مرجع الإعدادات الكامل

### تعريف بنية الإعدادات ذات المستوى الأعلى

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

### مثال YAML كامل

انظر `config/databases.example.yaml`.

---

## مرجع سريع لحقول Config لكل خلفية

### RDBMS — SqlxConfig

#### المجمعات الأصلية (PostgreSQL / MySQL / SQLite)

يُختار السائق الأصلي تلقائيًا حسب scheme في الرابط، دون أي إعداد إضافي:

| scheme | السائق |
|---|---|
| `postgres://` / `postgresql://` | PostgreSQL |
| `mysql://` / `mariadb://` | MySQL |
| `sqlite:` | SQLite |

- **دعم أصلي لأنواع الوقت**: المسار القديم عبر `AnyPool` من sqlx لم يكن يدعم أنواع الوقت، وكان لا بد من `CAST` إلى نص يدويًا؛ الآن تُقرأ أعمدة الوقت مباشرة.
- **حالة الأحرف والمسافات في بداية scheme ونهايته كلاهما مقبول** (`"POSTGRES://…"` و`" postgres://…"` يعملان)؛ أي scheme غير معروف يُنتج خطأ **يسمّيه بالاسم**.
- `mssql://` **لا توفّره هذه الخلفية** (استخدم `ecat-data-mssql`)؛ تمريره هنا يُرفض صراحةً.

#### مثال الإعداد

```yaml
sql:
  url: "postgres://host:5432/dbname"
  # username: "app_user"    # اختياري
  # password: "secret"      # اختياري
```

| الحقل | النوع | القيمة الافتراضية | الوصف |
|------|------|--------|------|
| `url` | `String` | — | سلسلة اتصال sqlx، تدعم SQLite/PG/MySQL/TiDB |
| `username` | `Option<String>` | `None` | اختياري: مصادقة مضمّنة في URL (مع password) |
| `password` | `Option<String>` | `None` | اختياري: مصادقة مضمّنة في URL (مع username) |
| `max_connections` | `u32` | `10` | الحد الأقصى لعدد الاتصالات في المجمّع |
| `min_connections` | `u32` | `0` | الحد الأدنى للاتصالات المحفوظة؛ يُقيَّد إلى ≤ `max_connections` |
| `acquire_timeout_secs` | `u64` | `30` | مهلة انتظار اتصال؛ **`0` = مهلة فورية** (يفشل إن لم يتوفر اتصال حر)، عكس `0` = تعطيل في `query_timeout_secs` |
| `idle_timeout_secs` | `u64` | `600` | تحرير الاتصالات الخاملة |
| `max_lifetime_secs` | `u64` | `1800` | أقصى عمر للاتصال |
| `query_timeout_secs` | `u64` | `30` | مهلة الاستعلام الواحد؛ **0 = تعطيل** |
| `slow_query_ms` | `u64` | `1000` | عتبة تحذير الاستعلام البطيء (مللي ثانية)؛ غير مُعدّ = 1000، **0 = لا تحذير** (ميزة `tracing` فقط) |
| `test_before_acquire` | `bool` | `false` | إجراء ping قبل تسليم الاتصال |
| `session_init` | `string[]` | حسب اللهجة | عبارات تهيئة الجلسة لكل اتصال جديد |

#### `session_init` (تهيئة الجلسة)

بعد إنشاء كل اتصال جديد تُنفَّذ هذه العبارات بالترتيب:

| اللهجة | الافتراضي `session_init` |
|---|---|
| PostgreSQL | `SET TIME ZONE 'UTC'`, `SET application_name = 'ecat'` |
| MySQL | `SET time_zone = '+00:00'` |
| SQLite | لا مفهوم للجلسة — فارغ افتراضيًا |

الهدف أن **تعيد قاعدة البيانات UTC مباشرة**، بما يتوافق مع عُرف الإطار بأن تُقدَّم كل الأوقات بشكل موحّد بصيغة RFC3339 UTC.

- **المصفوفة الفارغة الصريحة `[]` تعني "إيقاف متعمّد"** وتتجاوز الافتراضي الخاص باللهجة (نفس عُرف التجاوز الصريح الذي يمثله `query_timeout_secs: 0` أي التعطيل).
- إذا فشلت أي عبارة → يفشل إنشاء الاتصال، **دون تراجع صامت**.

```yaml
sql:
  url: "mysql://host:3306/dbname"
  session_init:
    - "SET time_zone = '+00:00'"
    - "SET NAMES utf8mb4"
```

#### `warm_up()` — التسخين

ينشئ عند بدء التشغيل `min_connections` اتصالًا بشكل فعّال ثم يعيدها، ليكون الخدمة في حالة جهوزية فور إقلاعها:

```rust
let db = SqlxClient::from_config(cfg).await?;
db.warm_up().await?;   // يُستدعى مرة واحدة عند بدء التشغيل
```

لماذا نحتاجها: يحافظ sqlx على `min_connections` **بشكل غير متزامن عبر مهمة خلفية**، لذا لا يضمن رجوع `connect()` أن المجمّع قد امتلأ — أول موجة من الطلبات ستتسابق مع المهمة الخلفية.

#### ميزات المراقبة (`metrics` / `health` / `tracing`)

الميزات الثلاث **معطّلة افتراضيًا** (حتى لا يدخل axum — وهو تبعية `ecat-metrics` / `ecat-health` — إلى شجرة تبعيات النواة)؛ فعّلها حسب الحاجة. ويوفّر `ecat-data-mssql` الثلاث نفسها.

```toml
ecat-data-sqlx = { path = "../ecat-data-sqlx", features = ["metrics", "health", "tracing"] }
```

```rust
use std::sync::Arc;
use ecat_data_sqlx::{RdbmsHealthCheck, SqlxClient, SqlxConfig, register_pool_metrics};
use ecat_health::HealthRegistry;

let db = SqlxClient::from_config(cfg).await?;

// metrics: بعد التسجيل يضيف الطرف /metrics أربع مقاييس —
// ecat_rdbms_pool_connections (gauge، مع state="idle"/"active")،
// ecat_rdbms_pool_timeouts_total و ecat_rdbms_query_timeout_total
// و ecat_rdbms_transactions_leaked_total (كلها counter، بعلامة backend)
register_pool_metrics("primary", db.pool());

// health: فحص اتصال SELECT 1، يُسجَّل في readyz لدى /health
let registry = HealthRegistry::new()
    .with_check(RdbmsHealthCheck::new("sql", Arc::new(db)));
```

تُصدر ميزة `tracing` تحذيرًا عندما يتجاوز الاستعلام `slow_query_ms` (الزمن المستغرق + SQL مقتطعًا إلى أول 200 حرف). هذا الحقل لا تقرأه سوى هذه الميزة — وعند تعطيلها يبقى يُحلَّل لكنه لا يؤثر.

#### الوقت والتاريخ: RFC3339 UTC بشكل موحّد

**يُعاد كتابة نص الوقت/التاريخ في sqlite إلى RFC3339 UTC**:

- نص بالشكل `2026-10-05 12:34:56` → `"2026-10-05T12:34:56Z"`
- نص بالشكل `2026-10-05` → `"2026-10-05T00:00:00Z"` (لحظة منتصف الليل UTC)

السبب: لا يمتلك sqlite نظام أنواع، والنص الذي يشبه التاريخ أو الوقت غامض بطبيعته، لذا يعالجه الإطار بشكل موحّد كوقت.

**أثر جانبي**: حتى أعمدة النصوص التي تشبه التاريخ بالمصادفة — أرقام الإصدارات وأكواد الأعمال — يُعاد كتابتها أيضًا. إن لم ترغب بهذا السلوك، أجرِ `CAST` صريحًا للعمود إلى شكل غير تاريخي، أو استخدم نوعًا آخر.

أعمدة `DATE` / `TIMESTAMP` الحقيقية في PG / MySQL تُقدَّم أيضًا كنصوص RFC3339 UTC، و**التاريخ المجرد يحمل كذلك `T00:00:00Z`** — لأن `"2026-10-05"` ليس RFC3339 صالحًا، والإطار يستخدم داخليًا صيغة واحدة موحّدة لتسهيل تحليلها في الطبقات الأعلى.

### Redis — RedisConfig

```yaml
redis:
  url: "redis://host:6379"
  # password: "auth_token"  # اختياري
  # query_timeout_secs: 30     # اختياري: مهلة لكل أمر، 0 = تعطيل
  # breaker: {}                # اختياري: إعداد breaker، عند الحذف = الافتراضي المتحفظ (0.5 / 30s / مفتوح 10s)
```

| الحقل | النوع | القيمة الافتراضية | الوصف |
|------|------|--------|------|
| `url` | `String` | — | عنوان اتصال Redis |
| `password` | `Option<String>` | `None` | اختياري: كلمة مرور Redis AUTH |
| `query_timeout_secs` | `Option<u64>` | `30` | مهلة الأمر الواحد بالثواني؛ **`0` = تعطيل** |
| `breaker` | `Option<BreakerConfig>` | افتراضي متحفظ | عتبات breaker ونافذته؛ يمكن حذف الحقول، `breaker: {}` يعني كل الافتراضيات |

**حدود القدرات**: يستخدم هذا client اتصال `MultiplexedConnection` (اتصال TCP واحد يخدم كل التزامن)، **وليس مجمّع اتصالات** — لحمولة التخزين المؤقت هذا أفضل من المجمّع: اتصالات وذهاب وإياب أقل. الثمن أن **تسلسلات الأوامر ذات الحالة لا يمكن استخدامه لها**: معاملات `MULTI`/`EXEC` و`WATCH` و`SUBSCRIBE` والأوامر الحاجبة تحتاج اتصالًا حصريًا؛ تحت تعدد الإرسال ستتشابك مع أوامر أخرى. عند الحاجة افتح اتصالًا مخصصًا عبر `redis::Client::get_async_connection()`.

**المهلات**: `query_timeout_secs: 0` في الإعداد يعني **تعطيلًا** (وليس «مهلة بعد 0 ثانية»)؛ الافتراضي 30 ثانية يُطبَّق فقط عند حذف الحقل. على مستوى المكتبة، `run_with_timeout(kind, Some(Duration::ZERO), fut)` هو العكس — ذاك **مهلة فورية** (tokio يستطلع الـfuture الداخلي أولًا، لذا الـfuture الجاهز أصلًا ينجح رغم ذلك). الـ«0»ان مختلفان في المعنى؛ لا تنسخ قيمة الإعداد عند استدعاء دالة المكتبة مباشرة.

**الـbreaker مفعّل افتراضيًا** — بعتبات متحفظة (نسبة الفشل 0.5، النافذة 30 ثانية، فحوص half-open 3، مفتوح 10 ثوانٍ) لا يُفتح إلا عند الفشل المستمر. **لا يوجد حاليًا مفتاح رئيسي**: `BreakerConfig` فيه فقط حقول العتبات الأربعة، بلا `enabled`؛ كتابة `{"enabled": false}` لا تعطي إلا خطأ فك تسلسل. لتعطيله فعلًا يجب دفع العتبات إلى ما لا يمكن بلوغه (مثل `failure_ratio: 1.1`).

### Memcached — MemcachedConfig

```yaml
memcached:
  # username: "memcache"    # اختياري: حقل محجوز (تنفيذ في الذاكرة حاليًا)
  # password: "secret"      # اختياري: حقل محجوز
  {}
```

| الحقل | النوع | الوصف |
|------|------|------|
| `username` | `Option<String>` | اختياري: حقل محجوز |
| `password` | `Option<String>` | اختياري: حقل محجوز |

حاليًا تنفيذ في الذاكرة، وحقول المصادقة محجوزة للاستخدام المستقبلي.

### ClickHouse — ClickhouseConfig

```yaml
clickhouse:
  base_url: "http://host:8123"
  database: "default"
  # username: "default"   # اختياري
  # password: "secret"    # اختياري
  # query_timeout_secs: 30  # اختياري: مهلة لكل استدعاء، 0 = تعطيل
  # breaker: {}             # اختياري: إعداد breaker، عند الحذف = الافتراضي المتحفظ (0.5 / 30s / مفتوح 10s)
  # max_concurrency: 32     # اختياري: حد التزامن (سيمافور هذا الـcrate)
```

| الحقل | النوع | القيمة الافتراضية | الوصف |
|------|------|--------|------|
| `base_url` | `String` | — | عنوان واجهة HTTP |
| `database` | `String` | `"default"` | اسم قاعدة البيانات |
| `username` | `Option<String>` | `None` | اختياري: اسم مستخدم HTTP Basic Auth |
| `password` | `Option<String>` | `None` | اختياري: كلمة مرور HTTP Basic Auth |
| `query_timeout_secs` | `Option<u64>` | `30` | مهلة الاستدعاء الواحد بالثواني؛ **`0` = تعطيل** (كما في Redis) |
| `breaker` | `Option<BreakerConfig>` | افتراضي متحفظ | عتبات breaker ونافذته؛ ولا يوجد هنا أيضًا مفتاح رئيسي `enabled` |
| `max_concurrency` | `Option<usize>` | `32` | حد التزامن؛ **سيمافور هذا الـcrate نفسه**، وليس مقبضًا في reqwest (لدى reqwest فقط `pool_max_idle_per_host` — عدد الاتصالات الخاملة المحفوظة، وليس حدًا أقصى) |

**طبقتان من المهلة**: عميل `reqwest::Client` الذي يبنيه `from_config` (`ecat-tls`) يحمل مهلة اتصال خاصة به 5 ثوانٍ + 30 ثانية إجمالًا؛ و`query_timeout_secs` هو الميزانية **الخارجية** — عند وجود الطبقتين **يفوز من ينتهي أولًا**؛ وإذا انتهت الداخلية فالخطأ `RdbmsError::Database` و**لا يُحتسب** في `ecat_outbound_timeouts_total` (عدّاد المهل الخارجية، ميزة `metrics`). أما `new` / `with_auth` فيستخدمان `reqwest::Client::new()` مجرّدًا بلا مهلة داخلية.

### QuestDB — QuestdbConfig

```yaml
questdb:
  base_url: "http://host:9000"
  # username: "admin"     # اختياري
  # password: "quest"     # اختياري
  # query_timeout_secs: 30   # اختياري: مهلة الاستدعاء الواحد، 0 = تعطيل
  # breaker: {}              # اختياري: إعداد الـbreaker، الحذف = افتراضي متحفظ (0.5 / 30s / فتح 10s)
  # max_concurrency: 32      # اختياري: حد التزامن (سيمافور هذا الـcrate)
```

| الحقل | النوع | الوصف |
|------|------|------|
| `base_url` | `String` | عنوان واجهة HTTP API |
| `username` | `Option<String>` | اختياري: اسم مستخدم HTTP Basic Auth |
| `password` | `Option<String>` | اختياري: كلمة مرور HTTP Basic Auth |
| `query_timeout_secs` | `Option<u64>` | مهلة الاستدعاء الواحد بالثواني؛ الحذف = `30`، **`0` = تعطيل** |
| `breaker` | `Option<BreakerConfig>` | عتبات الـbreaker ونافذته؛ لا مفتاح رئيسي `enabled` |
| `max_concurrency` | `Option<usize>` | حد التزامن (الافتراضي `32`)؛ سيمافور هذا الـcrate نفسه |

**نوع الخطأ**: QuestDB يمر عبر `SqlExecutor` (عائلة RDBMS) —— المهلة هي `RdbmsError::Timeout`،
ورفض الـbreaker هو `RdbmsError::Connection("circuit breaker is open")`؛ أما بقية الواجهات الخلفية HTTP فتوحّد
`ecat_errors::Error` (`code = DeadlineExceeded` / `Unavailable`، و`reason` = اسم الواجهة الخلفية).

### Elasticsearch — ElasticsearchConfig

```yaml
elasticsearch:
  base_url: "http://host:9200"
  # username: "elastic"   # اختياري
  # password: "secret"    # اختياري
  # query_timeout_secs: 30   # اختياري: مهلة الاستدعاء الواحد، 0 = تعطيل
  # breaker: {}              # اختياري: إعداد الـbreaker، الحذف = افتراضي متحفظ (0.5 / 30s / فتح 10s)
  # max_concurrency: 32      # اختياري: حد التزامن (سيمافور هذا الـcrate)
```

| الحقل | النوع | الوصف |
|------|------|------|
| `base_url` | `String` | عنوان REST API |
| `username` | `Option<String>` | اختياري: اسم مستخدم HTTP Basic Auth |
| `password` | `Option<String>` | اختياري: كلمة مرور HTTP Basic Auth |
| `query_timeout_secs` | `Option<u64>` | مهلة الاستدعاء الواحد بالثواني؛ الحذف = `30`، **`0` = تعطيل** |
| `breaker` | `Option<BreakerConfig>` | عتبات الـbreaker ونافذته؛ لا مفتاح رئيسي `enabled` |
| `max_concurrency` | `Option<usize>` | حد التزامن (الافتراضي `32`)؛ سيمافور هذا الـcrate نفسه |

### OpenSearch — OpenSearchConfig

```yaml
opensearch:
  base_url: "http://host:9200"
  # username: "admin"     # اختياري
  # password: "secret"    # اختياري
  # query_timeout_secs: 30   # اختياري: مهلة الاستدعاء الواحد، 0 = تعطيل
  # breaker: {}              # اختياري: إعداد الـbreaker، الحذف = افتراضي متحفظ (0.5 / 30s / فتح 10s)
  # max_concurrency: 32      # اختياري: حد التزامن (سيمافور هذا الـcrate)
```

| الحقل | النوع | الوصف |
|------|------|------|
| `base_url` | `String` | عنوان REST API |
| `username` | `Option<String>` | اختياري: اسم مستخدم HTTP Basic Auth |
| `password` | `Option<String>` | اختياري: كلمة مرور HTTP Basic Auth |
| `query_timeout_secs` | `Option<u64>` | مهلة الاستدعاء الواحد بالثواني؛ الحذف = `30`، **`0` = تعطيل** |
| `breaker` | `Option<BreakerConfig>` | عتبات الـbreaker ونافذته؛ لا مفتاح رئيسي `enabled` |
| `max_concurrency` | `Option<usize>` | حد التزامن (الافتراضي `32`)؛ سيمافور هذا الـcrate نفسه |

### InfluxDB — InfluxConfig

```yaml
influxdb:
  base_url: "http://host:8086"
  org: "myorg"
  bucket: "mybucket"
  token: "my-token"
  # query_timeout_secs: 30   # اختياري: مهلة الاستدعاء الواحد، 0 = تعطيل
  # breaker: {}              # اختياري: إعداد الـbreaker، الحذف = افتراضي متحفظ (0.5 / 30s / فتح 10s)
  # max_concurrency: 32      # اختياري: حد التزامن (سيمافور هذا الـcrate)
```

| الحقل | النوع | الوصف |
|------|------|------|
| `base_url` | `String` | عنوان واجهة InfluxDB 2.x API |
| `org` | `String` | اسم المؤسسة |
| `bucket` | `String` | اسم الوعاء |
| `token` | `String` | رمز المصادقة |
| `query_timeout_secs` | `Option<u64>` | مهلة الاستدعاء الواحد بالثواني؛ الحذف = `30`، **`0` = تعطيل** |
| `breaker` | `Option<BreakerConfig>` | عتبات الـbreaker ونافذته؛ لا مفتاح رئيسي `enabled` |
| `max_concurrency` | `Option<usize>` | حد التزامن (الافتراضي `32`)؛ سيمافور هذا الـcrate نفسه |

### Neo4j — Neo4jConfig

```yaml
neo4j:
  base_url: "http://host:7474"
  username: "neo4j"
  password: "secret"
  # query_timeout_secs: 30   # اختياري: مهلة الاستدعاء الواحد، 0 = تعطيل
  # breaker: {}              # اختياري: إعداد الـbreaker، الحذف = افتراضي متحفظ (0.5 / 30s / فتح 10s)
  # max_concurrency: 32      # اختياري: حد التزامن (سيمافور هذا الـcrate)
```

| الحقل | النوع | الوصف |
|------|------|------|
| `base_url` | `String` | عنوان REST API |
| `username` | `String` | اسم المستخدم |
| `password` | `String` | كلمة المرور |
| `query_timeout_secs` | `Option<u64>` | مهلة الاستدعاء الواحد بالثواني؛ الحذف = `30`، **`0` = تعطيل** |
| `breaker` | `Option<BreakerConfig>` | عتبات الـbreaker ونافذته؛ لا مفتاح رئيسي `enabled` |
| `max_concurrency` | `Option<usize>` | حد التزامن (الافتراضي `32`)؛ سيمافور هذا الـcrate نفسه |

### NebulaGraph — NebulaGraphConfig

```yaml
nebulagraph:
  base_url: "http://host:19669"
  space: "my_space"
  # username: "root"      # اختياري
  # password: "nebula"    # اختياري
  # query_timeout_secs: 30   # اختياري: مهلة الاستدعاء الواحد، 0 = تعطيل
  # breaker: {}              # اختياري: إعداد الـbreaker، الحذف = افتراضي متحفظ (0.5 / 30s / فتح 10s)
  # max_concurrency: 32      # اختياري: حد التزامن (سيمافور هذا الـcrate)
```

| الحقل | النوع | الوصف |
|------|------|------|
| `base_url` | `String` | عنوان API |
| `space` | `String` | اسم مساحة الرسم البياني |
| `username` | `Option<String>` | اختياري: اسم مستخدم HTTP Basic Auth |
| `password` | `Option<String>` | اختياري: كلمة مرور HTTP Basic Auth |
| `query_timeout_secs` | `Option<u64>` | مهلة الاستدعاء الواحد بالثواني؛ الحذف = `30`، **`0` = تعطيل** |
| `breaker` | `Option<BreakerConfig>` | عتبات الـbreaker ونافذته؛ لا مفتاح رئيسي `enabled` |
| `max_concurrency` | `Option<usize>` | حد التزامن (الافتراضي `32`)؛ سيمافور هذا الـcrate نفسه |

### ArangoDB — ArangoConfig

```yaml
arangodb:
  base_url: "http://host:8529"
  db: "mydb"
  username: "root"
  password: "secret"
  # query_timeout_secs: 30   # اختياري: مهلة الاستدعاء الواحد، 0 = تعطيل
  # breaker: {}              # اختياري: إعداد الـbreaker، الحذف = افتراضي متحفظ (0.5 / 30s / فتح 10s)
  # max_concurrency: 32      # اختياري: حد التزامن (سيمافور هذا الـcrate)
```

| الحقل | النوع | الوصف |
|------|------|------|
| `base_url` | `String` | عنوان API |
| `db` | `String` | اسم قاعدة البيانات |
| `username` | `String` | اسم المستخدم |
| `password` | `String` | كلمة المرور |
| `query_timeout_secs` | `Option<u64>` | مهلة الاستدعاء الواحد بالثواني؛ الحذف = `30`، **`0` = تعطيل** |
| `breaker` | `Option<BreakerConfig>` | عتبات الـbreaker ونافذته؛ لا مفتاح رئيسي `enabled` |
| `max_concurrency` | `Option<usize>` | حد التزامن (الافتراضي `32`)؛ سيمافور هذا الـcrate نفسه |

### IoTDB — IotdbConfig

```yaml
iotdb:
  base_url: "http://host:18080"
  username: "root"
  password: "root"
  # query_timeout_secs: 30   # اختياري: مهلة الاستدعاء الواحد، 0 = تعطيل
  # breaker: {}              # اختياري: إعداد الـbreaker، الحذف = افتراضي متحفظ (0.5 / 30s / فتح 10s)
  # max_concurrency: 32      # اختياري: حد التزامن (سيمافور هذا الـcrate)
```

| الحقل | النوع | الوصف |
|------|------|------|
| `base_url` | `String` | عنوان REST API |
| `username` | `String` | اسم المستخدم |
| `password` | `String` | كلمة المرور |
| `query_timeout_secs` | `Option<u64>` | مهلة الاستدعاء الواحد بالثواني؛ الحذف = `30`، **`0` = تعطيل** |
| `breaker` | `Option<BreakerConfig>` | عتبات الـbreaker ونافذته؛ لا مفتاح رئيسي `enabled` |
| `max_concurrency` | `Option<usize>` | حد التزامن (الافتراضي `32`)؛ سيمافور هذا الـcrate نفسه |

### TDengine — TdengineConfig

```yaml
tdengine:
  base_url: "http://host:6041"
  username: "root"
  password: "taosdata"
  # database: "my_db"        # اختياري: إن لم يُحدَّد تُستخدم القاعدة الافتراضية في مسار REST
  # query_timeout_secs: 30   # اختياري: مهلة الاستدعاء الواحد، 0 = تعطيل
  # breaker: {}              # اختياري: إعداد الـbreaker، الحذف = افتراضي متحفظ (0.5 / 30s / فتح 10s)
  # max_concurrency: 32      # اختياري: حد التزامن (سيمافور هذا الـcrate)
```

| الحقل | النوع | الوصف |
|------|------|------|
| `base_url` | `String` | عنوان واجهة REST (taosAdapter، المنفذ الافتراضي 6041) |
| `username` | `String` | اسم المستخدم |
| `password` | `String` | كلمة المرور |
| `database` | `Option<String>` | اختياري: اسم القاعدة الافتراضية (يُضاف إلى مسار REST) |
| `query_timeout_secs` | `Option<u64>` | مهلة الاستدعاء الواحد بالثواني؛ الحذف = `30`، **`0` = تعطيل** |
| `breaker` | `Option<BreakerConfig>` | عتبات الـbreaker ونافذته؛ لا مفتاح رئيسي `enabled` |
| `max_concurrency` | `Option<usize>` | حد التزامن (الافتراضي `32`)؛ سيمافور هذا الـcrate نفسه |

**ميزانية واحدة للاستدعاء كله**: يقسّم `write()` دفعة نقاط البيانات إلى عدة طلبات HTTP، و`query_timeout_secs` يغطي **الاستدعاء كله** (كل الأجزاء)، لا ميزانية لكل جزء.

### MongoDB — MongoConfig

```yaml
mongodb:
  url: "mongodb://host:27017"
  database: "app"
  # max_pool_size: 10        # اختياري: حد مجمّع الاتصالات الأعلى، الحذف = افتراضي السائق (**10**)
  # min_pool_size: 0         # اختياري: الحد الأدنى لمجمّع الاتصالات (اتصالات الإبقاء في الخلفية)
  # query_timeout_secs: 30   # اختياري: مهلة الأمر الواحد، 0 = تعطيل
  # breaker: {}              # اختياري: إعداد الـbreaker، الحذف = افتراضي متحفظ (0.5 / 30s / فتح 10s)
```

| الحقل | النوع | الوصف |
|------|------|------|
| `url` | `String` | URI الاتصال (المصادقة ومجموعة النسخ وخيارات TLS كلها داخل الـURI) |
| `database` | `String` | اسم القاعدة |
| `max_pool_size` | `Option<u32>` | حد مجمّع الاتصالات الأعلى؛ الحذف = افتراضي السائق **10** (مقاس على `mongodb` 3.8.0، وليس 100) |
| `min_pool_size` | `Option<u32>` | الحد الأدنى لمجمّع الاتصالات؛ الحذف = افتراضي السائق `0` |
| `query_timeout_secs` | `Option<u64>` | مهلة الأمر الواحد بالثواني؛ الحذف = `30`، **`0` = تعطيل** |
| `breaker` | `Option<BreakerConfig>` | عتبات الـbreaker ونافذته؛ لا مفتاح رئيسي `enabled` |

**الضغط الخلفي للتزامن يمر عبر مجمّع السائق**: هذا الـcrate **ليس فيه** `max_concurrency` (وليس HTTP أصلًا) —— فالسائق يحمل مجمّعه الخاص؛ وإن أردت تزامنًا أعلى فاضبط `max_pool_size` صراحةً.

### S3 / MinIO — S3Config

```yaml
s3:
  endpoint: "http://host:9000"
  region: "us-east-1"
  access_key: "minioadmin"
  secret_key: "minioadmin"
  # query_timeout_secs: 30   # اختياري: مهلة الاستدعاء الواحد، 0 = تعطيل
  # breaker: {}              # اختياري: إعداد الـbreaker، الحذف = افتراضي متحفظ (0.5 / 30s / فتح 10s)
  # max_concurrency: 32      # اختياري: حد التزامن (سيمافور هذا الـcrate)
```

| الحقل | النوع | الوصف |
|------|------|------|
| `endpoint` | `String` | عنوان خدمة متوافقة مع S3 (MinIO / بوابة مستضافة ذاتيًا) |
| `region` | `String` | المنطقة المستخدمة في التوقيع؛ MinIO لا يهتم بالقيمة، فـ`us-east-1` تكفي |
| `access_key` | `String` | مفتاح الوصول |
| `secret_key` | `String` | المفتاح السري |
| `query_timeout_secs` | `Option<u64>` | مهلة الاستدعاء الواحد بالثواني؛ الحذف = `30`، **`0` = تعطيل** |
| `breaker` | `Option<BreakerConfig>` | عتبات الـbreaker ونافذته؛ لا مفتاح رئيسي `enabled` |
| `max_concurrency` | `Option<usize>` | حد التزامن (الافتراضي `32`)؛ سيمافور هذا الـcrate نفسه |

**ميزانية واحدة للاستدعاء كله**: `list()` يتابع continuation token ويصدر عدة طلبات GET في استدعاء واحد، والمهلة تغطي **التنقل بين الصفحات كله**.

> **القواسم المشتركة للصلابة الصادرة** (كل واجهات HTTP الخلفية في هذا القسم): الترتيب **تصريح → breaker → مهلة**؛ خطأ المهلة `code = DeadlineExceeded`، ورفض الـbreaker `code = Unavailable` مع `message = "circuit breaker is open"`؛ ومع ميزة `metrics` تُحتسب تحت `ecat_outbound_timeouts_total{backend="<اسم قسم الإعداد>"}` —— والوسم هنا هو **اسم قسم الإعداد** (`"arangodb"` / `"mongodb"` / …)، وليس اسم فئة الـtrait.

---

## الإنشاء برمجيًا

### دون مصادقة

```rust
let es = ElasticsearchClient::new("http://localhost:9200");
let ch = ClickhouseClient::new("http://localhost:8123", "default");
```

### مع المصادقة

```rust
let es = ElasticsearchClient::with_auth("http://es:9200", "elastic", "secret");
let ch = ClickhouseClient::with_auth("http://ch:8123", "default", "admin", "pass");
let qdb = QuestdbClient::with_auth("http://qdb:9000", "admin", "quest");
let ng = NebulaGraphClient::with_auth("http://ng:19669", "space1", "root", "nebula");
```

---

---

## إعداد شهادات TLS

تدعم خلفيات البيانات عمومًا مصادقة عميل TLS اختيارية (حقل `tls`)، مع **استثناءين**: `ecat-data-sqlx` لا يدعم هذا الحقل — ضبطه يُفشل بدء التشغيل (TLS يمرّ عبر معاملات URL)؛ وحقل `ecat-data-memcached` **لا أثر له بصمت** — مُعلَن لكنه لا يُقرأ في أي مكان من الـ crate.

### مثال على الإعداد

```yaml
clickhouse:
  base_url: "https://ch.internal:8443"
  tls:
    ca_cert: "/etc/ecat/ca.pem"
    client_cert: "/etc/ecat/client.pem"
    client_key: "/etc/ecat/client-key.pem"
    # skip_verify: true  # بيئات الاختبار فقط
```

### التوليد التلقائي للشهادات (ecat-tls)

```rust
use ecat_tls::{generate_ca, generate_server_cert, generate_client_cert};

// 1. توليد CA
let ca = generate_ca("MyOrg")?;
std::fs::write("ca.pem", &ca.cert_pem)?;
std::fs::write("ca-key.pem", &ca.key_pem)?;

// 2. توليد شهادة الخادم
let srv = generate_server_cert("db.example.com")?;
std::fs::write("server.pem", &srv.cert_pem)?;
std::fs::write("server-key.pem", &srv.key_pem)?;

// 3. توليد شهادة العميل (mTLS)
let client = generate_client_cert("myapp")?;
std::fs::write("client.pem", &client.cert_pem)?;
std::fs::write("client-key.pem", &client.key_pem)?;
```

### التوليد اليدوي (OpenSSL)

```bash
# CA
openssl req -x509 -newkey rsa:4096 -keyout ca-key.pem -out ca.pem -days 3650 -nodes

# شهادة الخادم
openssl req -new -newkey rsa:4096 -keyout server-key.pem -out server.csr -nodes -subj "/CN=db.example.com"
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -out server.pem -days 365

# شهادة العميل (mTLS)
openssl req -new -newkey rsa:4096 -keyout client-key.pem -out client.csr -nodes -subj "/CN=myapp"
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca-key.pem -out client.pem -days 365
```

### وصف حقول TLS

| الحقل | النوع | الوصف |
|------|------|------|
| `ca_cert` | `Option<String>` | مسار PEM لشهادة CA (التحقق من الخادم) |
| `client_cert` | `Option<String>` | مسار PEM لشهادة العميل (mTLS) |
| `client_key` | `Option<String>` | مسار PEM للمفتاح الخاص للعميل (mTLS) |
| `skip_verify` | `Option<bool>` | تخطي التحقق من الشهادة (الاختبار فقط) |

---

## استخدامات متقدمة

### تجاوز عبر متغيرات البيئة

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

### الدمج مع إطار ecat-config

```rust
use ecat_config::{Config, FileSource};

let mut app_config = Config::new();
app_config.load(&FileSource::new("databases.yaml")).await?;

let redis_cfg: RedisConfig = serde_json::from_value(
    app_config.get::<serde_json::Value>("redis").unwrap()
)?;
let cache = RedisCache::from_config(redis_cfg).await?;
```

### الإعداد حسب الحاجة

احذف قواعد البيانات غير المستخدمة من YAML، وعلّم الحقول الاختيارية بـ `Option` في بنية Rust:

```rust
#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: Option<RedisConfig>,
    clickhouse: Option<ClickhouseConfig>,
}
```

---

## مستندات ذات صلة

- [تقرير التدقيق r5](audit-report-2026-08-01-r5.md)
- [برنامج تعليمي لمصادقة شهادات TLS](tls-certificate-tutorial.md)
- [ملف مثال الإعدادات](../../../config/databases.example.yaml)
