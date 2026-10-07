# e-cat इकोसिस्टम योजना v3 — अंतिम मूल्यांकन

> **अपडेट (2026-08-07, v2.3.3)**: शेष अंतराल #1「transport में mTLS एकीकरण」पूर्ण — `HttpServer::tls` / `GrpcServer::tls` tokio-rustls / tonic rustls पर आधारित वास्तविक रूप से प्रभावी है (CA सत्यापन और अनिवार्य क्लाइंट प्रमाणपत्र का समर्थन); अंतराल #2 (Redis रेट-लिमिट)、#3 (GitLab CI) पहले v2.3.0 के साथ पूरे हो चुके थे। योजना में सूचीबद्ध अंतराल अब सभी लागू हैं।

> **अद्यतन (2026-10-07, v5.0.0)**: v4.0 योजना पूरी तरह लागू हो चुकी है — `ecat-orm` / `ecat-orm-derive`, `ecat-data-mssql`, पूल संवर्धन (`CircuitBreakerExecutor` / `RdbmsRouting`) और तीन अवलोकनीयता feature (`metrics` / `health` / `tracing`) सभी कार्यान्वित हैं।

**संस्करण:** 2.4.2  
**दिनांक:** 2026-08-01  
**crate कुल:** 56 · सभी योजनाएँ पूर्ण

---

## वर्तमान कवरेज

| क्षेत्र | लागू | कवरेज |
|------|--------|--------|
| ट्रांसपोर्ट परत | HTTP (axum), gRPC (tonic), WebSocket | 100% |
| एन्कोडिंग | JSON, Protobuf | 100% |
| मिडलवेयर | Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, Auth×3 | 100% |
| कॉन्फ़िगरेशन | env, file (JSON/YAML), Consul KV, एन्क्रिप्शन (XOR) | 100% |
| रजिस्ट्री केंद्र | memory, Consul, etcd | 100% |
| सुरक्षा | हमले की पहचान, JWT, API Key, OAuth2, TLS क्लाइंट प्रमाणपत्र, mTLS | 95% |
| संचार | TLS क्लाइंट प्रमाणपत्र — सभी डेटा बैकएंड समर्थन | 95% |
| सेवा संचार | HTTP क्लाइंट, gRPC क्लाइंट, Resolver, LoadBalancer | 95% |
| डेटा | RDBMS (sqlx), Redis, OpenSearch, Elasticsearch, ClickHouse, Memcached, Neo4j, NebulaGraph, ArangoDB, InfluxDB, IoTDB, QuestDB — सभी Config फ़ाइल कॉन्फ़िगरेशन का समर्थन | 95% |
| संदेश | MessageQueue trait, InMemory, Kafka, EventBus | 100% |
| अवलोकनीयता | tracing, Prometheus, Health, वितरित ट्रेसिंग | 100% |
| DevOps | CLI, Dockerfile, K8s, Helm, GitHub Actions, Bench, Testing | 95% |
| API उपकरण | OpenAPI, Versioning, GraphQL | 100% |

---

## शेष अंतराल

### करने योग्य (3)

| # | अंतराल | मूल्य | कार्य-मात्रा |
|---|------|------|--------|
| 1 | **transport में mTLS एकीकरण** | TlsConfig मौजूद है, HttpServer/GrpcServer में नहीं जुड़ा | छोटा |
| 2 | **Redis रेट-लिमिट बैकएंड** | RateLimitLayer केवल मेमोरी, मल्टी-इंस्टेंस के लिए साझा करना आवश्यक | छोटा |
| 3 | **GitLab CI टेम्पलेट** | GitHub Actions मौजूद है | छोटा |

### आवश्यक नहीं (2)

| # | अंतराल | कारण |
|---|------|------|
| 4 | कॉन्फ़िगरेशन AES-GCM | वर्तमान XOR पर्याप्त है |
| 5 | सर्विस मेश/API गेटवे | समुदाय पर छोड़ें (Linkerd/Kong/K8s) |

---

## निर्णय

**e-cat प्रोडक्शन-रेडी परिपक्वता तक पहुँच चुका है।** 56 crates माइक्रोसर्विस पूर्ण स्टैक को कवर करते हैं: ट्रांसपोर्ट → मिडलवेयर → सेवा खोज → कॉन्फ़िगरेशन → सुरक्षा → डेटा → संदेश → अवलोकनीयता → DevOps → API उपकरण। शेष 3 अंतराल छोटे कार्य-मात्रा के अनुकूलन हैं, कोई संरचनात्मक कमी नहीं।

## डेटा बैकएंड कवरेज (16)

| श्रेणी | डेटाबेस | Crate | ड्राइवर विधि |
|------|--------|-------|----------|
| RDBMS | SQLite/PostgreSQL/MySQL/TiDB | `ecat-data-sqlx` | sqlx (आधिकारिक एसिंक ड्राइवर) |
| RDBMS | SQL Server | `ecat-data-mssql` | tiberius-ng + deadpool (TDS ड्राइवर + कनेक्शन पूल) |
| कैश | Redis | `ecat-data-redis` | redis-rs (आधिकारिक ड्राइवर) |
| कैश | Memcached | `ecat-data-memcached` | ⚠️ मेमोरी कार्यान्वयन (प्रोडक्शन नहीं) |
| दस्तावेज़ | MongoDB | `ecat-data-mongodb` | mongodb (आधिकारिक ड्राइवर) |
| ऑब्जेक्ट स्टोरेज | S3 / MinIO | `ecat-data-s3` | HTTP/REST (reqwest+rustls, स्व-कार्यान्वित SigV4) |
| OLAP | ClickHouse | `ecat-data-clickhouse` | HTTP/REST (reqwest) |
| खोज | OpenSearch | `ecat-data-opensearch` | HTTP/REST (reqwest) |
| खोज | Elasticsearch | `ecat-data-elasticsearch` | HTTP/REST (reqwest) |
| ग्राफ | Neo4j | `ecat-data-neo4j` | HTTP/REST (reqwest) |
| ग्राफ | NebulaGraph | `ecat-data-nebulagraph` | HTTP/REST (reqwest) |
| ग्राफ | ArangoDB | `ecat-data-arangodb` | HTTP/REST (reqwest) |
| टाइम-सीरीज़ | InfluxDB | `ecat-data-influxdb` | HTTP/REST (reqwest) |
| टाइम-सीरीज़ | Apache IoTDB | `ecat-data-iotdb` | HTTP/REST (reqwest) |
| टाइम-सीरीज़ | QuestDB | `ecat-data-questdb` | HTTP/REST (reqwest) |
| टाइम-सीरीज़ | TDengine | `ecat-data-tdengine` | HTTP/REST (reqwest) |

---

## v4.0 योजना (2026-10-05) — पूर्ण ORM और SQL Server

> स्थिति: **पूर्ण** (v5.0.0)। ORM, SQL Server, पूल संवर्धन और अवलोकनीयता सभी लागू हो चुके हैं — नीचे तालिका देखें।
> पूर्ण डिज़ाइन: [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../../../docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md)।

दो संरचनात्मक अंतराल भरने बाकी हैं:

1. **पूर्ण ORM**: वर्तमान डेटा परत में केवल हस्तलिखित SQL वाले RDBMS क्लाइंट हैं
   (`Row` = स्तंभ नाम + JSON मान) — कोई एंटिटी मैपिंग, संबंध या माइग्रेशन नहीं। v4.0 में जुड़ते हैं
   `ecat-orm` (एंटिटी मैक्रो / CRUD / क्वेरी बिल्डर / संबंध प्री-लोडिंग / जॉइन क्वेरी / बल्क /
   पेजिनेशन / माइग्रेशन) + `ecat-orm-derive`। एकीकृत `SqlExecutor` trait पर निर्मित, **सभी RDBMS
   बैकएंड को स्वाभाविक रूप से कवर करता है**।
2. **SQL Server बैकएंड**: sqlx मुख्य शाखा में MSSQL ड्राइवर नहीं है (0.7 से पहले हटा दिया गया,
   पुनर्लेखन अप्रकाशित); v4.0 में जुड़ता है `ecat-data-mssql` (`tiberius-ng` 0.13 + `deadpool` 0.13) —
   डेटा बैकएंड 15 से **16** हो गए (`ecat-data-mssql`)।

संबंधित आधारभूत परिवर्तन:

| परिवर्तन | विवरण | स्थिति |
|---|---|---|
| `ecat-data-sqlx` `AnyPool` छोड़कर नेटिव पूल अपनाता है | समय-प्रकार की सीमाएँ ठीक करता है (अब CAST बायपास की ज़रूरत नहीं), ड्राइवर इंस्टॉल panic सतह हटाता है, statement cache सक्षम करता है | ✅ पूर्ण (बैच 1, `PgPool`/`MySqlPool`/`SqlitePool` तीन नेटिव पूल) |
| `ecat-data` `SqlExecutor` supertrait अलग करता है | लेन-देन के भीतर SQL निष्पादन (वर्तमान `Transaction` केवल commit/rollback); ORM और पढ़ने/लिखने के पृथक्करण का आधार | ✅ पूर्ण (बैच 1) |
| कनेक्शन पूल संवर्धन | क्वेरी टाइमआउट, `warm_up()` वार्म-अप, स्मार्ट recycle, सर्किट ब्रेकर (`ecat-circuit-breaker` का पुनःउपयोग), पढ़ने/लिखने का पृथक्करण `RdbmsRouting` | ✅ पूर्ण (बैच 4 — क्वेरी टाइमआउट व `warm_up()` बैच 1 में; `CircuitBreakerExecutor` व `RdbmsRouting` बैच 4 में) |
| अवलोकनीयता | पूल मेट्रिक्स `ecat-metrics` को, पूल हेल्थ चेक `ecat-health` को, स्लो क्वेरी `ecat-tracing` को (सभी opt-in feature) | ✅ पूर्ण (बैच 4; `metrics` / `health` / `tracing` तीनों feature डिफ़ॉल्ट रूप से बंद) |

**ब्रेकिंग परिवर्तन**: trait विभाजन + `SqlxClient::from_pool` सिग्नेचर + `AnyPool` हटाना——तीनों
**v4.0.0** में जारी हो चुके। यह बैच (पूल सुधार और अवलोकनीयता) `RdbmsError` में जुड़े नए
`NoAvailableReplica` वेरिएंट के कारण **v5.0.0** के रूप में जारी होगा।
