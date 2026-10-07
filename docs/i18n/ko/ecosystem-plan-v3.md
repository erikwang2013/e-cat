# e-cat 생태계 계획 v3 — 최종 평가

> **업데이트 (2026-08-07, v2.3.3)**: 남은 격차 #1「mTLS transport 연동」완료 — `HttpServer::tls` / `GrpcServer::tls`가 tokio-rustls / tonic rustls 기반으로 실제 동작(CA 검증과 클라이언트 인증서 강제 지원); 격차 #2(Redis rate limit), #3(GitLab CI)은 이전에 v2.3.0과 함께 완료. 계획에 명시된 격차가 이로써 전부 구현되었습니다.

> **업데이트(2026-10-07, v5.0.0)**: v4.0 계획이 모두 완료되었습니다 — `ecat-orm` / `ecat-orm-derive`, `ecat-data-mssql`, 풀 강화(`CircuitBreakerExecutor` / `RdbmsRouting`), 관측성 3개 feature(`metrics` / `health` / `tracing`)가 모두 구현되었습니다.

**버전:** 2.4.2  
**날짜:** 2026-08-01  
**crate 총수:** 56 · 모든 계획 완료

---

## 현재 커버리지

| 영역 | 구현됨 | 커버리지 |
|------|--------|--------|
| 전송 계층 | HTTP (axum), gRPC (tonic), WebSocket | 100% |
| 인코딩 | JSON, Protobuf | 100% |
| 미들웨어 | Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, Auth×3 | 100% |
| 설정 | env, file (JSON/YAML), Consul KV, 암호화 (XOR) | 100% |
| 등록 센터 | memory, Consul, etcd | 100% |
| 보안 | 공격 탐지, JWT, API Key, OAuth2, TLS 클라이언트 인증서, mTLS | 95% |
| 통신 | TLS 클라이언트 인증서 — 모든 데이터 백엔드 지원 | 95% |
| 서비스 통신 | HTTP Client, gRPC Client, Resolver, LoadBalancer | 95% |
| 데이터 | RDBMS (sqlx), Redis, OpenSearch, Elasticsearch, ClickHouse, Memcached, Neo4j, NebulaGraph, ArangoDB, InfluxDB, IoTDB, QuestDB — 전부 Config 파일 설정 지원 | 95% |
| 메시지 | MessageQueue trait, InMemory, Kafka, EventBus | 100% |
| 관측성 | tracing, Prometheus, Health, 분산 추적 | 100% |
| DevOps | CLI, Dockerfile, K8s, Helm, GitHub Actions, Bench, Testing | 95% |
| API 도구 | OpenAPI, Versioning, GraphQL | 100% |

---

## 남은 격차

### 할 가치 있는 것 (3개)

| # | 격차 | 가치 | 작업량 |
|---|------|------|--------|
| 1 | **mTLS transport 연동** | TlsConfig는 이미 있으며, HttpServer/GrpcServer에 미연동 | 소 |
| 2 | **Redis rate limit 백엔드** | RateLimitLayer가 메모리 전용, 다중 인스턴스는 공유 필요 | 소 |
| 3 | **GitLab CI 템플릿** | GitHub Actions는 이미 있음 | 소 |

### 하지 않아도 되는 것 (2개)

| # | 격차 | 이유 |
|---|------|------|
| 4 | 설정 AES-GCM | 현재 XOR로 충분 |
| 5 | 서비스 메시/API 게이트웨이 | 커뮤니티에 맡김(Linkerd/Kong/K8s) |

---

## 판정

**e-cat은 프로덕션 사용 가능한 성숙도에 도달했습니다.** 56개 crate가 마이크로서비스 풀스택을 커버합니다: 전송 → 미들웨어 → 서비스 디스커버리 → 설정 → 보안 → 데이터 → 메시지 → 관측성 → DevOps → API 도구. 남은 3개 격차는 소규모 작업량 최적화이며, 구조적 결함은 없습니다.

## 데이터 백엔드 커버리지 (16개)

| 카테고리 | 데이터베이스 | Crate | 드라이버 방식 |
|------|--------|-------|----------|
| RDBMS | SQLite/PostgreSQL/MySQL/TiDB | `ecat-data-sqlx` | sqlx (공식 비동기 드라이버) |
| RDBMS | SQL Server | `ecat-data-mssql` | tiberius-ng + deadpool (TDS 드라이버 + 커넥션 풀) |
| 캐시 | Redis | `ecat-data-redis` | redis-rs (공식 드라이버) |
| 캐시 | Memcached | `ecat-data-memcached` | ⚠️ 메모리 구현 (비프로덕션) |
| 문서 | MongoDB | `ecat-data-mongodb` | mongodb (공식 드라이버) |
| 객체 스토리지 | S3 / MinIO | `ecat-data-s3` | HTTP/REST (reqwest+rustls, 자체 구현 SigV4) |
| OLAP | ClickHouse | `ecat-data-clickhouse` | HTTP/REST (reqwest) |
| 검색 | OpenSearch | `ecat-data-opensearch` | HTTP/REST (reqwest) |
| 검색 | Elasticsearch | `ecat-data-elasticsearch` | HTTP/REST (reqwest) |
| 그래프 | Neo4j | `ecat-data-neo4j` | HTTP/REST (reqwest) |
| 그래프 | NebulaGraph | `ecat-data-nebulagraph` | HTTP/REST (reqwest) |
| 그래프 | ArangoDB | `ecat-data-arangodb` | HTTP/REST (reqwest) |
| 시계열 | InfluxDB | `ecat-data-influxdb` | HTTP/REST (reqwest) |
| 시계열 | Apache IoTDB | `ecat-data-iotdb` | HTTP/REST (reqwest) |
| 시계열 | QuestDB | `ecat-data-questdb` | HTTP/REST (reqwest) |
| 시계열 | TDengine | `ecat-data-tdengine` | HTTP/REST (reqwest) |

---

## v4.0 계획 (2026-10-05) — 완전한 ORM과 SQL Server

> 상태: **완료**(v5.0.0). ORM, SQL Server, 풀 강화, 관측성이 모두 구현되었습니다(아래 표 참조).
> 전체 설계는 [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../../../docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md) 참조.

두 가지 구조적 격차를 채웁니다:

1. **완전한 ORM**: 현재 데이터 계층은 수기 SQL RDBMS 클라이언트만 제공하며(`Row` = 컬럼명 + JSON 값),
   엔티티 매핑, 연관, 마이그레이션이 없습니다. v4.0에서 `ecat-orm`
   (엔티티 매크로 / CRUD / 쿼리 빌더 / 연관 사전 로딩 / 조인 쿼리 / 벌크 / 페이지네이션 /
   마이그레이션) + `ecat-orm-derive`를 추가합니다. 통합 `SqlExecutor` trait 위에 구축되어
   **모든 RDBMS 백엔드를 자연스럽게 커버합니다**.
2. **SQL Server 백엔드**: sqlx 본류에는 MSSQL 드라이버가 없습니다(0.7 이전 제거, 재작성 미공개).
   v4.0에서 `ecat-data-mssql`(`tiberius-ng` 0.13 + `deadpool` 0.13)을 추가했습니다——
   데이터 백엔드는 15개에서 **16개**로 늘었습니다(`ecat-data-mssql`).

관련 기반 변경:

| 변경 | 설명 | 상태 |
|---|---|---|
| `ecat-data-sqlx`가 `AnyPool`을 버리고 네이티브 풀로 전환 | 시간 타입 제한 수정(더 이상 CAST 우회 불필요), 드라이버 설치 panic 표면 제거, statement cache 활성화 | ✅ 완료(배치 1, `PgPool`/`MySqlPool`/`SqlitePool` 3종 네이티브 풀) |
| `ecat-data`가 `SqlExecutor` supertrait 분리 | 트랜잭션 내부에서 SQL 실행 가능(현재 `Transaction`은 commit/rollback만). ORM과 읽기/쓰기 분리의 기반 | ✅ 완료(배치 1) |
| 커넥션 풀 강화 | 쿼리 타임아웃, `warm_up()` 웜업, 지능형 recycle, 서킷 브레이커(`ecat-circuit-breaker` 재사용), 읽기/쓰기 분리 `RdbmsRouting` | ✅ 완료(배치 4 — 쿼리 타임아웃과 `warm_up()`은 배치 1, `CircuitBreakerExecutor`와 `RdbmsRouting`은 배치 4) |
| 관측성 | 풀 메트릭을 `ecat-metrics`로, 풀 헬스 체크를 `ecat-health`로, 슬로우 쿼리를 `ecat-tracing`으로(모두 opt-in feature) | ✅ 완료(배치 4, `metrics` / `health` / `tracing` 3개 feature는 기본 비활성) |

**호환성을 깨는 변경**: trait 분리 + `SqlxClient::from_pool` 시그니처 + `AnyPool` 제거——세 가지 모두
**v4.0.0**에 출시되었습니다. 이번 배치(풀 강화와 관측성)는 `RdbmsError`에
`NoAvailableReplica`가 추가된 파괴적 변경이므로 **v5.0.0**으로 출시합니다.
