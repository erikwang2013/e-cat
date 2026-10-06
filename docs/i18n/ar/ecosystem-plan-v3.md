# خطة النظام البيئي لـ e-cat v3 — التقييم النهائي

> **تحديث (2026-08-07، v2.3.3)**: اكتملت الفجوة المتبقية #1 «دمج mTLS في النقل» — `HttpServer::tls` / `GrpcServer::tls` فعّالة فعليًا عبر tokio-rustls / rustls الخاص بـ tonic (تدعم التحقق من CA وفرض شهادة العميل)؛ الفجوتان #2 (حد معدل Redis) و#3 (GitLab CI) اكتملتا سابقًا مع v2.3.0. وبذلك تُنفَّذ جميع الفجوات المذكورة في الخطط.

**الإصدار:** 2.4.2  
**التاريخ:** 2026-08-01  
**إجمالي عدد crates:** 55 · اكتملت جميع الخطط

---

## التغطية الحالية

| المجال | المنفَّذ | نسبة التغطية |
|------|--------|--------|
| طبقة النقل | HTTP (axum), gRPC (tonic), WebSocket | 100% |
| الترميز | JSON, Protobuf | 100% |
| الوسائط | Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, Auth×3 | 100% |
| الإعدادات | env, file (JSON/YAML), Consul KV, تشفير (XOR) | 100% |
| سجل الخدمات | memory, Consul, etcd | 100% |
| الأمان | كشف الهجمات, JWT, API Key, OAuth2, شهادة عميل TLS, mTLS | 95% |
| التواصل | شهادة عميل TLS — جميع خلفيات البيانات تدعمها | 95% |
| التواصل بين الخدمات | HTTP Client, gRPC Client, Resolver, LoadBalancer | 95% |
| البيانات | RDBMS (sqlx), Redis, OpenSearch, Elasticsearch, ClickHouse, Memcached, Neo4j, NebulaGraph, ArangoDB, InfluxDB, IoTDB, QuestDB — الكل يدعم إعداد ملفات Config | 95% |
| الرسائل | trait MessageQueue, InMemory, Kafka, EventBus | 100% |
| المراقبة | tracing, Prometheus, Health, التتبع الموزع | 100% |
| DevOps | CLI, Dockerfile, K8s, Helm, GitHub Actions, Bench, Testing | 95% |
| أدوات API | OpenAPI, Versioning, GraphQL | 100% |

---

## الفجوات المتبقية

### تستحق التنفيذ (3 عناصر)

| # | الفجوة | القيمة | حجم العمل |
|---|------|------|--------|
| 1 | **دمج mTLS في النقل** | TlsConfig موجود، لم يُدمج في HttpServer/GrpcServer بعد | صغير |
| 2 | **خلفية حد المعدل Redis** | RateLimitLayer في الذاكرة فقط، وتحتاج المشاركة عبر مثيلات متعددة | صغير |
| 3 | **قوالب CI لـ GitLab** | GitHub Actions موجودة بالفعل | صغير |

### لا حاجة لتنفيذها (عنصران)

| # | الفجوة | السبب |
|---|------|------|
| 4 | تشفير AES-GCM | XOR الحالي كافٍ |
| 5 | شبكة الخدمات/بوابة API | يُترك للمجتمع (Linkerd/Kong/K8s) |

---

## الحكم

**وصل e-cat إلى درجة نضج قابلة للإنتاج.** تغطي 47 crate كامل حزمة الخدمات المصغرة: النقل ← الوسائط ← اكتشاف الخدمات ← الإعدادات ← الأمان ← البيانات ← الرسائل ← المراقبة ← DevOps ← أدوات API. الفجوات الثلاث المتبقية تحسينات بحجم عمل صغير، دون نقص بنيوي.

## تغطية خلفيات البيانات (15)

| الفئة | قاعدة البيانات | Crate | طريقة التشغيل |
|------|--------|-------|----------|
| RDBMS | SQLite/PostgreSQL/MySQL/TiDB | `ecat-data-sqlx` | sqlx (مشغّل غير متزامن رسمي) |
| التخزين المؤقت | Redis | `ecat-data-redis` | redis-rs (مشغّل رسمي) |
| التخزين المؤقت | Memcached | `ecat-data-memcached` | ⚠️ تنفيذ في الذاكرة (غير مناسب للإنتاج) |
| المستندات | MongoDB | `ecat-data-mongodb` | mongodb (مشغّل رسمي) |
| التخزين الكائني | S3 / MinIO | `ecat-data-s3` | HTTP/REST (reqwest+rustls، SigV4 منفَّذ ذاتيًا) |
| OLAP | ClickHouse | `ecat-data-clickhouse` | HTTP/REST (reqwest) |
| البحث | OpenSearch | `ecat-data-opensearch` | HTTP/REST (reqwest) |
| البحث | Elasticsearch | `ecat-data-elasticsearch` | HTTP/REST (reqwest) |
| الرسوم البيانية | Neo4j | `ecat-data-neo4j` | HTTP/REST (reqwest) |
| الرسوم البيانية | NebulaGraph | `ecat-data-nebulagraph` | HTTP/REST (reqwest) |
| الرسوم البيانية | ArangoDB | `ecat-data-arangodb` | HTTP/REST (reqwest) |
| السلاسل الزمنية | InfluxDB | `ecat-data-influxdb` | HTTP/REST (reqwest) |
| السلاسل الزمنية | Apache IoTDB | `ecat-data-iotdb` | HTTP/REST (reqwest) |
| السلاسل الزمنية | QuestDB | `ecat-data-questdb` | HTTP/REST (reqwest) |
| السلاسل الزمنية | TDengine | `ecat-data-tdengine` | HTTP/REST (reqwest) |

---

## خطة v4.0 (2026-10-05) — ORM كامل و SQL Server

> الحالة: **قيد التنفيذ**. اكتملت الدفعة 1 (تقسيم `SqlExecutor` + مجمعات الاتصال الأصلية) ودُمجت في
> الفرع `feat/orm-mssql`؛ ولا يزال ORM و SQL Server وتحسينات المجمع قيد الانتظار. ويُبيَّن التقدم
> الحالي لكل بند أدناه.
> التصميم الكامل: [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../../../docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md).

يبقى سدّ فجوتين بنيويتين:

1. **ORM كامل**: طبقة البيانات الحالية لا توفر سوى عملاء RDBMS بـ SQL مكتوب يدويًا
   (`Row` = أسماء الأعمدة + قيم JSON) — بلا تعيين كيانات ولا ارتباطات ولا ترحيلات. يضيف v4.0
   `ecat-orm` (ماكرو الكيانات / CRUD / باني الاستعلامات / التحميل المسبق للارتباطات / استعلامات
   الربط / الدفعات / الترقيم / الترحيلات) + `ecat-orm-derive`. مبني على trait `SqlExecutor` الموحد،
   و**يغطي بطبيعته جميع خلفيات RDBMS**.
2. **خلفية SQL Server**: لا يوجد في sqlx الرئيسي مشغّل MSSQL (أُزيل قبل 0.7، وإعادة الكتابة لم تُنشر)؛
   يضيف v4.0 الحزمة `ecat-data-mssql` (`tiberius-ng` 0.13 + `deadpool` 0.13) —
   خلفيات البيانات **الهدف** من 15 إلى **16** (حاليًا لا تزال 15، و SQL Server لم يُضف بعد).

تغييرات الأساس المرتبطة:

| التغيير | الوصف | الحالة |
|---|---|---|
| `ecat-data-sqlx` يتخلى عن `AnyPool` لصالح مجمعات أصلية | إصلاح قيود أنواع الوقت (لم تعد هناك حاجة إلى التحايل بـ CAST)، وإزالة سطح الـ panic عند تثبيت المشغّل، وتفعيل statement cache | ✅ مكتمل (الدفعة 1، ثلاثة مجمعات أصلية `PgPool`/`MySqlPool`/`SqlitePool`) |
| `ecat-data` يفصل supertrait باسم `SqlExecutor` | تنفيذ SQL داخل المعاملات (حاليًا `Transaction` يقتصر على commit/rollback)؛ أساس لـ ORM وفصل القراءة/الكتابة | ✅ مكتمل (الدفعة 1) |
| تحسينات مجمع الاتصالات | مهلة الاستعلام، التسخين المسبق `warm_up()`، recycle ذكي، قاطع الدائرة (يعيد استخدام `ecat-circuit-breaker`)، فصل القراءة/الكتابة `RdbmsRouting` | 🚧 جزئي: اكتملت مهلة الاستعلام و`warm_up()`؛ ويبقى قاطع الدائرة وفصل القراءة/الكتابة (الدفعة 4) |
| المراقبة | مقاييس المجمع إلى `ecat-metrics`، وفحص حالة المجمع إلى `ecat-health`، والاستعلامات البطيئة إلى `ecat-tracing` (كلها feature اختيارية) | ❌ لم يبدأ (الدفعة 4) |

**تغييرات كاسرة**: تقسيم trait + توقيع `SqlxClient::from_pool` + إزالة `AnyPool` — جميعها
مُنفَّذة بالفعل في الفرع `feat/orm-mssql` (الدفعة 1، لم تُصدَر بعد)؛ وعند الإصدار يصبح إصدار
workspace 3.0.3 → **4.0.0**.
