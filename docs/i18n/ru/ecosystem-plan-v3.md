# Экосистемный план e-cat v3 — финальная оценка

> **Обновление (2026-08-07, v2.3.3)**: оставшийся пробел #1 «mTLS в transport» выполнен — `HttpServer::tls` / `GrpcServer::tls` реально работают на основе tokio-rustls / rustls tonic (поддержка проверки CA и принудительных клиентских сертификатов); пробелы #2 (rate limit Redis) и #3 (GitLab CI) ранее выполнены в составе v2.3.0. Все пробелы из плана теперь закрыты.

> **Обновление (2026-10-07, v4.2.0)**: план v4.0 полностью реализован — `ecat-orm` / `ecat-orm-derive`, `ecat-data-mssql`, расширения пула (`CircuitBreakerExecutor` / `RdbmsRouting`) и три feature наблюдаемости (`metrics` / `health` / `tracing`) готовы.

**Версия:** 2.4.2  
**Дата:** 2026-08-01  
**Всего crates:** 55 · весь план выполнен

---

## Текущее покрытие

| Область | Реализовано | Покрытие |
|------|--------|--------|
| Транспорт | HTTP (axum), gRPC (tonic), WebSocket | 100% |
| Кодирование | JSON, Protobuf | 100% |
| Middleware | Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, Auth×3 | 100% |
| Конфигурация | env, file (JSON/YAML), Consul KV, шифрование (XOR) | 100% |
| Реестр | memory, Consul, etcd | 100% |
| Безопасность | Обнаружение атак, JWT, API Key, OAuth2, клиентские TLS-сертификаты, mTLS | 95% |
| Коммуникации | Клиентские TLS-сертификаты — поддерживаются всеми бэкендами данных | 95% |
| Сервисные коммуникации | HTTP Client, gRPC Client, Resolver, LoadBalancer | 95% |
| Данные | RDBMS (sqlx), Redis, OpenSearch, Elasticsearch, ClickHouse, Memcached, Neo4j, NebulaGraph, ArangoDB, InfluxDB, IoTDB, QuestDB — все поддерживают файловую конфигурацию | 95% |
| Сообщения | trait MessageQueue, InMemory, Kafka, EventBus | 100% |
| Наблюдаемость | tracing, Prometheus, Health, распределённая трассировка | 100% |
| DevOps | CLI, Dockerfile, K8s, Helm, GitHub Actions, Bench, Testing | 95% |
| API-инструменты | OpenAPI, Versioning, GraphQL | 100% |

---

## Оставшиеся пробелы

### Стоит сделать (3 пункта)

| # | Пробел | Ценность | Объём работы |
|---|------|------|--------|
| 1 | **mTLS в transport** | TlsConfig уже есть, не подключён к HttpServer/GrpcServer | Малый |
| 2 | **Бэкенд rate limit на Redis** | RateLimitLayer только в памяти, для нескольких инстансов нужен общий | Малый |
| 3 | **Шаблон GitLab CI** | GitHub Actions уже есть | Малый |

### Делать не нужно (2 пункта)

| # | Пробел | Причина |
|---|------|------|
| 4 | Конфигурация AES-GCM | Текущего XOR достаточно |
| 5 | Service mesh / API-шлюз | Оставить сообществу (Linkerd/Kong/K8s) |

---

## Вывод

**e-cat достиг зрелости, пригодной для продакшена.** 47 crates покрывают полный стек микросервисов: транспорт → middleware → service discovery → конфигурация → безопасность → данные → сообщения → наблюдаемость → DevOps → API-инструменты. Оставшиеся 3 пробела — оптимизации малого объёма, структурных недочётов нет.

## Покрытие бэкендов данных (16 шт.)

| Категория | База данных | Crate | Способ драйвера |
|------|--------|-------|----------|
| RDBMS | SQLite/PostgreSQL/MySQL/TiDB | `ecat-data-sqlx` | sqlx (официальный асинхронный драйвер) |
| RDBMS | SQL Server | `ecat-data-mssql` | tiberius-ng + deadpool (драйвер TDS + пул соединений) |
| Кэш | Redis | `ecat-data-redis` | redis-rs (официальный драйвер) |
| Кэш | Memcached | `ecat-data-memcached` | ⚠️ Реализация в памяти (не для продакшена) |
| Документы | MongoDB | `ecat-data-mongodb` | mongodb (официальный драйвер) |
| Объекты | S3 / MinIO | `ecat-data-s3` | HTTP/REST (reqwest+rustls, собственный SigV4) |
| OLAP | ClickHouse | `ecat-data-clickhouse` | HTTP/REST (reqwest) |
| Поиск | OpenSearch | `ecat-data-opensearch` | HTTP/REST (reqwest) |
| Поиск | Elasticsearch | `ecat-data-elasticsearch` | HTTP/REST (reqwest) |
| Граф | Neo4j | `ecat-data-neo4j` | HTTP/REST (reqwest) |
| Граф | NebulaGraph | `ecat-data-nebulagraph` | HTTP/REST (reqwest) |
| Граф | ArangoDB | `ecat-data-arangodb` | HTTP/REST (reqwest) |
| Врем. ряды | InfluxDB | `ecat-data-influxdb` | HTTP/REST (reqwest) |
| Врем. ряды | Apache IoTDB | `ecat-data-iotdb` | HTTP/REST (reqwest) |
| Врем. ряды | QuestDB | `ecat-data-questdb` | HTTP/REST (reqwest) |
| Врем. ряды | TDengine | `ecat-data-tdengine` | HTTP/REST (reqwest) |

---

## План v4.0 (2026-10-05) — полный ORM и SQL Server

> Статус: **завершено** (v4.2.0). ORM, SQL Server, расширения пула и наблюдаемость реализованы — см. таблицу ниже.
> Полный дизайн: [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../../../docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md).

Предстоит закрыть два структурных пробела:

1. **Полный ORM**: сейчас слой данных предлагает только RDBMS-клиенты с рукописным SQL
   (`Row` = имена столбцов + значения JSON) — без маппинга сущностей, связей и миграций. v4.0
   добавляет `ecat-orm` (макросы сущностей / CRUD / конструктор запросов / предварительная загрузка
   связей / соединения / массовые операции / пагинация / миграции) + `ecat-orm-derive`. Построен на
   едином trait `SqlExecutor` и **естественно покрывает все бэкенды RDBMS**.
2. **Бэкенд SQL Server**: в основной ветке sqlx нет драйвера MSSQL (удалён до 0.7, переписывание не
   выпущено); v4.0 добавляет `ecat-data-mssql` (`tiberius-ng` 0.13 + `deadpool` 0.13) —
   бэкендов данных стало 15 → **16** (`ecat-data-mssql`).

Сопутствующие изменения фундамента:

| Изменение | Описание | Статус |
|---|---|---|
| `ecat-data-sqlx` отказывается от `AnyPool` в пользу нативных пулов | Исправляет ограничения временных типов (обходные пути через CAST больше не нужны), убирает поверхность panic при установке драйвера, включает statement cache | ✅ Выполнено (пакет 1, три нативных пула `PgPool`/`MySqlPool`/`SqlitePool`) |
| `ecat-data` выделяет supertrait `SqlExecutor` | SQL можно выполнять внутри транзакций (сейчас `Transaction` умеет только commit/rollback); основа для ORM и разделения чтения/записи | ✅ Выполнено (пакет 1) |
| Расширения пула соединений | Таймаут запроса, прогрев `warm_up()`, умный recycle, предохранитель (переиспользует `ecat-circuit-breaker`), разделение чтения/записи `RdbmsRouting` | ✅ Выполнено (пакет 4 — таймаут запроса и `warm_up()` в пакете 1; `CircuitBreakerExecutor` и `RdbmsRouting` в пакете 4) |
| Наблюдаемость | Метрики пула в `ecat-metrics`, проверки работоспособности пула в `ecat-health`, медленные запросы в `ecat-tracing` (все — opt-in feature) | ✅ Выполнено (пакет 4; feature `metrics` / `health` / `tracing` по умолчанию выключены) |

**Критические изменения**: разделение trait + сигнатура `SqlxClient::from_pool` + удаление
`AnyPool`——все три уже реализованы в ветке `feat/orm-mssql` (пакет 1, ещё не выпущен); при выпуске
версия workspace 3.0.3 → **4.0.0**.
