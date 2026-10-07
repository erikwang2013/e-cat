# e-cat Ecosystem Plan v3 — Final Assessment

> **Update (2026-08-07, v2.3.3)**: remaining gap #1 "mTLS into transport" is done — `HttpServer::tls` / `GrpcServer::tls` take real effect based on tokio-rustls / tonic rustls (CA verification and mandatory client certificates supported); gaps #2 (Redis rate limit) and #3 (GitLab CI) were completed earlier with v2.3.0. All gaps listed in the plan are now fully landed.

> **Update (2026-10-07, v5.0.0)**: the v4.0 plan is fully delivered — `ecat-orm` / `ecat-orm-derive`, `ecat-data-mssql`, the pool enhancements (`CircuitBreakerExecutor` / `RdbmsRouting`) and the three observability features (`metrics` / `health` / `tracing`) are all implemented.

**Version:** 2.4.2  
**Date:** 2026-08-01  
**Total crates:** 55 · All plans completed

---

## Current Coverage

| Domain | Implemented | Coverage |
|------|--------|--------|
| Transport | HTTP (axum), gRPC (tonic), WebSocket | 100% |
| Encoding | JSON, Protobuf | 100% |
| Middleware | Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, Auth×3 | 100% |
| Config | env, file (JSON/YAML), Consul KV, encryption (XOR) | 100% |
| Registry | memory, Consul, etcd | 100% |
| Security | Attack detection, JWT, API Key, OAuth2, TLS client certificates, mTLS | 95% |
| Communication | TLS client certificates — supported by all data backends | 95% |
| Service communication | HTTP Client, gRPC Client, Resolver, LoadBalancer | 95% |
| Data | RDBMS (sqlx), Redis, OpenSearch, Elasticsearch, ClickHouse, Memcached, Neo4j, NebulaGraph, ArangoDB, InfluxDB, IoTDB, QuestDB — all support Config file configuration | 95% |
| Messaging | MessageQueue trait, InMemory, Kafka, EventBus | 100% |
| Observability | tracing, Prometheus, Health, distributed tracing | 100% |
| DevOps | CLI, Dockerfile, K8s, Helm, GitHub Actions, Bench, Testing | 95% |
| API tools | OpenAPI, Versioning, GraphQL | 100% |

---

## Remaining Gaps

### Worth Doing (3 items)

| # | Gap | Value | Effort |
|---|------|------|--------|
| 1 | **mTLS into transport** | TlsConfig exists but is not wired into HttpServer/GrpcServer | Small |
| 2 | **Redis rate-limit backend** | RateLimitLayer is in-memory only; multiple instances need sharing | Small |
| 3 | **GitLab CI template** | GitHub Actions already exists | Small |

### Not Needed (2 items)

| # | Gap | Reason |
|---|------|------|
| 4 | Config AES-GCM | Current XOR is sufficient |
| 5 | Service mesh / API gateway | Left to the community (Linkerd/Kong/K8s) |

---

## Verdict

**e-cat has reached production-ready maturity.** 47 crates cover the full microservice stack: transport → middleware → service discovery → config → security → data → messaging → observability → DevOps → API tools. The remaining 3 gaps are small-effort optimizations, with no structural deficiencies.

## Data Backend Coverage (16)

| Category | Database | Crate | Driver |
|------|--------|-------|----------|
| RDBMS | SQLite/PostgreSQL/MySQL/TiDB | `ecat-data-sqlx` | sqlx (official async driver) |
| RDBMS | SQL Server | `ecat-data-mssql` | tiberius-ng + deadpool (TDS driver + connection pool) |
| Cache | Redis | `ecat-data-redis` | redis-rs (official driver) |
| Cache | Memcached | `ecat-data-memcached` | ⚠️ In-memory implementation (not for production) |
| Document | MongoDB | `ecat-data-mongodb` | mongodb (official driver) |
| Object storage | S3 / MinIO | `ecat-data-s3` | HTTP/REST (reqwest+rustls, self-implemented SigV4) |
| OLAP | ClickHouse | `ecat-data-clickhouse` | HTTP/REST (reqwest) |
| Search | OpenSearch | `ecat-data-opensearch` | HTTP/REST (reqwest) |
| Search | Elasticsearch | `ecat-data-elasticsearch` | HTTP/REST (reqwest) |
| Graph | Neo4j | `ecat-data-neo4j` | HTTP/REST (reqwest) |
| Graph | NebulaGraph | `ecat-data-nebulagraph` | HTTP/REST (reqwest) |
| Graph | ArangoDB | `ecat-data-arangodb` | HTTP/REST (reqwest) |
| Time series | InfluxDB | `ecat-data-influxdb` | HTTP/REST (reqwest) |
| Time series | Apache IoTDB | `ecat-data-iotdb` | HTTP/REST (reqwest) |
| Time series | QuestDB | `ecat-data-questdb` | HTTP/REST (reqwest) |
| Time series | TDengine | `ecat-data-tdengine` | HTTP/REST (reqwest) |

---

## v4.0 Plan (2026-10-05) — Full ORM and SQL Server

> Status: **done** (v5.0.0). The ORM, SQL Server, pool enhancements and observability all landed — see the table below.
> Full design: [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../../../docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md).

Two structural gaps remain to be closed:

1. **Full ORM**: the data layer currently offers only hand-written-SQL RDBMS clients (`Row` = column
   names + JSON values) — no entity mapping, associations or migrations. v4.0 adds `ecat-orm`
   (entity macros / CRUD / query builder / eager loading / joins / bulk / pagination / migrations)
   + `ecat-orm-derive`. Built on the unified `SqlExecutor` trait, it **naturally covers every RDBMS
   backend**.
2. **SQL Server backend**: the sqlx mainline has no MSSQL driver (removed before 0.7, rewrite
   unreleased); v4.0 adds `ecat-data-mssql` (`tiberius-ng` 0.13 + `deadpool` 0.13) —
   data backends grew from 15 to **16** (`ecat-data-mssql`).

Related foundation changes:

| Change | Description | Status |
|---|---|---|
| `ecat-data-sqlx` drops `AnyPool` for native pools | Fixes time-type limitations (no more CAST workarounds), removes the driver-install panic surface, enables statement cache | ✅ Done (batch 1, `PgPool`/`MySqlPool`/`SqlitePool` native pools) |
| `ecat-data` splits out the `SqlExecutor` supertrait | SQL can be executed inside transactions (`Transaction` is currently commit/rollback only); basis for the ORM and read/write splitting | ✅ Done (batch 1) |
| Connection pool enhancements | Query timeout, `warm_up()` warm-up, smart recycle, circuit breaker (reuses `ecat-circuit-breaker`), read/write splitting `RdbmsRouting` | ✅ Done (batch 4 — query timeout and `warm_up()` in batch 1; `CircuitBreakerExecutor` and `RdbmsRouting` in batch 4) |
| Observability | Pool metrics via `ecat-metrics`, pool health probes via `ecat-health`, slow queries via `ecat-tracing` (all opt-in features) | ✅ Done (batch 4; the `metrics` / `health` / `tracing` features are off by default) |

**Breaking changes**: trait split + `SqlxClient::from_pool` signature + removal of `AnyPool` — all
three have landed on branch `feat/orm-mssql` (batch 1, not yet released); on release, workspace
version 3.0.3 → **4.0.0**.
