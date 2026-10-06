<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Ecat API-Referenz

Diese Seite fasst die Schnittstellen (API) des Ecat-Frameworks zusammen: Port-Konventionen, eingebaute Endpunkte, Fehlerformat und Erweiterungsschnittstellen. Geschäfts-Routen werden von den jeweiligen Diensten selbst registriert.

## Port-Konventionen

| Protokoll | Lauschadresse | Beschreibung |
|------|----------|------|
| HTTP | `0.0.0.0:8000` | axum-Routing, Standard-Port im Beispiel |
| gRPC | `0.0.0.0:9000` | tonic Server, Standard-Port im Beispiel |

## Eingebaute Endpunkte

Die folgenden Endpunkte werden von den Ökosystem-Crates bereitgestellt und mit dem Dienst gemountet:

| Endpunkt | Quelle | Beschreibung |
|------|------|------|
| `/health` | ecat-health | Liveness-Check (liefert Dienstname, Version, Startzeit) |
| `/ready` | ecat-health | Readiness-Check (liefert 200, sobald Abhängigkeiten bereit sind) |
| `/metrics` | ecat-metrics | Prometheus-Metrik-Export (`ecat_http_requests_total` / `ecat_http_request_duration_seconds`) |
| `/{service}/{method}` | Benutzer-Routen | Beispiel: `/helloworld/ecat` |

> Bei Hochkardinalität wie IDs im Metrik-Endpunkt-Pfad bitte `MetricsLayer::new().with_path_fn(...)` zur Normalisierung verwenden, um Metrik-Kardinalitätsexplosion zu vermeiden.

## Anfrageverarbeitungsablauf

```
Client-Anfrage
  ├─ HTTP :8000 ──→ axum::Router ─┐
  └─ gRPC :9000 ──→ tonic::Server ─┤
                              ┌─────┴──────┐
                              │ Middleware │  Recovery→Tracing→Logging→Auth→Metrics→Security→CircuitBreaker
                              └─────┬──────┘
                                    ▼
                               Handler (tower::Service)
                                    ▼
                               Response (JSON/Protobuf-Codierung)
```

## Fehlerformat

`ecat-errors` bietet `ErrorCode` + `Error` mit HTTP-Statuscode-Zuordnung zur Compilezeit:

```rust
use ecat_errors::{Error, ErrorCode};

Error::new(ErrorCode::InvalidArgument, "bad_request", "user id must be positive");
```

Fehler-Responses werden über die Middleware als JSON (oder Protobuf) codiert und tragen code / reason / message.

## Erweiterungsschnittstellen

| Fähigkeit | Crate | Schnittstelle |
|------|-------|------|
| GraphQL | ecat-graphql | `/graphql`-Endpunkt; unterstützt Feldargumente und verschachtelte Selections, keine Aliase, Fragmente oder mehrere Top-Level-Felder |
| OpenAPI | ecat-openapi | Generiert OpenAPI-spec aus den Routen |
| WebSocket | ecat-transport-ws | Aktualisierter WS-Transport |
| API-Versions-Routing | ecat-versioning | Versions-Routing mit `/v1/...`-Präfix |
| Authentifizierung | ecat-auth | JWT-/API-Key-Middleware; JWT-Schlüssel muss ≥32 Bytes sein, verkettbar `required_issuer`/`required_audience` |
| gRPC-Client | ecat-transport-grpc | Integriert Service Discovery und Load Balancing |

## Dienstkommunikation

- `HttpClient` (ecat-client): integriert Service Discovery und Load Balancing, Schutz durch CircuitBreaker
- `GrpcClient` (ecat-transport-grpc): wie oben, gRPC-Protokoll
- Middleware wird einheitlich über `tower::ServiceBuilder` kombiniert (Recovery / Tracing / Logging / Timeout / RateLimit / Security / CircuitBreaker / Metrics / Retry / Validate / CORS)

## Daten-Backend-Schnittstellen

Alle Daten-Backends (`ecat-data-*`) sind über einheitliche Traits abstrahiert (`RdbmsClient` für Transaktionen, `SqlExecutor` für Ausführung und Dialekt / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`); REST-artige Backends (Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine / OpenSearch / Elasticsearch / S3) greifen über `base_url` auf die jeweiligen HTTP-Schnittstellen zu. Verbindungskonfiguration siehe [Tutorial zur Datenbankkonfiguration](database-config-tutorial.md).

## ORM (ecat-orm)

`ecat-orm` bietet ein Entity-Derive-Makro, einen typsicheren Query-Builder, CRUD, Relation-Preloading
und Migrationen. Jede Datenoperation nimmt `&impl SqlExecutor`, deshalb funktioniert **dieselbe API mit
einem Client und mit einer `Transaction`**:

```rust
User::find_by_id(&db, 1).await?;              // db: SqlxClient / MssqlClient
let tx = db.transaction().await?;
User::update(&tx, &user).await?;              // tx: Transaction
User::insert(&tx, &user).await?;              // auch in einer Transaktion: beide Statements laufen dort
tx.commit().await?;
```

Das SQL erzeugt die Dialektschicht: SQLite / PostgreSQL / MySQL / TiDB (`ecat-data-sqlx`) und
SQL Server (`ecat-data-mssql`) teilen sich eine Entity-Definition.

### Entity-Definition und die Attributgrammatik von `#[entity(...)]`

`#[derive(Entity)]` erzeugt `Entity::META` (Tabelle / Spalten / Relationen / Flags), `from_row` /
`to_values` / `pk_value` sowie pro Entity ein `XxxRelation`-Enum (Variantennamen sind die PascalCase-
Form der Relationsfeldnamen).

Container-Attribut:

| Schreibweise | Bedeutung |
|------|------|
| `#[entity(table = "users")]` | Tabellenname; ohne Angabe der snake_case des Strukturnamens (`User` → `user`, `UserProfile` → `user_profile`) |

Spaltenfelder:

| Schreibweise | Bedeutung |
|------|------|
| `#[entity(column = "user_name")]` | Spaltenname überschreiben, Standard ist der Feldname |
| `#[entity(pk)]` | Primärschlüssel |
| `#[entity(auto_increment)]` | Auto-Inkrement (impliziert pk); der Auto-Inkrement-Schlüssel steht nicht in der Insert-Spaltenliste, sondern wird von der Datenbank erzeugt |
| `#[entity(created_at)]` / `#[entity(updated_at)]` | Automatische Zeitstempel: beim Insert werden beide gesetzt, beim Update nur `updated_at` |
| `#[entity(soft_delete)]` | Soft-Delete-Spalte: der Lesepfad ergänzt automatisch ein `IS NULL`-Gate darauf |
| `#[entity(version)]` | Optimistic-Lock-Spalte: Updates schreiben `version + 1` und vergleichen mit dem alten Wert; Konflikte liefern `OrmError::OptimisticLockConflict` |

Relationsfelder (**der Containertyp ist eine harte Vorgabe**; ein nackter Entity-Typ wird zur
Kompilierzeit abgelehnt — er kann „nicht gefunden" nicht ausdrücken):

| Schreibweise | Feldtyp | Bedeutung |
|------|----------|------|
| `#[entity(has_many = "Post", foreign_key = "user_id")]` | `Vec<Post>` | 1:n — der Wert von `local_key` dieser Tabelle wird gegen die Spalte `foreign_key` der Zieltabelle geprüft |
| `#[entity(has_one = "Profile", foreign_key = "user_id")]` | `Option<Profile>` | 1:1 — eine einwertige Relation nimmt nur die erste Zeile |
| `#[entity(belongs_to = "Tag", foreign_key = "tag_code")]` | `Option<Tag>` | n:1 — der Wert von `foreign_key` dieser Tabelle wird gegen den **Primärschlüssel der Zieltabelle** geprüft |

`local_key` darf entfallen: `has_many` / `has_one` nutzen standardmäßig den Primärschlüssel dieser
Tabelle, `belongs_to` den der Zieltabelle. Relationsfelder **sind keine Spalten**. Die Zuordnung vom
Feldtyp zum Spaltentyp lebt ausschließlich in den `value::ColumnValue`-Impls (das Makro führt keine
zweite Kopie dieser Tabelle); ein nicht unterstützter Typ erzeugt einen unerfüllten
`T: ColumnValue`-Fehler, der auf dieses Feld zeigt.

### CRUD

| Methode | Hinweis |
|------|------|
| `Entity::insert(&db, &e) -> i64` | Fügt ein und liefert den neuen Primärschlüssel. MySQLs zweistufiger Weg (`INSERT`, dann `SELECT LAST_INSERT_ID()`, das **verbindungsgebunden** ist) wird von `SqlExecutor::execute_then_query` in eine Transaktion gehüllt |
| `Entity::insert_many(&db, &[e]) -> u64` | Massen-Insert, liefert betroffene Zeilen (keine Schlüssel zurück: `LAST_INSERT_ID()` liefert die erste, SQLite die letzte Zeile — backendübergreifend unzuverlässig) |
| `Entity::save(&db, &e)` | Fügt ein, wenn der Primärschlüssel „ungesetzt" ist (Auto-Inkrement und Wert 0), sonst Update; liefert `()` |
| `Entity::update(&db, &e) -> u64` | Vollständiges Update über den Primärschlüssel. Null betroffene Zeilen ist ein Fehler: `OrmError::NotFound` ohne `version`, `OrmError::OptimisticLockConflict` mit einer (ohne Zusatzabfrage sind beide nicht unterscheidbar) |
| `Entity::update_many(&db, &[e]) -> u64` | Zeilenweises Update, liefert die summierte Zeilenzahl; ein Massenaufruf **kann nicht sagen, welche Zeile** kollidierte (ein Ergebnis kleiner als die Eingabe heißt: jemand wurde vom Version-Gate gestoppt) |
| `Entity::upsert(&db, &e) -> u64` | Upsert über den Primärschlüssel (je Dialekt `ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE`) |
| `Entity::find_by_id(&db, pk) -> Option<Self>` | Holt eine Zeile über den Primärschlüssel, liefert `Ok(None)` wenn nicht vorhanden |
| `Entity::find_all(&db) -> Vec<Self>` | Alle Zeilen ohne Filter (**bei großen Tabellen ein Full Table Scan**; zum Blättern `paginate` verwenden) |
| `Entity::delete_by_id(&db, pk) -> u64` | Mit deklariertem `soft_delete` wird ein `UPDATE` gesendet, das den Löschzeitpunkt setzt; die Zeile bleibt in der Tabelle, erneutes Löschen liefert `NotFound` und aktualisiert den Zeitpunkt nicht |
| `Entity::hard_delete_by_id(&db, pk) -> u64` | Umgeht Soft Delete und sendet wirklich `DELETE` |

### Query-Builder

`User::query()` liefert `Query<User, Unfiltered>`; ein Filter macht daraus
`Query<User, Filtered>` (Typzustand) — „löschen ohne WHERE" kompiliert nicht.

```rust
use ecat_orm::query::{Op, Order};

let users = User::query()
    .filter("name", Op::Like, "alice%")?        // Eq / Ne / Lt / Le / Gt / Ge / Like
    .filter("email", Op::NotNull, serde_json::json!(null))?
    .filter("id", Op::In, serde_json::json!([1, 2, 3]))?  // In / NotIn nehmen einen Array-Wert
    .order_by("id", Order::Desc)?
    .limit(10)
    .offset(20)
    .fetch(&db)
    .await?;
```

- Spaltennamen werden gegen die Whitelist `EntityMeta.columns` geprüft (`OrmError::UnknownColumn`);
  Spalten gejointer Tabellen stehen dort nicht — dafür `filter_raw("...")` verwenden, und zwar
  wissentlich, dass **keinerlei Validierung stattfindet: die Eingabe muss vertrauenswürdig sein**.
- `with_trashed()` schaltet das Soft-Delete-Gate ab (soft gelöschte Zeilen kommen mit).
- `join(JoinType::Left, "posts", "posts.user_id = users.id")` unterstützt `Inner` / `Left` und dient dem
  Filtern über Spalten der gejointen Tabelle in `filter_raw`. Die **Spaltenliste trägt kein Tabellen-
  präfix**, daher meldet die echte Datenbank `ambiguous column name`, wenn die gejointe Tabelle einen
  Spaltennamen mit dem Subjekt teilt — also Tabellen joinen, deren Spaltennamen sich unterscheiden
  (auf SQLite verifiziert).
- `delete_where(&db)` / `hard_delete_where(&db)` löschen nach denselben Bedingungen (Soft-Delete-Entities
  laufen über `UPDATE`).
- `fetch(&db)` ist der Ausführungseinstieg; `find_by_id` / `find_all` / `paginate` nutzen denselben
  SQL-Erzeugungspfad dahinter.

### Chunking und Pagination

- **Massen-Schreibvorgänge chunkieren automatisch**: `insert_many` / `update_many` teilen nach dem
  Parameterlimit pro Statement des Dialekts (z. B. 2100 bei SQL Server) und summieren die Zeilenzahlen.
  Manuelles Chunking nutzt dasselbe Limit:
  `ecat_orm::dialect::lookup(dialect).max_params_per_stmt()`.
- **Pagination**: `paginate(&db, page, per_page)` sendet 2 Abfragen (COUNT + Seitenabruf), Seiten sind
  **1-basiert**; COUNT nutzt dasselbe WHERE / JOIN, lässt aber ORDER BY / LIMIT / OFFSET weg. Zurück kommt
  `Page { items, total, page, per_page }`, und `total_pages()` / `has_next()` leiten sich aus `total` ab.
- Für tiefes Blättern über große Tabellen `paginate_without_count(&db, page, per_page)` (1 Abfrage):
  `total` ist `None`, und `total_pages()` / `has_next()` können ebenfalls nicht antworten — was sich nicht
  berechnen lässt, wird als solches gemeldet und nicht geraten.

### Relation-Preloading

```rust
let users = User::query()
    .with(&[UserRelation::Posts, UserRelation::Profile])
    .fetch(&db)
    .await?;
```

- Jede Relation sendet **eine** `IN`-Abfrage (gechunkt, wenn die Schlüssel das Parameterlimit pro Statement
  überschreiten), und die Subjektabfrage joint selbst nicht — **kein N+1**: 3 Nutzer und ihre Posts sind
  2 SELECTs (1 Subjekt + 1 `IN`), nicht 4.
- Ohne `with()` sind die Relationsfelder leer (`Vec::new()` / `None`), niemals veraltete Werte aus einem
  früheren Laden; `set_relation` wird für jedes Subjekt aufgerufen (auch bei leeren Ergebnissen), genau um
  Reste zu löschen.
- Einwertige Relationen (`has_one` / `belongs_to`) nehmen nur die erste Zeile.

### Migrationen

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Migrator, create_table};

Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .add("002_posts", create_table::<Post>())
    .status().await?;      // MigrationStatus { applied, pending, unknown }
    .run().await?;         // wendet pending an und protokolliert jede in der Versionstabelle; erneutes Ausführen ist idempotent
    .down(1).await?;       // führt das Reverse-SQL von 001 aus und entfernt die Zeile aus der Versionstabelle
```

- Das Zahlenpräfix eines Migrationsnamens ist seine Version (`"001_users"` → 1); ein nicht numerisches
  Präfix löst `OrmError::InvalidMigrationName` aus — es wird nie als 0 geraten.
- `create_table::<E>()` / `drop_table::<E>()` erzeugen DDL aus `EntityMeta`, und **der Dialekt wird erst
  bei `run()` aus dem `dialect()` der Verbindung bestimmt**, wodurch die Migrationsliste vom
  Verbindungsstring entkoppelt ist. Eigenes SQL (ALTER, Datenmigration) nutzt
  `ecat_orm::migrate::MigrationBuilder::new(|d| ...)`.
- `down` auf einer Migration ohne Reverse-SQL löst `OrmError::MigrationIrreversible` aus — „DROP und dann
  wieder CREATE" verliert Daten, das wird nicht für den Aufrufer geraten.
- Die Versionstabelle legt `Migrator` automatisch an (MSSQL kennt kein `CREATE TABLE IF NOT EXISTS`,
  deshalb wird zuerst die Existenz geprüft).
