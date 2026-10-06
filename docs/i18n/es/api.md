<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Referencia de API de Ecat

Esta página resume la superficie de interfaz (API) del framework Ecat: convenciones de puertos, endpoints integrados, formato de errores e interfaces extensibles. Las rutas de negocio son registradas por cada servicio.

## Convenciones de puertos

| Protocolo | Dirección de escucha | Descripción |
|------|----------|------|
| HTTP | `0.0.0.0:8000` | Router axum, puerto de ejemplo por defecto |
| gRPC | `0.0.0.0:9000` | Servidor tonic, puerto de ejemplo por defecto |

## Endpoints integrados

Los siguientes endpoints los proporcionan los crates del ecosistema y se montan junto con el servicio:

| Endpoint | Origen | Descripción |
|------|------|------|
| `/health` | ecat-health | Comprobación de liveness (devuelve nombre del servicio, versión, tiempo de arranque) |
| `/ready` | ecat-health | Comprobación de readiness (devuelve 200 cuando las dependencias están listas) |
| `/metrics` | ecat-metrics | Exposición de métricas Prometheus (`ecat_http_requests_total` / `ecat_http_request_duration_seconds`) |
| `/{service}/{method}` | Rutas del usuario | Ejemplo: `/helloworld/ecat` |

> En escenarios de alta cardinalidad (rutas que contienen IDs, etc.), usa `MetricsLayer::new().with_path_fn(...)` para normalizar y evitar la explosión de cardinalidad de métricas.

## Flujo de procesamiento de peticiones

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

## Formato de errores

`ecat-errors` proporciona `ErrorCode` + `Error`, con mapeo de códigos de estado HTTP en tiempo de compilación:

```rust
use ecat_errors::{Error, ErrorCode};

Error::new(ErrorCode::InvalidArgument, "bad_request", "user id must be positive");
```

La respuesta de error se codifica como JSON (o Protobuf) a través del middleware, con code / reason / message.

## Interfaces extensibles

| Capacidad | Crate | Interfaz |
|------|-------|------|
| GraphQL | ecat-graphql | Endpoint `/graphql`; admite parámetros de campo y selections anidadas; no admite alias, fragments ni múltiples campos de nivel superior |
| OpenAPI | ecat-openapi | Genera el spec OpenAPI a partir de las rutas |
| WebSocket | ecat-transport-ws | Transporte WS actualizado |
| Enrutado por versión de API | ecat-versioning | Enrutado con prefijo de versión `/v1/...` |
| Autenticación | ecat-auth | Middleware JWT / API Key; la clave JWT debe tener ≥32 bytes, admite encadenar `required_issuer`/`required_audience` |
| Cliente gRPC | ecat-transport-grpc | Integra descubrimiento de servicios y balanceo de carga |

## Comunicación entre servicios

- `HttpClient` (ecat-client): integra descubrimiento de servicios y balanceo de carga, con protección por disyuntor CircuitBreaker
- `GrpcClient` (ecat-transport-grpc): igual, sobre protocolo gRPC
- Los middleware se componen unificadamente con `tower::ServiceBuilder` (Recovery / Tracing / Logging / Timeout / RateLimit / Security / CircuitBreaker / Metrics / Retry / Validate / CORS)

## Interfaces de backends de datos

Todos los backends de datos (`ecat-data-*`) se abstraen mediante traits unificados (`RdbmsClient` para transacciones, `SqlExecutor` para ejecución y dialecto / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`); los backends tipo REST (Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine / OpenSearch / Elasticsearch / S3) acceden a sus interfaces HTTP correspondientes a través de `base_url`. Consulta la configuración de conexión en el [Tutorial de configuración de base de datos](database-config-tutorial.md).

## ORM (ecat-orm)

`ecat-orm` proporciona una macro derivada de entidad, un constructor de consultas tipado, CRUD,
precarga de relaciones y migraciones. Toda operación de datos recibe `&impl SqlExecutor`, así que **la
misma API sirve con un cliente y con una `Transaction`**:

```rust
User::find_by_id(&db, 1).await?;              // db: SqlxClient / MssqlClient
let tx = db.transaction().await?;
User::update(&tx, &user).await?;              // tx: Transaction
User::insert(&tx, &user).await?;              // también dentro de una transacción: ambas sentencias corren ahí
tx.commit().await?;
```

El SQL lo genera la capa de dialecto: SQLite / PostgreSQL / MySQL / TiDB (`ecat-data-sqlx`) y
SQL Server (`ecat-data-mssql`) comparten una misma definición de entidad.

### Definición de entidad y gramática del atributo `#[entity(...)]`

`#[derive(Entity)]` genera `Entity::META` (tabla / columnas / relaciones / banderas), `from_row` /
`to_values` / `pk_value`, y una enumeración `XxxRelation` por entidad (los nombres de variante son el
PascalCase del nombre del campo de relación).

Atributo de contenedor:

| Sintaxis | Significado |
|------|------|
| `#[entity(table = "users")]` | Nombre de tabla; si se omite, el snake_case del nombre de la estructura (`User` → `user`, `UserProfile` → `user_profile`) |

Campos de columna:

| Sintaxis | Significado |
|------|------|
| `#[entity(column = "user_name")]` | Nombre de columna alternativo; por defecto, el nombre del campo |
| `#[entity(pk)]` | Clave primaria |
| `#[entity(auto_increment)]` | Autoincremento (implica pk); la clave autoincremental queda fuera de la lista de columnas de inserción y la genera la base de datos |
| `#[entity(created_at)]` / `#[entity(updated_at)]` | Marcas de tiempo automáticas: ambas se rellenan al insertar, solo `updated_at` se refresca al actualizar |
| `#[entity(soft_delete)]` | Columna de borrado lógico: la ruta de lectura añade automáticamente un filtro `IS NULL` sobre ella |
| `#[entity(version)]` | Columna de bloqueo optimista: las actualizaciones escriben `version + 1` y comparan con el valor anterior; un conflicto devuelve `OrmError::OptimisticLockConflict` |

Campos de relación (**el tipo contenedor es una restricción dura**; un tipo de entidad desnudo se
rechaza en compilación — no puede expresar «no encontrado»):

| Sintaxis | Tipo del campo | Significado |
|------|----------|------|
| `#[entity(has_many = "Post", foreign_key = "user_id")]` | `Vec<Post>` | Uno a muchos: el valor de `local_key` de esta tabla se compara con la columna `foreign_key` del destino |
| `#[entity(has_one = "Profile", foreign_key = "user_id")]` | `Option<Profile>` | Uno a uno; una relación de valor único toma solo la primera fila |
| `#[entity(belongs_to = "Tag", foreign_key = "tag_code")]` | `Option<Tag>` | Muchos a uno: el valor de `foreign_key` de esta tabla se compara con la **clave primaria del destino** |

`local_key` puede omitirse: `has_many` / `has_one` usan por defecto la clave primaria de esta tabla,
`belongs_to` la del destino. Los campos de relación **no son columnas**. La correspondencia entre tipo de
campo y tipo de columna solo existe en las impls de `value::ColumnValue` (la macro no guarda una segunda
copia); un tipo no admitido produce un error de `T: ColumnValue` no satisfecho que apunta a ese campo.

### CRUD

| Método | Nota |
|------|------|
| `Entity::insert(&db, &e) -> i64` | Inserta y devuelve la nueva clave primaria. El camino en dos pasos de MySQL (`INSERT` y luego `SELECT LAST_INSERT_ID()`, que está **ligado a la conexión**) lo envuelve en una sola transacción `SqlExecutor::execute_then_query` |
| `Entity::insert_many(&db, &[e]) -> u64` | Inserción masiva, devuelve filas afectadas (sin claves de vuelta: `LAST_INSERT_ID()` da la primera fila, SQLite la última — poco fiable entre backends) |
| `Entity::save(&db, &e)` | Inserta si la clave primaria está «sin definir» (autoincremental y con valor 0); si no, actualiza. Devuelve `()` |
| `Entity::update(&db, &e) -> u64` | Actualización completa por clave primaria. Cero filas afectadas es un error: `OrmError::NotFound` sin `version`, `OrmError::OptimisticLockConflict` con ella (sin una consulta extra no se distinguen) |
| `Entity::update_many(&db, &[e]) -> u64` | Actualiza fila a fila y suma las filas; una llamada masiva **no puede decir qué fila** entró en conflicto (un resultado menor que la entrada significa que alguien fue detenido por la barrera de versión) |
| `Entity::upsert(&db, &e) -> u64` | Upsert por clave primaria (`ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE` según el dialecto) |
| `Entity::find_by_id(&db, pk) -> Option<Self>` | Obtiene una fila por clave primaria; devuelve `Ok(None)` si no existe |
| `Entity::find_all(&db) -> Vec<Self>` | Todas las filas sin filtro (**un escaneo completo en tablas grandes**; para paginar usa `paginate`) |
| `Entity::delete_by_id(&db, pk) -> u64` | Con `soft_delete` declarado envía un `UPDATE` que fija la marca de borrado; la fila sigue en la tabla, y volver a borrar devuelve `NotFound` sin refrescar la marca |
| `Entity::hard_delete_by_id(&db, pk) -> u64` | Omite el borrado lógico y envía realmente `DELETE` |

### Constructor de consultas

Se empieza con `User::query()`, que devuelve `Query<User, Unfiltered>`; añadir un filtro lo convierte en
`Query<User, Filtered>` (estado de tipo): «borrar sin WHERE» no compila.

```rust
use ecat_orm::query::{Op, Order};

let users = User::query()
    .filter("name", Op::Like, "alice%")?        // Eq / Ne / Lt / Le / Gt / Ge / Like
    .filter("email", Op::NotNull, serde_json::json!(null))?
    .filter("id", Op::In, serde_json::json!([1, 2, 3]))?  // In / NotIn toman un valor de array
    .order_by("id", Order::Desc)?
    .limit(10)
    .offset(20)
    .fetch(&db)
    .await?;
```

- Los nombres de columna se validan contra la lista blanca `EntityMeta.columns` (`OrmError::UnknownColumn`);
  las columnas de las tablas unidas no están ahí — para ellas usa `filter_raw("...")`, sabiendo que **no
  hace ninguna validación: la entrada debe ser de confianza**.
- `with_trashed()` desactiva el filtro de borrado lógico (vuelven también las filas borradas lógicamente).
- `join(JoinType::Left, "posts", "posts.user_id = users.id")` admite `Inner` / `Left`, y sirve para filtrar
  por columnas de la tabla unida dentro de `filter_raw`. La **lista de columnas no lleva prefijo de tabla**,
  así que si la tabla unida comparte un nombre de columna con el sujeto, la base real informa
  `ambiguous column name` — une tablas cuyos nombres de columna difieran del sujeto (verificado en SQLite).
- `delete_where(&db)` / `hard_delete_where(&db)` borran con las mismas condiciones (las entidades con
  borrado lógico pasan por `UPDATE`).
- `fetch(&db)` es el punto de entrada de ejecución; `find_by_id` / `find_all` / `paginate` reutilizan la
  misma ruta de generación de SQL que hay detrás.

### Troceado y paginación

- **Las escrituras masivas se trocean automáticamente**: `insert_many` / `update_many` se dividen según el
  límite de parámetros por sentencia del dialecto (p. ej. 2100 en SQL Server) y suman las filas. Para
  trocear a mano usa el mismo límite:
  `ecat_orm::dialect::lookup(dialect).max_params_per_stmt()`.
- **Paginación**: `paginate(&db, page, per_page)` emite 2 consultas (COUNT + obtención de página) y las
  páginas **empiezan en 1**; el COUNT reutiliza el mismo WHERE / JOIN pero quita ORDER BY / LIMIT / OFFSET.
  Devuelve `Page { items, total, page, per_page }`, y `total_pages()` / `has_next()` se calculan desde `total`.
- Para paginar en profundidad sobre tablas grandes usa `paginate_without_count(&db, page, per_page)`
  (1 consulta): `total` es `None`, y `total_pages()` / `has_next()` tampoco pueden responder — lo que no se
  puede calcular se informa como tal en lugar de adivinarse.

### Precarga de relaciones

```rust
let users = User::query()
    .with(&[UserRelation::Posts, UserRelation::Profile])
    .fetch(&db)
    .await?;
```

- Cada relación emite **una** consulta `IN` (troceada cuando las claves superan el límite de parámetros por
  sentencia), y la consulta de los sujetos no hace join — **sin N+1**: obtener 3 usuarios y sus posts son
  2 SELECT (1 del sujeto + 1 `IN`), no 4.
- Sin `with()` los campos de relación quedan vacíos (`Vec::new()` / `None`), nunca con valores obsoletos de
  una carga anterior; `set_relation` se llama para cada sujeto (incluso con resultado vacío), precisamente
  para limpiar restos.
- Las relaciones de valor único (`has_one` / `belongs_to`) toman solo la primera fila.

### Migraciones

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Migrator, create_table};

Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .add("002_posts", create_table::<Post>())
    .status().await?;      // MigrationStatus { applied, pending, unknown }
    .run().await?;         // aplica las pendientes y las registra una a una; reejecutar es idempotente
    .down(1).await?;       // ejecuta el SQL inverso de 001 y borra esa fila de la tabla de versiones
```

- El prefijo numérico del nombre de una migración es su versión (`"001_users"` → 1); un prefijo no numérico
  produce `OrmError::InvalidMigrationName` — nunca se adivina como 0.
- `create_table::<E>()` / `drop_table::<E>()` generan el DDL desde `EntityMeta`, y **el dialecto se resuelve
  en `run()` a partir del `dialect()` de la conexión**, así que la lista de migraciones queda desacoplada de
  la cadena de conexión. Para SQL propio (ALTER, relleno de datos) usa
  `ecat_orm::migrate::MigrationBuilder::new(|d| ...)`.
- Llamar a `down` en una migración sin SQL inverso produce `OrmError::MigrationIrreversible` — «DROP y
  volver a CREATE» pierde datos, y eso no se adivina por cuenta del llamante.
- La tabla de versiones la crea automáticamente `Migrator` (MSSQL no tiene `CREATE TABLE IF NOT EXISTS`,
  así que antes se comprueba su existencia).
