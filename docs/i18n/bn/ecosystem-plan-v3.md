# e-cat ইকোসিস্টেম পরিকল্পনা v3 — চূড়ান্ত মূল্যায়ন

> **আপডেট (2026-08-07, v2.3.3)**: অবশিষ্ট ফাঁক #1「mTLS ট্রান্সপোর্টে যুক্ত」সম্পন্ন হয়েছে——`HttpServer::tls` / `GrpcServer::tls` tokio-rustls / tonic rustls ভিত্তিতে বাস্তবে কার্যকর (CA যাচাই ও বাধ্যতামূলক ক্লায়েন্ট সার্টিফিকেট সমর্থন করে); ফাঁক #2 (Redis রেট-লিমিট)、#3 (GitLab CI) আগে v2.3.0-এর সাথে সম্পন্ন হয়েছে। পরিকল্পনায় তালিকাভুক্ত সব ফাঁক এই পর্যন্ত সম্পন্ন।

> **হালনাগাদ (2026-10-07, v5.0.0)**: v4.0 পরিকল্পনা পুরোপুরি বাস্তবায়িত — `ecat-orm` / `ecat-orm-derive`, `ecat-data-mssql`, পুল উন্নয়ন (`CircuitBreakerExecutor` / `RdbmsRouting`) এবং তিনটি অবজারভেবিলিটি feature (`metrics` / `health` / `tracing`) সবই কার্যকর।

**ভার্সন:** 2.4.2  
**তারিখ:** 2026-08-01  
**crate মোট:** 55 · সব পরিকল্পনা সম্পন্ন

---

## বর্তমান কভারেজ

| ক্ষেত্র | বাস্তবায়িত | কভারেজ |
|------|--------|--------|
| ট্রান্সপোর্ট স্তর | HTTP (axum), gRPC (tonic), WebSocket | 100% |
| এনকোডিং | JSON, Protobuf | 100% |
| মিডলওয়্যার | Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, Auth×3 | 100% |
| কনফিগ | env, file (JSON/YAML), Consul KV, এনক্রিপশন (XOR) | 100% |
| রেজিস্ট্রি সেন্টার | memory, Consul, etcd | 100% |
| সিকিউরিটি | আক্রমণ সনাক্তকরণ, JWT, API Key, OAuth2, TLS ক্লায়েন্ট সার্টিফিকেট, mTLS | 95% |
| কমিউনিকেশন | TLS ক্লায়েন্ট সার্টিফিকেট — সব ডেটা ব্যাকএন্ড সমর্থন | 95% |
| সার্ভিস কমিউনিকেশন | HTTP Client, gRPC Client, Resolver, LoadBalancer | 95% |
| ডেটা | RDBMS (sqlx), Redis, OpenSearch, Elasticsearch, ClickHouse, Memcached, Neo4j, NebulaGraph, ArangoDB, InfluxDB, IoTDB, QuestDB — সব Config ফাইল কনফিগ সমর্থন | 95% |
| মেসেজ | MessageQueue trait, InMemory, Kafka, EventBus | 100% |
| অবজারভেবিলিটি | tracing, Prometheus, Health, ডিস্ট্রিবিউটেড ট্রেসিং | 100% |
| DevOps | CLI, Dockerfile, K8s, Helm, GitHub Actions, Bench, Testing | 95% |
| API টুল | OpenAPI, Versioning, GraphQL | 100% |

---

## অবশিষ্ট ফাঁক

### করার যোগ্য (৩টি)

| # | ফাঁক | মূল্য | কাজের পরিমাণ |
|---|------|------|--------|
| 1 | **mTLS ট্রান্সপোর্টে যুক্ত** | TlsConfig আছে, HttpServer/GrpcServer-এ যুক্ত হয়নি | ছোট |
| 2 | **Redis রেট-লিমিট ব্যাকএন্ড** | RateLimitLayer শুধুমাত্র মেমরি, মাল্টি-ইনস্ট্যান্সে শেয়ার দরকার | ছোট |
| 3 | **GitLab CI টেমপ্লেট** | GitHub Actions আছে | ছোট |

### করার প্রয়োজন নেই (২টি)

| # | ফাঁক | কারণ |
|---|------|------|
| 4 | কনফিগ AES-GCM | বর্তমান XOR যথেষ্ট |
| 5 | সার্ভিস মেশ/API গেটওয়ে | কমিউনিটির হাতে ছাড়া (Linkerd/Kong/K8s) |

---

## রায়

**e-cat প্রোডাকশন-রেডি পরিপক্বতায় পৌঁছেছে।** 47টি crate মাইক্রোসার্ভিস ফুল-স্ট্যাক কভার করে: ট্রান্সপোর্ট → মিডলওয়্যার → সার্ভিস ডিসকভারি → কনফিগ → সিকিউরিটি → ডেটা → মেসেজ → অবজারভেবিলিটি → DevOps → API টুল। অবশিষ্ট ৩টি ফাঁক ছোট কাজের অপটিমাইজেশন, কোনো কাঠামোগত ঘাটতি নেই।

## ডেটা ব্যাকএন্ড কভারেজ (১৬টি)

| ক্যাটাগরি | ডেটাবেস | Crate | ড্রাইভার পদ্ধতি |
|------|--------|-------|----------|
| RDBMS | SQLite/PostgreSQL/MySQL/TiDB | `ecat-data-sqlx` | sqlx (অফিসিয়াল অ্যাসিংক ড্রাইভার) |
| RDBMS | SQL Server | `ecat-data-mssql` | tiberius-ng + deadpool (TDS ড্রাইভার + কানেকশন পুল) |
| ক্যাশ | Redis | `ecat-data-redis` | redis-rs (অফিসিয়াল ড্রাইভার) |
| ক্যাশ | Memcached | `ecat-data-memcached` | ⚠️ মেমরি-ভিত্তিক (প্রোডাকশন নয়) |
| ডকুমেন্ট | MongoDB | `ecat-data-mongodb` | mongodb (অফিসিয়াল ড্রাইভার) |
| অবজেক্ট স্টোরেজ | S3 / MinIO | `ecat-data-s3` | HTTP/REST (reqwest+rustls, নিজস্ব SigV4) |
| OLAP | ClickHouse | `ecat-data-clickhouse` | HTTP/REST (reqwest) |
| সার্চ | OpenSearch | `ecat-data-opensearch` | HTTP/REST (reqwest) |
| সার্চ | Elasticsearch | `ecat-data-elasticsearch` | HTTP/REST (reqwest) |
| গ্রাফ | Neo4j | `ecat-data-neo4j` | HTTP/REST (reqwest) |
| গ্রাফ | NebulaGraph | `ecat-data-nebulagraph` | HTTP/REST (reqwest) |
| গ্রাফ | ArangoDB | `ecat-data-arangodb` | HTTP/REST (reqwest) |
| টাইম-সিরিজ | InfluxDB | `ecat-data-influxdb` | HTTP/REST (reqwest) |
| টাইম-সিরিজ | Apache IoTDB | `ecat-data-iotdb` | HTTP/REST (reqwest) |
| টাইম-সিরিজ | QuestDB | `ecat-data-questdb` | HTTP/REST (reqwest) |
| টাইম-সিরিজ | TDengine | `ecat-data-tdengine` | HTTP/REST (reqwest) |

---

## v4.0 পরিকল্পনা (2026-10-05) — সম্পূর্ণ ORM ও SQL Server

> অবস্থা: **সম্পন্ন** (v5.0.0)। ORM, SQL Server, পুল উন্নয়ন ও অবজারভেবিলিটি সবই বাস্তবায়িত — নিচের সারণি দেখুন।
> সম্পূর্ণ ডিজাইন: [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../../../docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md)।

দুটি কাঠামোগত ফাঁক বাকি:

1. **সম্পূর্ণ ORM**: বর্তমান ডেটা স্তরে শুধু হাতে লেখা SQL-এর RDBMS ক্লায়েন্ট
   (`Row` = কলামের নাম + JSON মান) — কোনো এনটিটি ম্যাপিং, সম্পর্ক বা মাইগ্রেশন নেই। v4.0-তে যোগ হচ্ছে
   `ecat-orm` (এনটিটি ম্যাক্রো / CRUD / কুয়েরি বিল্ডার / সম্পর্কের প্রি-লোডিং / জয়েন কুয়েরি / বাল্ক /
   পেজিনেশন / মাইগ্রেশন) + `ecat-orm-derive`। ঐক্যবদ্ধ `SqlExecutor` trait-এর উপর নির্মিত,
   **সব RDBMS ব্যাকএন্ড স্বাভাবিকভাবেই কভার করে**।
2. **SQL Server ব্যাকএন্ড**: sqlx মূল শাখায় MSSQL ড্রাইভার নেই (0.7-এর আগে সরানো হয়েছে, পুনর্লিখন
   প্রকাশিত হয়নি); v4.0-তে যোগ হচ্ছে `ecat-data-mssql` (`tiberius-ng` 0.13 + `deadpool` 0.13) —
   ডেটা ব্যাকএন্ড ১৫টি থেকে **১৬টি** হয়েছে (`ecat-data-mssql`)।

সম্পর্কিত ভিত্তি পরিবর্তন:

| পরিবর্তন | বিবরণ | অবস্থা |
|---|---|---|
| `ecat-data-sqlx` `AnyPool` ছেড়ে নেটিভ পুলে | সময়-টাইপ সীমাবদ্ধতা ঠিক করে (আর CAST বাইপাস লাগে না), ড্রাইভার ইনস্টলের panic তলদেশ সরায়, statement cache সক্রিয় করে | ✅ সম্পন্ন (ব্যাচ ১, `PgPool`/`MySqlPool`/`SqlitePool` তিনটি নেটিভ পুল) |
| `ecat-data` `SqlExecutor` supertrait আলাদা করে | ট্রানজ্যাকশনের ভেতরে SQL চালানো যায় (বর্তমানে `Transaction` শুধু commit/rollback); ORM ও রিড/রাইট বিভাজনের ভিত্তি | ✅ সম্পন্ন (ব্যাচ ১) |
| কানেকশন পুল উন্নয়ন | কুয়েরি টাইমআউট, `warm_up()` ওয়ার্ম-আপ, স্মার্ট recycle, সার্কিট ব্রেকার (`ecat-circuit-breaker` পুনঃব্যবহার), রিড/রাইট বিভাজন `RdbmsRouting` | ✅ সম্পন্ন (ব্যাচ ৪ — কুয়েরি টাইমআউট ও `warm_up()` ব্যাচ ১-এ; `CircuitBreakerExecutor` ও `RdbmsRouting` ব্যাচ ৪-এ) |
| অবজারভেবিলিটি | পুল মেট্রিক `ecat-metrics`-এ, পুল হেলথ চেক `ecat-health`-এ, স্লো কোয়েরি `ecat-tracing`-এ (সবই opt-in feature) | ✅ সম্পন্ন (ব্যাচ ৪; `metrics` / `health` / `tracing` তিনটি feature ডিফল্টে বন্ধ) |

**ভাঙনকারী পরিবর্তন**: trait বিভাজন + `SqlxClient::from_pool` সিগনেচার + `AnyPool` অপসারণ——তিনটিই
`feat/orm-mssql` শাখায় ইতিমধ্যে বাস্তবায়িত (ব্যাচ ১, এখনও প্রকাশিত হয়নি); প্রকাশের সময় workspace
ভার্সন 3.0.3 → **4.0.0**।
