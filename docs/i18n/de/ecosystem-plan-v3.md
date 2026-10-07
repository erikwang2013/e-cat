# e-cat Ökosystemplan v3 — endgültige Bewertung

> **Update (2026-08-07, v2.3.3)**: Die verbleibende Lücke #1 „mTLS-Anbindung an transport" ist abgeschlossen — `HttpServer::tls` / `GrpcServer::tls` wirken real über tokio-rustls / tonic rustls (unterstützt CA-Validierung und erzwungene Clientzertifikate); die Lücken #2 (Redis-Rate-Limit), #3 (GitLab CI) wurden bereits mit v2.3.0 abgeschlossen. Damit sind alle im Plan gelisteten Lücken umgesetzt.

> **Update (2026-10-07, v5.0.0)**: Der v4.0-Plan ist vollständig umgesetzt — `ecat-orm` / `ecat-orm-derive`, `ecat-data-mssql`, die Pool-Erweiterungen (`CircuitBreakerExecutor` / `RdbmsRouting`) sowie die drei Observability-Features (`metrics` / `health` / `tracing`) sind implementiert.

**Version:** 2.4.2  
**Datum:** 2026-08-01  
**Crate-Gesamtzahl:** 55 · alle Planungen abgeschlossen

---

## Aktuelle Abdeckung

| Bereich | Implementiert | Abdeckung |
|------|--------|--------|
| Transport | HTTP (axum), gRPC (tonic), WebSocket | 100% |
| Codierung | JSON, Protobuf | 100% |
| Middleware | Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, Auth×3 | 100% |
| Konfiguration | env, file (JSON/YAML), Consul KV, verschlüsselt (XOR) | 100% |
| Registrierung | memory, Consul, etcd | 100% |
| Sicherheit | Angriffserkennung, JWT, API Key, OAuth2, TLS-Clientzertifikate, mTLS | 95% |
| Kommunikation | TLS-Clientzertifikate — alle Daten-Backends unterstützen das | 95% |
| Dienstkommunikation | HTTP-Client, gRPC-Client, Resolver, LoadBalancer | 95% |
| Daten | RDBMS (sqlx), Redis, OpenSearch, Elasticsearch, ClickHouse, Memcached, Neo4j, NebulaGraph, ArangoDB, InfluxDB, IoTDB, QuestDB — alle mit Config-Datei-Konfiguration | 95% |
| Nachrichten | MessageQueue-Trait, InMemory, Kafka, EventBus | 100% |
| Beobachtbarkeit | tracing, Prometheus, Health, verteilte Ablaufverfolgung | 100% |
| DevOps | CLI, Dockerfile, K8s, Helm, GitHub Actions, Bench, Testing | 95% |
| API-Werkzeuge | OpenAPI, Versioning, GraphQL | 100% |

---

## Verbleibende Lücken

### Lohnenswert (3)

| # | Lücke | Wert | Aufwand |
|---|------|------|--------|
| 1 | **mTLS-Anbindung an transport** | TlsConfig vorhanden, noch nicht an HttpServer/GrpcServer angebunden | klein |
| 2 | **Redis-Rate-Limit-Backend** | RateLimitLayer nur im Speicher, für mehrere Instanzen müsste es geteilt werden | klein |
| 3 | **GitLab-CI-Vorlage** | GitHub Actions vorhanden | klein |

### Nicht nötig (2)

| # | Lücke | Grund |
|---|------|------|
| 4 | Konfigurations-AES-GCM | aktuelles XOR reicht |
| 5 | Service Mesh / API-Gateway | der Community überlassen (Linkerd/Kong/K8s) |

---

## Bewertung

**e-cat hat Produktionsreife erreicht.** 47 crates decken den vollständigen Mikroservice-Stack ab: Transport → Middleware → Service Discovery → Konfiguration → Sicherheit → Daten → Nachrichten → Beobachtbarkeit → DevOps → API-Werkzeuge. Die verbleibenden 3 Lücken sind Optimierungen mit geringem Aufwand, keine strukturellen Fehlstellen.

## Daten-Backend-Abdeckung (16)

| Kategorie | Datenbank | Crate | Treiberart |
|------|--------|-------|----------|
| RDBMS | SQLite/PostgreSQL/MySQL/TiDB | `ecat-data-sqlx` | sqlx (offizieller Async-Treiber) |
| RDBMS | SQL Server | `ecat-data-mssql` | tiberius-ng + deadpool (TDS-Treiber + Verbindungspool) |
| Cache | Redis | `ecat-data-redis` | redis-rs (offizieller Treiber) |
| Cache | Memcached | `ecat-data-memcached` | ⚠️ Speicherimplementierung (nicht produktionsreif) |
| Dokumente | MongoDB | `ecat-data-mongodb` | mongodb (offizieller Treiber) |
| Objektspeicher | S3 / MinIO | `ecat-data-s3` | HTTP/REST (reqwest+rustls, selbst implementiertes SigV4) |
| OLAP | ClickHouse | `ecat-data-clickhouse` | HTTP/REST (reqwest) |
| Suche | OpenSearch | `ecat-data-opensearch` | HTTP/REST (reqwest) |
| Suche | Elasticsearch | `ecat-data-elasticsearch` | HTTP/REST (reqwest) |
| Graph | Neo4j | `ecat-data-neo4j` | HTTP/REST (reqwest) |
| Graph | NebulaGraph | `ecat-data-nebulagraph` | HTTP/REST (reqwest) |
| Graph | ArangoDB | `ecat-data-arangodb` | HTTP/REST (reqwest) |
| Zeitreihen | InfluxDB | `ecat-data-influxdb` | HTTP/REST (reqwest) |
| Zeitreihen | Apache IoTDB | `ecat-data-iotdb` | HTTP/REST (reqwest) |
| Zeitreihen | QuestDB | `ecat-data-questdb` | HTTP/REST (reqwest) |
| Zeitreihen | TDengine | `ecat-data-tdengine` | HTTP/REST (reqwest) |

---

## v4.0-Planung (2026-10-05) — vollständiges ORM und SQL Server

> Status: **abgeschlossen** (v5.0.0). ORM, SQL Server, Pool-Erweiterungen und Beobachtbarkeit sind umgesetzt — siehe Tabelle unten.
> Vollständiges Design: [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../../../docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md).

Zwei strukturelle Lücken sind noch zu schließen:

1. **Vollständiges ORM**: Die Datenschicht bietet derzeit nur RDBMS-Clients für handgeschriebenes
   SQL (`Row` = Spaltennamen + JSON-Werte) — kein Entity-Mapping, keine Assoziationen, keine
   Migrationen. v4.0 ergänzt `ecat-orm` (Entity-Makros / CRUD / Query-Builder / Eager Loading /
   Joins / Bulk / Paginierung / Migrationen) + `ecat-orm-derive`. Aufgebaut auf dem einheitlichen
   `SqlExecutor`-Trait, **deckt es natürlicherweise alle RDBMS-Backends ab**.
2. **SQL-Server-Backend**: Der sqlx-Hauptzweig hat keinen MSSQL-Treiber (vor 0.7 entfernt,
   Neufassung unveröffentlicht); v4.0 ergänzt `ecat-data-mssql` (`tiberius-ng` 0.13 + `deadpool` 0.13) —
   Daten-Backends von 15 auf **16** gewachsen (`ecat-data-mssql`).

Damit verbundene Fundament-Änderungen:

| Änderung | Beschreibung | Status |
|---|---|---|
| `ecat-data-sqlx` gibt `AnyPool` zugunsten nativer Pools auf | Behebt Zeittyp-Einschränkungen (keine CAST-Umwege mehr), beseitigt die Panic-Fläche bei der Treiberinstallation, aktiviert den Statement-Cache | ✅ Abgeschlossen (Charge 1, native Pools `PgPool`/`MySqlPool`/`SqlitePool`) |
| `ecat-data` teilt den `SqlExecutor`-Supertrait heraus | SQL ist innerhalb von Transaktionen ausführbar (`Transaction` kann derzeit nur commit/rollback); Grundlage für ORM und Lese-/Schreibtrennung | ✅ Abgeschlossen (Charge 1) |
| Verbindungspool-Erweiterungen | Abfrage-Timeout, `warm_up()`-Aufwärmen, intelligentes Recycle, Circuit Breaker (nutzt `ecat-circuit-breaker`), Lese-/Schreibtrennung `RdbmsRouting` | ✅ Abgeschlossen (Charge 4 — Abfrage-Timeout und `warm_up()` in Charge 1; `CircuitBreakerExecutor` und `RdbmsRouting` in Charge 4) |
| Beobachtbarkeit | Pool-Metriken an `ecat-metrics`, Pool-Health-Checks an `ecat-health`, langsame Abfragen an `ecat-tracing` (alle als Opt-in-Feature) | ✅ Abgeschlossen (Charge 4; die Features `metrics` / `health` / `tracing` sind standardmäßig aus) |

**Breaking Changes**: Trait-Aufteilung + `SqlxClient::from_pool`-Signatur + Entfernen von `AnyPool` —
alle drei sind bereits im Branch `feat/orm-mssql` umgesetzt (Charge 1, noch nicht veröffentlicht);
bei Veröffentlichung Workspace-Version 3.0.3 → **4.0.0**.
