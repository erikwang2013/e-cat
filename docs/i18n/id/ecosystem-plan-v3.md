# Rencana Ekosistem e-cat v3 — Evaluasi Akhir

> **Pembaruan (2026-08-07, v2.3.3)**: Sisa kesenjangan #1 "mTLS terhubung ke transport" telah selesai — `HttpServer::tls` / `GrpcServer::tls` benar-benar berfungsi berbasis tokio-rustls / tonic rustls (mendukung verifikasi CA dan pemaksaan sertifikat klien); kesenjangan #2 (rate limit Redis), #3 (CI GitLab) sebelumnya telah selesai bersama v2.3.0. Semua kesenjangan yang tercantum dalam perencanaan hingga saat ini telah diwujudkan.

**Versi:** 2.4.2  
**Tanggal:** 2026-08-01  
**Jumlah crate:** 55 · Semua perencanaan telah selesai

---

## Cakupan Saat Ini

| Bidang | Terimplementasi | Tingkat cakupan |
|------|--------|--------|
| Transport | HTTP (axum), gRPC (tonic), WebSocket | 100% |
| Encoding | JSON, Protobuf | 100% |
| Middleware | Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, Auth×3 | 100% |
| Konfigurasi | env, file (JSON/YAML), Consul KV, enkripsi (XOR) | 100% |
| Registry | memory, Consul, etcd | 100% |
| Keamanan | Deteksi serangan, JWT, API Key, OAuth2, sertifikat klien TLS, mTLS | 95% |
| Komunikasi | Sertifikat klien TLS — semua backend data mendukung | 95% |
| Komunikasi layanan | HTTP Client, gRPC Client, Resolver, LoadBalancer | 95% |
| Data | RDBMS (sqlx), Redis, OpenSearch, Elasticsearch, ClickHouse, Memcached, Neo4j, NebulaGraph, ArangoDB, InfluxDB, IoTDB, QuestDB — semua mendukung konfigurasi file Config | 95% |
| Pesan | trait MessageQueue, InMemory, Kafka, EventBus | 100% |
| Observabilitas | tracing, Prometheus, Health, pelacakan terdistribusi | 100% |
| DevOps | CLI, Dockerfile, K8s, Helm, GitHub Actions, Bench, Testing | 95% |
| Alat API | OpenAPI, Versioning, GraphQL | 100% |

---

## Sisa Kesenjangan

### Layak Dikerjakan (3 Item)

| # | Kesenjangan | Nilai | Beban kerja |
|---|------|------|--------|
| 1 | **mTLS terhubung ke transport** | TlsConfig sudah ada, belum terhubung ke HttpServer/GrpcServer | Kecil |
| 2 | **Backend rate limit Redis** | RateLimitLayer hanya memori, multi-instance perlu berbagi | Kecil |
| 3 | **Template CI GitLab** | Sudah ada GitHub Actions | Kecil |

### Tidak Perlu Dikerjakan (2 Item)

| # | Kesenjangan | Alasan |
|---|------|------|
| 4 | Konfigurasi AES-GCM | XOR saat ini sudah cukup |
| 5 | Service mesh / API gateway | Diserahkan ke komunitas (Linkerd/Kong/K8s) |

---

## Penilaian

**e-cat telah mencapai kematangan siap produksi.** 47 crate mencakup tumpukan lengkap microservice: transport → middleware → service discovery → konfigurasi → keamanan → data → pesan → observabilitas → DevOps → alat API. Sisa 3 kesenjangan adalah optimasi beban kerja kecil, tanpa kekurangan struktural.

## Cakupan Backend Data (15 buah)

| Kategori | Database | Crate | Cara penggerak |
|------|--------|-------|----------|
| RDBMS | SQLite/PostgreSQL/MySQL/TiDB | `ecat-data-sqlx` | sqlx (driver asinkron resmi) |
| Cache | Redis | `ecat-data-redis` | redis-rs (driver resmi) |
| Cache | Memcached | `ecat-data-memcached` | ⚠️ Implementasi memori (bukan produksi) |
| Dokumen | MongoDB | `ecat-data-mongodb` | mongodb (driver resmi) |
| Object storage | S3 / MinIO | `ecat-data-s3` | HTTP/REST (reqwest+rustls, SigV4 implementasi sendiri) |
| OLAP | ClickHouse | `ecat-data-clickhouse` | HTTP/REST (reqwest) |
| Pencarian | OpenSearch | `ecat-data-opensearch` | HTTP/REST (reqwest) |
| Pencarian | Elasticsearch | `ecat-data-elasticsearch` | HTTP/REST (reqwest) |
| Graf | Neo4j | `ecat-data-neo4j` | HTTP/REST (reqwest) |
| Graf | NebulaGraph | `ecat-data-nebulagraph` | HTTP/REST (reqwest) |
| Graf | ArangoDB | `ecat-data-arangodb` | HTTP/REST (reqwest) |
| Time-series | InfluxDB | `ecat-data-influxdb` | HTTP/REST (reqwest) |
| Time-series | Apache IoTDB | `ecat-data-iotdb` | HTTP/REST (reqwest) |
| Time-series | QuestDB | `ecat-data-questdb` | HTTP/REST (reqwest) |
| Time-series | TDengine | `ecat-data-tdengine` | HTTP/REST (reqwest) |

---

## Rencana v4.0 (2026-10-05) — ORM Lengkap dan SQL Server

> Status: **sedang berjalan**. Batch 1 (pemisahan `SqlExecutor` + pool native) telah selesai dan
> digabungkan ke branch `feat/orm-mssql`; ORM, SQL Server, dan peningkatan pool masih tertunda.
> Kemajuan setiap item ditandai di bawah ini.
> Desain lengkap: [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../../../docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md).

Dua kesenjangan struktural masih harus ditutup:

1. **ORM lengkap**: lapisan data saat ini hanya menyediakan klien RDBMS dengan SQL tulis tangan
   (`Row` = nama kolom + nilai JSON) — tanpa pemetaan entitas, relasi, atau migrasi. v4.0 menambahkan
   `ecat-orm` (makro entitas / CRUD / pembuat kueri / pramuat relasi / kueri join / massal / paginasi /
   migrasi) + `ecat-orm-derive`. Dibangun di atas trait `SqlExecutor` yang terpadu, **secara alami
   mencakup semua backend RDBMS**.
2. **Backend SQL Server**: sqlx utama tidak memiliki driver MSSQL (dihapus sebelum 0.7, penulisan ulang
   belum dirilis); v4.0 menambahkan `ecat-data-mssql` (`tiberius-ng` 0.13 + `deadpool` 0.13) —
   backend data **target** dari 15 menjadi **16** (saat ini masih 15, SQL Server belum termasuk).

Perubahan fondasi terkait:

| Perubahan | Keterangan | Status |
|---|---|---|
| `ecat-data-sqlx` beralih dari `AnyPool` ke pool native | Memperbaiki batasan tipe waktu (tidak lagi perlu akal-akalan CAST), menghilangkan permukaan panic saat pemasangan driver, mengaktifkan statement cache | ✅ Selesai (batch 1, tiga pool native `PgPool`/`MySqlPool`/`SqlitePool`) |
| `ecat-data` memisahkan supertrait `SqlExecutor` | SQL dapat dijalankan di dalam transaksi (saat ini `Transaction` hanya bisa commit/rollback); dasar untuk ORM dan pemisahan baca/tulis | ✅ Selesai (batch 1) |
| Peningkatan connection pool | Timeout kueri, pemanasan `warm_up()`, recycle cerdas, pemutus sirkuit (memakai ulang `ecat-circuit-breaker`), pemisahan baca/tulis `RdbmsRouting` | 🚧 Sebagian: timeout kueri dan `warm_up()` selesai; pemutus sirkuit dan pemisahan baca/tulis menunggu batch 4 |
| Observabilitas | Metrik pool ke `ecat-metrics`, pemeriksaan kesehatan pool ke `ecat-health`, kueri lambat ke `ecat-tracing` (semua opt-in feature) | ❌ Belum dikerjakan (batch 4) |

**Perubahan yang merusak**: pemisahan trait + tanda tangan `SqlxClient::from_pool` + penghapusan
`AnyPool` — ketiganya sudah diterapkan di branch `feat/orm-mssql` (batch 1, belum dirilis);
saat rilis, versi workspace 3.0.3 → **4.0.0**.
