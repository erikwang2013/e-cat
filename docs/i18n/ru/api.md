<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Справочник API Ecat

На этой странице собраны интерфейсы (API) фреймворка Ecat: соглашения о портах, встроенные эндпоинты, формат ошибок и интерфейсы расширений. Бизнес-маршруты регистрируются каждым сервисом самостоятельно.

## Соглашения о портах

| Протокол | Адрес прослушивания | Описание |
|------|----------|------|
| HTTP | `0.0.0.0:8000` | Маршруты axum, порт примера по умолчанию |
| gRPC | `0.0.0.0:9000` | tonic Server, порт примера по умолчанию |

## Встроенные эндпоинты

Следующие эндпоинты предоставляются экосистемными crate-ами и монтируются вместе с сервисом:

| Эндпоинт | Источник | Описание |
|------|------|------|
| `/health` | ecat-health | Проверка живости (возвращает имя сервиса, версию, время запуска) |
| `/ready` | ecat-health | Проверка готовности (возвращает 200, когда зависимости готовы) |
| `/metrics` | ecat-metrics | Выдача метрик Prometheus (`ecat_http_requests_total` / `ecat_http_request_duration_seconds`) |
| `/{service}/{method}` | Маршруты пользователя | Пример: `/helloworld/ecat` |

> В сценариях высокой кардинальности путей (например, пути с ID) используйте `MetricsLayer::new().with_path_fn(...)` для нормализации и предотвращения взрыва кардинальности метрик.

## Поток обработки запроса

```
客户端请求
  ├─ HTTP :8000 ──→ axum::Router ─┐
  └─ gRPC :9000 ──→ tonic::Server ─┤
                              ┌─────┴──────┐
                              │ Middleware │  Recovery→Tracing→Logging→Auth→Metrics→Security→CircuitBreaker
                              └─────┬──────┘
                                    ▼
                               Handler（tower::Service）
                                    ▼
                               Response（JSON/Protobuf 编码）
```

## Формат ошибок

`ecat-errors` предоставляет `ErrorCode` + `Error` с сопоставлением HTTP-статусов на этапе компиляции:

```rust
use ecat_errors::{Error, ErrorCode};

Error::new(ErrorCode::InvalidArgument, "bad_request", "user id must be positive");
```

Ответы с ошибками кодируются middleware в JSON (или Protobuf) и содержат code / reason / message.

## Интерфейсы расширений

| Возможность | Crate | Интерфейс |
|------|-------|------|
| GraphQL | ecat-graphql | Эндпоинт `/graphql`; поддерживает параметры полей и вложенные selection, не поддерживает алиасы, fragment-ы и несколько полей верхнего уровня |
| OpenAPI | ecat-openapi | Генерация OpenAPI spec из маршрутов |
| WebSocket | ecat-transport-ws | Апгрейднутый WS-транспорт |
| Версионирование API | ecat-versioning | Версионирование маршрутов с префиксом `/v1/...` |
| Аутентификация | ecat-auth | Middleware JWT / API Key; ключ JWT должен быть ≥32 байт, доступны цепочные `required_issuer`/`required_audience` |
| gRPC-клиент | ecat-transport-grpc | Интеграция service discovery и балансировки нагрузки |

## Взаимодействие между сервисами

- `HttpClient` (ecat-client): интеграция service discovery и балансировки нагрузки, защита CircuitBreaker
- `GrpcClient` (ecat-transport-grpc): то же самое, по протоколу gRPC
- Middleware единообразно комбинируются через `tower::ServiceBuilder` (Recovery / Tracing / Logging / Timeout / RateLimit / Security / CircuitBreaker / Metrics / Retry / Validate / CORS)

## Интерфейсы бэкендов данных

Все бэкенды данных (`ecat-data-*`) абстрагированы через единые trait-ы (`RdbmsClient` для транзакций, `SqlExecutor` для выполнения и диалекта / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`); REST-подобные бэкенды (Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine / OpenSearch / Elasticsearch / S3) обращаются к соответствующим HTTP-интерфейсам через `base_url`. Настройка подключения — в [Руководстве по настройке баз данных](database-config-tutorial.md).

## ORM (ecat-orm)

`ecat-orm` предоставляет макрос-производный для сущностей, типобезопасный построитель запросов, CRUD,
предзагрузку связей и миграции. Любая операция с данными принимает `&impl SqlExecutor`, поэтому **один и
тот же API работает и с клиентом, и с `Transaction`**:

```rust
User::find_by_id(&db, 1).await?;              // db: SqlxClient / MssqlClient
let tx = db.transaction().await?;
User::update(&tx, &user).await?;              // tx: Transaction
User::insert(&tx, &user).await?;              // работает и внутри транзакции: оба оператора идут в ней
tx.commit().await?;
```

SQL генерирует слой диалектов: SQLite / PostgreSQL / MySQL / TiDB (`ecat-data-sqlx`) и
SQL Server (`ecat-data-mssql`) делят одно определение сущности.

### Определение сущности и грамматика атрибута `#[entity(...)]`

`#[derive(Entity)]` генерирует `Entity::META` (таблица / столбцы / связи / флаги), `from_row` /
`to_values` / `pk_value`, а также по одному перечислению `XxxRelation` на сущность (имена вариантов —
PascalCase имени поля связи).

Атрибут контейнера:

| Запись | Значение |
|------|------|
| `#[entity(table = "users")]` | Имя таблицы; если опущено — snake_case имени структуры (`User` → `user`, `UserProfile` → `user_profile`) |

Поля-столбцы:

| Запись | Значение |
|------|------|
| `#[entity(column = "user_name")]` | Переопределение имени столбца, по умолчанию — имя поля |
| `#[entity(pk)]` | Первичный ключ |
| `#[entity(auto_increment)]` | Автоинкремент (подразумевает pk); автоинкрементный ключ не попадает в список столбцов вставки, его генерирует база |
| `#[entity(created_at)]` / `#[entity(updated_at)]` | Автоматические метки времени: при вставке заполняются обе, при обновлении обновляется только `updated_at` |
| `#[entity(soft_delete)]` | Столбец мягкого удаления: путь чтения автоматически добавляет к нему условие `IS NULL` |
| `#[entity(version)]` | Столбец оптимистичной блокировки: обновление пишет `version + 1` и сверяет старое значение; конфликт возвращает `OrmError::OptimisticLockConflict` |

Поля-связи (**тип контейнера — жёсткое требование**; «голый» тип сущности отвергается на этапе
компиляции — он не может выразить «не найдено»):

| Запись | Тип поля | Значение |
|------|----------|------|
| `#[entity(has_many = "Post", foreign_key = "user_id")]` | `Vec<Post>` | Один-ко-многим: значение `local_key` этой таблицы ищется в столбце `foreign_key` целевой |
| `#[entity(has_one = "Profile", foreign_key = "user_id")]` | `Option<Profile>` | Один-к-одному; для однозначной связи берётся только первая строка |
| `#[entity(belongs_to = "Tag", foreign_key = "tag_code")]` | `Option<Tag>` | Многие-к-одному: значение `foreign_key` этой таблицы ищется по **первичному ключу целевой таблицы** |

`local_key` можно опустить: `has_many` / `has_one` по умолчанию берут первичный ключ этой таблицы,
`belongs_to` — первичный ключ целевой. Поля-связи **не являются столбцами**. Соответствие типа поля типу
столбца существует только в реализациях `value::ColumnValue` (макрос не хранит второй копии этой
таблицы); неподдерживаемый тип даёт неудовлетворённое требование `T: ColumnValue` с указанием на это поле.

### CRUD

| Метод | Примечание |
|------|------|
| `Entity::insert(&db, &e) -> i64` | Вставляет и возвращает новый первичный ключ. Двухшаговый путь MySQL (`INSERT`, затем `SELECT LAST_INSERT_ID()`, а он **привязан к соединению**) оборачивается в одну транзакцию в `SqlExecutor::execute_then_query` |
| `Entity::insert_many(&db, &[e]) -> u64` | Массовая вставка, возвращает число затронутых строк (ключи не возвращаются: `LAST_INSERT_ID()` даёт первую строку, SQLite — последнюю, между бэкендами это ненадёжно) |
| `Entity::save(&db, &e)` | Вставляет, если первичный ключ «не задан» (автоинкремент и значение 0), иначе обновляет; возвращает `()` |
| `Entity::update(&db, &e) -> u64` | Полное обновление строки по первичному ключу. Ноль затронутых строк — ошибка: `OrmError::NotFound` без `version`, `OrmError::OptimisticLockConflict` с ним (без дополнительного запроса их не различить) |
| `Entity::update_many(&db, &[e]) -> u64` | Обновляет построчно и суммирует строки; массовый вызов **не сообщает, какая строка** конфликтовала (результат меньше входа означает, что кого-то остановил барьер версии) |
| `Entity::upsert(&db, &e) -> u64` | Upsert по первичному ключу (`ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE` в зависимости от диалекта) |
| `Entity::find_by_id(&db, pk) -> Option<Self>` | Берёт одну строку по первичному ключу; если её нет — `Ok(None)` |
| `Entity::find_all(&db) -> Vec<Self>` | Все строки без фильтра (**на больших таблицах это полный перебор**; для постраничного вывода используйте `paginate`) |
| `Entity::delete_by_id(&db, pk) -> u64` | При объявленном `soft_delete` отправляет `UPDATE`, проставляющий время удаления; строка остаётся в таблице, а повторное удаление возвращает `NotFound` и не обновляет время |
| `Entity::hard_delete_by_id(&db, pk) -> u64` | Обходит мягкое удаление и действительно отправляет `DELETE` |

### Построитель запросов

Начинаем с `User::query()`, что даёт `Query<User, Unfiltered>`; добавление фильтра превращает его в
`Query<User, Filtered>` (состояние типа) — «удалить без WHERE» не компилируется.

```rust
use ecat_orm::query::{Op, Order};

let users = User::query()
    .filter("name", Op::Like, "alice%")?        // Eq / Ne / Lt / Le / Gt / Ge / Like
    .filter("email", Op::NotNull, serde_json::json!(null))?
    .filter("id", Op::In, serde_json::json!([1, 2, 3]))?  // In / NotIn принимают массив
    .order_by("id", Order::Desc)?
    .limit(10)
    .offset(20)
    .fetch(&db)
    .await?;
```

- Имена столбцов проверяются по белому списку `EntityMeta.columns` (`OrmError::UnknownColumn`); столбцов
  присоединённых таблиц там нет — для них используйте `filter_raw("...")`, помня, что он **не выполняет
  никакой проверки: входные данные должны быть доверенными**.
- `with_trashed()` отключает условие мягкого удаления (мягко удалённые строки тоже возвращаются).
- `join(JoinType::Left, "posts", "posts.user_id = users.id")` поддерживает `Inner` / `Left` и служит для
  фильтрации по столбцам присоединённой таблицы внутри `filter_raw`. **Список столбцов идёт без префикса
  таблицы**, поэтому если присоединённая таблица имеет общий столбец с основной, реальная база сообщит
  `ambiguous column name` — присоединяйте таблицы, имена столбцов которых не совпадают с основной
  (проверено на SQLite).
- `delete_where(&db)` / `hard_delete_where(&db)` удаляют по тем же условиям (сущности с мягким удалением
  идут через `UPDATE`).
- `fetch(&db)` — точка входа выполнения; `find_by_id` / `find_all` / `paginate` переиспользуют тот же путь
  генерации SQL за ней.

### Разбиение на блоки и постраничный вывод

- **Массовые записи разбиваются автоматически**: `insert_many` / `update_many` делятся по пределу
  параметров на один оператор у диалекта (например, 2100 в SQL Server) и суммируют строки. Для ручного
  разбиения используйте тот же предел:
  `ecat_orm::dialect::lookup(dialect).max_params_per_stmt()`.
- **Постраничный вывод**: `paginate(&db, page, per_page)` отправляет 2 запроса (COUNT + выборка страницы),
  страницы **нумеруются с 1**; COUNT переиспользует тот же WHERE / JOIN, но убирает ORDER BY / LIMIT /
  OFFSET. Возвращается `Page { items, total, page, per_page }`, а `total_pages()` / `has_next()` считаются
  из `total`.
- Для глубокого листания больших таблиц используйте `paginate_without_count(&db, page, per_page)` (1 запрос):
  `total` равен `None`, и `total_pages()` / `has_next()` тоже не могут ответить — то, что нельзя вычислить,
  так и сообщается, а не угадывается.

### Предзагрузка связей

```rust
let users = User::query()
    .with(&[UserRelation::Posts, UserRelation::Profile])
    .fetch(&db)
    .await?;
```

- Каждая связь отправляет **один** запрос `IN` (с разбиением, когда ключей больше предела параметров на
  оператор), а сам запрос субъектов не делает join — **никакого N+1**: получить 3 пользователей и их posts
  стоит 2 SELECT (1 по субъектам + 1 `IN`), а не 4.
- Без `with()` поля связей пусты (`Vec::new()` / `None`) и никогда не содержат устаревших значений от
  предыдущей загрузки; `set_relation` вызывается для каждого субъекта (в том числе при пустом результате)
  именно для очистки остатков.
- Однозначные связи (`has_one` / `belongs_to`) берут только первую строку.

### Миграции

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Migrator, create_table};

Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .add("002_posts", create_table::<Post>())
    .status().await?;      // MigrationStatus { applied, pending, unknown }
    .run().await?;         // применяет pending и записывает каждую в таблицу версий; повторный запуск идемпотентен
    .down(1).await?;       // выполняет обратный SQL миграции 001 и удаляет её строку из таблицы версий
```

- Числовой префикс имени миграции — это её версия (`"001_users"` → 1); нечисловой префикс даёт
  `OrmError::InvalidMigrationName` — он никогда не трактуется как 0.
- `create_table::<E>()` / `drop_table::<E>()` генерируют DDL из `EntityMeta`, а **диалект определяется в
  момент `run()` по `dialect()` соединения**, поэтому список миграций не зависит от строки подключения.
  Для собственного SQL (ALTER, перенос данных) используйте
  `ecat_orm::migrate::MigrationBuilder::new(|d| ...)`.
- Вызов `down` для миграции без обратного SQL даёт `OrmError::MigrationIrreversible` — «DROP, а потом
  снова CREATE» теряет данные, и это не угадывается за вызывающего.
- Таблицу версий `Migrator` создаёт сам (в MSSQL нет `CREATE TABLE IF NOT EXISTS`, поэтому сначала
  проверяется наличие).
