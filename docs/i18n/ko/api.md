<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Ecat API 참조

이 페이지는 Ecat 프레임워크의 인터페이스(API) 면을 요약합니다: 포트 규약, 내장 엔드포인트, 오류 형식과 확장 인터페이스. 비즈니스 라우팅은 각 서비스가 직접 등록합니다.

## 포트 규약

| 프로토콜 | 리슨 주소 | 설명 |
|------|----------|------|
| HTTP | `0.0.0.0:8000` | axum 라우팅, 기본 예시 포트 |
| gRPC | `0.0.0.0:9000` | tonic Server, 기본 예시 포트 |

## 내장 엔드포인트

다음 엔드포인트는 생태계 crate가 제공하며, 서비스에 함께 마운트됩니다:

| 엔드포인트 | 출처 | 설명 |
|------|------|------|
| `/health` | ecat-health | 생존 확인(서비스 이름, 버전, 시작 시간 반환) |
| `/ready` | ecat-health | 준비 확인(의존성 준비 후 200 반환) |
| `/metrics` | ecat-metrics | Prometheus 메트릭 노출(`ecat_http_requests_total` / `ecat_http_request_duration_seconds`) |
| `/{service}/{method}` | 사용자 라우팅 | 예시: `/helloworld/ecat` |

> 메트릭 엔드포인트 경로에 ID 등 고기수(高基数) 시나리오는 `MetricsLayer::new().with_path_fn(...)`으로 정규화하여 메트릭 카디널리티 폭발을 방지하세요.

## 요청 처리 흐름

```
클라이언트 요청
  ├─ HTTP :8000 ──→ axum::Router ─┐
  └─ gRPC :9000 ──→ tonic::Server ─┤
                              ┌─────┴──────┐
                              │ Middleware │  Recovery→Tracing→Logging→Auth→Metrics→Security→CircuitBreaker
                              └─────┬──────┘
                                    ▼
                               Handler（tower::Service）
                                    ▼
                               Response（JSON/Protobuf 인코딩）
```

## 오류 형식

`ecat-errors`가 `ErrorCode` + `Error`를 제공하며, 컴파일 타임에 HTTP 상태 코드를 매핑합니다:

```rust
use ecat_errors::{Error, ErrorCode};

Error::new(ErrorCode::InvalidArgument, "bad_request", "user id must be positive");
```

오류 응답은 middleware를 통해 JSON(또는 Protobuf)으로 인코딩되며, code / reason / message를 담습니다.

## 확장 인터페이스

| 기능 | Crate | 인터페이스 |
|------|-------|------|
| GraphQL | ecat-graphql | `/graphql` 엔드포인트; 필드 파라미터와 중첩 selection 지원, 별칭·fragment·다중 최상위 필드 미지원 |
| OpenAPI | ecat-openapi | 라우팅에서 OpenAPI spec 생성 |
| WebSocket | ecat-transport-ws | 업그레이드된 WS 전송 |
| API 버전 라우팅 | ecat-versioning | `/v1/...` 접두사 버전 라우팅 |
| 인증 | ecat-auth | JWT / API Key 미들웨어; JWT 키는 ≥32바이트 필요, 체이닝 `required_issuer`/`required_audience` |
| gRPC 클라이언트 | ecat-transport-grpc | 서비스 디스커버리·로드 밸런싱 통합 |

## 서비스 간 통신

- `HttpClient`(ecat-client): 서비스 디스커버리·로드 밸런싱 통합, CircuitBreaker 회로 차단 보호
- `GrpcClient`(ecat-transport-grpc): 동일, gRPC 프로토콜
- 미들웨어는 `tower::ServiceBuilder`로 통합 조합(Recovery / Tracing / Logging / Timeout / RateLimit / Security / CircuitBreaker / Metrics / Retry / Validate / CORS)

## 데이터 백엔드 인터페이스

모든 데이터 백엔드(`ecat-data-*`)는 통일된 trait(`RdbmsClient`는 트랜잭션, `SqlExecutor`는 실행과 방언 / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`)으로 추상화됩니다; REST 계열 백엔드(Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine / OpenSearch / Elasticsearch / S3)는 `base_url` 기반으로 해당 HTTP 인터페이스에 접근합니다. 연결 설정은 [데이터베이스 설정 튜토리얼](database-config-tutorial.md)을 참조하세요.

## ORM (ecat-orm)

`ecat-orm`은 엔티티 파생 매크로, 타입 안전한 쿼리 빌더, CRUD, 연관 프리로드, 마이그레이션을 제공합니다.
모든 데이터 작업은 `&impl SqlExecutor`를 받으므로 **클라이언트든 `Transaction`이든 같은 API**를 씁니다:

```rust
User::find_by_id(&db, 1).await?;              // db: SqlxClient / MssqlClient
let tx = db.transaction().await?;
User::update(&tx, &user).await?;              // tx: Transaction
User::insert(&tx, &user).await?;              // 트랜잭션 안에서도 삽입 가능: 두 문장이 그 트랜잭션 안에서 실행됩니다
tx.commit().await?;
```

SQL은 방언 계층이 생성합니다. SQLite / PostgreSQL / MySQL / TiDB(`ecat-data-sqlx`)와
SQL Server(`ecat-data-mssql`)가 같은 엔티티 정의를 공유합니다.

### 엔티티 정의와 `#[entity(...)]` 속성 문법

`#[derive(Entity)]`는 `Entity::META`(테이블명 / 컬럼 / 연관 / 플래그), `from_row` / `to_values` /
`pk_value`를 생성하고, 엔티티마다 `XxxRelation` 열거형 하나를 만듭니다(변형 이름은 연관 필드명의
PascalCase).

컨테이너 속성:

| 표기 | 의미 |
|------|------|
| `#[entity(table = "users")]` | 테이블명. 생략하면 구조체 이름의 snake_case(`User` → `user`, `UserProfile` → `user_profile`) |

컬럼 필드:

| 표기 | 의미 |
|------|------|
| `#[entity(column = "user_name")]` | 컬럼명 재정의. 기본값은 필드명 |
| `#[entity(pk)]` | 기본 키 |
| `#[entity(auto_increment)]` | 자동 증가(pk 포함). 자동 증가 기본 키는 삽입 컬럼 목록에서 빠지고 데이터베이스가 생성합니다 |
| `#[entity(created_at)]` / `#[entity(updated_at)]` | 자동 타임스탬프: 삽입 시 둘 다 채우고, 갱신 시 `updated_at`만 새로 씁니다 |
| `#[entity(soft_delete)]` | 소프트 삭제 컬럼: 읽기 경로가 자동으로 그 컬럼의 `IS NULL` 게이트를 붙입니다 |
| `#[entity(version)]` | 낙관적 잠금 컬럼: 갱신은 `version + 1`을 쓰고 이전 값을 비교합니다. 충돌 시 `OrmError::OptimisticLockConflict` |

연관 필드(**컨테이너 타입은 하드 제약**이며, 맨 엔티티 타입은 컴파일 단계에서 거부됩니다 — "찾지 못함"을
표현할 수 없기 때문):

| 표기 | 필드 타입 | 의미 |
|------|----------|------|
| `#[entity(has_many = "Post", foreign_key = "user_id")]` | `Vec<Post>` | 1:N — 이 테이블의 `local_key` 값을 대상의 `foreign_key` 컬럼에서 찾습니다 |
| `#[entity(has_one = "Profile", foreign_key = "user_id")]` | `Option<Profile>` | 1:1 — 단일 값 연관은 첫 행만 취합니다 |
| `#[entity(belongs_to = "Tag", foreign_key = "tag_code")]` | `Option<Tag>` | N:1 — 이 테이블의 `foreign_key` 값을 **대상의 기본 키**에서 찾습니다 |

`local_key`는 생략할 수 있습니다: `has_many` / `has_one`은 이 테이블의 기본 키, `belongs_to`는 대상의
기본 키가 기본값입니다. 연관 필드는 **컬럼이 아닙니다**. 필드 타입에서 컬럼 타입으로의 대응은
`value::ColumnValue`의 impl에만 존재합니다(매크로는 대응표를 복제하지 않습니다). 지원하지 않는 타입은
그 필드를 가리키는 `T: ColumnValue` 미충족 오류로 나타납니다.

### CRUD

| 메서드 | 설명 |
|------|------|
| `Entity::insert(&db, &e) -> i64` | 삽입 후 새 기본 키를 반환합니다. MySQL의 2단계(`INSERT` 뒤에 `SELECT LAST_INSERT_ID()`, 게다가 **연결 범위**)는 `SqlExecutor::execute_then_query`가 하나의 트랜잭션으로 감쌉니다 |
| `Entity::insert_many(&db, &[e]) -> u64` | 대량 삽입, 영향 행 수를 반환합니다(기본 키는 반환하지 않음: `LAST_INSERT_ID()`는 첫 행, SQLite는 마지막 행이라 백엔드 간 의미가 신뢰할 수 없습니다) |
| `Entity::save(&db, &e)` | 기본 키가 "미설정"(자동 증가이면서 값이 0)이면 삽입, 아니면 갱신. `()`를 반환합니다 |
| `Entity::update(&db, &e) -> u64` | 기본 키로 행 전체를 갱신합니다. 영향 0행은 오류: `version`이 없으면 `OrmError::NotFound`, 있으면 `OrmError::OptimisticLockConflict`(추가 조회 없이는 둘을 구분할 수 없습니다) |
| `Entity::update_many(&db, &[e]) -> u64` | 행마다 갱신하고 행 수를 합산합니다. 대량 호출은 **어느 행이** 충돌했는지 알려주지 못합니다(반환 행 수가 입력보다 적으면 누군가 버전 게이트에 걸린 것입니다) |
| `Entity::upsert(&db, &e) -> u64` | 기본 키 기준 upsert(방언별 `ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE`) |
| `Entity::find_by_id(&db, pk) -> Option<Self>` | 기본 키로 한 행 조회. 없으면 `Ok(None)` |
| `Entity::find_all(&db) -> Vec<Self>` | 조건 없는 전체 조회(**큰 테이블에서는 전 테이블 스캔**. 페이징에는 `paginate`를 쓰세요) |
| `Entity::delete_by_id(&db, pk) -> u64` | `soft_delete`가 선언되면 삭제 시각을 설정하는 `UPDATE`를 보냅니다. 행은 테이블에 남고, 다시 삭제하면 `NotFound`이며 시각도 갱신하지 않습니다 |
| `Entity::hard_delete_by_id(&db, pk) -> u64` | 소프트 삭제를 우회해 실제 `DELETE`를 보냅니다 |

### 쿼리 빌더

`User::query()`로 시작하면 `Query<User, Unfiltered>`가 나오고, 필터를 더하면
`Query<User, Filtered>`(타입 상태)가 됩니다 — "WHERE 없이 삭제"는 컴파일되지 않습니다.

```rust
use ecat_orm::query::{Op, Order};

let users = User::query()
    .filter("name", Op::Like, "alice%")?        // Eq / Ne / Lt / Le / Gt / Ge / Like
    .filter("email", Op::NotNull, serde_json::json!(null))?
    .filter("id", Op::In, serde_json::json!([1, 2, 3]))?  // In / NotIn은 배열 값을 받습니다
    .order_by("id", Order::Desc)?
    .limit(10)
    .offset(20)
    .fetch(&db)
    .await?;
```

- 컬럼명은 `EntityMeta.columns` 화이트리스트로 검증합니다(`OrmError::UnknownColumn`). 조인 대상 테이블의
  컬럼은 화이트리스트에 없으므로 `filter_raw("...")`를 쓰는데, 이 함수는 **아무 검증도 하지 않으므로
  입력을 신뢰할 수 있어야 합니다**.
- `with_trashed()`는 소프트 삭제 게이트를 해제합니다(소프트 삭제된 행도 함께 조회).
- `join(JoinType::Left, "posts", "posts.user_id = users.id")`는 `Inner` / `Left`를 지원하며,
  `filter_raw`에서 조인 대상 컬럼으로 걸러내기 위한 것입니다. **컬럼 목록에 테이블 접두사가 붙지 않으므로**
  조인 대상이 주체와 같은 컬럼명을 가지면 실제 데이터베이스는 `ambiguous column name`을 냅니다 —
  주체와 컬럼명이 겹치지 않는 테이블을 조인하세요(SQLite에서 실측).
- `delete_where(&db)` / `hard_delete_where(&db)`는 같은 조건으로 삭제합니다(소프트 삭제 엔티티는 `UPDATE`).
- `fetch(&db)`가 실행 진입점이며 `find_by_id` / `find_all` / `paginate`도 그 뒤의 같은 SQL 생성 경로를 재사용합니다.

### 청크 분할과 페이징

- **대량 쓰기는 자동으로 청크 분할**: `insert_many` / `update_many`는 방언의 문장당 파라미터 상한
  (SQL Server는 2100 등)으로 나누고 행 수를 합산합니다. 직접 나눌 때도 같은 상한을 쓰세요:
  `ecat_orm::dialect::lookup(dialect).max_params_per_stmt()`.
- **페이징**: `paginate(&db, page, per_page)`는 쿼리 2개(COUNT + 페이지 조회)를 보내고 페이지 번호는
  **1부터** 시작합니다. COUNT는 같은 WHERE / JOIN을 재사용하되 ORDER BY / LIMIT / OFFSET을 뺍니다.
  반환값은 `Page { items, total, page, per_page }`이고 `total_pages()` / `has_next()`는 `total`로 계산합니다.
- 큰 테이블의 깊은 페이징에는 `paginate_without_count(&db, page, per_page)`(쿼리 1개): `total`은 `None`이고
  `total_pages()` / `has_next()`도 답을 줄 수 없습니다 — 계산할 수 없는 것은 추측하지 않고 그대로 알립니다.

### 연관 프리로드

```rust
let users = User::query()
    .with(&[UserRelation::Posts, UserRelation::Profile])
    .fetch(&db)
    .await?;
```

- 연관마다 **하나의** `IN` 쿼리를 보내고(키가 문장당 파라미터 상한을 넘으면 청크로 나눔), 주체 쿼리 자체는
  JOIN하지 않습니다 — **N+1 제거**: 사용자 3명과 그 posts를 가져오는 것은 4개가 아니라
  2개의 SELECT(주체 1 + `IN` 1)입니다.
- `with()`를 선언하지 않으면 연관 필드는 비어 있고(`Vec::new()` / `None`) 이전 로드의 낡은 값을 남기지
  않습니다. `set_relation`은 (빈 결과일 때도) 모든 주체에 대해 호출되며, 이것이 잔여 값을 지우는 장치입니다.
- 단일 값 연관(`has_one` / `belongs_to`)은 첫 행만 취합니다.

### 마이그레이션

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Migrator, create_table};

Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .add("002_posts", create_table::<Post>())
    .status().await?;      // MigrationStatus { applied, pending, unknown }
    .run().await?;         // pending을 적용하고 하나씩 버전 테이블에 기록. 재실행은 멱등
    .down(1).await?;       // 001의 역방향 SQL을 실행하고 버전 테이블에서 그 행을 삭제
```

- 마이그레이션 이름의 숫자 접두사가 곧 버전입니다(`"001_users"` → 1). 숫자가 아닌 접두사는
  `OrmError::InvalidMigrationName` — 0으로 임의 해석하지 않습니다.
- `create_table::<E>()` / `drop_table::<E>()`는 `EntityMeta`에서 DDL을 생성하고, **방언은 `run()` 시점에
  연결의 `dialect()`에서 해석**하므로 마이그레이션 목록이 연결 문자열과 분리됩니다. 직접 쓴 SQL(ALTER,
  데이터 이관)에는 `ecat_orm::migrate::MigrationBuilder::new(|d| ...)`를 씁니다.
- 역방향 SQL이 없는 마이그레이션에 `down`을 호출하면 `OrmError::MigrationIrreversible` — "DROP 후 다시
  CREATE"는 데이터를 잃으므로 호출자 대신 추측하지 않습니다.
- 버전 테이블은 `Migrator`가 자동으로 만듭니다(MSSQL에는 `CREATE TABLE IF NOT EXISTS`가 없어 존재 여부를
  먼저 확인합니다).
