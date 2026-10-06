<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Ecat API संदर्भ

यह पृष्ठ Ecat फ्रेमवर्क के API सतह का सारांश प्रस्तुत करता है: पोर्ट सम्मेलन, अंतर्निहित एंडपॉइंट, त्रुटि प्रारूप और विस्तार इंटरफ़ेस। व्यावसायिक रूट प्रत्येक सेवा द्वारा स्वयं पंजीकृत किए जाते हैं।

## पोर्ट सम्मेलन

| प्रोटोकॉल | सुनने का पता | स्पष्टीकरण |
|------|----------|------|
| HTTP | `0.0.0.0:8000` | axum रूट, डिफ़ॉल्ट उदाहरण पोर्ट |
| gRPC | `0.0.0.0:9000` | tonic Server, डिफ़ॉल्ट उदाहरण पोर्ट |

## अंतर्निहित एंडपॉइंट

निम्न एंडपॉइंट इकोसिस्टम crates द्वारा प्रदान किए जाते हैं, सेवा के साथ माउंट होते हैं:

| एंडपॉइंट | स्रोत | स्पष्टीकरण |
|------|------|------|
| `/health` | ecat-health | लिवनेस चेक (सेवा नाम, संस्करण, प्रारंभ समय लौटाता है) |
| `/ready` | ecat-health | रेडिनेस चेक (निर्भरताएँ तैयार होने पर 200 लौटाता है) |
| `/metrics` | ecat-metrics | Prometheus मेट्रिक्स एक्सपोज़र (`ecat_http_requests_total` / `ecat_http_request_duration_seconds`) |
| `/{service}/{method}` | उपयोगकर्ता रूट | उदाहरण: `/helloworld/ecat` |

> मेट्रिक्स एंडपॉइंट पथ में ID जैसे उच्च-कार्डिनैलिटी परिदृश्यों के लिए `MetricsLayer::new().with_path_fn(...)` से नॉर्मलाइज़ करें, मेट्रिक्स कार्डिनैलिटी विस्फोट से बचें।

## अनुरोध प्रोसेसिंग प्रवाह

```
क्लाइंट अनुरोध
  ├─ HTTP :8000 ──→ axum::Router ─┐
  └─ gRPC :9000 ──→ tonic::Server ─┤
                              ┌─────┴──────┐
                              │ Middleware │  Recovery→Tracing→Logging→Auth→Metrics→Security→CircuitBreaker
                              └─────┬──────┘
                                    ▼
                               Handler (tower::Service)
                                    ▼
                               Response (JSON/Protobuf एन्कोडिंग)
```

## त्रुटि प्रारूप

`ecat-errors` `ErrorCode` + `Error` प्रदान करता है, कंपाइल-टाइम पर HTTP स्टेटस कोड मैप करता है:

```rust
use ecat_errors::{Error, ErrorCode};

Error::new(ErrorCode::InvalidArgument, "bad_request", "user id must be positive");
```

त्रुटि प्रतिक्रिया middleware द्वारा JSON (या Protobuf) में एन्कोड होती है, जिसमें code / reason / message होता है।

## विस्तार इंटरफ़ेस

| क्षमता | Crate | इंटरफ़ेस |
|------|-------|------|
| GraphQL | ecat-graphql | `/graphql` एंडपॉइंट; फ़ील्ड पैरामीटर और नेस्टेड selection का समर्थन करता है, alias, fragment और मल्टी-टॉप-लेवल फ़ील्ड का नहीं |
| OpenAPI | ecat-openapi | रूट से OpenAPI spec जनरेट करता है |
| WebSocket | ecat-transport-ws | अपग्रेडेड WS ट्रांसपोर्ट |
| API संस्करण रूटिंग | ecat-versioning | `/v1/...` उपसर्ग संस्करण रूटिंग |
| प्रमाणीकरण | ecat-auth | JWT / API Key मिडलवेयर; JWT कुंजी ≥32 बाइट्स होनी चाहिए, चेन किया जा सकता है `required_issuer`/`required_audience` |
| gRPC क्लाइंट | ecat-transport-grpc | सेवा खोज और लोड बैलेंसिंग के साथ एकीकृत |

## सेवा-दर-सेवा संचार

- `HttpClient` (ecat-client)：सेवा खोज और लोड बैलेंसिंग के साथ एकीकृत, CircuitBreaker सर्किट ब्रेकर सुरक्षा
- `GrpcClient` (ecat-transport-grpc)：वही, gRPC प्रोटोकॉल
- मिडलवेयर एकीकृत रूप से `tower::ServiceBuilder` से संयोजित होता है (Recovery / Tracing / Logging / Timeout / RateLimit / Security / CircuitBreaker / Metrics / Retry / Validate / CORS)

## डेटा बैकएंड इंटरफ़ेस

सभी डेटा बैकएंड (`ecat-data-*`) एकीकृत traits (`RdbmsClient` ट्रांज़ैक्शन के लिए, `SqlExecutor` निष्पादन और डायलेक्ट के लिए / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`) के माध्यम से एब्स्ट्रैक्ट किए गए हैं; REST प्रकार के बैकएंड (Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine / OpenSearch / Elasticsearch / S3) `base_url` के आधार पर संबंधित HTTP इंटरफ़ेस तक पहुँचते हैं। कनेक्शन कॉन्फ़िगरेशन के लिए देखें [डेटाबेस कॉन्फ़िगरेशन ट्यूटोरियल](database-config-tutorial.md)।

## ORM (ecat-orm)

`ecat-orm` एंटिटी डिराइव मैक्रो, टाइप-सुरक्षित क्वेरी बिल्डर, CRUD, रिलेशन प्रीलोडिंग और माइग्रेशन प्रदान
करता है। हर डेटा ऑपरेशन `&impl SqlExecutor` लेता है, इसलिए **यही API क्लाइंट के साथ और `Transaction` के
साथ भी काम करता है**:

```rust
User::find_by_id(&db, 1).await?;              // db: SqlxClient / MssqlClient
let tx = db.transaction().await?;
User::update(&tx, &user).await?;              // tx: Transaction
User::insert(&tx, &user).await?;              // ट्रांज़ैक्शन के अंदर भी काम करता है: दोनों स्टेटमेंट वहीं चलते हैं
tx.commit().await?;
```

SQL डायलेक्ट लेयर बनाती है: SQLite / PostgreSQL / MySQL / TiDB (`ecat-data-sqlx`) और SQL Server
(`ecat-data-mssql`) एक ही एंटिटी परिभाषा साझा करते हैं।

### एंटिटी परिभाषा और `#[entity(...)]` एट्रिब्यूट व्याकरण

`#[derive(Entity)]` ये जनरेट करता है: `Entity::META` (टेबल / कॉलम / रिलेशन / फ़्लैग), `from_row` /
`to_values` / `pk_value`, और प्रति एंटिटी एक `XxxRelation` एनम (वेरिएंट नाम रिलेशन फ़ील्ड के नाम का
PascalCase रूप)।

कंटेनर एट्रिब्यूट:

| रूप | अर्थ |
|------|------|
| `#[entity(table = "users")]` | टेबल नाम; छोड़ने पर स्ट्रक्चर नाम का snake_case लिया जाता है (`User` → `user`, `UserProfile` → `user_profile`) |

कॉलम फ़ील्ड:

| रूप | अर्थ |
|------|------|
| `#[entity(column = "user_name")]` | कॉलम नाम का ओवरराइड, डिफ़ॉल्ट फ़ील्ड नाम |
| `#[entity(pk)]` | प्राइमरी की |
| `#[entity(auto_increment)]` | ऑटो-इंक्रीमेंट (pk को दर्शाता है); ऑटो-इंक्रीमेंट की INSERT के कॉलम लिस्ट में नहीं आती, उसे डेटाबेस बनाता है |
| `#[entity(created_at)]` / `#[entity(updated_at)]` | स्वचालित टाइमस्टैम्प: insert पर दोनों भरते हैं, update पर सिर्फ़ `updated_at` ताज़ा होता है |
| `#[entity(soft_delete)]` | सॉफ़्ट-डिलीट कॉलम: पढ़ने का रास्ता अपने-आप उस पर `IS NULL` शर्त जोड़ता है |
| `#[entity(version)]` | ऑप्टिमिस्टिक लॉक कॉलम: update `version + 1` लिखता है और पुराने मान से तुलना करता है, टकराव पर `OrmError::OptimisticLockConflict` लौटता है |

रिलेशन फ़ील्ड (**कंटेनर प्रकार कड़ी शर्त है**; नंगा एंटिटी प्रकार कंपाइल-टाइम पर अस्वीकार होता है — वह
"न मिला" व्यक्त नहीं कर सकता):

| रूप | फ़ील्ड प्रकार | अर्थ |
|------|----------|------|
| `#[entity(has_many = "Post", foreign_key = "user_id")]` | `Vec<Post>` | एक-से-अनेक: इस टेबल की `local_key` वैल्यू लक्ष्य टेबल के `foreign_key` कॉलम से मिलाई जाती है |
| `#[entity(has_one = "Profile", foreign_key = "user_id")]` | `Option<Profile>` | एक-से-एक; एकल-मान वाला रिलेशन सिर्फ़ पहली पंक्ति लेता है |
| `#[entity(belongs_to = "Tag", foreign_key = "tag_code")]` | `Option<Tag>` | अनेक-से-एक: इस टेबल की `foreign_key` वैल्यू **लक्ष्य टेबल की प्राइमरी की** से मिलाई जाती है |

`local_key` छोड़ा जा सकता है: `has_many` / `has_one` डिफ़ॉल्ट रूप से इस टेबल की प्राइमरी की लेते हैं,
`belongs_to` लक्ष्य की प्राइमरी की। रिलेशन फ़ील्ड **कॉलम नहीं हैं**। फ़ील्ड प्रकार से कॉलम प्रकार का
नक्शा सिर्फ़ `value::ColumnValue` इम्प्ल में रहता है (मैक्रो उस टेबल की दूसरी प्रति नहीं रखता);
असमर्थित प्रकार उस फ़ील्ड की ओर इशारा करते हुए असंतुष्ट `T: ColumnValue` त्रुटि देता है।

### CRUD

| मेथड | टिप्पणी |
|------|------|
| `Entity::insert(&db, &e) -> i64` | इन्सर्ट कर नई प्राइमरी की लौटाता है। MySQL का दो-चरणीय रास्ता (`INSERT` फिर `SELECT LAST_INSERT_ID()`, जो **कनेक्शन-स्कोप्ड** है) `SqlExecutor::execute_then_query` में एक ही ट्रांज़ैक्शन में लिपटा रहता है |
| `Entity::insert_many(&db, &[e]) -> u64` | बल्क इन्सर्ट, प्रभावित पंक्तियाँ लौटाता है (की वापस नहीं: `LAST_INSERT_ID()` पहली पंक्ति देता है, SQLite आख़िरी — बैकएंड के बीच अविश्वसनीय) |
| `Entity::save(&db, &e)` | प्राइमरी की "अनसेट" हो (ऑटो-इंक्रीमेंट और मान 0) तो इन्सर्ट, वरना अपडेट; `()` लौटाता है |
| `Entity::update(&db, &e) -> u64` | प्राइमरी की से पूरी पंक्ति का अपडेट। शून्य प्रभावित पंक्तियाँ त्रुटि हैं: `version` के बिना `OrmError::NotFound`, उसके साथ `OrmError::OptimisticLockConflict` (एक अतिरिक्त क्वेरी के बिना दोनों अलग नहीं किए जा सकते) |
| `Entity::update_many(&db, &[e]) -> u64` | पंक्ति-दर-पंक्ति अपडेट कर पंक्तियाँ जोड़ता है; बल्क कॉल **यह नहीं बता सकती कि कौन-सी पंक्ति** टकराई (इनपुट से कम परिणाम का मतलब है कि किसी को वर्शन शर्त ने रोका) |
| `Entity::upsert(&db, &e) -> u64` | प्राइमरी की से upsert (डायलेक्ट के अनुसार `ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE`) |
| `Entity::find_by_id(&db, pk) -> Option<Self>` | प्राइमरी की से एक पंक्ति लाता है; न हो तो `Ok(None)` |
| `Entity::find_all(&db) -> Vec<Self>` | बिना फ़िल्टर की सभी पंक्तियाँ (**बड़ी टेबलों पर पूरा टेबल स्कैन**; पेजिंग के लिए `paginate` इस्तेमाल करें) |
| `Entity::delete_by_id(&db, pk) -> u64` | `soft_delete` घोषित हो तो हटाने का समय भरता `UPDATE` भेजता है; पंक्ति टेबल में ही रहती है, और दोबारा हटाने पर `NotFound` लौटता है, समय ताज़ा नहीं होता |
| `Entity::hard_delete_by_id(&db, pk) -> u64` | सॉफ़्ट-डिलीट को बायपास कर सचमुच `DELETE` भेजता है |

### क्वेरी बिल्डर

शुरुआत `User::query()` से होती है, जो `Query<User, Unfiltered>` देता है; फ़िल्टर जोड़ने पर वह
`Query<User, Filtered>` (टाइप स्टेट) बन जाता है — "बिना WHERE का delete" कंपाइल नहीं होता।

```rust
use ecat_orm::query::{Op, Order};

let users = User::query()
    .filter("name", Op::Like, "alice%")?        // Eq / Ne / Lt / Le / Gt / Ge / Like
    .filter("email", Op::NotNull, serde_json::json!(null))?
    .filter("id", Op::In, serde_json::json!([1, 2, 3]))?  // In / NotIn ऐरे मान लेते हैं
    .order_by("id", Order::Desc)?
    .limit(10)
    .offset(20)
    .fetch(&db)
    .await?;
```

- कॉलम नाम `EntityMeta.columns` व्हाइटलिस्ट के विरुद्ध जाँचे जाते हैं (`OrmError::UnknownColumn`);
  जॉइन किए गए टेबलों के कॉलम उसमें नहीं हैं — उनके लिए `filter_raw("...")` इस्तेमाल करें, और याद रखें कि
  वह **कोई वैलिडेशन नहीं करता, इसलिए इनपुट विश्वसनीय होना चाहिए**।
- `with_trashed()` सॉफ़्ट-डिलीट शर्त बंद कर देता है (सॉफ़्ट-डिलीट हुई पंक्तियाँ भी आ जाती हैं)।
- `join(JoinType::Left, "posts", "posts.user_id = users.id")` `Inner` / `Left` समर्थित करता है और
  `filter_raw` के भीतर जॉइन किए गए टेबल के कॉलम पर फ़िल्टर करने के लिए है। **कॉलम सूची में टेबल प्रीफ़िक्स
  नहीं होता**, इसलिए अगर जॉइन की गई टेबल का कॉलम नाम मुख्य टेबल से मिलता है तो असली डेटाबेस
  `ambiguous column name` बताता है — ऐसे टेबल जॉइन करें जिनके कॉलम नाम मुख्य टेबल से अलग हों (SQLite पर
  सत्यापित)।
- `delete_where(&db)` / `hard_delete_where(&db)` उन्हीं शर्तों से हटाते हैं (सॉफ़्ट-डिलीट वाली एंटिटी
  `UPDATE` से गुज़रती हैं)।
- `fetch(&db)` निष्पादन का प्रवेश-बिंदु है; `find_by_id` / `find_all` / `paginate` उसी SQL जनरेशन रास्ते
  का पुनरुपयोग करते हैं।

### चंकिंग और पेजिंग

- **बल्क राइट अपने-आप चंक होते हैं**: `insert_many` / `update_many` डायलेक्ट की प्रति-स्टेटमेंट पैरामीटर
  सीमा (जैसे SQL Server में 2100) से बँटते हैं और पंक्तियाँ जोड़ते हैं। हाथ से चंक करने के लिए वही सीमा
  इस्तेमाल करें: `ecat_orm::dialect::lookup(dialect).max_params_per_stmt()`।
- **पेजिंग**: `paginate(&db, page, per_page)` 2 क्वेरी भेजता है (COUNT + पेज फ़ेच) और पेज **1 से शुरू**
  होते हैं; COUNT वही WHERE / JOIN दोहराता है पर ORDER BY / LIMIT / OFFSET हटा देता है। यह
  `Page { items, total, page, per_page }` लौटाता है, और `total_pages()` / `has_next()` `total` से
  निकाले जाते हैं।
- बड़ी टेबलों में गहरे पेजिंग के लिए `paginate_without_count(&db, page, per_page)` (1 क्वेरी) इस्तेमाल
  करें: `total` `None` रहता है, और `total_pages()` / `has_next()` भी जवाब नहीं दे सकते — जो गिना नहीं जा
  सकता, उसे अनुमान लगाने के बजाय वैसा ही बताया जाता है।

### रिलेशन प्रीलोडिंग

```rust
let users = User::query()
    .with(&[UserRelation::Posts, UserRelation::Profile])
    .fetch(&db)
    .await?;
```

- हर रिलेशन **एक** `IN` क्वेरी भेजता है (की प्रति-स्टेटमेंट पैरामीटर सीमा से बढ़ने पर चंक होकर), और मुख्य
  एंटिटी की क्वेरी ख़ुद कोई join नहीं करती — **कोई N+1 नहीं**: 3 यूज़र और उनके posts लाने पर 2 SELECT
  (1 मुख्य + 1 `IN`) लगते हैं, 4 नहीं।
- `with()` के बिना रिलेशन फ़ील्ड ख़ाली रहते हैं (`Vec::new()` / `None`), पिछली लोडिंग की पुरानी वैल्यू
  कभी नहीं ढोते; बची-खुची वैल्यू साफ़ करने के लिए ही हर मुख्य एंटिटी पर `set_relation` बुलाया जाता है
  (ख़ाली परिणाम पर भी)।
- एकल-मान वाले रिलेशन (`has_one` / `belongs_to`) सिर्फ़ पहली पंक्ति लेते हैं।

### माइग्रेशन

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Migrator, create_table};

Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .add("002_posts", create_table::<Post>())
    .status().await?;      // MigrationStatus { applied, pending, unknown }
    .run().await?;         // लंबित माइग्रेशन लागू कर हर एक को वर्शन टेबल में दर्ज करता है; दोबारा चलाना idempotent है
    .down(1).await?;       // माइग्रेशन 001 का उल्टा SQL चलाकर उसकी पंक्ति वर्शन टेबल से हटाता है
```

- माइग्रेशन नाम का संख्या-प्रीफ़िक्स ही उसका वर्शन है (`"001_users"` → 1); गैर-संख्यात्मक प्रीफ़िक्स
  `OrmError::InvalidMigrationName` देता है — उसे कभी 0 नहीं मान लिया जाता।
- `create_table::<E>()` / `drop_table::<E>()` `EntityMeta` से DDL बनाते हैं, और **डायलेक्ट `run()` के समय
  कनेक्शन के `dialect()` से तय होता है**, इसलिए माइग्रेशन सूची कनेक्शन स्ट्रिंग से अलग रहती है। हाथ से
  लिखे SQL (ALTER, डेटा भरना) के लिए `ecat_orm::migrate::MigrationBuilder::new(|d| ...)` इस्तेमाल करें।
- उल्टे SQL के बिना किसी माइग्रेशन पर `down` बुलाने से `OrmError::MigrationIrreversible` आता है —
  "DROP कर फिर से CREATE" से डेटा चला जाता है, और वह कॉलर की ओर से अनुमान नहीं लगाया जाता।
- वर्शन टेबल `Migrator` ख़ुद बनाता है (MSSQL में `CREATE TABLE IF NOT EXISTS` नहीं है, इसलिए पहले
  मौजूदगी जाँची जाती है)।
