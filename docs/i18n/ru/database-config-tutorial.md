# Руководство по настройке баз данных

**Версия:** 2.4.2 · **Дата:** 2026-08-01

Все 14 бэкендов данных e-cat поддерживают загрузку параметров подключения из конфигурационного файла — без хардкода в коде. Поля `username` / `password` опциональны: если опущены, аутентификация пропускается.

---

## Быстрый старт

### 1. Создание конфигурационного файла

Скопируйте пример шаблона и измените под своё окружение:

```bash
cp config/databases.example.yaml databases.yaml
```

Отредактируйте `databases.yaml`, вписав реальные параметры подключения:

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

### 2. Добавление зависимостей

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
ecat-data-sqlx = { path = "../ecat-data-sqlx" }
ecat-data-redis = { path = "../ecat-data-redis" }
ecat-data-clickhouse = { path = "../ecat-data-clickhouse" }
```

### 3. Загрузка и использование

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
    // 加载 YAML 配置
    let yaml = std::fs::read_to_string("databases.yaml")?;
    let cfg: AppConfig = serde_yaml::from_str(&yaml)?;

    // 创建数据库客户端 — 无硬编码连接信息
    let db = SqlxClient::from_config(cfg.sql).await?;
    let cache = RedisCache::from_config(cfg.redis).await?;
    let ch = ClickhouseClient::from_config(cfg.clickhouse);

    // 使用
    let rows = db.query("SELECT id, name FROM users LIMIT 10").await?;
    cache.set("health", b"ok", std::time::Duration::from_secs(30)).await?;

    Ok(())
}
```

---

## Полный справочник конфигурации

### Определение структуры конфигурации верхнего уровня

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

### Полный пример YAML

См. `config/databases.example.yaml`.

---

## Справочник полей Config по бэкендам

### RDBMS — SqlxConfig

#### Нативные пулы (PostgreSQL / MySQL / SQLite)

Нативный драйвер выбирается автоматически по схеме URL — дополнительная настройка не нужна:

| scheme | Драйвер |
|---|---|
| `postgres://` / `postgresql://` | PostgreSQL |
| `mysql://` / `mariadb://` | MySQL |
| `sqlite:` | SQLite |

- **Нативная поддержка типов времени**: прежний путь через `AnyPool` из sqlx не поддерживал типы времени, приходилось самому делать `CAST` в текст; теперь столбцы времени читаются напрямую.
- **Регистр, а также пробелы в начале и конце** схемы **допускаются** (`"POSTGRES://…"` и `" postgres://…"` работают); нераспознанная схема даёт ошибку с **указанием её имени**.
- `mssql://` **не обслуживается этим бэкендом** (используйте `ecat-data-mssql`); передача его сюда явно отклоняется.

#### Пример конфигурации

```yaml
sql:
  url: "postgres://host:5432/dbname"
  # username: "app_user"    # 可选
  # password: "secret"      # 可选
```

| Поле | Тип | Значение по умолчанию | Описание |
|------|------|--------|------|
| `url` | `String` | — | Строка подключения sqlx, поддерживает SQLite/PG/MySQL/TiDB |
| `username` | `Option<String>` | `None` | Опционально: встраивание аутентификации в URL (совместно с password) |
| `password` | `Option<String>` | `None` | Опционально: встраивание аутентификации в URL (совместно с username) |
| `max_connections` | `u32` | `10` | Максимальное число соединений в пуле |
| `min_connections` | `u32` | `0` | Минимальное поддерживаемое число соединений; ограничивается до ≤ `max_connections` |
| `acquire_timeout_secs` | `u64` | `30` | Таймаут ожидания соединения |
| `idle_timeout_secs` | `u64` | `600` | Возврат простаивающих соединений |
| `max_lifetime_secs` | `u64` | `1800` | Максимальное время жизни соединения |
| `query_timeout_secs` | `u64` | `30` | Таймаут одного запроса; **0 = отключено** |
| `slow_query_ms` | `u64` | `1000` | Порог предупреждения о медленных запросах (мс); не задан = 1000, **0 = выключено** (только feature `tracing`) |
| `test_before_acquire` | `bool` | `false` | Пинговать соединение перед выдачей |
| `session_init` | `string[]` | Зависит от диалекта | Инструкции инициализации сессии для каждого нового соединения |

#### `session_init` (инициализация сессии)

После установки каждого нового соединения эти инструкции выполняются по порядку:

| Диалект | По умолчанию `session_init` |
|---|---|
| PostgreSQL | `SET TIME ZONE 'UTC'`, `SET application_name = 'ecat'` |
| MySQL | `SET time_zone = '+00:00'` |
| SQLite | Нет понятия сессии — по умолчанию пусто |

Цель — чтобы **база сама возвращала UTC**, в согласии с конвенцией фреймворка представлять всё время единообразно как RFC3339 UTC.

- **Явный пустой массив `[]` означает «намеренно отключено»** и перекрывает значение по умолчанию для диалекта (та же конвенция явного переопределения, что и `query_timeout_secs: 0` для отключения).
- Если любая инструкция не выполнена → создание соединения завершается ошибкой, **без тихого отката**.

```yaml
sql:
  url: "mysql://host:3306/dbname"
  session_init:
    - "SET time_zone = '+00:00'"
    - "SET NAMES utf8mb4"
```

#### `warm_up()` — Прогрев

На старте активно открывает `min_connections` соединений и возвращает их, чтобы сервис был готов сразу после запуска:

```rust
let db = SqlxClient::from_config(cfg).await?;
db.warm_up().await?;   // вызвать один раз при старте
```

Зачем это нужно: sqlx поддерживает `min_connections` **асинхронно, фоновой задачей**, поэтому возврат из `connect()` не гарантирует, что пул уже заполнен — первая волна запросов пойдёт наперегонки с фоновой задачей.

#### Features наблюдаемости (`metrics` / `health` / `tracing`)

Все три feature **по умолчанию выключены** (чтобы axum — зависимость `ecat-metrics` / `ecat-health` — не попадал в дерево зависимостей ядра); включайте по необходимости. `ecat-data-mssql` предоставляет те же три.

```toml
ecat-data-sqlx = { path = "../ecat-data-sqlx", features = ["metrics", "health", "tracing"] }
```

```rust
use std::sync::Arc;
use ecat_data_sqlx::{RdbmsHealthCheck, SqlxClient, SqlxConfig, register_pool_metrics};
use ecat_health::HealthRegistry;

let db = SqlxClient::from_config(cfg).await?;

// metrics: после регистрации на /metrics появляются четыре метрики —
// ecat_rdbms_pool_connections (gauge, с state="idle"/"active"),
// ecat_rdbms_pool_timeouts_total, ecat_rdbms_query_timeout_total,
// ecat_rdbms_transactions_leaked_total (все counter, с меткой backend)
register_pool_metrics("primary", db.pool());

// health: проба доступности SELECT 1, регистрируется в readyz у /health
let registry = HealthRegistry::new()
    .with_check(RdbmsHealthCheck::new("sql", Arc::new(db)));
```

Feature `tracing` пишет warn, когда запрос превышает `slow_query_ms` (затраченное время + SQL, обрезанный до первых 200 символов). Это поле читает только данная feature — без неё оно по-прежнему разбирается, но не действует.

#### Время и дата: единообразно RFC3339 UTC

**Текст даты/времени в sqlite переписывается в RFC3339 UTC**:

- текст вида `2026-10-05 12:34:56` → `"2026-10-05T12:34:56Z"`
- текст вида `2026-10-05` → `"2026-10-05T00:00:00Z"` (момент полуночи UTC)

Причина: в sqlite нет системы типов, и текст в форме даты или времени изначально неоднозначен — фреймворк единообразно трактует его как время.

**Побочный эффект**: Текстовые столбцы, которые лишь случайно выглядят как дата — номера версий, бизнес-коды — тоже перезаписываются. Если такое поведение не нужно, явно приведите столбец к не-датовой форме через CAST или используйте другой тип.

Настоящие столбцы `DATE` / `TIMESTAMP` в PG / MySQL тоже представляются строками RFC3339 UTC, и **чистая дата всё равно несёт `T00:00:00Z`** — потому что `"2026-10-05"` не является корректным RFC3339, а фреймворк внутри использует один единый формат, чтобы верхним слоям было проще парсить.

### Redis — RedisConfig

```yaml
redis:
  url: "redis://host:6379"
  # password: "auth_token"  # 可选
```

| Поле | Тип | Описание |
|------|------|------|
| `url` | `String` | URL подключения к Redis |
| `password` | `Option<String>` | Опционально: пароль Redis AUTH |

### Memcached — MemcachedConfig

```yaml
memcached:
  # username: "memcache"    # 可选: 保留字段（当前为内存实现）
  # password: "secret"      # 可选: 保留字段
  {}
```

| Поле | Тип | Описание |
|------|------|------|
| `username` | `Option<String>` | Опционально: зарезервированное поле |
| `password` | `Option<String>` | Опционально: зарезервированное поле |

В настоящее время это реализация в памяти; поля аутентификации зарезервированы.

### ClickHouse — ClickhouseConfig

```yaml
clickhouse:
  base_url: "http://host:8123"
  database: "default"
  # username: "default"   # 可选
  # password: "secret"    # 可选
```

| Поле | Тип | Значение по умолчанию | Описание |
|------|------|--------|------|
| `base_url` | `String` | — | Адрес HTTP-интерфейса |
| `database` | `String` | `"default"` | Имя базы данных |
| `username` | `Option<String>` | `None` | Опционально: имя пользователя HTTP Basic Auth |
| `password` | `Option<String>` | `None` | Опционально: пароль HTTP Basic Auth |

### QuestDB — QuestdbConfig

```yaml
questdb:
  base_url: "http://host:9000"
  # username: "admin"     # 可选
  # password: "quest"     # 可选
```

| Поле | Тип | Описание |
|------|------|------|
| `base_url` | `String` | Адрес HTTP API |
| `username` | `Option<String>` | Опционально: имя пользователя HTTP Basic Auth |
| `password` | `Option<String>` | Опционально: пароль HTTP Basic Auth |

### Elasticsearch — ElasticsearchConfig

```yaml
elasticsearch:
  base_url: "http://host:9200"
  # username: "elastic"   # 可选
  # password: "secret"    # 可选
```

| Поле | Тип | Описание |
|------|------|------|
| `base_url` | `String` | Адрес REST API |
| `username` | `Option<String>` | Опционально: имя пользователя HTTP Basic Auth |
| `password` | `Option<String>` | Опционально: пароль HTTP Basic Auth |

### OpenSearch — OpenSearchConfig

```yaml
opensearch:
  base_url: "http://host:9200"
  # username: "admin"     # 可选
  # password: "secret"    # 可选
```

| Поле | Тип | Описание |
|------|------|------|
| `base_url` | `String` | Адрес REST API |
| `username` | `Option<String>` | Опционально: имя пользователя HTTP Basic Auth |
| `password` | `Option<String>` | Опционально: пароль HTTP Basic Auth |

### InfluxDB — InfluxConfig

```yaml
influxdb:
  base_url: "http://host:8086"
  org: "myorg"
  bucket: "mybucket"
  token: "my-token"
```

| Поле | Тип | Описание |
|------|------|------|
| `base_url` | `String` | Адрес API InfluxDB 2.x |
| `org` | `String` | Имя организации |
| `bucket` | `String` | Имя bucket-а |
| `token` | `String` | Токен аутентификации |

### Neo4j — Neo4jConfig

```yaml
neo4j:
  base_url: "http://host:7474"
  username: "neo4j"
  password: "secret"
```

| Поле | Тип | Описание |
|------|------|------|
| `base_url` | `String` | Адрес REST API |
| `username` | `String` | Имя пользователя |
| `password` | `String` | Пароль |

### NebulaGraph — NebulaGraphConfig

```yaml
nebulagraph:
  base_url: "http://host:19669"
  space: "my_space"
  # username: "root"      # 可选
  # password: "nebula"    # 可选
```

| Поле | Тип | Описание |
|------|------|------|
| `base_url` | `String` | Адрес API |
| `space` | `String` | Имя graph space |
| `username` | `Option<String>` | Опционально: имя пользователя HTTP Basic Auth |
| `password` | `Option<String>` | Опционально: пароль HTTP Basic Auth |

### ArangoDB — ArangoConfig

```yaml
arangodb:
  base_url: "http://host:8529"
  db: "mydb"
  username: "root"
  password: "secret"
```

| Поле | Тип | Описание |
|------|------|------|
| `base_url` | `String` | Адрес API |
| `db` | `String` | Имя базы данных |
| `username` | `String` | Имя пользователя |
| `password` | `String` | Пароль |

### IoTDB — IotdbConfig

```yaml
iotdb:
  base_url: "http://host:18080"
  username: "root"
  password: "root"
```

| Поле | Тип | Описание |
|------|------|------|
| `base_url` | `String` | Адрес REST API |
| `username` | `String` | Имя пользователя |
| `password` | `String` | Пароль |

---

## Программное создание

### Без аутентификации

```rust
let es = ElasticsearchClient::new("http://localhost:9200");
let ch = ClickhouseClient::new("http://localhost:8123", "default");
```

### С аутентификацией

```rust
let es = ElasticsearchClient::with_auth("http://es:9200", "elastic", "secret");
let ch = ClickhouseClient::with_auth("http://ch:8123", "default", "admin", "pass");
let qdb = QuestdbClient::with_auth("http://qdb:9000", "admin", "quest");
let ng = NebulaGraphClient::with_auth("http://ng:19669", "space1", "root", "nebula");
```

---

---

## Настройка TLS-сертификатов

Все бэкенды данных, как правило, поддерживают опциональную TLS-аутентификацию клиента (поле `tls`), но есть **два исключения**: `ecat-data-sqlx` это поле не поддерживает — при его задании запуск завершится ошибкой (TLS идёт через параметры URL); у `ecat-data-memcached` поле **молча не работает** — объявлено, но нигде в crate не читается.

### Пример конфигурации

```yaml
clickhouse:
  base_url: "https://ch.internal:8443"
  tls:
    ca_cert: "/etc/ecat/ca.pem"
    client_cert: "/etc/ecat/client.pem"
    client_key: "/etc/ecat/client-key.pem"
    # skip_verify: true  # 仅测试环境
```

### Автогенерация сертификатов (ecat-tls)

```rust
use ecat_tls::{generate_ca, generate_server_cert, generate_client_cert};

// 1. 生成 CA
let ca = generate_ca("MyOrg")?;
std::fs::write("ca.pem", &ca.cert_pem)?;
std::fs::write("ca-key.pem", &ca.key_pem)?;

// 2. 生成服务端证书
let srv = generate_server_cert("db.example.com")?;
std::fs::write("server.pem", &srv.cert_pem)?;
std::fs::write("server-key.pem", &srv.key_pem)?;

// 3. 生成客户端证书（mTLS）
let client = generate_client_cert("myapp")?;
std::fs::write("client.pem", &client.cert_pem)?;
std::fs::write("client-key.pem", &client.key_pem)?;
```

### Ручная генерация (OpenSSL)

```bash
# CA
openssl req -x509 -newkey rsa:4096 -keyout ca-key.pem -out ca.pem -days 3650 -nodes

# 服务端证书
openssl req -new -newkey rsa:4096 -keyout server-key.pem -out server.csr -nodes -subj "/CN=db.example.com"
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -out server.pem -days 365

# 客户端证书 (mTLS)
openssl req -new -newkey rsa:4096 -keyout client-key.pem -out client.csr -nodes -subj "/CN=myapp"
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca-key.pem -out client.pem -days 365
```

### Описание полей TLS

| Поле | Тип | Описание |
|------|------|------|
| `ca_cert` | `Option<String>` | Путь к PEM-файлу CA-сертификата (проверка сервера) |
| `client_cert` | `Option<String>` | Путь к PEM-файлу клиентского сертификата (mTLS) |
| `client_key` | `Option<String>` | Путь к PEM-файлу приватного ключа клиента (mTLS) |
| `skip_verify` | `Option<bool>` | Пропустить проверку сертификатов (только тест) |

---

## Продвинутое использование

### Переопределение через переменные окружения

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

### Совместно с фреймворком ecat-config

```rust
use ecat_config::{Config, FileSource};

let mut app_config = Config::new();
app_config.load(&FileSource::new("databases.yaml")).await?;

let redis_cfg: RedisConfig = serde_json::from_value(
    app_config.get::<serde_json::Value>("redis").unwrap()
)?;
let cache = RedisCache::from_config(redis_cfg).await?;
```

### Конфигурация по необходимости

Неиспользуемые базы данных опускаются в YAML, в Rust-структуре помечаются `Option`:

```rust
#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: Option<RedisConfig>,
    clickhouse: Option<ClickhouseConfig>,
}
```

---

## Связанные документы

- [Отчёт об аудите r5](audit-report-2026-08-01-r5.md)
- [Руководство по TLS-сертификатам](tls-certificate-tutorial.md)
- [Пример файла конфигурации](../../../config/databases.example.yaml)
