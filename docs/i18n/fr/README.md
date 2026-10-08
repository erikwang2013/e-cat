<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Ecat

[简体中文](../../../README.md) | [English](../../../README.en.md) | [日本語](../ja/README.md) | [한국어](../ko/README.md) | [Русский](../ru/README.md) | [Deutsch](../de/README.md) | **[Français](../fr/README.md)** | [Español](../es/README.md) | [Português](../pt/README.md) | [हिन्दी](../hi/README.md) | [العربية](../ar/README.md) | [বাংলা](../bn/README.md) | [Bahasa Indonesia](../id/README.md)

Le nom chinois d'Ecat : une chatte (一只猫)

**Une chatte** est un framework de microservices Rust inspiré de [go-kratos/kratos](https://github.com/go-kratos/kratos) v3 (v7.0.0 · 56 crates).

Il offre une expérience de développement API-first, une architecture de composants enfichables, une abstraction unifiée des middleware HTTP/gRPC, ainsi qu'une chaîne d'outils CLI complète. Les développeurs familiers avec Kratos peuvent démarrer sans difficulté, tout en tirant pleinement parti de la sécurité de typage, des abstractions à coût zéro et des performances extrêmes de Rust.

<p align="center">
  <img src="e-cat.svg" alt="Mascotte du projet Ecat (animée)" width="220" />
</p>

## Architecture de conception

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

### Flux de traitement des requêtes

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

## Fonctionnalités

- **API-first** : définition des API, codes d'erreur et métadonnées via Protobuf ; génération de code avec prost + tonic-build
- **Double protocole** : HTTP (axum) et gRPC (tonic) partagent le même ensemble de middleware `tower::Layer`
- **Architecture enfichable** : Registry, Config, Logging et Encoding sont tous abstraits par des traits, avec des implémentations prêtes pour la production par défaut
- **Système de middleware** : Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, MetricsLayer, RetryLayer, ValidateLayer, CORS (feature « cors ») intégrés ; composition via `tower::ServiceBuilder`
- **Cycle de vie de l'application** : construction de l'App en mode Builder, démarrage concurrent de plusieurs serveurs, gestion des signaux SIGTERM/SIGINT, hooks de cycle de vie start/stop
- **Sécurité de typage** : système de codes d'erreur basé sur protobuf, mappage du statut HTTP à la compilation
- **Observabilité** : tracing + Prometheus + endpoints de santé (/health, /ready)
- **Détection d'attaques** : SecurityLayer détecte automatiquement les injections SQL, XSS, SSRF et autres schémas d'attaque, et bloque les requêtes à haut risque
- **Communication inter-services** : HttpClient intégré avec découverte de services et équilibrage de charge, protection par CircuitBreaker
- **Authentification et autorisation** : middleware d'authentification JWT / API Key, transmission des Claims dans le contexte de requête
- **Messages et événements** : trait MessageQueue + EventBus Pub/Sub local/à distance
- **Traçage distribué** : spans de requête, injection/extraction de trace_id
- **Client gRPC** : GrpcClient intégré avec découverte de services et équilibrage de charge
- **Multi-protocole** : routage unifié HTTP, gRPC, WebSocket et GraphQL
- **Multi-sources de données** : RDBMS (SQLite/PG/MySQL/TiDB/SQL Server), cache (Redis/Memcached), recherche (OpenSearch/Elasticsearch), graphes (Neo4j/NebulaGraph/ArangoDB), séries temporelles (InfluxDB/IoTDB/QuestDB/TDengine), documents (MongoDB), stockage d'objets (S3/MinIO), OLAP (ClickHouse)

### Correspondance des concepts Kratos

| Kratos (Go) | e-cat (Rust) | Description |
|-------------|-------------|------|
| `kratos.New()` | `App::builder()` | Pattern Builder |
| `http.Handler` | `tower::Service` | Trait standard de l'écosystème Rust |
| `http.Server` | `axum::Router` | Framework HTTP dominant de la communauté |
| `grpc.Server` | `tonic::transport::Server` | Implémentation gRPC la plus mature |
| `proto generate` | `prost + tonic-build` | Protobuf standard de la communauté |
| `registry.Discovery` | `Registry` trait | Découverte et enregistrement enfichables |
| `config.Source` | `ConfigSource` trait | Chargement de configuration multi-sources |

## Pile technologique

| Composant | Choix |
|------|------|
| Runtime asynchrone | **tokio** |
| HTTP | **axum** |
| gRPC | **tonic** |
| Protobuf | **prost + tonic-build** |
| Middleware | **tower::Service / Layer** |
| Journalisation/traçage | **tracing + trace_id propagation** |
| Métriques | **prometheus** |
| Sérialisation | **serde + prost** |
| Détection d'attaques | **security-rust** |
| RDBMS | **sqlx** |
| Redis | **redis-rs** |
| JWT | **jsonwebtoken** |
| Client HTTP | **reqwest** |
| CLI | **clap** |

## Bases de données prises en charge

| Catégorie | Base de données | Crate | Statut | Timeout/disjoncteur |
|------|--------|-------|------|------|
| RDBMS | SQLite | `ecat-data-sqlx` | ✅ Implémenté | ✅ Timeout + disjoncteur |
| RDBMS | PostgreSQL | `ecat-data-sqlx` | ✅ Implémenté | ✅ Timeout + disjoncteur |
| RDBMS | MySQL | `ecat-data-sqlx` | ✅ Implémenté | ✅ Timeout + disjoncteur |
| RDBMS | TiDB | `ecat-data-sqlx` | ✅ Implémenté | ✅ Timeout + disjoncteur |
| RDBMS | SQL Server | `ecat-data-mssql` | ✅ tiberius-ng | ✅ Timeout + disjoncteur |
| Cache | Redis | `ecat-data-redis` | ✅ Implémenté | ✅ Timeout + disjoncteur |
| Recherche | OpenSearch | `ecat-data-opensearch` | ✅ Implémenté | ✅ Timeout + disjoncteur |
| Recherche | Elasticsearch | `ecat-data-elasticsearch` | ✅ Implémenté | ✅ Timeout + disjoncteur |
| Cache | Memcached | `ecat-data-memcached` | ⚠️ Implémentation en mémoire (non destinée à la production, ne pas utiliser pour un cache persistant) | — |
| OLAP | ClickHouse | `ecat-data-clickhouse` | ✅ Implémenté | ✅ Timeout + disjoncteur |
| Graphe | Neo4j | `ecat-data-neo4j` | ✅ API REST | ✅ Timeout + disjoncteur |
| Graphe | NebulaGraph | `ecat-data-nebulagraph` | ✅ API REST | ✅ Timeout + disjoncteur |
| Graphe | ArangoDB | `ecat-data-arangodb` | ✅ API REST | ✅ Timeout + disjoncteur |
| Séries temporelles | InfluxDB | `ecat-data-influxdb` | ✅ API HTTP | ✅ Timeout + disjoncteur |
| Séries temporelles | Apache IoTDB | `ecat-data-iotdb` | ✅ API REST | ✅ Timeout + disjoncteur |
| Séries temporelles | QuestDB | `ecat-data-questdb` | ✅ API HTTP | ✅ Timeout + disjoncteur |
| Séries temporelles | TDengine | `ecat-data-tdengine` | ✅ API REST | ✅ Timeout + disjoncteur |
| Documents | MongoDB | `ecat-data-mongodb` | ✅ Pilote natif | ✅ Timeout + disjoncteur |
| Stockage d'objets | S3 / MinIO | `ecat-data-s3` | ✅ reqwest+rustls | ✅ Timeout + disjoncteur |

> **Timeout/disjoncteur** : Timeout = le délai d'expiration des requêtes `query_timeout_secs` (30 s par défaut, `0` = désactivé ; configurable sur **tous** les backends de données sauf l'implémentation en mémoire de memcached ; la contre-pression de concurrence de MongoDB passe par le pool de connexions du pilote `max_pool_size`) ; disjoncteur = **chaque client possède le sien** `ecat_circuit_breaker::Breaker` (taux d'échec 0.5 / fenêtre 30 s / sondes semi-ouvertes 3 / ouvert 10 s, sans interrupteur général `enabled`) ; les backends HTTP ont en plus `max_concurrency` (32 par défaut). `ecat_data::CircuitBreakerExecutor` peut toujours envelopper n'importe quel backend `SqlExecutor`. La séparation lecture/écriture s'appuie sur `ecat_data::RdbmsRouting` : les écritures vont au primaire, les lectures sont réparties en tourniquet sur les réplicas en **ignorant celles dont le disjoncteur est ouvert** ; si aucune réplica n'est disponible, repli sur le primaire par défaut, ou `RdbmsError::NoAvailableReplica` avec `fallback_to_primary(false)`.

> Tous les backends de données sont abstraits via des traits unifiés (`RdbmsClient` pour les transactions, `SqlExecutor` pour l'exécution et le dialecte / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`) ; il suffit d'importer le crate contrib correspondant. Chaque backend fournit une structure `XxxConfig` (`#[derive(Deserialize)]`) qui permet de charger les informations de connexion depuis un fichier de configuration JSON/YAML.

> **Convention de nommage des constructeurs** : pour les crates de file de messages (`ecat-mq-*`), le constructeur principal est uniformément `connect` (par ex. `KafkaMq::connect(brokers)`, `MqttMq::connect(url)`), avec `from_config` pour charger depuis la configuration ; pour les crates de backend de données (`ecat-data-*`), le constructeur principal est le plus souvent `new`, exceptions : `ecat-data-redis` / `ecat-data-sqlx` / `ecat-data-mssql` conservent `connect`, `ecat-data-mongodb` / `ecat-data-s3` ne fournissent que `from_config`. Il s'agit d'une convention existante, non imposée (pour éviter les changements incompatibles) ; une unification pourra être évaluée dans la fenêtre 3.0.

### Exemple de configuration de base de données

Chaque backend de données fournit une structure `XxxConfig` et une méthode `from_config()` qui découple les informations de connexion du code vers le fichier de configuration :

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

**Référence des champs de configuration** :

| Backend | Config | Champs | Exemples de valeurs |
|------|--------|------|--------|
| Redis | `RedisConfig` | `url`, `password`? | `redis://localhost:6379` |
| RDBMS | `SqlxConfig` | `url`, `username`?, `password`?, paramètres de pool (`max_connections` / `query_timeout_secs` / `session_init` etc., voir [Tutoriel de configuration des bases de données](database-config-tutorial.md)) | `postgres://localhost/db` |
| ClickHouse | `ClickhouseConfig` | `base_url`, `database`, `username`?, `password`? | `http://localhost:8123`, `default` |
| QuestDB | `QuestdbConfig` | `base_url`, `username`?, `password`? | `http://localhost:9000` |
| Elasticsearch | `ElasticsearchConfig` | `base_url`, `username`?, `password`? | `http://localhost:9200` |
| OpenSearch | `OpenSearchConfig` | `base_url`, `username`?, `password`? | `http://localhost:9200` |
| InfluxDB | `InfluxConfig` | `base_url`, `org`, `bucket`, `token` | — |
| Neo4j | `Neo4jConfig` | `base_url`, `username`, `password` | — |
| NebulaGraph | `NebulaGraphConfig` | `base_url`, `space`, `username`?, `password`? | — |
| ArangoDB | `ArangoConfig` | `base_url`, `db`, `username`, `password` | — |
| IoTDB | `IotdbConfig` | `base_url`, `username`, `password` | — |
| Memcached | `MemcachedConfig` | `username`?, `password`? (champs réservés) | — |
| TDengine | `TdengineConfig` | `base_url`, `username`, `password`, `database`? | `http://localhost:6041` |
| MongoDB | `MongoConfig` | `url`, `database`, `tls`? | `mongodb://localhost:27017`, `app` |
| S3 | `S3Config` | `endpoint`, `region`, `access_key`, `secret_key`, `tls`? | `http://localhost:9000`, `us-east-1` |

> Sauf deux exceptions, tous les Config de backend prennent en charge le champ optionnel `tls` (`TlsClientConfig`) pour configurer l'authentification par certificat client TLS : `ecat-data-sqlx` **ne le prend pas en charge** — le renseigner fait échouer le démarrage (son TLS passe par les paramètres d'URL) ; `ecat-data-memcached` accepte le champ mais l'**ignore silencieusement** (déclaré, jamais lu). Voir [Tutoriel de configuration des bases de données](database-config-tutorial.md).

## ORM (ecat-orm)

`ecat-orm` est l'ORM maison (macro dérivée d'entité + constructeur de requêtes typé + migrations) bâti
sur le `SqlExecutor` d'`ecat-data` : **la même API fonctionne avec un client et avec une `Transaction`**.
`User::insert(&db, &user)` et `User::insert(&tx, &user)` sont le même appel — passer `&tx` exécute les
requêtes dans la transaction de l'appelant, sans en ouvrir ni en valider une. Une même définition d'entité
vaut pour SQLite / PostgreSQL / MySQL / TiDB (`ecat-data-sqlx`) et SQL Server (`ecat-data-mssql`), le SQL
étant généré par la couche dialecte.

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

// Migrations : création des tables + SQL inverse (le dialecte est résolu à run() depuis la connexion)
Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .run()
    .await?;

// CRUD : fonctionne avec &db et avec &tx
let id = User::insert(&db, &user).await?;
let mut got = User::find_by_id(&db, id).await?.unwrap();
got.name = "alice-v2".into();
User::update(&db, &got).await?;       // verrou optimiste : WHERE version = ancienne valeur, conflit -> OptimisticLockConflict
User::delete_by_id(&db, id).await?;   // suppression logique : UPDATE pose deleted_at, la ligne reste en table

// Requête + pagination + préchargement : une requête IN par relation, pas de N+1
let page = User::query()
    .filter("name", Op::Like, "ali%")?
    .order_by("id", Order::Desc)?
    .with(&[UserRelation::Posts])
    .paginate(&db, 1, 20)
    .await?;
```

| Capacité | Point d'entrée |
|------|------|
| Entité | `#[derive(Entity)]` + `#[entity(table / pk / column / auto_increment / created_at / updated_at / soft_delete / version / has_many / has_one / belongs_to)]` |
| CRUD | `insert` / `insert_many` / `save` / `update` / `update_many` / `upsert` / `find_by_id` / `find_all` / `delete_by_id` / `hard_delete_by_id` |
| Constructeur de requêtes | `query().filter(..).order_by(..).limit(..).offset(..).join(..).fetch(..)` ; le filtre de suppression logique s'enlève avec `with_trashed()` |
| Découpage en lots | `insert_many` / `update_many` se découpent automatiquement selon la limite de paramètres par requête du dialecte |
| Pagination | `paginate` (COUNT + récupération de page) / `paginate_without_count` (sans COUNT) ; les pages commencent à 1 |
| Préchargement | `with(&[UserRelation::Posts])` — 3 sujets et leurs relations coûtent 2 SELECT, pas 4 |
| Migrations | `Migrator::{status, run, down}` + `create_table::<E>()` / `drop_table_sql()` ; la table de versions est maintenue automatiquement |

La grammaire complète des attributs, la sémantique d'erreur de chaque méthode et les différences entre
dialectes se trouvent dans la section ORM de la [référence API](api.md).

## Structure du projet

```
e-cat/
├── ecat/                       # Cœur : cycle de vie de l'App
├── ecat-transport/             # Abstraction de transport (Server trait)
├── ecat-transport-http/        # implémentation axum
├── ecat-transport-grpc/        # implémentation tonic
├── ecat-middleware/            # middleware tower::Layer
├── ecat-protos/                # Définitions Protobuf
├── ecat-errors/                # Système de codes d'erreur
├── ecat-metadata/              # Propagation des métadonnées
├── ecat-encoding/              # Abstraction de sérialisation
├── ecat-logging/               # Intégration tracing
├── ecat-registry/              # Enregistrement et découverte de services
├── ecat-config/                # Gestion de la configuration
├── ecat-metrics/               # Intégration Prometheus
├── ecat-data/                  # Traits d'accès aux données
├── ecat-security/              # Détection d'attaques (security-rust)
├── ecat-cli/                   # Outillage CLI
├── ecat-health/                # Contrôles de santé (/health /ready)
├── ecat-auth/                  # Middleware d'authentification (JWT / API Key)
├── ecat-client/                # Client HTTP inter-services
├── ecat-circuit-breaker/       # Coupe-circuit (Tower Layer)
├── ecat-registry-consul/       # Enregistrement de services Consul
├── ecat-config-remote/         # Configuration distante Consul KV
├── ecat-data-redis/            # Implémentation de cache Redis
├── ecat-mq/                    # Abstraction de file de messages
├── ecat-events/                # Bus d'événements (local + distant)
├── ecat-openapi/               # Génération de spec OpenAPI
├── ecat-bench/                 # Benchmarks de performance
├── ecat-tracing/               # Traçage distribué (injection/extraction de trace_id)
├── ecat-registry-etcd/         # Enregistrement de services etcd
├── ecat-mq-kafka/              # Adaptateur de file de messages Kafka
├── ecat-data-opensearch/       # Backend de recherche OpenSearch
├── ecat-data-influxdb/         # Backend de séries temporelles InfluxDB
├── ecat-graphql/               # GraphQL endpoint
├── ecat-data-elasticsearch/    # Backend de recherche Elasticsearch
├── ecat-data-clickhouse/       # Backend OLAP ClickHouse
├── ecat-data-sqlx/             # Backend RDBMS (SQLite/PG/MySQL/TiDB)
├── ecat-data-mssql/            # Backend SQL Server (tiberius-ng)
├── ecat-orm/                   # ORM : macro d'entité, constructeur de requêtes, migrations
├── ecat-orm-derive/            # Macro dérivée #[derive(Entity)]
├── ecat-data-memcached/        # Backend de cache Memcached (implémentation en mémoire)
├── ecat-data-neo4j/            # Backend de graphes Neo4j
├── ecat-data-nebulagraph/      # Backend de graphes NebulaGraph
├── ecat-data-arangodb/         # Backend de graphes ArangoDB
├── ecat-data-iotdb/            # Backend de séries temporelles IoTDB
├── ecat-data-questdb/          # Backend de séries temporelles QuestDB
├── ecat-transport-ws/          # WebSocket transport
├── ecat-versioning/            # Routage des versions d'API
├── ecat-tls/                   # Configuration et génération automatique de certificats TLS
├── ecat-deploy/                # Docker / K8s / Helm / CI/CD
├── ecat-lock/                  # Abstraction de verrou distribué (implémentation Redis)
├── ecat-scheduler/             # Planificateur de tâches tokio
├── ecat-data-tdengine/         # Backend de séries temporelles TDengine
├── ecat-data-mongodb/          # Backend de documents MongoDB
├── ecat-data-s3/               # Backend de stockage objet S3 / MinIO
├── ecat-mq-rabbitmq/           # Backend de messagerie RabbitMQ
├── ecat-mq-mqtt/               # Backend de messagerie MQTT
├── ecat-mq-nats/               # Backend de messagerie NATS
├── config/                     # Fichiers de configuration d'exemple
├── docs/                       # Documents de conception et feuille de route de l'écosystème
└── examples/                   # Projets d'exemple
```

## Démarrage rapide

### Prérequis

- Rust 1.85+ (chaîne stable, exigence édition 2024)
- [protoc](https://github.com/protocolbuffers/protobuf) (compilateur Protocol Buffers)

### Installation du CLI

```bash
cargo install ecat-cli
```

### Création d'un service

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

Accédez à `http://localhost:8000/helloworld/ecat`.

### Exemple de code

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

### Crate agrégé (ecat)

`ecat` fournit un point d'entrée de re-export conditionné par features — n'activez que les composants nécessaires :

```rust
use ecat::transport_http::HttpServer;   // feature "http"（默认）
use ecat::middleware::RecoveryLayer;     // feature "middleware"
use ecat::auth::JwtAuthLayer;            // feature "auth"
use ecat::data::redis::RedisCache;       // feature "redis"
```

Features par défaut = `http+grpc` ; utilisez `--no-default-features --features <composant>` pour alléger l'arbre de dépendances. Liste complète des features : `http` `grpc` `middleware` `auth` `client` `events` `metrics` `tracing` `circuit-breaker` `consul` `remote` `redis` `orm` `mssql`.

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

> Note : `ecat_middleware::TracingLayer` n'injecte pas de trace_id ; pour une injection de trace_id au niveau de la requête, utilisez `ecat_tracing::TracingLayer::new()`.

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

### Gestion des erreurs

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

## Phases d'implémentation

| Phase | Statut | Contenu |
|------|------|------|
| Phase 1 | ✅ Terminée | Squelette du projet, protos, errors, metadata, encoding, logging |
| Phase 2 | ✅ Terminée | Couche Transport (HTTP + gRPC) |
| Phase 3 | ✅ Terminée | Système de middleware (Recovery/Tracing/Logging/Timeout) |
| Phase 4 | ✅ Terminée | Gestion du cycle de vie de l'App |
| Phase 5 | ✅ Terminée | Registry, Config, Metrics |
| Phase 5.5 | ✅ Terminée | Couche d'accès aux données (traits + backend sqlx) |
| Phase 6 | ✅ Terminée | Chaîne d'outils CLI (new/proto/run/build) |
| Phase 7 | ✅ Terminée | README, exemples (helloworld), documents de conception |
| Phase 8 | ✅ Terminée | Intégration de la détection d'attaques (security-rust, ecat-security) |
| Phase 9 | ✅ Terminée | Écosystème phase 1 (health / client / circuit-breaker / auth / registry-consul) |
| Phase 10 | ✅ Terminée | Écosystème phase 2 (redis / mq / events / config-remote) |
| Phase 11 | ✅ Terminée | Écosystème phase 3 (testing / deploy / bench / openapi) |
| Phase 12 | ✅ Terminée | Renforcement communication et sécurité (client gRPC / OAuth2 / mTLS / traçage distribué) |
| Phase 13 | ✅ Terminée | Complétion des backends de données (etcd / Kafka / OpenSearch / InfluxDB) |
| Phase 14 | ✅ Terminée | Exploitation et expérience (WebSocket / gestion des versions d'API / Helm / CI/CD) |
| Phase 15 | ✅ Terminée | Extension d'écosystème v2 (vrai Kafka / RabbitMQ / MQTT / NATS / MongoDB / S3 / TDengine / OTLP / verrous distribués / planification / CLI watch+upgrade) |
| Phase 16 | ✅ Terminée | Renforcement de maintenance v2.4 (M1 MetricsLayer / M2 RetryLayer / M3 ValidateLayer / M4 CORS / U1 crate agrégé ecat / U2 examples / hash des tokens OAuth2 / suivi CVE) |

## Limites connues

- **Analyse GraphQL (ecat-graphql)** : prend en charge les paramètres de champ et les sélections imbriquées (les resolvers enrichis `query_field`/`mutation_field` peuvent accéder à `args`/`variables`/`selection`) ; les alias, fragments et champs multiples de niveau supérieur ne sont toujours pas pris en charge — ne l'exposez pas comme endpoint GraphQL générique.
- **Cache d'introspection OAuth2 (ecat-auth)** : la clé de cache est le hash SHA-256 du token (le token en clair n'est jamais stocké) ; la valeur en cache est filtrée par liste blanche (par défaut sub/exp/iat/role + iss/aud/scope/roles de extra, configurable via `cache_claims_whitelist` ; en cas d'absence, les claims complètes sont tout de même renvoyées, seul le cache est filtré) ; les entrées expirées TTL sont purgées activement à l'écriture (TTL par défaut 300 s).
- **Offset Kafka (ecat-mq-kafka)** : `enable.auto.commit=false` par défaut et aucun commit manuel — après un redémarrage du processus, la lecture reprend depuis la fin de la partition (latest) et les messages produits pendant l'arrêt sont ignorés ; il faut configurer explicitement `auto_commit=true` pour obtenir une sémantique at-least-once (le redémarrage reprend depuis le dernier point de commit).

## Objectifs de conception

| # | Objectif | Description |
|---|------|------|
| 1 | **Alignement Kratos** | Conserver la philosophie API-first, enfichable et d'abstraction unifiée de Kratos |
| 2 | **Idiomatique Rust** | Réutiliser tower::Service, les traits génériques et les abstractions à coût zéro ; pas de « Go in Rust » |
| 3 | **Sécurité de typage** | Capturer les erreurs à la compilation, définitions Protobuf entièrement typées |
| 4 | **Enfichable** | Registry, Config, Logging, Encoding tous abstraits par des traits |
| 5 | **Chaîne d'outils complète** | Le CLI prend en charge le scaffolding de projet, la génération de code proto et le mode développement |
| 6 | **Performance d'abord** | Abstractions à coût zéro + runtime asynchrone |
| 7 | **Observable** | tracing + Prometheus prêts à l'emploi |
| 8 | **Écosystème complet** | Client, circuit-breaker, authentification, health check, backends de registre |

## Notes techniques

### Pourquoi tower::Service

[`tower::Service`](https://docs.rs/tower/latest/tower/trait.Service.html) est l'équivalent de `http.Handler` dans l'écosystème asynchrone Rust. axum et tonic sont tous deux construits sur tower, donc e-cat n'a pas besoin d'un trait de middleware personnalisé — fournir directement des implémentations tower::Layer suffit à obtenir le même effet que les middleware Kratos, sans aucun coût d'adaptateur.

### Pourquoi un Cargo Workspace

Conforme à la conception modulaire de Kratos. Tous les crates `ecat-*` sont publiés en version synchronisée avec le workspace (actuellement 7.0.0), chacun compilé indépendamment, l'utilisateur les importe au besoin. Les crates centraux gardent un minimum de dépendances, les crates contrib fournissent des intégrations optionnelles.

### Pourquoi prost (plutôt que protobuf-rs)

prost est l'implémentation protobuf la plus utilisée de la communauté Rust ; elle génère du code typé sûr à la compilation et s'intègre profondément avec tonic.

## Documents de conception

- [Spécifications de conception](../../../docs/superpowers/specs/2026-07-29-ecat-framework-design.md)
- [Plan d'implémentation](../../../docs/superpowers/plans/2026-07-29-ecat-framework.md)
- [Plan d'écosystème v1](ecosystem-plan.md) (terminé)
- [Plan d'écosystème v2](ecosystem-plan-v2.md) (terminé)
- [Plan d'écosystème v3](ecosystem-plan-v3.md) (évaluation finale)
- [Référence API](api.md)
- [Rapport d'audit r5](audit-report-2026-08-01-r5.md) (2026-08-01)
- [Tutoriel de configuration des bases de données](database-config-tutorial.md)
- [Suivi des CVE de dépendances](dependency-cve-tracking.md)
- [Tutoriel d'authentification par certificat TLS](tls-certificate-tutorial.md)
- [Exemples de fichiers de configuration](../../../config/databases.example.yaml)

## Soutien

Soutenez ce projet, vous êtes les bienvenus !

| WeChat Pay | Alipay |
|:---:|:---:|
| <img src="weixinpay.png" width="130" height="130" alt="WeChat Pay"> | <img src="alipay.png" width="130" height="130" alt="Alipay"> |

### Virement international (virement bancaire)

| Champ | Détails |
|------|------|
| Nom du bénéficiaire | WANG KEXUN |
| Numéro de compte du bénéficiaire | 881015918251 |
| Banque du bénéficiaire | ZA Bank Limited |
| Code SWIFT | AABLHKHHXXX |
| Code bancaire | 387 |
| Adresse de la banque | Core F, Cyberport 3, 100 Cyberport Road, Hong Kong |

> **Banque correspondante pour virements internationaux (si nécessaire)** : il s'agit des informations de la banque correspondante (banque intermédiaire), et non de la banque bénéficiaire. Veuillez demander à votre banque émettrice si ces informations sont requises.
>
> - Pour les virements en HKD, CNY et USD : **Citibank N.A. Hong Kong** (SWIFT : `CITIHKHXXXX`, code bancaire : 006, succursale : Hong Kong Branch, numéro de succursale : 391, adresse : Citibank Tower, Citibank Plaza, 3 Garden Road, Central, Hong Kong)
> - Pour les autres devises : **THE BANK OF NEW YORK MELLON** (SWIFT : `IRVTUS3NXXX`, adresse : 240 GREENWICH STREET, NEW YORK, United States)

### Don en cryptomonnaie (Crypto Donation)

Si ce projet vous est utile, scannez le code QR pour faire un don, merci !

| <img src="../../coin/1.jpg" width="200" alt="BNB Smart Chain (BEP20)"><br>**BNB Smart Chain (BEP20)**<br>`0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../coin/2.jpg" width="200" alt="Tron (TRC20)"><br>**Tron (TRC20)**<br>`TEdDHWLajt1XvqtPDWmQctdrJaC3pzZZzz` |
| <img src="../../coin/3.jpg" width="200" alt="Ethereum (ERC20)"><br>**Ethereum (ERC20)**<br>`0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../coin/4.jpg" width="200" alt="Aptos"><br>**Aptos**<br>`0x836e3780edfc3f7b2372b39e2a1a3a5d7adfaccd96c726f21cfde1b50dd68030` |
| <img src="../../coin/5.jpg" width="200" alt="Plasma"><br>**Plasma**<br>`0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../coin/6.jpg" width="200" alt="Polygon POS"><br>**Polygon POS**<br>`0x355d429f97511897ccb4e271ec888205f9ab6629` |
| <img src="../../coin/7.jpg" width="200" alt="Solana"><br>**Solana**<br>`2hfhboHdmdrYsY25XfQSsEWxq5ip4EQsR7f4AzSRMUyr` | <img src="../../coin/8.jpg" width="200" alt="The Open Network (TON)"><br>**The Open Network (TON)**<br>`UQB9kFQohzmXUir9QSSZq01iwl9aQZIDdBpNmDklljRtCoGK` |
| <img src="../../coin/9.jpg" width="200" alt="Arbitrum One"><br>**Arbitrum One**<br>`0x355d429f97511897ccb4e271ec888205f9ab6629` | <img src="../../coin/10.jpg" width="200" alt="AVAX C-Chain"><br>**AVAX C-Chain**<br>`0x355d429f97511897ccb4e271ec888205f9ab6629` |

## Licence

Apache-2.0
