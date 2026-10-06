<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Ecat API রেফারেন্স

এই পৃষ্ঠায় Ecat ফ্রেমওয়ার্কের ইন্টারফেস (API) সারফেসের সারসংক্ষেপ দেওয়া হয়েছে: পোর্ট কনভেনশন, বিল্ট-ইন এন্ডপয়েন্ট, এরর ফরম্যাট ও এক্সটেনশন ইন্টারফেস। ব্যবসায়িক রাউটিং প্রতিটি সার্ভিস নিজে রেজিস্টার করে।

## পোর্ট কনভেনশন

| প্রোটোকল | লিসেনিং ঠিকানা | ব্যাখ্যা |
|------|----------|------|
| HTTP | `0.0.0.0:8000` | axum রাউটিং, ডিফল্ট উদাহরণ পোর্ট |
| gRPC | `0.0.0.0:9000` | tonic Server, ডিফল্ট উদাহরণ পোর্ট |

## বিল্ট-ইন এন্ডপয়েন্ট

নিচের এন্ডপয়েন্টগুলো ইকোসিস্টেম crates থেকে আসে, সার্ভিসের সাথে মাউন্ট হয়:

| এন্ডপয়েন্ট | উৎস | ব্যাখ্যা |
|------|------|------|
| `/health` | ecat-health | লিভনেস চেক (সার্ভিস নাম, ভার্সন, স্টার্ট টাইম রিটার্ন করে) |
| `/ready` | ecat-health | রেডিনেস চেক (ডিপেন্ডেন্সি প্রস্তুত হলে 200 রিটার্ন) |
| `/metrics` | ecat-metrics | Prometheus মেট্রিক এক্সপোজ (`ecat_http_requests_total` / `ecat_http_request_duration_seconds`) |
| `/{service}/{method}` | ব্যবহারকারী রাউট | উদাহরণ: `/helloworld/ecat` |

> মেট্রিক এন্ডপয়েন্ট পাথে ID-এর মতো উচ্চ-কার্ডিনালিটি হলে `MetricsLayer::new().with_path_fn(...)` দিয়ে নরমালাইজ করুন, মেট্রিক কার্ডিনালিটি বিস্ফোরণ এড়াতে।

## রিকোয়েস্ট প্রসেসিং ফ্লো

```
ক্লায়েন্ট রিকোয়েস্ট
  ├─ HTTP :8000 ──→ axum::Router ─┐
  └─ gRPC :9000 ──→ tonic::Server ─┤
                              ┌─────┴──────┐
                              │ Middleware │  Recovery→Tracing→Logging→Auth→Metrics→Security→CircuitBreaker
                              └─────┬──────┘
                                    ▼
                               Handler（tower::Service）
                                    ▼
                               Response（JSON/Protobuf এনকোডিং）
```

## এরর ফরম্যাট

`ecat-errors` `ErrorCode` + `Error` প্রদান করে, কম্পাইল-টাইমে HTTP স্ট্যাটাস কোড ম্যাপিং:

```rust
use ecat_errors::{Error, ErrorCode};

Error::new(ErrorCode::InvalidArgument, "bad_request", "user id must be positive");
```

এরর রেসপন্স middleware-এর মাধ্যমে JSON (বা Protobuf) এ এনকোড হয়, code / reason / message বহন করে।

## এক্সটেনশন ইন্টারফেস

| ক্ষমতা | Crate | ইন্টারফেস |
|------|-------|------|
| GraphQL | ecat-graphql | `/graphql` এন্ডপয়েন্ট; ফিল্ড প্যারামিটার ও নেস্টেড selection সমর্থন করে, alias, fragment ও মাল্টি-টপ-লেভেল ফিল্ড সমর্থন করে না |
| OpenAPI | ecat-openapi | রাউট থেকে OpenAPI spec জেনারেট |
| WebSocket | ecat-transport-ws | আপগ্রেডেড WS ট্রান্সপোর্ট |
| API ভার্সন রাউটিং | ecat-versioning | `/v1/...` প্রিফিক্স ভার্সন রাউটিং |
| অথেনটিকেশন | ecat-auth | JWT / API Key মিডলওয়্যার; JWT সিক্রেট ≥32 বাইট, চেইনযোগ্য `required_issuer`/`required_audience` |
| gRPC ক্লায়েন্ট | ecat-transport-grpc | সার্ভিস ডিসকভারি ও লোড ব্যালেন্সিং একীভূত |

## সার্ভিস-টু-সার্ভিস কমিউনিকেশন

- `HttpClient`（ecat-client）：সার্ভিস ডিসকভারি ও লোড ব্যালেন্সিং একীভূত, CircuitBreaker সার্কিট ব্রেকার সুরক্ষা
- `GrpcClient`（ecat-transport-grpc）：একই, gRPC প্রোটোকলে
- মিডলওয়্যার ইউনিফাইড `tower::ServiceBuilder` দিয়ে কম্পোজ (Recovery / Tracing / Logging / Timeout / RateLimit / Security / CircuitBreaker / Metrics / Retry / Validate / CORS)

## ডেটা ব্যাকএন্ড ইন্টারফেস

সব ডেটা ব্যাকএন্ড (`ecat-data-*`) ইউনিফাইড trait (`RdbmsClient` ট্রানজেকশনের জন্য, `SqlExecutor` এক্সিকিউশন ও ডায়ালেক্টের জন্য / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`) অ্যাবস্ট্রাকশনের মাধ্যমে; REST-টাইপ ব্যাকএন্ডগুলো (Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine / OpenSearch / Elasticsearch / S3) `base_url`-এর মাধ্যমে সংশ্লিষ্ট HTTP ইন্টারফেস অ্যাক্সেস করে। সংযোগ কনফিগ দেখুন [ডেটাবেস কনফিগ টিউটোরিয়াল](database-config-tutorial.md)。

## ORM (ecat-orm)

`ecat-orm` এন্টিটি ডিরাইভ ম্যাক্রো, টাইপ-নিরাপদ কোয়েরি বিল্ডার, CRUD, রিলেশন প্রিলোডিং এবং মাইগ্রেশন
প্রদান করে। প্রতিটি ডেটা অপারেশন `&impl SqlExecutor` নেয়, তাই **একই API ক্লায়েন্টের সাথেও এবং
`Transaction`-এর সাথেও কাজ করে**:

```rust
User::find_by_id(&db, 1).await?;              // db: SqlxClient / MssqlClient
let tx = db.transaction().await?;
User::update(&tx, &user).await?;              // tx: Transaction
User::insert(&tx, &user).await?;              // ট্রানজ্যাকশনের ভেতরেও চলে: দুটি স্টেটমেন্ট সেখানেই চলে
tx.commit().await?;
```

SQL ডায়ালেক্ট লেয়ার তৈরি করে: SQLite / PostgreSQL / MySQL / TiDB (`ecat-data-sqlx`) এবং SQL Server
(`ecat-data-mssql`) একই এন্টিটি সংজ্ঞা শেয়ার করে।

### এন্টিটি সংজ্ঞা ও `#[entity(...)]` অ্যাট্রিবিউট ব্যাকরণ

`#[derive(Entity)]` যা তৈরি করে: `Entity::META` (টেবিল / কলাম / রিলেশন / ফ্ল্যাগ), `from_row` /
`to_values` / `pk_value`, এবং প্রতি এন্টিটিতে একটি `XxxRelation` এনাম (ভ্যারিয়েন্ট নাম রিলেশন ফিল্ডের
নামের PascalCase রূপ)।

কনটেইনার অ্যাট্রিবিউট:

| রূপ | অর্থ |
|------|------|
| `#[entity(table = "users")]` | টেবিলের নাম; বাদ দিলে স্ট্রাক্ট নামের snake_case নেওয়া হয় (`User` → `user`, `UserProfile` → `user_profile`) |

কলাম ফিল্ড:

| রূপ | অর্থ |
|------|------|
| `#[entity(column = "user_name")]` | কলাম নামের ওভাররাইড, ডিফল্ট ফিল্ডের নাম |
| `#[entity(pk)]` | প্রাইমারি কী |
| `#[entity(auto_increment)]` | অটো-ইনক্রিমেন্ট (pk বোঝায়); অটো-ইনক্রিমেন্ট কী INSERT-এর কলাম তালিকায় যায় না, ডেটাবেস তা তৈরি করে |
| `#[entity(created_at)]` / `#[entity(updated_at)]` | স্বয়ংক্রিয় টাইমস্ট্যাম্প: insert-এ দুটোই ভরে, update-এ কেবল `updated_at` নবায়ন হয় |
| `#[entity(soft_delete)]` | সফট-ডিলিট কলাম: পড়ার পথ নিজে থেকে তার উপর `IS NULL` শর্ত যোগ করে |
| `#[entity(version)]` | অপটিমিস্টিক লক কলাম: update `version + 1` লেখে ও পুরোনো মানের সাথে মেলায়, সংঘর্ষে `OrmError::OptimisticLockConflict` ফেরে |

রিলেশন ফিল্ড (**কনটেইনার টাইপ কঠোর শর্ত**; খালি এন্টিটি টাইপ কম্পাইল-টাইমে প্রত্যাখ্যাত হয় — তা
"পাওয়া যায়নি" প্রকাশ করতে পারে না):

| রূপ | ফিল্ড টাইপ | অর্থ |
|------|----------|------|
| `#[entity(has_many = "Post", foreign_key = "user_id")]` | `Vec<Post>` | এক-থেকে-অনেক: এই টেবিলের `local_key` মান লক্ষ্য টেবিলের `foreign_key` কলামের সাথে মেলানো হয় |
| `#[entity(has_one = "Profile", foreign_key = "user_id")]` | `Option<Profile>` | এক-থেকে-এক; একক-মানের রিলেশন কেবল প্রথম সারি নেয় |
| `#[entity(belongs_to = "Tag", foreign_key = "tag_code")]` | `Option<Tag>` | অনেক-থেকে-এক: এই টেবিলের `foreign_key` মান **লক্ষ্য টেবিলের প্রাইমারি কী**-এর সাথে মেলানো হয় |

`local_key` বাদ দেওয়া যায়: `has_many` / `has_one` ডিফল্টে এই টেবিলের প্রাইমারি কী নেয়, `belongs_to`
লক্ষ্য টেবিলের প্রাইমারি কী। রিলেশন ফিল্ড **কলাম নয়**। ফিল্ড টাইপ থেকে কলাম টাইপের ম্যাপ কেবল
`value::ColumnValue` ইমপ্লে থাকে (ম্যাক্রো সেই টেবিলের দ্বিতীয় কপি রাখে না); অসমর্থিত টাইপ সেই ফিল্ডের
দিকে ইঙ্গিত করা একটি অপূরণ `T: ColumnValue` ত্রুটি দেয়।

### CRUD

| মেথড | মন্তব্য |
|------|------|
| `Entity::insert(&db, &e) -> i64` | insert করে নতুন প্রাইমারি কী ফেরে। MySQL-এর দুই-ধাপ পথ (`INSERT` পরে `SELECT LAST_INSERT_ID()`, যা **কানেকশন-স্কোপড**) `SqlExecutor::execute_then_query`-তে এক ট্রানজ্যাকশনে মোড়া থাকে |
| `Entity::insert_many(&db, &[e]) -> u64` | বাল্ক insert, প্রভাবিত সারি ফেরে (কী ফেরে না: `LAST_INSERT_ID()` প্রথম সারি দেয়, SQLite শেষ — ব্যাকএন্ড জুড়ে অনির্ভরযোগ্য) |
| `Entity::save(&db, &e)` | প্রাইমারি কী "সেট হয়নি" হলে (অটো-ইনক্রিমেন্ট ও মান 0) insert, নাহলে update; `()` ফেরে |
| `Entity::update(&db, &e) -> u64` | প্রাইমারি কী দিয়ে পুরো সারি update। শূন্য প্রভাবিত সারি ত্রুটি: `version` ছাড়া `OrmError::NotFound`, থাকলে `OrmError::OptimisticLockConflict` (অতিরিক্ত কোয়েরি ছাড়া দুটো আলাদা করা যায় না) |
| `Entity::update_many(&db, &[e]) -> u64` | সারি ধরে ধরে update করে সারি যোগ করে; বাল্ক কলে **কোন সারি সংঘর্ষ করেছে তা বলা যায় না** (ইনপুটের চেয়ে কম ফল মানে কোনোটি ভার্সন শর্তে আটকেছে) |
| `Entity::upsert(&db, &e) -> u64` | প্রাইমারি কী দিয়ে upsert (ডায়ালেক্ট অনুযায়ী `ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE`) |
| `Entity::find_by_id(&db, pk) -> Option<Self>` | প্রাইমারি কী দিয়ে একটি সারি আনে; না থাকলে `Ok(None)` |
| `Entity::find_all(&db) -> Vec<Self>` | ফিল্টার ছাড়া সব সারি (**বড় টেবিলে পুরো টেবিল স্ক্যান**; পেজিংয়ের জন্য `paginate` ব্যবহার করুন) |
| `Entity::delete_by_id(&db, pk) -> u64` | `soft_delete` ঘোষিত থাকলে মুছে ফেলার সময় ভরা একটি `UPDATE` পাঠায়; সারি টেবিলেই থাকে, আর আবার মুছলে `NotFound` ফেরে ও সময় নবায়ন হয় না |
| `Entity::hard_delete_by_id(&db, pk) -> u64` | সফট-ডিলিট এড়িয়ে সত্যিই `DELETE` পাঠায় |

### কোয়েরি বিল্ডার

শুরু `User::query()` দিয়ে, যা `Query<User, Unfiltered>` দেয়; ফিল্টার যোগ করলে তা
`Query<User, Filtered>` (টাইপ স্টেট) হয়ে যায় — "WHERE ছাড়া delete" কম্পাইল হয় না।

```rust
use ecat_orm::query::{Op, Order};

let users = User::query()
    .filter("name", Op::Like, "alice%")?        // Eq / Ne / Lt / Le / Gt / Ge / Like
    .filter("email", Op::NotNull, serde_json::json!(null))?
    .filter("id", Op::In, serde_json::json!([1, 2, 3]))?  // In / NotIn অ্যারে মান নেয়
    .order_by("id", Order::Desc)?
    .limit(10)
    .offset(20)
    .fetch(&db)
    .await?;
```

- কলামের নাম `EntityMeta.columns` হোয়াইটলিস্টের বিপরীতে যাচাই হয় (`OrmError::UnknownColumn`); join করা
  টেবিলের কলাম সেখানে নেই — সেগুলোর জন্য `filter_raw("...")` ব্যবহার করুন, এবং মনে রাখুন এটি
  **কোনো ভ্যালিডেশন করে না, তাই ইনপুট বিশ্বস্ত হতে হবে**।
- `with_trashed()` সফট-ডিলিট শর্ত বন্ধ করে (সফট-ডিলিট হওয়া সারিও আসে)।
- `join(JoinType::Left, "posts", "posts.user_id = users.id")` `Inner` / `Left` সমর্থন করে এবং
  `filter_raw`-এর ভেতরে join করা টেবিলের কলামে ফিল্টার করার জন্য। **কলাম তালিকায় টেবিল প্রিফিক্স
  থাকে না**, তাই join করা টেবিলের কলাম নাম মূল টেবিলের সাথে মিললে আসল ডেটাবেস
  `ambiguous column name` জানায় — এমন টেবিল join করুন যাদের কলাম নাম মূল টেবিল থেকে আলাদা (SQLite-এ
  যাচাই করা)।
- `delete_where(&db)` / `hard_delete_where(&db)` একই শর্তে মোছে (সফট-ডিলিট এন্টিটি `UPDATE` দিয়ে যায়)।
- `fetch(&db)` হলো সম্পাদনের প্রবেশবিন্দু; `find_by_id` / `find_all` / `paginate` তার পিছনের একই SQL
  তৈরির পথ পুনর্ব্যবহার করে।

### চাঙ্কিং ও পেজিং

- **বাল্ক রাইট নিজে থেকেই চাঙ্ক হয়**: `insert_many` / `update_many` ডায়ালেক্টের প্রতি-স্টেটমেন্ট
  প্যারামিটার সীমা (যেমন SQL Server-এ 2100) অনুযায়ী ভাগ হয় ও সারি যোগ করে। হাতে চাঙ্ক করতে সেই একই সীমা
  ব্যবহার করুন: `ecat_orm::dialect::lookup(dialect).max_params_per_stmt()`।
- **পেজিং**: `paginate(&db, page, per_page)` 2টি কোয়েরি পাঠায় (COUNT + পেজ আনয়ন) এবং পেজ **1 থেকে
  শুরু**; COUNT একই WHERE / JOIN ব্যবহার করে কিন্তু ORDER BY / LIMIT / OFFSET বাদ দেয়। এটি
  `Page { items, total, page, per_page }` ফেরে, আর `total_pages()` / `has_next()` `total` থেকে
  হিসাব হয়।
- বড় টেবিলে গভীর পেজিংয়ের জন্য `paginate_without_count(&db, page, per_page)` (1 কোয়েরি) ব্যবহার
  করুন: `total` থাকে `None`, এবং `total_pages()` / `has_next()`-ও উত্তর দিতে পারে না — যা গণনা করা যায়
  না, তা অনুমান না করে ঠিক তাই জানানো হয়।

### রিলেশন প্রিলোডিং

```rust
let users = User::query()
    .with(&[UserRelation::Posts, UserRelation::Profile])
    .fetch(&db)
    .await?;
```

- প্রতি রিলেশন **একটি** `IN` কোয়েরি পাঠায় (কী প্রতি-স্টেটমেন্ট প্যারামিটার সীমা ছাড়ালে চাঙ্ক হয়ে),
  আর মূল এন্টিটির কোয়েরি নিজে কোনো join করে না — **N+1 নেই**: 3 জন ব্যবহারকারী ও তাদের posts আনতে
  2টি SELECT (1 মূল + 1 `IN`) লাগে, 4টি নয়।
- `with()` ছাড়া রিলেশন ফিল্ড খালি থাকে (`Vec::new()` / `None`) এবং আগের লোডের পুরোনো মান কখনো বহন
  করে না; অবশিষ্টাংশ মুছতেই প্রতি মূল এন্টিটিতে `set_relation` ডাকা হয় (খালি ফলেও)।
- একক-মানের রিলেশন (`has_one` / `belongs_to`) কেবল প্রথম সারি নেয়।

### মাইগ্রেশন

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Migrator, create_table};

Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .add("002_posts", create_table::<Post>())
    .status().await?;      // MigrationStatus { applied, pending, unknown }
    .run().await?;         // অপেক্ষমাণগুলো প্রয়োগ করে ও প্রতিটি ভার্সন টেবিলে লিখে রাখে; আবার চালানো idempotent
    .down(1).await?;       // 001 মাইগ্রেশনের উল্টো SQL চালায় ও ভার্সন টেবিল থেকে তার সারি মোছে
```

- মাইগ্রেশন নামের সংখ্যাসূচক প্রিফিক্সই তার ভার্সন (`"001_users"` → 1); অ-সংখ্যাসূচক প্রিফিক্স
  `OrmError::InvalidMigrationName` দেয় — তাকে কখনো 0 ধরে নেওয়া হয় না।
- `create_table::<E>()` / `drop_table::<E>()` `EntityMeta` থেকে DDL তৈরি করে, আর **ডায়ালেক্ট `run()`-এর
  সময় কানেকশনের `dialect()` থেকে নির্ধারিত হয়**, তাই মাইগ্রেশন তালিকা কানেকশন স্ট্রিং থেকে আলাদা থাকে।
  হাতে লেখা SQL (ALTER, ডেটা পূরণ) জন্য `ecat_orm::migrate::MigrationBuilder::new(|d| ...)` ব্যবহার
  করুন।
- উল্টো SQL ছাড়া মাইগ্রেশনে `down` ডাকলে `OrmError::MigrationIrreversible` আসে — "DROP করে আবার CREATE"
  করলে ডেটা হারায়, আর তা কলারের হয়ে অনুমান করা হয় না।
- ভার্সন টেবিল `Migrator` নিজেই তৈরি করে (MSSQL-এ `CREATE TABLE IF NOT EXISTS` নেই, তাই আগে অস্তিত্ব
  যাচাই করা হয়)।
