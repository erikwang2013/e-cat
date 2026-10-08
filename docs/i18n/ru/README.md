<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Ecat

[简体中文](../../../README.md) | [English](../../../README.en.md) | [日本語](../ja/README.md) | [한국어](../ko/README.md) | **[Русский](../ru/README.md)** | [Deutsch](../de/README.md) | [Français](../fr/README.md) | [Español](../es/README.md) | [Português](../pt/README.md) | [हिन्दी](../hi/README.md) | [العربية](../ar/README.md) | [বাংলা](../bn/README.md) | [Bahasa Indonesia](../id/README.md)

Китайское название Ecat: «одна кошка» (一只猫)

**«Одна кошка»** — это Rust-фреймворк для микросервисов, ориентированный на [go-kratos/kratos](https://github.com/go-kratos/kratos) v3 (v6.0.0 · 56 crate).

API-first подход к разработке, плагинная архитектура компонентов, единая абстракция HTTP/gRPC-промежуточных слоёв (middleware) и полный набор инструментов CLI. Разработчики, знакомые с Kratos, могут начать работать без переучивания, получая при этом типобезопасность Rust, zero-cost абстракции и экстремальную производительность.

<p align="center">
  <img src="e-cat.svg" alt="Талисман проекта Ecat (динамический)" width="220" />
</p>

## Архитектура

```
┌──────────────────────────────────────────────────────────────┐
│                         ecat-cli                             │
│        (new │ proto │ run --watch │ build │ upgrade)         │
├──────────────────────────────────────────────────────────────┤
│                     ecat (应用生命周期)                         │
│      AppBuilder → App { name, servers, hooks, ... }         │
├────────────────────┬────────────────────┬────────────────────┤
│     transport      │    middleware      │     registry       │
│     ─────────      │    ──────────      │     ────────       │
│     HTTP (axum)    │    RecoveryLayer   │     memory         │
│     gRPC (tonic)   │    TracingLayer    │     consul         │
│     encoding       │    LoggingLayer    │                    │
│                    │    TimeoutLayer    │                    │
│                    │    RateLimitLayer  │                    │
│                    │    SecurityLayer   │                    │
│                    │    CircuitBreaker  │                    │
│                    │    Auth (JWT/API)  │                    │
├────────────────────┼────────────────────┼────────────────────┤
│     config         │     errors         │     metadata       │
│     ──────         │     ──────         │     ────────       │
│     file / env     │     ErrorCode      │     key-value      │
│     remote source  │     Error          │     HTTP/gRPC      │
├────────────────────┴────────────────────┴────────────────────┤
│                         data 层                               │
│     ────────────────────────────────────────────────          │
│     rdbms:   SQLite / PostgreSQL / MySQL / TiDB              │
│     cache:   Redis ✓                                         │
│     config:  remote (Consul KV)                              │
│     registry: consul                                         │
├──────────────────────────────────────────────────────────────┤
│                       ecat-protos                             │
│     (共享 .proto 定义: errors, metadata, ...)                 │
└──────────────────────────────────────────────────────────────┘
```

### Поток обработки запроса

```
客户端请求
  │
  ├─ HTTP 0.0.0.0:8000 ──→ axum::Router ──┐
  │                                        │
  └─ gRPC 0.0.0.0:9000 ──→ tonic::Server ─┤
                                      │
                              ┌───────┴───────┐
                              │   Middleware   │
                              │   ──────────   │
                              │ 1. Recovery    │  捕获 panic
                              │ 2. Tracing     │  注入 trace_id
                              │ 3. Logging     │  请求日志
                              │ 4. Auth        │  认证鉴权
                              │ 5. Metrics     │  指标采集
│ 6. Security    │  攻击检测
│ 7. CircuitBrk  │  熔断保护
                              └───────┬───────┘
                                      │
                              ┌───────┴───────┐
                              │    Handler     │  用户业务逻辑
                              │ (tower::Service)│
                              └───────┬───────┘
                                      │
                              ┌───────┴───────┐
                              │   Response     │  编码序列化
                              │ JSON/Protobuf  │
                              └───────────────┘
```

## Возможности

- **API-first**: Protobuf определяет API, коды ошибок и метаданные; генерация кода через prost + tonic-build
- **Два протокола**: HTTP (axum) и gRPC (tonic) используют один и тот же набор middleware `tower::Layer`
- **Плагинная архитектура**: Registry, Config, Logging, Encoding — всё абстрагировано через trait с готовыми продакшен-реализациями по умолчанию
- **Система middleware**: встроенные Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, MetricsLayer, RetryLayer, ValidateLayer, CORS (feature `cors`); комбинируются через `tower::ServiceBuilder`
- **Жизненный цикл приложения**: сборка App через Builder, параллельный запуск нескольких серверов, обработка сигналов SIGTERM/SIGINT, хуки жизненного цикла start/stop
- **Типобезопасность**: система кодов ошибок на основе protobuf, сопоставление HTTP-статусов на этапе компиляции
- **Наблюдаемость**: tracing + Prometheus + эндпоинты Health (/health, /ready)
- **Обнаружение атак**: SecurityLayer автоматически обнаруживает SQL-инъекции, XSS, SSRF и другие паттерны атак, блокируя высокорисковые запросы
- **Взаимодействие между сервисами**: HttpClient с интеграцией service discovery и балансировки нагрузки, защита CircuitBreaker
- **Аутентификация**: middleware JWT / API Key, Claims передаются в контекст запроса
- **Сообщения и события**: trait MessageQueue + EventBus (локальный/удалённый Pub/Sub)
- **Распределённая трассировка**: span запросов, инъекция/извлечение trace_id
- **gRPC-клиент**: GrpcClient с интеграцией service discovery и балансировки нагрузки
- **Мультипротокол**: единая маршрутизация HTTP, gRPC, WebSocket, GraphQL
- **Мульти-источники данных**: RDBMS (SQLite/PG/MySQL/TiDB/SQL Server), кэш (Redis/Memcached), поиск (OpenSearch/Elasticsearch), графы (Neo4j/NebulaGraph/ArangoDB), временные ряды (InfluxDB/IoTDB/QuestDB/TDengine), документы (MongoDB), объектное хранилище (S3/MinIO), OLAP (ClickHouse)

### Сопоставление концепций Kratos

| Kratos (Go) | e-cat (Rust) | Описание |
|-------------|-------------|------|
| `kratos.New()` | `App::builder()` | Паттерн Builder |
| `http.Handler` | `tower::Service` | Стандартный trait экосистемы Rust |
| `http.Server` | `axum::Router` | Популярный HTTP-фреймворк сообщества |
| `grpc.Server` | `tonic::transport::Server` | Наиболее зрелая gRPC-реализация |
| `proto generate` | `prost + tonic-build` | Стандартный protobuf сообщества |
| `registry.Discovery` | `Registry` trait | Плагинные регистрация и discovery |
| `config.Source` | `ConfigSource` trait | Загрузка конфигурации из нескольких источников |

## Технологический стек

| Компонент | Выбор |
|------|------|
| Асинхронный runtime | **tokio** |
| HTTP | **axum** |
| gRPC | **tonic** |
| Protobuf | **prost + tonic-build** |
| Middleware | **tower::Service / Layer** |
| Логирование/трассировка | **tracing + trace_id propagation** |
| Метрики | **prometheus** |
| Сериализация | **serde + prost** |
| Обнаружение атак | **security-rust** |
| RDBMS | **sqlx** |
| Redis | **redis-rs** |
| JWT | **jsonwebtoken** |
| HTTP Client | **reqwest** |
| CLI | **clap** |

## Поддерживаемые базы данных

| Категория | База данных | Crate | Статус | Таймаут/Circuit Breaker |
|------|--------|-------|------|------|
| RDBMS | SQLite | `ecat-data-sqlx` | ✅ Реализовано | ✅ Таймаут + Circuit Breaker |
| RDBMS | PostgreSQL | `ecat-data-sqlx` | ✅ Реализовано | ✅ Таймаут + Circuit Breaker |
| RDBMS | MySQL | `ecat-data-sqlx` | ✅ Реализовано | ✅ Таймаут + Circuit Breaker |
| RDBMS | TiDB | `ecat-data-sqlx` | ✅ Реализовано | ✅ Таймаут + Circuit Breaker |
| RDBMS | SQL Server | `ecat-data-mssql` | ✅ tiberius-ng | ✅ Таймаут + Circuit Breaker |
| Кэш | Redis | `ecat-data-redis` | ✅ Реализовано | — |
| Поиск | OpenSearch | `ecat-data-opensearch` | ✅ Реализовано | — |
| Поиск | Elasticsearch | `ecat-data-elasticsearch` | ✅ Реализовано | — |
| Кэш | Memcached | `ecat-data-memcached` | ⚠️ Реализация в памяти (не для продакшена, не использовать для постоянного кэша) | — |
| OLAP | ClickHouse | `ecat-data-clickhouse` | ✅ Реализовано | ✅ Circuit Breaker |
| Граф | Neo4j | `ecat-data-neo4j` | ✅ REST API | — |
| Граф | NebulaGraph | `ecat-data-nebulagraph` | ✅ REST API | — |
| Граф | ArangoDB | `ecat-data-arangodb` | ✅ REST API | — |
| Врем. ряды | InfluxDB | `ecat-data-influxdb` | ✅ HTTP API | — |
| Врем. ряды | Apache IoTDB | `ecat-data-iotdb` | ✅ REST API | — |
| Врем. ряды | QuestDB | `ecat-data-questdb` | ✅ HTTP API | ✅ Circuit Breaker |
| Врем. ряды | TDengine | `ecat-data-tdengine` | ✅ REST API | — |
| Документы | MongoDB | `ecat-data-mongodb` | ✅ Нативный драйвер | — |
| Объекты | S3 / MinIO | `ecat-data-s3` | ✅ reqwest+rustls | — |

> **Таймаут/Circuit Breaker**: Таймаут = таймаут запроса `query_timeout_secs` (по умолчанию 30 с, `0` = отключено; настраивается пока только для sqlx / mssql); предохранитель = `ecat_data::CircuitBreakerExecutor` (оборачивает любой бэкенд `SqlExecutor`). Разделение чтения/записи — `ecat_data::RdbmsRouting`: запись идёт в primary, чтение по кругу в реплики с **пропуском реплик с открытым предохранителем**; если ни одна реплика недоступна, по умолчанию идёт деградация на primary, а с `fallback_to_primary(false)` возвращается `RdbmsError::NoAvailableReplica`.

> Все бэкенды данных абстрагированы через единый trait (`RdbmsClient` для транзакций, `SqlExecutor` для выполнения и диалекта / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`); подключайте нужный contrib crate по необходимости. Каждый бэкенд предоставляет структуру `XxxConfig` (`#[derive(Deserialize)]`) для загрузки параметров подключения из JSON/YAML-конфигурации.

> **Соглашение об именовании конструкторов**: у crate-ов сообщений (`ecat-mq-*`) главный конструктор — `connect` (например, `KafkaMq::connect(brokers)`, `MqttMq::connect(url)`), плюс `from_config` для загрузки из конфигурации; у бэкендов данных (`ecat-data-*`) большинство главных конструкторов — `new`, исключения: `ecat-data-redis` / `ecat-data-sqlx` / `ecat-data-mssql` используют `connect`, `ecat-data-mongodb` / `ecat-data-s3` предоставляют только `from_config`. Это сложившееся соглашение, принудительно не унифицируется (во избежание ломающих изменений); в окне 3.0 может быть оценена унификация.

### Пример конфигурации баз данных

Каждый бэкенд данных предоставляет структуру `XxxConfig` и метод `from_config()`, выносящие параметры подключения из кода в конфигурационный файл:

```rust
use ecat_data_redis::{RedisCache, RedisConfig};
use ecat_data_sqlx::{SqlxClient, SqlxConfig};
use ecat_data_clickhouse::{ClickhouseClient, ClickhouseConfig};

// 从配置文件加载（JSON 或 YAML）
let config: serde_json::Value = serde_json::from_str(r#"{
    "redis":     {"url": "redis://localhost:6379"},
    "sql":       {"url": "postgres://user:pass@localhost/db"},
    "clickhouse":{"base_url": "http://localhost:8123", "database": "mydb"}
}"#)?;

// Redis
let redis_cfg: RedisConfig = serde_json::from_value(config["redis"].clone())?;
let cache = RedisCache::from_config(redis_cfg).await?;
cache.set("key", b"value", Duration::from_secs(60)).await?;

// RDBMS
let sql_cfg: SqlxConfig = serde_json::from_value(config["sql"].clone())?;
let db = SqlxClient::from_config(sql_cfg).await?;
let rows = db.query("SELECT * FROM users").await?;

// ClickHouse
let ch_cfg: ClickhouseConfig = serde_json::from_value(config["clickhouse"].clone())?;
let ch = ClickhouseClient::from_config(ch_cfg);
ch.execute("INSERT INTO events VALUES (1, 'start')").await?;
```

**Справочник полей конфигурации**:

| Бэкенд | Config | Поля | Пример значения |
|------|--------|------|--------|
| Redis | `RedisConfig` | `url`, `password`? | `redis://localhost:6379` |
| RDBMS | `SqlxConfig` | `url`, `username`?, `password`?, параметры пула (`max_connections` / `query_timeout_secs` / `session_init` и др., см. [Руководство по настройке баз данных](database-config-tutorial.md)) | `postgres://localhost/db` |
| ClickHouse | `ClickhouseConfig` | `base_url`, `database`, `username`?, `password`? | `http://localhost:8123`, `default` |
| QuestDB | `QuestdbConfig` | `base_url`, `username`?, `password`? | `http://localhost:9000` |
| Elasticsearch | `ElasticsearchConfig` | `base_url`, `username`?, `password`? | `http://localhost:9200` |
| OpenSearch | `OpenSearchConfig` | `base_url`, `username`?, `password`? | `http://localhost:9200` |
| InfluxDB | `InfluxConfig` | `base_url`, `org`, `bucket`, `token` | — |
| Neo4j | `Neo4jConfig` | `base_url`, `username`, `password` | — |
| NebulaGraph | `NebulaGraphConfig` | `base_url`, `space`, `username`?, `password`? | — |
| ArangoDB | `ArangoConfig` | `base_url`, `db`, `username`, `password` | — |
| IoTDB | `IotdbConfig` | `base_url`, `username`, `password` | — |
| Memcached | `MemcachedConfig` | `username`?, `password`? (зарезервированные поля) | — |
| TDengine | `TdengineConfig` | `base_url`, `username`, `password`, `database`? | `http://localhost:6041` |
| MongoDB | `MongoConfig` | `url`, `database`, `tls`? | `mongodb://localhost:27017`, `app` |
| S3 | `S3Config` | `endpoint`, `region`, `access_key`, `secret_key`, `tls`? | `http://localhost:9000`, `us-east-1` |

> За двумя исключениями, все Config бэкендов поддерживают опциональное поле `tls` (`TlsClientConfig`) для настройки TLS-аутентификации клиентским сертификатом: `ecat-data-sqlx` его **не поддерживает** — при задании запуск завершится ошибкой (его TLS идёт через параметры URL); `ecat-data-memcached` поле принимает, но **молча игнорирует** (объявлено, нигде не читается). Подробнее см. [Руководство по настройке баз данных](database-config-tutorial.md).

## ORM (ecat-orm)

`ecat-orm` — собственный ORM (макрос сущностей + типобезопасный построитель запросов + миграции) на базе
`SqlExecutor` из `ecat-data`, поэтому **один и тот же API работает и с клиентом, и с `Transaction`**:
`User::insert(&db, &user)` и `User::insert(&tx, &user)` — это один и тот же вызов; при передаче `&tx`
операторы выполняются внутри транзакции вызывающего, ничего не открывая и не фиксируя. Одно определение
сущности годится для SQLite / PostgreSQL / MySQL / TiDB (`ecat-data-sqlx`) и SQL Server (`ecat-data-mssql`),
SQL генерирует слой диалектов.

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::query::{Op, Order};
use ecat_orm::{Entity, Migrator, create_table};
use time::OffsetDateTime;

#[derive(Entity, Debug, Clone)]
#[entity(table = "users")]
pub struct User {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub name: String,
    #[entity(created_at)]
    pub created_at: Option<OffsetDateTime>,
    #[entity(soft_delete)]
    pub deleted_at: Option<OffsetDateTime>,
    #[entity(version)]
    pub version: i64,
    #[entity(has_many = "Post", foreign_key = "user_id")]
    pub posts: Vec<Post>,
}

// Миграции: создание таблиц + обратный SQL (диалект определяется в run() по соединению)
Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .run()
    .await?;

// CRUD: работает и с &db, и с &tx
let id = User::insert(&db, &user).await?;
let mut got = User::find_by_id(&db, id).await?.unwrap();
got.name = "alice-v2".into();
User::update(&db, &got).await?;       // оптимистичная блокировка: WHERE version = старое значение, конфликт -> OptimisticLockConflict
User::delete_by_id(&db, id).await?;   // мягкое удаление: UPDATE проставляет deleted_at, строка остаётся в таблице

// Запрос + постраничный вывод + предзагрузка: по одному запросу IN на связь, без N+1
let page = User::query()
    .filter("name", Op::Like, "ali%")?
    .order_by("id", Order::Desc)?
    .with(&[UserRelation::Posts])
    .paginate(&db, 1, 20)
    .await?;
```

| Возможность | Точка входа |
|------|------|
| Сущность | `#[derive(Entity)]` + `#[entity(table / pk / column / auto_increment / created_at / updated_at / soft_delete / version / has_many / has_one / belongs_to)]` |
| CRUD | `insert` / `insert_many` / `save` / `update` / `update_many` / `upsert` / `find_by_id` / `find_all` / `delete_by_id` / `hard_delete_by_id` |
| Построитель запросов | `query().filter(..).order_by(..).limit(..).offset(..).join(..).fetch(..)`; условие мягкого удаления отключается через `with_trashed()` |
| Блочная запись | `insert_many` / `update_many` автоматически разбиваются по пределу параметров на оператор у диалекта |
| Постраничный вывод | `paginate` (COUNT + выборка страницы) / `paginate_without_count` (без COUNT); страницы нумеруются с 1 |
| Предзагрузка связей | `with(&[UserRelation::Posts])` — 3 субъекта со связями стоят 2 SELECT, а не 4 |
| Миграции | `Migrator::{status, run, down}` + `create_table::<E>()` / `drop_table_sql()`; таблица версий ведётся автоматически |

Полная грамматика атрибутов, семантика ошибок каждого метода и различия диалектов — в разделе ORM
[справочника API](api.md).

## Структура проекта

```
e-cat/
├── ecat/                       # Ядро: жизненный цикл App
├── ecat-transport/             # Абстракция транспорта (Server trait)
├── ecat-transport-http/        # реализация axum
├── ecat-transport-grpc/        # реализация tonic
├── ecat-middleware/            # промежуточное ПО tower::Layer
├── ecat-protos/                # Определения Protobuf
├── ecat-errors/                # Система кодов ошибок
├── ecat-metadata/              # Передача метаданных
├── ecat-encoding/              # Абстракция сериализации
├── ecat-logging/               # Интеграция tracing
├── ecat-registry/              # Регистрация и обнаружение сервисов
├── ecat-config/                # Управление конфигурацией
├── ecat-metrics/               # Интеграция Prometheus
├── ecat-data/                  # Трейты доступа к данным
├── ecat-security/              # Обнаружение атак (security-rust)
├── ecat-cli/                   # Инструменты CLI
├── ecat-health/                # Проверки работоспособности (/health /ready)
├── ecat-auth/                  # Промежуточное ПО аутентификации (JWT / API Key)
├── ecat-client/                # HTTP-клиент для межсервисных вызовов
├── ecat-circuit-breaker/       # Размыкатель цепи (Tower Layer)
├── ecat-registry-consul/       # Регистрация сервисов Consul
├── ecat-config-remote/         # Удалённая конфигурация Consul KV
├── ecat-data-redis/            # Реализация кэша Redis
├── ecat-mq/                    # Абстракция очереди сообщений
├── ecat-events/                # Шина событий (локальная + удалённая)
├── ecat-openapi/               # Генерация spec OpenAPI
├── ecat-bench/                 # Бенчмарки производительности
├── ecat-tracing/               # Распределённая трассировка (внедрение/извлечение trace_id)
├── ecat-registry-etcd/         # Регистрация сервисов etcd
├── ecat-mq-kafka/              # Адаптер очереди сообщений Kafka
├── ecat-data-opensearch/       # Бэкенд поиска OpenSearch
├── ecat-data-influxdb/         # Бэкенд временных рядов InfluxDB
├── ecat-graphql/               # GraphQL endpoint
├── ecat-data-elasticsearch/    # Бэкенд поиска Elasticsearch
├── ecat-data-clickhouse/       # Бэкенд OLAP ClickHouse
├── ecat-data-sqlx/             # Бэкенд RDBMS (SQLite/PG/MySQL/TiDB)
├── ecat-data-mssql/            # Бэкенд SQL Server (tiberius-ng)
├── ecat-orm/                   # ORM: макрос сущностей, построитель запросов, миграции
├── ecat-orm-derive/            # Макрос-производный #[derive(Entity)]
├── ecat-data-memcached/        # Бэкенд кэша Memcached (реализация в памяти)
├── ecat-data-neo4j/            # Бэкенд графов Neo4j
├── ecat-data-nebulagraph/      # Бэкенд графов NebulaGraph
├── ecat-data-arangodb/         # Бэкенд графов ArangoDB
├── ecat-data-iotdb/            # Бэкенд временных рядов IoTDB
├── ecat-data-questdb/          # Бэкенд временных рядов QuestDB
├── ecat-transport-ws/          # WebSocket transport
├── ecat-versioning/            # Маршрутизация версий API
├── ecat-tls/                   # Настройка и авто-генерация сертификатов TLS
├── ecat-deploy/                # Docker / K8s / Helm / CI/CD
├── ecat-lock/                  # Абстракция распределённой блокировки (реализация Redis)
├── ecat-scheduler/             # Планировщик задач tokio
├── ecat-data-tdengine/         # Бэкенд временных рядов TDengine
├── ecat-data-mongodb/          # Бэкенд документов MongoDB
├── ecat-data-s3/               # Бэкенд объектного хранилища S3 / MinIO
├── ecat-mq-rabbitmq/           # Бэкенд сообщений RabbitMQ
├── ecat-mq-mqtt/               # Бэкенд сообщений MQTT
├── ecat-mq-nats/               # Бэкенд сообщений NATS
├── config/                     # Примеры файлов конфигурации
├── docs/                       # Документы проектирования и план экосистемы
└── examples/                   # Примеры проектов
```

## Быстрый старт

### Предварительные требования

- Rust 1.85+ (stable toolchain, требование edition 2024)
- [protoc](https://github.com/protocolbuffers/protobuf) (компилятор Protocol Buffers)

### Установка CLI

```bash
cargo install ecat-cli
```

### Создание сервиса

```bash
# 脚手架生成项目
ecat new helloworld
cd helloworld

# 添加 proto 定义
ecat proto add proto/service.proto

# 生成客户端和服务端代码（tonic-build build.rs，自动补齐 Cargo.toml 依赖）
ecat proto client proto/service.proto
ecat proto server proto/service.proto -t internal/service

# 开发模式运行
ecat run

# 监听 src/ 变更自动重启
ecat run --watch

# 更新所有 ecat-* 依赖
ecat upgrade
```

Откройте `http://localhost:8000/helloworld/ecat`.

### Пример кода

```rust
use ecat::App;
use ecat_transport_http::HttpServer;
use ecat_transport_grpc::GrpcServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let http_srv = HttpServer::new("0.0.0.0:8000");
    let grpc_srv = GrpcServer::new("0.0.0.0:9000");

    let app = App::builder()
        .name("my-service")
        .version("v1.0.0")
        .server(http_srv)
        .server(grpc_srv)
        .on_start(|| async {
            tracing::info!("service started");
            Ok(())
        })
        .on_stop(|| async {
            tracing::info!("service stopped");
            Ok(())
        })
        .build()?;

    app.run().await?; // 阻塞直到 SIGTERM/SIGINT
    Ok(())
}
```

### Агрегирующий crate (ecat)

`ecat` предоставляет re-export с feature-gates — включайте только нужные компоненты:

```rust
use ecat::transport_http::HttpServer;   // feature "http"（默认）
use ecat::middleware::RecoveryLayer;     // feature "middleware"
use ecat::auth::JwtAuthLayer;            // feature "auth"
use ecat::data::redis::RedisCache;       // feature "redis"
```

Features по умолчанию = `http+grpc`; используйте `--no-default-features --features <компонент>` для урезания дерева зависимостей. Полный список features: `http` `grpc` `middleware` `auth` `client` `events` `metrics` `tracing` `circuit-breaker` `consul` `remote` `redis` `orm` `mssql`.

### Middleware

```rust
use tower::ServiceBuilder;
use ecat_middleware::{RecoveryLayer, TracingLayer, LoggingLayer, TimeoutLayer};
use ecat_circuit_breaker::CircuitBreakerLayer;
use ecat_security::SecurityLayer;
use ecat_auth::JwtAuthLayer;
use std::time::Duration;

// JWT 密钥需 ≥32 字节；可链式强制校验 iss/aud 声明（可选，默认不校验）：
// JwtAuthLayer::new(secret)?.required_issuer("my-issuer").required_audience("my-api")
let jwt = JwtAuthLayer::new("change-me-32-bytes-minimum-secret").expect("valid jwt secret");

let layer = ServiceBuilder::new()
    .layer(RecoveryLayer)
    .layer(TracingLayer)
    .layer(LoggingLayer)
    .layer(TimeoutLayer::new(Duration::from_secs(30)))
    .layer(CircuitBreakerLayer::new())
    .layer(jwt)
    .layer(SecurityLayer::new());
```

> Примечание: `ecat_middleware::TracingLayer` не инжектит trace_id; для инъекции trace_id на уровне запроса используйте `ecat_tracing::TracingLayer::new()`.

```rust
// 指标：记录请求计数与时延到全局 registry（与 /metrics 端点共享）
use ecat_metrics::MetricsLayer;
let app = Router::new().route("/hello", get(hello)).layer(MetricsLayer::new());
// 指标名：ecat_http_requests_total / ecat_http_request_duration_seconds
// （标签 method/path/status）。路径含 ID 等高基数场景请用
// MetricsLayer::new().with_path_fn(...) 归一化，避免指标基数爆炸。

// 重试：指数退避；⚠️ 仅对幂等请求（GET/HEAD/PUT/DELETE）安全
use ecat_middleware::RetryLayer;
let retry = RetryLayer::new(3, Duration::from_secs(1), Duration::from_secs(30)); // 含首次共 3 次尝试
// 自定义重试规则：RetryLayer::new(3, ...).with_rule(MyRule)  // 按状态码/响应内容判定

// 校验：路由前校验 header/参数，失败短路返回 JSON 错误（默认 400，with_status 可改 422 等）
use ecat_middleware::{ValidateLayer, ValidateError};
let validate = ValidateLayer::from_fn(|req: &http::Request<axum::body::Body>| {
    if req.headers().contains_key("x-api-key") {
        Ok(())
    } else {
        Err(ValidateError::new("missing x-api-key").with_status(422))
    }
});

// CORS：ecat-middleware 需启用 "cors" feature
use ecat_middleware::{CorsLayer, AllowOrigin};
let cors = CorsLayer::new().allow_origin(AllowOrigin::any());
```

### Обработка ошибок

```rust
use ecat_errors::{Error, ErrorCode};

fn get_user(id: u64) -> Result<User, Error> {
    if id == 0 {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "bad_request",
            "user id must be positive",
        ));
    }
    // ...
}
```

## Этапы реализации

| Этап | Статус | Содержание |
|------|------|------|
| Phase 1 | ✅ Завершено | Скелет проекта, protos, errors, metadata, encoding, logging |
| Phase 2 | ✅ Завершено | Слой Transport (HTTP + gRPC) |
| Phase 3 | ✅ Завершено | Система middleware (Recovery/Tracing/Logging/Timeout) |
| Phase 4 | ✅ Завершено | Управление жизненным циклом App |
| Phase 5 | ✅ Завершено | Registry, Config, Metrics |
| Phase 5.5 | ✅ Завершено | Слой доступа к данным (traits + sqlx backend) |
| Phase 6 | ✅ Завершено | Инструменты CLI (new/proto/run/build) |
| Phase 7 | ✅ Завершено | README, примеры (helloworld), проектная документация |
| Phase 8 | ✅ Завершено | Интеграция обнаружения атак (security-rust, ecat-security) |
| Phase 9 | ✅ Завершено | Экосистема, этап 1 (health / client / circuit-breaker / auth / registry-consul) |
| Phase 10 | ✅ Завершено | Экосистема, этап 2 (redis / mq / events / config-remote) |
| Phase 11 | ✅ Завершено | Экосистема, этап 3 (testing / deploy / bench / openapi) |
| Phase 12 | ✅ Завершено | Усиление коммуникаций и безопасности (gRPC-клиент / OAuth2 / mTLS / распределённая трассировка) |
| Phase 13 | ✅ Завершено | Дополнение бэкендов данных (etcd / Kafka / OpenSearch / InfluxDB) |
| Phase 14 | ✅ Завершено | Эксплуатация и UX (WebSocket / API-версионирование / Helm / CI/CD) |
| Phase 15 | ✅ Завершено | Расширение экосистемы v2 (настоящий Kafka / RabbitMQ / MQTT / NATS / MongoDB / S3 / TDengine / OTLP / распределённые блокировки / планировщик / CLI watch+upgrade) |
| Phase 16 | ✅ Завершено | Усиление поддержки v2.4 (M1 MetricsLayer / M2 RetryLayer / M3 ValidateLayer / M4 CORS / U1 агрегирующий crate ecat / U2 examples / OAuth2 token hash / отслеживание CVE) |

## Известные ограничения

- **Парсинг GraphQL (ecat-graphql)**: поддерживаются параметры полей и вложенные selection (`query_field`/`mutation_field` — богатые resolver-ы имеют доступ к `args`/`variables`/`selection`); по-прежнему не поддерживаются алиасы, fragment-ы и несколько полей верхнего уровня — не выставляйте его как универсальный GraphQL-эндпоинт.
- **Кэш интроспекции OAuth2 (ecat-auth)**: ключ кэша — SHA-256 hash токена (открытый текст токена не хранится); значения кэша фильтруются по whitelist (по умолчанию сохраняются sub/exp/iat/role + iss/aud/scope/roles из extra, настраивается через `cache_claims_whitelist`; при miss возвращаются полные claims, фильтруется только кэшируемое значение); просроченные записи активно удаляются при записи (TTL по умолчанию 300s).
- **Kafka offset (ecat-mq-kafka)**: по умолчанию `enable.auto.commit=false` и нет ручного commit — после перезапуска процесса чтение начинается с конца партиции (latest), сообщения, созданные за время простоя, пропускаются; семантика at-least-once появляется только при явной настройке `auto_commit=true` (после перезапуска продолжение с последней точки commit).

## Цели дизайна

| # | Цель | Описание |
|---|------|------|
| 1 | **Совместимость с Kratos** | Сохранить идеи Kratos: API-first, плагинность, единые абстракции |
| 2 | **Идиоматичный Rust** | Переиспользовать tower::Service, generic-трейты, zero-cost абстракции; никакого «Go in Rust» |
| 3 | **Типобезопасность** | Ошибки ловятся на этапе компиляции, Protobuf-определения полностью типизированы |
| 4 | **Плагинность** | Registry, Config, Logging, Encoding — всё через trait-абстракции |
| 5 | **Полный инструментарий** | CLI поддерживает скаффолдинг проектов, генерацию proto-кода, dev-запуск |
| 6 | **Производительность прежде всего** | Zero-cost абстракции + асинхронный runtime |
| 7 | **Наблюдаемость** | tracing + Prometheus из коробки |
| 8 | **Полная экосистема** | Клиенты, circuit breaker, аутентификация, health check, бэкенды registry |

## Технические пояснения

### Почему tower::Service

[`tower::Service`](https://docs.rs/tower/latest/tower/trait.Service.html) — эквивалент `http.Handler` в асинхронной экосистеме Rust. И axum, и tonic построены на tower, поэтому e-cat не нужен собственный trait для middleware — достаточно предоставить реализации tower::Layer, чтобы добиться того же эффекта, что и middleware Kratos, с нулевыми накладными расходами на адаптеры.

### Почему Cargo Workspace

В соответствии с модульным дизайном Kratos. Все crate-ы `ecat-*` выпускаются в workspace с синхронизированными версиями (сейчас 6.0.0), компилируются независимо, пользователь подключает по необходимости. Базовые crate-ы держат минимальные зависимости, contrib crate-ы дают опциональные интеграции.

### Почему prost (а не protobuf-rs)

prost — наиболее широко используемая protobuf-реализация в сообществе Rust, генерирует типобезопасный код на этапе компиляции и глубоко интегрирована с tonic.

## Проектная документация

- [Спецификация дизайна](../../../docs/superpowers/specs/2026-07-29-ecat-framework-design.md)
- [План реализации](../../../docs/superpowers/plans/2026-07-29-ecat-framework.md)
- [Экосистемный план v1](ecosystem-plan.md) (завершён)
- [Экосистемный план v2](ecosystem-plan-v2.md) (завершён)
- [Экосистемный план v3](ecosystem-plan-v3.md) (финальная оценка)
- [API-справочник](api.md)
- [Отчёт об аудите r5](audit-report-2026-08-01-r5.md) (2026-08-01)
- [Руководство по настройке баз данных](database-config-tutorial.md)
- [Отслеживание CVE зависимостей](dependency-cve-tracking.md)
- [Руководство по TLS-сертификатам](tls-certificate-tutorial.md)
- [Пример файла конфигурации](../../../config/databases.example.yaml)

## Поддержка

Будем рады вашей поддержке!

| WeChat Pay | Alipay |
|:---:|:---:|
| <img src="weixinpay.png" width="130" height="130" alt="WeChat Pay"> | <img src="alipay.png" width="130" height="130" alt="Alipay"> |

### Глобальные переводы (банковский перевод)

| Поле | Информация |
|------|------|
| Recipient Name (收款人姓名) | WANG KEXUN |
| Account Number (收款账户号码) | 881015918251 |
| Receiving Bank (收款银行) | ZA Bank Limited |
| SWIFT Code | AABLHKHHXXX |
| Bank Code (银行编号) | 387 |
| Bank Address (银行地址) | Core F, Cyberport 3, 100 Cyberport Road, Hong Kong |

> **Агент-посредник для трансграничных переводов (при необходимости)**: это информация об агентском (промежуточном) банке, а не о банке-получателе; уточните у своего банка-отправителя, требуется ли она.
>
> - Переводы в гонконгских долларах, юанях и долларах США: **Citibank N.A. Hong Kong** (SWIFT: `CITIHKHXXXX`, Bank Code: 006, Branch: Hong Kong Branch, Branch Code: 391, Address: Citibank Tower, Citibank Plaza, 3 Garden Road, Central, Hong Kong)
> - Переводы в других валютах: **THE BANK OF NEW YORK MELLON** (SWIFT: `IRVTUS3NXXX`, Address: 240 GREENWICH STREET, NEW YORK, United States)

### Пожертвование в криптовалюте (Crypto Donation)

Если этот проект помог вам, отсканируйте QR-код, чтобы сделать пожертвование, спасибо!

| <img src="../../coin/1.jpg" width="200" alt="BNB Smart Chain (BEP20)"><br>**BNB Smart Chain (BEP20)**<br>`0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../coin/2.jpg" width="200" alt="Tron (TRC20)"><br>**Tron (TRC20)**<br>`TEdDHWLajt1XvqtPDWmQctdrJaC3pzZZzz` |
| <img src="../../coin/3.jpg" width="200" alt="Ethereum (ERC20)"><br>**Ethereum (ERC20)**<br>`0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../coin/4.jpg" width="200" alt="Aptos"><br>**Aptos**<br>`0x836e3780edfc3f7b2372b39e2a1a3a5d7adfaccd96c726f21cfde1b50dd68030` |
| <img src="../../coin/5.jpg" width="200" alt="Plasma"><br>**Plasma**<br>`0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../coin/6.jpg" width="200" alt="Polygon POS"><br>**Polygon POS**<br>`0x355d429f97511897ccb4e271ec888205f9ab6629` |
| <img src="../../coin/7.jpg" width="200" alt="Solana"><br>**Solana**<br>`2hfhboHdmdrYsY25XfQSsEWxq5ip4EQsR7f4AzSRMUyr` | <img src="../../coin/8.jpg" width="200" alt="The Open Network (TON)"><br>**The Open Network (TON)**<br>`UQB9kFQohzmXUir9QSSZq01iwl9aQZIDdBpNmDklljRtCoGK` |
| <img src="../../coin/9.jpg" width="200" alt="Arbitrum One"><br>**Arbitrum One**<br>`0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../coin/10.jpg" width="200" alt="AVAX C-Chain"><br>**AVAX C-Chain**<br>`0x355d429f97511897ccb4e271ec888205f9ab6629` |

## Лицензия

Apache-2.0
