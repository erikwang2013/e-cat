# 데이터베이스 설정 튜토리얼

**버전:** 2.4.2 · **날짜:** 2026-08-01

e-cat의 14개 데이터 백엔드는 모두 설정 파일에서 연결 정보를 로드할 수 있으며, 코드에 하드코딩할 필요가 없습니다. `username` / `password`는 모두 선택 필드이며, 생략하면 인증을 건너뜁니다.

---

## 빠른 시작

### 1. 설정 파일 생성

예시 템플릿을 복사한 후 실제 환경에 맞게 수정합니다:

```bash
cp config/databases.example.yaml databases.yaml
```

`databases.yaml`을 편집하여 실제 연결 정보를 입력합니다:

```yaml
# databases.yaml
sql:
  url: "postgres://myapp:secret@db.internal:5432/myapp"

redis:
  url: "redis://cache.internal:6379"

clickhouse:
  base_url: "http://ch.internal:8123"
  database: "analytics"
```

### 2. 의존성 추가

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
ecat-data-sqlx = { path = "../ecat-data-sqlx" }
ecat-data-redis = { path = "../ecat-data-redis" }
ecat-data-clickhouse = { path = "../ecat-data-clickhouse" }
```

### 3. 로드하여 사용

```rust
use ecat_data_redis::{RedisCache, RedisConfig};
use ecat_data_sqlx::{SqlxClient, SqlxConfig};
use ecat_data_clickhouse::{ClickhouseClient, ClickhouseConfig};
use serde::Deserialize;

#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: RedisConfig,
    clickhouse: ClickhouseConfig,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // YAML 설정 로드
    let yaml = std::fs::read_to_string("databases.yaml")?;
    let cfg: AppConfig = serde_yaml::from_str(&yaml)?;

    // 데이터베이스 클라이언트 생성 — 하드코딩된 연결 정보 없음
    let db = SqlxClient::from_config(cfg.sql).await?;
    let cache = RedisCache::from_config(cfg.redis).await?;
    let ch = ClickhouseClient::from_config(cfg.clickhouse);

    // 사용
    let rows = db.query("SELECT id, name FROM users LIMIT 10").await?;
    cache.set("health", b"ok", std::time::Duration::from_secs(30)).await?;

    Ok(())
}
```

---

## 전체 설정 참조

### 최상위 설정 구조체 정의

```rust
use serde::Deserialize;

#[derive(Deserialize)]
pub struct DatabasesConfig {
    pub sql: ecat_data_sqlx::SqlxConfig,
    pub redis: ecat_data_redis::RedisConfig,
    pub memcached: ecat_data_memcached::MemcachedConfig,
    pub clickhouse: ecat_data_clickhouse::ClickhouseConfig,
    pub questdb: ecat_data_questdb::QuestdbConfig,
    pub elasticsearch: ecat_data_elasticsearch::ElasticsearchConfig,
    pub opensearch: ecat_data_opensearch::OpenSearchConfig,
    pub neo4j: ecat_data_neo4j::Neo4jConfig,
    pub nebulagraph: ecat_data_nebulagraph::NebulaGraphConfig,
    pub arangodb: ecat_data_arangodb::ArangoConfig,
    pub influxdb: ecat_data_influxdb::InfluxConfig,
    pub iotdb: ecat_data_iotdb::IotdbConfig,
}
```

### YAML 전체 예시

`config/databases.example.yaml`을 참조하세요.

---

## 각 백엔드 Config 필드 속성 참조

### RDBMS — SqlxConfig

#### 네이티브 풀(PostgreSQL / MySQL / SQLite)

URL scheme에 따라 네이티브 드라이버가 자동으로 선택됩니다. 추가 설정은 필요하지 않습니다:

| scheme | 드라이버 |
|---|---|
| `postgres://` / `postgresql://` | PostgreSQL |
| `mysql://` / `mariadb://` | MySQL |
| `sqlite:` | SQLite |

- **시간 타입 네이티브 지원**: 예전 sqlx `AnyPool` 경로에서는 시간 타입을 지원하지 않아 직접 `CAST`로 텍스트로 바꿔야 했습니다. 이제 시간 컬럼을 그대로 읽을 수 있습니다.
- scheme의 **대소문자와 앞뒤 공백이 모두 허용**됩니다(`"POSTGRES://…"`, `" postgres://…"` 모두 사용 가능). 인식할 수 없는 scheme은 오류를 내고 **어떤 scheme인지 지목**합니다.
- `mssql://`는 **이 백엔드가 제공하지 않습니다**(`ecat-data-mssql`를 사용하세요). 여기로 넘기면 명시적으로 거부됩니다.

#### 설정 예시

```yaml
sql:
  url: "postgres://host:5432/dbname"
  # username: "app_user"    # 선택
  # password: "secret"      # 선택
```

| 필드 | 타입 | 기본값 | 설명 |
|------|------|--------|------|
| `url` | `String` | — | sqlx 연결 문자열, SQLite/PG/MySQL/TiDB 지원 |
| `username` | `Option<String>` | `None` | 선택: URL 내장 인증(password와 함께 사용) |
| `password` | `Option<String>` | `None` | 선택: URL 내장 인증(username과 함께 사용) |
| `max_connections` | `u32` | `10` | 풀의 최대 연결 수 |
| `min_connections` | `u32` | `0` | 유지할 최소 연결 수; ≤ `max_connections`로 잘립니다 |
| `acquire_timeout_secs` | `u64` | `30` | 연결을 기다리는 타임아웃 |
| `idle_timeout_secs` | `u64` | `600` | 유휴 연결 회수 |
| `max_lifetime_secs` | `u64` | `1800` | 연결의 최대 수명 |
| `query_timeout_secs` | `u64` | `30` | 쿼리 단위 타임아웃; **0 = 비활성** |
| `slow_query_ms` | `u64` | `1000` | 슬로우 쿼리 경고 임계값(밀리초). 미설정 = 1000, **0 = 사용 안 함**(`tracing` feature 전용) |
| `test_before_acquire` | `bool` | `false` | 연결을 넘겨주기 전에 ping 할지 |
| `session_init` | `string[]` | 방언별 | 새 연결마다의 세션 초기화 문장 |

#### `session_init`(세션 초기화)

새 연결이 맺어질 때마다 이 문장들이 순서대로 실행됩니다:

| 방언 | 기본값 `session_init` |
|---|---|
| PostgreSQL | `SET TIME ZONE 'UTC'`, `SET application_name = 'ecat'` |
| MySQL | `SET time_zone = '+00:00'` |
| SQLite | 세션 개념이 없어 기본적으로 비어 있음 |

목적은 **DB 쪽에서 직접 UTC를 반환하게** 하여, 모든 시간을 RFC3339 UTC로 통일해 제시한다는 프레임워크 규약에 맞추는 것입니다.

- **명시적 빈 배열 `[]`은 "의도적 해제"**를 뜻하며 방언 기본값을 덮어씁니다(`query_timeout_secs: 0`이 비활성을 뜻하는 것과 같은 "명시적 재정의" 규약입니다).
- 문장 중 하나라도 실패하면 → 연결 생성이 실패합니다. **조용한 폴백은 없습니다**.

```yaml
sql:
  url: "mysql://host:3306/dbname"
  session_init:
    - "SET time_zone = '+00:00'"
    - "SET NAMES utf8mb4"
```

#### `warm_up()` — 워밍업

시작 시 `min_connections`개만큼 연결을 미리 만들어 반납하여, 서비스가 뜨자마자 준비 상태가 되게 합니다:

```rust
let db = SqlxClient::from_config(cfg).await?;
db.warm_up().await?;   // 시작 시 한 번 호출
```

필요한 이유: sqlx의 `min_connections`는 **백그라운드 태스크가 비동기로 유지**하므로 `connect()`가 반환된 시점에 이미 채워져 있다고 보장할 수 없습니다 —— 시작 직후의 첫 요청 물결이 백그라운드 태스크와 경쟁하게 됩니다.

#### 관측성 feature(`metrics` / `health` / `tracing`)

세 feature는 **기본적으로 꺼져 있습니다**(`ecat-metrics` / `ecat-health`의 의존성인 axum을 코어 의존성 트리에 넣지 않기 위함). 필요할 때만 켭니다. `ecat-data-mssql`도 동일한 세 가지를 제공합니다.

```toml
ecat-data-sqlx = { path = "../ecat-data-sqlx", features = ["metrics", "health", "tracing"] }
```

```rust
use std::sync::Arc;
use ecat_data_sqlx::{RdbmsHealthCheck, SqlxClient, SqlxConfig, register_pool_metrics};
use ecat_health::HealthRegistry;

let db = SqlxClient::from_config(cfg).await?;

// metrics: 등록하면 /metrics 엔드포인트에 네 개의 지표가 추가됩니다 —
// ecat_rdbms_pool_connections(gauge, state="idle"/"active"),
// ecat_rdbms_pool_timeouts_total, ecat_rdbms_query_timeout_total,
// ecat_rdbms_transactions_leaked_total(모두 counter, backend 레이블)
register_pool_metrics("primary", db.pool());

// health: SELECT 1 연결성 프로브. /health의 readyz에 등록합니다
let registry = HealthRegistry::new()
    .with_check(RdbmsHealthCheck::new("sql", Arc::new(db)));
```

`tracing` feature는 쿼리가 `slow_query_ms`를 초과하면 warn을 남깁니다(소요 시간 + 앞 200자로 자른 SQL). 이 필드를 읽는 것은 이 feature뿐입니다 — 꺼져 있어도 파싱은 되지만 효과는 없습니다.

#### 시간과 날짜: RFC3339 UTC로 통일

**sqlite의 시간/날짜 텍스트는 RFC3339 UTC로 다시 쓰입니다**:

- `2026-10-05 12:34:56` 형태의 텍스트 → `"2026-10-05T12:34:56Z"`
- `2026-10-05` 형태의 텍스트 → `"2026-10-05T00:00:00Z"`(UTC 자정 시점)

이유: sqlite에는 타입 시스템이 없어 날짜/시간 형태의 텍스트는 본래 모호하므로, 프레임워크가 일괄적으로 시간으로 취급합니다.

**부작용**: 버전 번호나 업무 코드처럼 "우연히 날짜 모양이 된" 텍스트 컬럼도 함께 다시 쓰입니다. 이 동작이 필요 없다면 해당 컬럼을 날짜가 아닌 모양으로 명시적으로 CAST하거나 다른 타입을 사용하세요.

PG / MySQL의 실제 `DATE` / `TIMESTAMP` 컬럼도 마찬가지로 RFC3339 UTC 문자열로 제시되며, **순수 날짜에도 `T00:00:00Z`가 붙습니다** —— `"2026-10-05"`는 올바른 RFC3339가 아니기 때문에, 상위 계층이 파싱하기 쉽도록 프레임워크 내부에서 하나의 형식으로 통일하기 때문입니다.

### Redis — RedisConfig

```yaml
redis:
  url: "redis://host:6379"
  # password: "auth_token"  # 선택
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `url` | `String` | Redis 연결 URL |
| `password` | `Option<String>` | 선택: Redis AUTH 비밀번호 |

### Memcached — MemcachedConfig

```yaml
memcached:
  # username: "memcache"    # 선택: 예약 필드 (현재 메모리 구현)
  # password: "secret"      # 선택: 예약 필드
  {}
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `username` | `Option<String>` | 선택: 예약 필드 |
| `password` | `Option<String>` | 선택: 예약 필드 |

현재는 메모리 구현이며, 인증 필드는 예약되어 있습니다.

### ClickHouse — ClickhouseConfig

```yaml
clickhouse:
  base_url: "http://host:8123"
  database: "default"
  # username: "default"   # 선택
  # password: "secret"    # 선택
```

| 필드 | 타입 | 기본값 | 설명 |
|------|------|--------|------|
| `base_url` | `String` | — | HTTP 인터페이스 주소 |
| `database` | `String` | `"default"` | 데이터베이스 이름 |
| `username` | `Option<String>` | `None` | 선택: HTTP Basic Auth 사용자 이름 |
| `password` | `Option<String>` | `None` | 선택: HTTP Basic Auth 비밀번호 |

### QuestDB — QuestdbConfig

```yaml
questdb:
  base_url: "http://host:9000"
  # username: "admin"     # 선택
  # password: "quest"     # 선택
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `base_url` | `String` | HTTP API 주소 |
| `username` | `Option<String>` | 선택: HTTP Basic Auth 사용자 이름 |
| `password` | `Option<String>` | 선택: HTTP Basic Auth 비밀번호 |

### Elasticsearch — ElasticsearchConfig

```yaml
elasticsearch:
  base_url: "http://host:9200"
  # username: "elastic"   # 선택
  # password: "secret"    # 선택
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `base_url` | `String` | REST API 주소 |
| `username` | `Option<String>` | 선택: HTTP Basic Auth 사용자 이름 |
| `password` | `Option<String>` | 선택: HTTP Basic Auth 비밀번호 |

### OpenSearch — OpenSearchConfig

```yaml
opensearch:
  base_url: "http://host:9200"
  # username: "admin"     # 선택
  # password: "secret"    # 선택
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `base_url` | `String` | REST API 주소 |
| `username` | `Option<String>` | 선택: HTTP Basic Auth 사용자 이름 |
| `password` | `Option<String>` | 선택: HTTP Basic Auth 비밀번호 |

### InfluxDB — InfluxConfig

```yaml
influxdb:
  base_url: "http://host:8086"
  org: "myorg"
  bucket: "mybucket"
  token: "my-token"
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `base_url` | `String` | InfluxDB 2.x API 주소 |
| `org` | `String` | 조직 이름 |
| `bucket` | `String` | 버킷 이름 |
| `token` | `String` | 인증 토큰 |

### Neo4j — Neo4jConfig

```yaml
neo4j:
  base_url: "http://host:7474"
  username: "neo4j"
  password: "secret"
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `base_url` | `String` | REST API 주소 |
| `username` | `String` | 사용자 이름 |
| `password` | `String` | 비밀번호 |

### NebulaGraph — NebulaGraphConfig

```yaml
nebulagraph:
  base_url: "http://host:19669"
  space: "my_space"
  # username: "root"      # 선택
  # password: "nebula"    # 선택
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `base_url` | `String` | API 주소 |
| `space` | `String` | 그래프 스페이스 이름 |
| `username` | `Option<String>` | 선택: HTTP Basic Auth 사용자 이름 |
| `password` | `Option<String>` | 선택: HTTP Basic Auth 비밀번호 |

### ArangoDB — ArangoConfig

```yaml
arangodb:
  base_url: "http://host:8529"
  db: "mydb"
  username: "root"
  password: "secret"
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `base_url` | `String` | API 주소 |
| `db` | `String` | 데이터베이스 이름 |
| `username` | `String` | 사용자 이름 |
| `password` | `String` | 비밀번호 |

### IoTDB — IotdbConfig

```yaml
iotdb:
  base_url: "http://host:18080"
  username: "root"
  password: "root"
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `base_url` | `String` | REST API 주소 |
| `username` | `String` | 사용자 이름 |
| `password` | `String` | 비밀번호 |

---

## 프로그래밍 방식 생성

### 인증 없이

```rust
let es = ElasticsearchClient::new("http://localhost:9200");
let ch = ClickhouseClient::new("http://localhost:8123", "default");
```

### 인증 포함

```rust
let es = ElasticsearchClient::with_auth("http://es:9200", "elastic", "secret");
let ch = ClickhouseClient::with_auth("http://ch:8123", "default", "admin", "pass");
let qdb = QuestdbClient::with_auth("http://qdb:9000", "admin", "quest");
let ng = NebulaGraphClient::with_auth("http://ng:19669", "space1", "root", "nebula");
```

---

---

## TLS 인증서 설정

데이터 백엔드는 대체로 선택적 TLS 클라이언트 인증(`tls` 필드)을 지원하지만 **두 가지 예외**가 있습니다: `ecat-data-sqlx`는 이 필드를 지원하지 않으며 설정하면 기동 시 오류가 납니다(TLS는 URL 파라미터로 지정). `ecat-data-memcached`의 이 필드는 **조용히 무시됩니다** — 선언만 되어 있고 crate 어디에서도 읽지 않습니다.

### 설정 예시

```yaml
clickhouse:
  base_url: "https://ch.internal:8443"
  tls:
    ca_cert: "/etc/ecat/ca.pem"
    client_cert: "/etc/ecat/client.pem"
    client_key: "/etc/ecat/client-key.pem"
    # skip_verify: true  # 테스트 환경 전용
```

### 인증서 자동 생성 (ecat-tls)

```rust
use ecat_tls::{generate_ca, generate_server_cert, generate_client_cert};

// 1. CA 생성
let ca = generate_ca("MyOrg")?;
std::fs::write("ca.pem", &ca.cert_pem)?;
std::fs::write("ca-key.pem", &ca.key_pem)?;

// 2. 서버 인증서 생성
let srv = generate_server_cert("db.example.com")?;
std::fs::write("server.pem", &srv.cert_pem)?;
std::fs::write("server-key.pem", &srv.key_pem)?;

// 3. 클라이언트 인증서 생성 (mTLS)
let client = generate_client_cert("myapp")?;
std::fs::write("client.pem", &client.cert_pem)?;
std::fs::write("client-key.pem", &client.key_pem)?;
```

### 수동 생성 (OpenSSL)

```bash
# CA
openssl req -x509 -newkey rsa:4096 -keyout ca-key.pem -out ca.pem -days 3650 -nodes

# 서버 인증서
openssl req -new -newkey rsa:4096 -keyout server-key.pem -out server.csr -nodes -subj "/CN=db.example.com"
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -out server.pem -days 365

# 클라이언트 인증서 (mTLS)
openssl req -new -newkey rsa:4096 -keyout client-key.pem -out client.csr -nodes -subj "/CN=myapp"
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca-key.pem -out client.pem -days 365
```

### TLS 필드 설명

| 필드 | 타입 | 설명 |
|------|------|------|
| `ca_cert` | `Option<String>` | CA 인증서 PEM 경로(서버 검증) |
| `client_cert` | `Option<String>` | 클라이언트 인증서 PEM 경로(mTLS) |
| `client_key` | `Option<String>` | 클라이언트 개인키 PEM 경로(mTLS) |
| `skip_verify` | `Option<bool>` | 인증서 검증 건너뛰기(테스트 전용) |

---

## 고급 사용법

### 환경 변수 오버라이드

```rust
use std::env;

fn load_config() -> Result<SqlxConfig, Box<dyn std::error::Error>> {
    let mut cfg: SqlxConfig = serde_yaml::from_str(
        &std::fs::read_to_string("databases.yaml")?
    )?;
    if let Ok(url) = env::var("DATABASE_URL") {
        cfg.url = url;
    }
    Ok(cfg)
}
```

### ecat-config 프레임워크와 결합

```rust
use ecat_config::{Config, FileSource};

let mut app_config = Config::new();
app_config.load(&FileSource::new("databases.yaml")).await?;

let redis_cfg: RedisConfig = serde_json::from_value(
    app_config.get::<serde_json::Value>("redis").unwrap()
)?;
let cache = RedisCache::from_config(redis_cfg).await?;
```

### 필요에 따른 설정

사용하지 않는 데이터베이스는 YAML에서 생략하고, Rust 구조체는 `Option`으로 표시합니다:

```rust
#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: Option<RedisConfig>,
    clickhouse: Option<ClickhouseConfig>,
}
```

---

## 관련 문서

- [감사 보고서 r5](audit-report-2026-08-01-r5.md)
- [TLS 인증서 인증 튜토리얼](tls-certificate-tutorial.md)
- [설정 예시 파일](../../../config/databases.example.yaml)
