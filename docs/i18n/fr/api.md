<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Référence API d'Ecat

Cette page récapitule la surface d'interface (API) du framework Ecat : conventions de ports, endpoints intégrés, format d'erreur et interfaces d'extension. Les routes métier sont enregistrées par chaque service.

## Conventions de ports

| Protocole | Adresse d'écoute | Description |
|------|----------|------|
| HTTP | `0.0.0.0:8000` | Routes axum, port d'exemple par défaut |
| gRPC | `0.0.0.0:9000` | Serveur tonic, port d'exemple par défaut |

## Endpoints intégrés

Les endpoints suivants sont fournis par les crates d'écosystème et montés avec le service :

| Endpoint | Source | Description |
|------|------|------|
| `/health` | ecat-health | Contrôle de survie (renvoie le nom du service, la version, l'heure de démarrage) |
| `/ready` | ecat-health | Contrôle de disponibilité (renvoie 200 quand les dépendances sont prêtes) |
| `/metrics` | ecat-metrics | Exposition des métriques Prometheus (`ecat_http_requests_total` / `ecat_http_request_duration_seconds`) |
| `/{service}/{method}` | Routes utilisateur | Exemple : `/helloworld/ecat` |

> Pour les chemins de métriques à haute cardinalité (contenant des ID, etc.), utilisez `MetricsLayer::new().with_path_fn(...)` pour normaliser et éviter l'explosion de cardinalité.

## Flux de traitement des requêtes

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

## Format d'erreur

`ecat-errors` fournit `ErrorCode` + `Error`, avec mappage du statut HTTP à la compilation :

```rust
use ecat_errors::{Error, ErrorCode};

Error::new(ErrorCode::InvalidArgument, "bad_request", "user id must be positive");
```

La réponse d'erreur est encodée par le middleware en JSON (ou Protobuf), avec code / reason / message.

## Interfaces d'extension

| Capacité | Crate | Interface |
|------|-------|------|
| GraphQL | ecat-graphql | Endpoint `/graphql` ; prend en charge les paramètres de champ et les sélections imbriquées, pas les alias, fragments ni champs multiples de niveau supérieur |
| OpenAPI | ecat-openapi | Génère une spec OpenAPI depuis les routes |
| WebSocket | ecat-transport-ws | Transport WS mis à niveau |
| Routage par version d'API | ecat-versioning | Routage par version avec préfixe `/v1/...` |
| Authentification | ecat-auth | Middleware JWT / API Key ; la clé JWT doit faire ≥32 octets, chaînage possible `required_issuer`/`required_audience` |
| Client gRPC | ecat-transport-grpc | Intégration de la découverte de services et de l'équilibrage de charge |

## Communication inter-services

- `HttpClient` (ecat-client) : intégration de la découverte de services et de l'équilibrage de charge, protection par CircuitBreaker
- `GrpcClient` (ecat-transport-grpc) : idem, protocole gRPC
- Les middleware sont composés uniformément avec `tower::ServiceBuilder` (Recovery / Tracing / Logging / Timeout / RateLimit / Security / CircuitBreaker / Metrics / Retry / Validate / CORS)

## Interfaces des backends de données

Tous les backends de données (`ecat-data-*`) sont abstraits par des traits unifiés (`RdbmsClient` pour les transactions, `SqlExecutor` pour l'exécution et le dialecte / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`) ; les backends de type REST (Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine / OpenSearch / Elasticsearch / S3) accèdent à l'interface HTTP correspondante via `base_url`. Voir [Tutoriel de configuration des bases de données](database-config-tutorial.md) pour la configuration de connexion.

## ORM (ecat-orm)

`ecat-orm` fournit une macro dérivée d'entité, un constructeur de requêtes typé, le CRUD, le
préchargement des relations et les migrations. Chaque opération de données prend `&impl SqlExecutor` :
**la même API fonctionne avec un client et avec une `Transaction`**.

```rust
User::find_by_id(&db, 1).await?;              // db: SqlxClient / MssqlClient
let tx = db.transaction().await?;
User::update(&tx, &user).await?;              // tx: Transaction
User::insert(&tx, &user).await?;              // fonctionne dans une transaction : les deux requêtes y tournent
tx.commit().await?;
```

Le SQL est généré par la couche dialecte : SQLite / PostgreSQL / MySQL / TiDB (`ecat-data-sqlx`) et
SQL Server (`ecat-data-mssql`) partagent une même définition d'entité.

### Définition d'entité et grammaire de l'attribut `#[entity(...)]`

`#[derive(Entity)]` génère `Entity::META` (table / colonnes / relations / drapeaux), `from_row` /
`to_values` / `pk_value`, ainsi qu'une énumération `XxxRelation` par entité (les variantes sont le
PascalCase du nom du champ de relation).

Attribut de conteneur :

| Syntaxe | Signification |
|------|------|
| `#[entity(table = "users")]` | Nom de table ; omis, c'est le snake_case du nom de structure (`User` → `user`, `UserProfile` → `user_profile`) |

Champs de colonne :

| Syntaxe | Signification |
|------|------|
| `#[entity(column = "user_name")]` | Nom de colonne personnalisé, par défaut le nom du champ |
| `#[entity(pk)]` | Clé primaire |
| `#[entity(auto_increment)]` | Auto-incrément (implique pk) ; la clé auto-incrémentée est exclue de la liste des colonnes d'insertion et générée par la base |
| `#[entity(created_at)]` / `#[entity(updated_at)]` | Horodatages automatiques : les deux sont remplis à l'insertion, seul `updated_at` est rafraîchi à la mise à jour |
| `#[entity(soft_delete)]` | Colonne de suppression logique : le chemin de lecture ajoute automatiquement un filtre `IS NULL` dessus |
| `#[entity(version)]` | Colonne de verrouillage optimiste : les mises à jour écrivent `version + 1` et comparent à l'ancienne valeur ; un conflit renvoie `OrmError::OptimisticLockConflict` |

Champs de relation (**le type conteneur est une contrainte dure** ; un type d'entité nu est refusé à la
compilation — il ne peut pas exprimer « non trouvé ») :

| Syntaxe | Type du champ | Signification |
|------|----------|------|
| `#[entity(has_many = "Post", foreign_key = "user_id")]` | `Vec<Post>` | Un-à-plusieurs : la valeur de `local_key` de cette table est comparée à la colonne `foreign_key` de la cible |
| `#[entity(has_one = "Profile", foreign_key = "user_id")]` | `Option<Profile>` | Un-à-un ; une relation à valeur unique ne prend que la première ligne |
| `#[entity(belongs_to = "Tag", foreign_key = "tag_code")]` | `Option<Tag>` | Plusieurs-à-un : la valeur de `foreign_key` de cette table est comparée à la **clé primaire de la cible** |

`local_key` peut être omis : `has_many` / `has_one` prennent par défaut la clé primaire de cette table,
`belongs_to` celle de la cible. Les champs de relation **ne sont pas des colonnes**. La correspondance
entre type de champ et type de colonne n'existe que dans les implémentations de `value::ColumnValue`
(la macro n'en garde pas de seconde copie) ; un type non pris en charge produit une erreur
`T: ColumnValue` non satisfaite pointant sur ce champ.

### CRUD

| Méthode | Remarque |
|------|------|
| `Entity::insert(&db, &e) -> i64` | Insère et renvoie la nouvelle clé primaire. Le chemin en deux étapes de MySQL (`INSERT` puis `SELECT LAST_INSERT_ID()`, qui est **lié à la connexion**) est encapsulé dans une seule transaction par `SqlExecutor::execute_then_query` |
| `Entity::insert_many(&db, &[e]) -> u64` | Insertion en masse, renvoie le nombre de lignes affectées (pas de clés en retour : `LAST_INSERT_ID()` donne la première ligne, SQLite la dernière — peu fiable d'un backend à l'autre) |
| `Entity::save(&db, &e)` | Insère si la clé primaire est « non définie » (auto-incrémentée et égale à 0), sinon met à jour ; renvoie `()` |
| `Entity::update(&db, &e) -> u64` | Mise à jour complète par clé primaire. Zéro ligne affectée est une erreur : `OrmError::NotFound` sans `version`, `OrmError::OptimisticLockConflict` avec (sans requête supplémentaire, les deux sont indiscernables) |
| `Entity::update_many(&db, &[e]) -> u64` | Mise à jour ligne par ligne, renvoie la somme des lignes ; un appel en masse **ne dit pas quelle ligne** a conflicté (un résultat inférieur à l'entrée signifie qu'une ligne a été arrêtée par la barrière de version) |
| `Entity::upsert(&db, &e) -> u64` | Upsert par clé primaire (`ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE` selon le dialecte) |
| `Entity::find_by_id(&db, pk) -> Option<Self>` | Récupère une ligne par clé primaire, renvoie `Ok(None)` si absente |
| `Entity::find_all(&db) -> Vec<Self>` | Toutes les lignes sans filtre (**un balayage complet sur les grosses tables** ; pour paginer, utilisez `paginate`) |
| `Entity::delete_by_id(&db, pk) -> u64` | Avec `soft_delete` déclaré, envoie un `UPDATE` qui pose l'horodatage de suppression ; la ligne reste dans la table, et supprimer à nouveau renvoie `NotFound` sans rafraîchir l'horodatage |
| `Entity::hard_delete_by_id(&db, pk) -> u64` | Contourne la suppression logique et envoie réellement `DELETE` |

### Constructeur de requêtes

On part de `User::query()`, qui renvoie `Query<User, Unfiltered>` ; ajouter un filtre donne
`Query<User, Filtered>` (état de type) — « supprimer sans WHERE » ne compile pas.

```rust
use ecat_orm::query::{Op, Order};

let users = User::query()
    .filter("name", Op::Like, "alice%")?        // Eq / Ne / Lt / Le / Gt / Ge / Like
    .filter("email", Op::NotNull, serde_json::json!(null))?
    .filter("id", Op::In, serde_json::json!([1, 2, 3]))?  // In / NotIn prennent un tableau
    .order_by("id", Order::Desc)?
    .limit(10)
    .offset(20)
    .fetch(&db)
    .await?;
```

- Les noms de colonnes sont validés contre la liste blanche `EntityMeta.columns` (`OrmError::UnknownColumn`) ;
  les colonnes des tables jointes n'y figurent pas — utilisez `filter_raw("...")` pour elles, en sachant
  qu'il n'effectue **aucune validation : l'entrée doit être fiable**.
- `with_trashed()` désactive le filtre de suppression logique (les lignes supprimées logiquement reviennent).
- `join(JoinType::Left, "posts", "posts.user_id = users.id")` prend en charge `Inner` / `Left`, et sert à
  filtrer sur les colonnes d'une table jointe dans `filter_raw`. La **liste de colonnes ne porte pas de
  préfixe de table**, donc si la table jointe partage un nom de colonne avec le sujet, la base réelle
  signale `ambiguous column name` — joignez des tables dont les noms de colonnes diffèrent du sujet
  (vérifié sur SQLite).
- `delete_where(&db)` / `hard_delete_where(&db)` suppriment selon les mêmes conditions (les entités en
  suppression logique passent par `UPDATE`).
- `fetch(&db)` est le point d'entrée d'exécution ; `find_by_id` / `find_all` / `paginate` réutilisent tous
  le même chemin de génération SQL derrière.

### Découpage en lots et pagination

- **Les écritures en masse se découpent automatiquement** : `insert_many` / `update_many` se découpent
  selon la limite de paramètres par requête du dialecte (par ex. 2100 sur SQL Server) et additionnent les
  lignes. Pour découper à la main, utilisez la même limite :
  `ecat_orm::dialect::lookup(dialect).max_params_per_stmt()`.
- **Pagination** : `paginate(&db, page, per_page)` émet 2 requêtes (COUNT + récupération de la page) et les
  pages **commencent à 1** ; le COUNT réutilise le même WHERE / JOIN mais retire ORDER BY / LIMIT / OFFSET.
  Il renvoie `Page { items, total, page, per_page }`, et `total_pages()` / `has_next()` se calculent à
  partir de `total`.
- Pour paginer en profondeur sur de grosses tables, utilisez `paginate_without_count(&db, page, per_page)`
  (1 requête) : `total` vaut `None`, et `total_pages()` / `has_next()` ne peuvent pas répondre non plus —
  ce qui ne peut pas être calculé est signalé comme tel au lieu d'être deviné.

### Préchargement des relations

```rust
let users = User::query()
    .with(&[UserRelation::Posts, UserRelation::Profile])
    .fetch(&db)
    .await?;
```

- Chaque relation émet **une** requête `IN` (découpée quand les clés dépassent la limite de paramètres par
  requête), et la requête des sujets ne fait pas de jointure — **pas de N+1** : récupérer 3 utilisateurs et
  leurs posts, c'est 2 SELECT (1 sujet + 1 `IN`), pas 4.
- Sans `with()`, les champs de relation sont vides (`Vec::new()` / `None`), jamais des valeurs périmées
  d'un chargement précédent ; `set_relation` est appelé pour chaque sujet (même sur un résultat vide),
  précisément pour effacer les restes.
- Les relations à valeur unique (`has_one` / `belongs_to`) ne prennent que la première ligne.

### Migrations

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Migrator, create_table};

Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .add("002_posts", create_table::<Post>())
    .status().await?;      // MigrationStatus { applied, pending, unknown }
    .run().await?;         // applique les migrations pending et les enregistre une à une ; réexécuter est idempotent
    .down(1).await?;       // exécute le SQL inverse de 001 et retire cette ligne de la table de versions
```

- Le préfixe numérique d'un nom de migration est sa version (`"001_users"` → 1) ; un préfixe non numérique
  lève `OrmError::InvalidMigrationName` — il n'est jamais deviné comme 0.
- `create_table::<E>()` / `drop_table::<E>()` génèrent le DDL depuis `EntityMeta`, et **le dialecte est
  résolu au moment de `run()` depuis le `dialect()` de la connexion**, ce qui découple la liste des
  migrations de la chaîne de connexion. Pour du SQL maison (ALTER, reprise de données), utilisez
  `ecat_orm::migrate::MigrationBuilder::new(|d| ...)`.
- Appeler `down` sur une migration sans SQL inverse lève `OrmError::MigrationIrreversible` — « DROP puis
  re-CREATE » perd des données, cela n'est pas deviné à la place de l'appelant.
- La table de versions est créée automatiquement par `Migrator` (MSSQL n'a pas de
  `CREATE TABLE IF NOT EXISTS`, l'existence est donc vérifiée d'abord).
