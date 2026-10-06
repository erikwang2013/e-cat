<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# مرجع API الخاص بـ Ecat

تصف هذه الصفحة سطح واجهات (API) إطار عمل Ecat: اصطلاحات المنافذ، والنقاط النهائية المدمجة، وتنسيق الأخطاء، والواجهات الموسّعة. يتم تسجيل مسارات الأعمال من قبل كل خدمة بنفسها.

## اصطلاحات المنافذ

| البروتوكول | عنوان الاستماع | الوصف |
|------|----------|------|
| HTTP | `0.0.0.0:8000` | توجيه axum، منفذ المثال الافتراضي |
| gRPC | `0.0.0.0:9000` | خادم tonic، منفذ المثال الافتراضي |

## النقاط النهائية المدمجة

توفر النقاط النهائية التالية من crates النظام البيئي، وتُركَّب مع الخدمة:

| النقطة النهائية | المصدر | الوصف |
|------|------|------|
| `/health` | ecat-health | فحص البقاء على قيد الحياة (يُرجع اسم الخدمة والإصدار ووقت البدء) |
| `/ready` | ecat-health | فحص الجاهزية (يُرجع 200 بعد جاهزية التبعيات) |
| `/metrics` | ecat-metrics | كشف مقاييس Prometheus (`ecat_http_requests_total` / `ecat_http_request_duration_seconds`) |
| `/{service}/{method}` | مسارات المستخدم | مثال: `/helloworld/ecat` |

> في السيناريوهات عالية الكاردينالية مثل مسارات تحتوي على معرّفات، استخدم `MetricsLayer::new().with_path_fn(...)` لتطبيع مسار المقاييس وتجنب انفجار الكاردينالية.

## تدفق معالجة الطلبات

```
طلب العميل
  ├─ HTTP :8000 ──→ axum::Router ─┐
  └─ gRPC :9000 ──→ tonic::Server ─┤
                              ┌─────┴──────┐
                              │ Middleware │  Recovery→Tracing→Logging→Auth→Metrics→Security→CircuitBreaker
                              └─────┬──────┘
                                    ▼
                               Handler (tower::Service)
                                    ▼
                               Response (ترميز JSON/Protobuf)
```

## تنسيق الأخطاء

يوفر `ecat-errors` `ErrorCode` + `Error`، مع ربط أكواد حالة HTTP في وقت الترجمة:

```rust
use ecat_errors::{Error, ErrorCode};

Error::new(ErrorCode::InvalidArgument, "bad_request", "user id must be positive");
```

تُرمَّز استجابات الأخطاء عبر middleware إلى JSON (أو Protobuf)، وتحمل code / reason / message.

## الواجهات الموسّعة

| القدرة | Crate | الواجهة |
|------|-------|------|
| GraphQL | ecat-graphql | نقطة `/graphql`؛ تدعم معاملات الحقول وselection المتداخلة، ولا تدعم aliases ولا fragments ولا حقولًا متعددة على المستوى الأعلى |
| OpenAPI | ecat-openapi | توليد مواصفات OpenAPI من المسارات |
| WebSocket | ecat-transport-ws | نقل WS مُرقّى |
| توجيه إصدارات API | ecat-versioning | توجيه الإصدارات ببادئة `/v1/...` |
| المصادقة | ecat-auth | وسائط JWT / API Key؛ يجب أن يكون مفتاح JWT ≥32 بايت، مع دعم `required_issuer`/`required_audience` المتسلسل |
| عميل gRPC | ecat-transport-grpc | تكامل اكتشاف الخدمات وموازنة الحمل |

## التواصل بين الخدمات

- `HttpClient` (ecat-client): يدمج اكتشاف الخدمات وموازنة الحمل، مع حماية عبر CircuitBreaker
- `GrpcClient` (ecat-transport-grpc): كما سبق، عبر بروتوكول gRPC
- تُركَّب الوسائط بشكل موحد عبر `tower::ServiceBuilder` (Recovery / Tracing / Logging / Timeout / RateLimit / Security / CircuitBreaker / Metrics / Retry / Validate / CORS)

## واجهات خلفيات البيانات

جميع خلفيات البيانات (`ecat-data-*`) مُجرّدة عبر traits موحّدة (`RdbmsClient` يدير المعاملات، و`SqlExecutor` يدير التنفيذ واللهجة / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`)؛ تصل خلفيات نمط REST (Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine / OpenSearch / Elasticsearch / S3) إلى واجهات HTTP المقابلة عبر `base_url`. راجع [برنامج تعليمي لإعداد قاعدة البيانات](database-config-tutorial.md) لإعدادات الاتصال.

## ORM ‏(ecat-orm)

يوفّر `ecat-orm` ماكرو اشتقاق الكيانات، وبانِي استعلامات آمن الأنواع، وعمليات CRUD، والتحميل المسبق
للعلاقات، والترحيلات. تأخذ كل عملية بيانات `&impl SqlExecutor`، لذا **تعمل الواجهة نفسها مع العميل ومع
`Transaction`**:

```rust
User::find_by_id(&db, 1).await?;              // db: SqlxClient / MssqlClient
let tx = db.transaction().await?;
User::update(&tx, &user).await?;              // tx: Transaction
User::insert(&tx, &user).await?;              // يعمل داخل معاملة أيضًا: تُنفَّذ العبارتان داخلها
tx.commit().await?;
```

يُولَّد SQL في طبقة اللهجات: تتشارك SQLite / PostgreSQL / MySQL / TiDB ‏(`ecat-data-sqlx`) و
SQL Server ‏(`ecat-data-mssql`) تعريف كيان واحدًا.

### تعريف الكيان وقواعد السمة `#[entity(...)]`

يولّد `#[derive(Entity)]` الثابت `Entity::META` (الجدول / الأعمدة / العلاقات / الأعلام)، والدوال
`from_row` / `to_values` / `pk_value`، ومُعدَّدًا `XxxRelation` واحدًا لكل كيان (أسماء الحالات هي صيغة
PascalCase لأسماء حقول العلاقات).

سمة الحاوية:

| الصيغة | المعنى |
|------|------|
| `#[entity(table = "users")]` | اسم الجدول؛ عند إغفاله يُؤخذ snake_case لاسم البنية (`User` → `user`، ‏`UserProfile` → `user_profile`) |

حقول الأعمدة:

| الصيغة | المعنى |
|------|------|
| `#[entity(column = "user_name")]` | تجاوز اسم العمود، والافتراضي اسم الحقل |
| `#[entity(pk)]` | المفتاح الأساسي |
| `#[entity(auto_increment)]` | زيادة تلقائية (تستلزم pk)؛ لا يدخل المفتاح التلقائي في قائمة أعمدة الإدراج، بل تولّده قاعدة البيانات |
| `#[entity(created_at)]` / `#[entity(updated_at)]` | طوابع زمنية تلقائية: تُملأ الاثنتان عند الإدراج، ويُحدَّث `updated_at` وحده عند التعديل |
| `#[entity(soft_delete)]` | عمود الحذف المنطقي: يضيف مسار القراءة تلقائيًا شرط `IS NULL` عليه |
| `#[entity(version)]` | عمود القفل المتفائل: يكتب التعديل `version + 1` ويقارن بالقيمة القديمة، ويرجع التعارض `OrmError::OptimisticLockConflict` |

حقول العلاقات (**نوع الحاوية قيد صارم**؛ يُرفض نوع الكيان المجرّد وقت الترجمة لأنه لا يستطيع التعبير عن
«غير موجود»):

| الصيغة | نوع الحقل | المعنى |
|------|----------|------|
| `#[entity(has_many = "Post", foreign_key = "user_id")]` | `Vec<Post>` | واحد إلى متعدد: تُطابَق قيمة `local_key` في هذا الجدول مع عمود `foreign_key` في الجدول الهدف |
| `#[entity(has_one = "Profile", foreign_key = "user_id")]` | `Option<Profile>` | واحد إلى واحد؛ العلاقة أحادية القيمة تأخذ الصف الأول فقط |
| `#[entity(belongs_to = "Tag", foreign_key = "tag_code")]` | `Option<Tag>` | متعدد إلى واحد: تُطابَق قيمة `foreign_key` في هذا الجدول مع **المفتاح الأساسي للجدول الهدف** |

يمكن إغفال `local_key`: يأخذ `has_many` / `has_one` المفتاح الأساسي لهذا الجدول افتراضيًا، ويأخذ
`belongs_to` المفتاح الأساسي للجدول الهدف. حقول العلاقات **ليست أعمدة**. المقابلة بين نوع الحقل ونوع
العمود موجودة فقط في تطبيقات `value::ColumnValue` (لا يحتفظ الماكرو بنسخة ثانية منها)؛ والنوع غير المدعوم
يُنتج خطأ عدم تحقّق `T: ColumnValue` يشير إلى ذلك الحقل.

### عمليات CRUD

| الدالة | ملاحظة |
|------|------|
| `Entity::insert(&db, &e) -> i64` | تُدرج وترجع المفتاح الأساسي الجديد. مسار MySQL ذو الخطوتين (`INSERT` ثم `SELECT LAST_INSERT_ID()`، وهو **مرتبط بالاتصال**) يلفّه `SqlExecutor::execute_then_query` في معاملة واحدة |
| `Entity::insert_many(&db, &[e]) -> u64` | إدراج جماعي، وترجع عدد الصفوف المتأثرة (دون إرجاع المفاتيح: `LAST_INSERT_ID()` يعطي أول صف و SQLite آخر صف — دلالة غير موثوقة بين الخلفيات) |
| `Entity::save(&db, &e)` | تُدرج إذا كان المفتاح الأساسي «غير محدَّد» (تلقائي وقيمته 0)، وإلا فتحدّث؛ وترجع `()` |
| `Entity::update(&db, &e) -> u64` | تحديث كامل للصف بالمفتاح الأساسي. صفر صفوف متأثرة خطأ: `OrmError::NotFound` بدون `version`، و`OrmError::OptimisticLockConflict` معه (لا يمكن التمييز بينهما دون استعلام إضافي) |
| `Entity::update_many(&db, &[e]) -> u64` | تحديث صفًا صفًا وجمع الصفوف؛ ولا تستطيع الدعوة الجماعية **تحديد أي صف** تعارض (نتيجة أقل من المدخلات تعني أن أحدهم أوقفه شرط الإصدار) |
| `Entity::upsert(&db, &e) -> u64` | إدراج أو تحديث بالمفتاح الأساسي (`ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE` حسب اللهجة) |
| `Entity::find_by_id(&db, pk) -> Option<Self>` | يجلب صفًا بالمفتاح الأساسي، ويرجع `Ok(None)` عند عدم وجوده |
| `Entity::find_all(&db) -> Vec<Self>` | كل الصفوف دون تصفية (**مسح كامل للجدول في الجداول الكبيرة**؛ للتصفح استخدم `paginate`) |
| `Entity::delete_by_id(&db, pk) -> u64` | عند إعلان `soft_delete` ترسل `UPDATE` يضبط وقت الحذف؛ يبقى الصف في الجدول، والحذف المتكرر يرجع `NotFound` دون تحديث الوقت |
| `Entity::hard_delete_by_id(&db, pk) -> u64` | تتجاوز الحذف المنطقي وترسل `DELETE` فعليًا |

### بانِي الاستعلامات

تبدأ بـ `User::query()` التي ترجع `Query<User, Unfiltered>`؛ وإضافة مُرشِّح تحوّلها إلى
`Query<User, Filtered>` (حالة نوعية) — أي «حذف بلا WHERE» لا يُترجم.

```rust
use ecat_orm::query::{Op, Order};

let users = User::query()
    .filter("name", Op::Like, "alice%")?        // Eq / Ne / Lt / Le / Gt / Ge / Like
    .filter("email", Op::NotNull, serde_json::json!(null))?
    .filter("id", Op::In, serde_json::json!([1, 2, 3]))?  // يأخذ In / NotIn قيمة مصفوفة
    .order_by("id", Order::Desc)?
    .limit(10)
    .offset(20)
    .fetch(&db)
    .await?;
```

- تُتحقَّق أسماء الأعمدة مقابل القائمة البيضاء `EntityMeta.columns` (`OrmError::UnknownColumn`)؛ وأعمدة
  الجداول الموصولة ليست فيها — استخدم لها `filter_raw("...")` مع العلم أنه **لا يجري أي تحقق: يجب أن
  تكون المدخلات موثوقة**.
- `with_trashed()` يعطّل شرط الحذف المنطقي (فتُجلب الصفوف المحذوفة منطقيًا أيضًا).
- `join(JoinType::Left, "posts", "posts.user_id = users.id")` يدعم `Inner` / `Left`، والغرض منه التصفية
  بأعمدة الجدول الموصول داخل `filter_raw`. **قائمة الأعمدة بلا بادئة جدول**، لذا إذا شارك الجدول الموصول
  اسم عمود مع الجدول الأساسي فستُبلغ قاعدة البيانات الحقيقية عن `ambiguous column name` — اوصل جداول
  تختلف أسماء أعمدتها عن الجدول الأساسي (مُتحقَّق منه على SQLite).
- `delete_where(&db)` / `hard_delete_where(&db)` تحذف بالشروط نفسها (الكيانات ذات الحذف المنطقي تمر عبر `UPDATE`).
- `fetch(&db)` هي نقطة التنفيذ؛ ويعيد `find_by_id` / `find_all` / `paginate` استخدام مسار توليد SQL نفسه خلفها.

### التقسيم إلى دفعات والتصفح

- **تُقسَّم الكتابات الجماعية تلقائيًا**: يقسم `insert_many` / `update_many` حسب حدّ المعاملات لكل عبارة
  في اللهجة (مثلًا 2100 في SQL Server) ويجمع الصفوف. وللتقسيم يدويًا استخدم الحدّ نفسه:
  `ecat_orm::dialect::lookup(dialect).max_params_per_stmt()`.
- **التصفح**: يُرسِل `paginate(&db, page, per_page)` استعلامين (COUNT + جلب الصفحة)، وترقيم الصفحات **يبدأ من 1**؛
  ويعيد COUNT استخدام نفس WHERE / JOIN لكنه يزيل ORDER BY / LIMIT / OFFSET. ويعيد
  `Page { items, total, page, per_page }`، وتُحسب `total_pages()` / `has_next()` من `total`.
- للتصفح العميق في الجداول الكبيرة استخدم `paginate_without_count(&db, page, per_page)` (استعلام واحد):
  تكون `total` مساوية لـ `None`، ولا تستطيع `total_pages()` / `has_next()` الإجابة أيضًا — فما لا يمكن
  حسابه يُبلَّغ عنه كذلك بدل تخمينه.

### التحميل المسبق للعلاقات

```rust
let users = User::query()
    .with(&[UserRelation::Posts, UserRelation::Profile])
    .fetch(&db)
    .await?;
```

- تصدر كل علاقة **استعلام `IN` واحدًا** (مع تقسيمه إلى دفعات إذا تجاوزت المفاتيح حدّ المعاملات لكل عبارة)،
  ولا يجري استعلام الكيانات الأساسية أي JOIN — **لا N+1**: جلب 3 مستخدمين ومنشوراتهم هو استعلاما SELECT
  (واحد للأساسي + واحد `IN`) لا أربعة.
- بدون `with()` تكون حقول العلاقات فارغة (`Vec::new()` / `None`)، ولا تحمل أبدًا قيمًا قديمة من تحميل
  سابق؛ ويُستدعى `set_relation` لكل كيان أساسي (حتى مع نتيجة فارغة) تحديدًا لمسح البقايا.
- العلاقات أحادية القيمة (`has_one` / `belongs_to`) تأخذ الصف الأول فقط.

### الترحيلات

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Migrator, create_table};

Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .add("002_posts", create_table::<Post>())
    .status().await?;      // MigrationStatus { applied, pending, unknown }
    .run().await?;         // يطبّق المعلّقة ويسجّل كل واحدة في جدول الإصدارات؛ وإعادة التشغيل آمنة (idempotent)
    .down(1).await?;       // ينفّذ SQL العكسي للترحيل 001 ويحذف صفّه من جدول الإصدارات
```

- البادئة الرقمية في اسم الترحيل هي إصداره (`"001_users"` → 1)؛ والبادئة غير الرقمية تُنتج
  `OrmError::InvalidMigrationName` — ولا تُفسَّر تخمينًا على أنها 0.
- يولّد `create_table::<E>()` / `drop_table::<E>()` تعليمات DDL من `EntityMeta`، و**تُحدَّد اللهجة عند
  `run()` من `dialect()` للاتصال**، فتنفصل قائمة الترحيلات عن سلسلة الاتصال. أما SQL المكتوب يدويًا
  (ALTER، تعبئة البيانات) فيُبنى بـ `ecat_orm::migrate::MigrationBuilder::new(|d| ...)`.
- استدعاء `down` على ترحيل بلا SQL عكسي يُنتج `OrmError::MigrationIrreversible` — فـ«الحذف ثم الإنشاء
  من جديد» يفقد البيانات، ولا يُخمَّن ذلك نيابة عن المستدعي.
- ينشئ `Migrator` جدول الإصدارات تلقائيًا (لا يملك MSSQL الأمر `CREATE TABLE IF NOT EXISTS`، لذا يُتحقَّق
  من الوجود أولًا).
