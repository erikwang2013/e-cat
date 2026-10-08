# Tutoriel de configuration des bases de données

**Version :** 2.4.2 · **Date :** 2026-08-01

Les 14 backends de données d'e-cat prennent tous en charge le chargement des informations de connexion depuis un fichier de configuration, sans codage en dur dans le code. `username` / `password` sont des champs optionnels ; s'ils sont omis, l'authentification est ignorée.

---

## Démarrage rapide

### 1. Créer le fichier de configuration

Copiez le modèle d'exemple et adaptez-le à votre environnement réel :

```bash
cp config/databases.example.yaml databases.yaml
```

Modifiez `databases.yaml` et renseignez les véritables informations de connexion :

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

### 2. Ajouter les dépendances

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
ecat-data-sqlx = { path = "../ecat-data-sqlx" }
ecat-data-redis = { path = "../ecat-data-redis" }
ecat-data-clickhouse = { path = "../ecat-data-clickhouse" }
```

### 3. Charger et utiliser

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

## Référence complète de la configuration

### Définir la structure de configuration de niveau supérieur

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

### Exemple YAML complet

Voir `config/databases.example.yaml`.

---

## Référence rapide des champs Config par backend

### RDBMS — SqlxConfig

#### Pools natifs (PostgreSQL / MySQL / SQLite)

Le pilote natif est choisi automatiquement selon le schéma de l'URL, sans configuration supplémentaire :

| scheme | Pilote |
|---|---|
| `postgres://` / `postgresql://` | PostgreSQL |
| `mysql://` / `mariadb://` | MySQL |
| `sqlite:` | SQLite |

- **Types temporels natifs** : l'ancien chemin via `AnyPool` de sqlx ne gérait pas les types temporels, il fallait faire soi-même un `CAST` en texte ; les colonnes temporelles sont désormais lues directement.
- **La casse et les espaces autour** du schéma **sont tous deux tolérés** (`"POSTGRES://…"` et `" postgres://…"` fonctionnent) ; un schéma non reconnu lève une erreur qui **le nomme**.
- `mssql://` **n'est pas pris en charge par ce backend** (utiliser `ecat-data-mssql`) ; le transmettre ici est explicitement refusé.

#### Exemple de configuration

```yaml
sql:
  url: "postgres://host:5432/dbname"
  # username: "app_user"    # 可选
  # password: "secret"      # 可选
```

| Champ | Type | Valeur par défaut | Description |
|------|------|--------|------|
| `url` | `String` | — | Chaîne de connexion sqlx, prend en charge SQLite/PG/MySQL/TiDB |
| `username` | `Option<String>` | `None` | Optionnel : authentification intégrée à l'URL (avec password) |
| `password` | `Option<String>` | `None` | Optionnel : authentification intégrée à l'URL (avec username) |
| `max_connections` | `u32` | `10` | Nombre maximal de connexions dans le pool |
| `min_connections` | `u32` | `0` | Nombre minimal de connexions maintenues ; ramené à ≤ `max_connections` |
| `acquire_timeout_secs` | `u64` | `30` | Délai d'attente d'une connexion ; **`0` = timeout immédiat** (échec si aucune connexion n'est libre), l'inverse de `0` = désactivé dans `query_timeout_secs` |
| `idle_timeout_secs` | `u64` | `600` | Recyclage des connexions inactives |
| `max_lifetime_secs` | `u64` | `1800` | Durée de vie maximale d'une connexion |
| `query_timeout_secs` | `u64` | `30` | Timeout par requête ; **0 = désactivé** |
| `slow_query_ms` | `u64` | `1000` | Seuil d'alerte de requête lente (ms) ; non configuré = 1000, **0 = désactivé** (feature `tracing` uniquement) |
| `test_before_acquire` | `bool` | `false` | Ping avant de remettre la connexion |
| `session_init` | `string[]` | Selon le dialecte | Instructions d'initialisation de session pour chaque nouvelle connexion |

#### `session_init` (initialisation de session)

Après l'établissement de chaque nouvelle connexion, ces instructions sont exécutées dans l'ordre :

| Dialecte | Défaut `session_init` |
|---|---|
| PostgreSQL | `SET TIME ZONE 'UTC'`, `SET application_name = 'ecat'` |
| MySQL | `SET time_zone = '+00:00'` |
| SQLite | Pas de notion de session — vide par défaut |

L'objectif est que la **base renvoie directement de l'UTC**, en accord avec la convention du framework de présenter tous les temps en RFC3339 UTC.

- Un **tableau vide explicite `[]` signifie "désactivé volontairement"** et écrase le défaut du dialecte (même convention de remplacement explicite que `query_timeout_secs: 0` pour désactiver).
- Si une instruction échoue → la création de la connexion échoue, **sans repli silencieux**.

```yaml
sql:
  url: "mysql://host:3306/dbname"
  session_init:
    - "SET time_zone = '+00:00'"
    - "SET NAMES utf8mb4"
```

#### `warm_up()` — Préchauffage

Ouvre activement `min_connections` connexions au démarrage puis les rend, afin que le service soit prêt dès son démarrage :

```rust
let db = SqlxClient::from_config(cfg).await?;
db.warm_up().await?;   // à appeler une fois au démarrage
```

Pourquoi c'est nécessaire : sqlx maintient `min_connections` **de manière asynchrone dans une tâche d'arrière-plan**, donc le retour de `connect()` ne garantit pas que le pool soit rempli — la première vague de requêtes ferait la course avec la tâche d'arrière-plan.

#### Features d'observabilité (`metrics` / `health` / `tracing`)

Les trois features sont **désactivées par défaut** (pour ne pas faire entrer axum — dépendance de `ecat-metrics` / `ecat-health` — dans l'arbre de dépendances du cœur) ; activez-les selon vos besoins. `ecat-data-mssql` propose les mêmes trois.

```toml
ecat-data-sqlx = { path = "../ecat-data-sqlx", features = ["metrics", "health", "tracing"] }
```

```rust
use std::sync::Arc;
use ecat_data_sqlx::{RdbmsHealthCheck, SqlxClient, SqlxConfig, register_pool_metrics};
use ecat_health::HealthRegistry;

let db = SqlxClient::from_config(cfg).await?;

// metrics : après enregistrement, l'endpoint /metrics expose quatre métriques —
// ecat_rdbms_pool_connections (gauge, avec state="idle"/"active"),
// ecat_rdbms_pool_timeouts_total, ecat_rdbms_query_timeout_total,
// ecat_rdbms_transactions_leaked_total (toutes counter, avec le label backend)
register_pool_metrics("primary", db.pool());

// health : sonde de connectivité SELECT 1, enregistrée sur le readyz de /health
let registry = HealthRegistry::new()
    .with_check(RdbmsHealthCheck::new("sql", Arc::new(db)));
```

La feature `tracing` émet un warn lorsqu'une requête dépasse `slow_query_ms` (durée + le SQL tronqué aux 200 premiers caractères). Ce champ n'est lu que par cette feature : sans elle, il reste analysé mais sans effet.

#### Temps et dates : RFC3339 UTC uniformément

**Les textes date/heure de sqlite sont réécrits en RFC3339 UTC** :

- un texte de la forme `2026-10-05 12:34:56` → `"2026-10-05T12:34:56Z"`
- un texte de la forme `2026-10-05` → `"2026-10-05T00:00:00Z"` (l'instant de minuit UTC)

Raison : sqlite n'a pas de système de types, un texte en forme de date ou d'heure est ambigu par nature ; le framework le traite uniformément comme un temps.

**Effet secondaire**: Les colonnes textuelles qui ressemblent à une date par simple coïncidence — numéros de version, codes métier — sont réécrites elles aussi. Si vous ne voulez pas de ce comportement, faites un CAST explicite de la colonne vers une forme non-date, ou utilisez un autre type.

Les vraies colonnes `DATE` / `TIMESTAMP` de PG / MySQL sont elles aussi présentées en chaînes RFC3339 UTC, et **une date pure porte quand même `T00:00:00Z`** — car `"2026-10-05"` n'est pas du RFC3339 valide, et le framework utilise en interne un format unique pour que les couches supérieures puissent le parser.

### Redis — RedisConfig

```yaml
redis:
  url: "redis://host:6379"
  # password: "auth_token"  # facultatif
  # query_timeout_secs: 30     # facultatif : timeout par commande, 0 = désactivé
  # breaker: {}                # facultatif : config du breaker, omis = valeurs par défaut prudentes (0,5 / 30 s / ouvert 10 s)
```

| Champ | Type | Valeur par défaut | Description |
|------|------|--------|------|
| `url` | `String` | — | URL de connexion Redis |
| `password` | `Option<String>` | `None` | Optionnel : mot de passe Redis AUTH |
| `query_timeout_secs` | `Option<u64>` | `30` | Timeout par commande, en secondes ; **`0` = désactivé** |
| `breaker` | `Option<BreakerConfig>` | défauts prudents | Seuils et fenêtre du breaker ; des champs peuvent être omis, `breaker: {}` = tous les défauts |

**Limite de capacité** : ce client utilise une `MultiplexedConnection` (une seule connexion TCP sert toute la concurrence), **pas un pool de connexions** — pour une charge de cache, c'est mieux qu'un pool : moins de connexions et moins d'allers-retours. Le prix : **les séquences de commandes à état ne peuvent pas l'utiliser** — les transactions `MULTI`/`EXEC`, `WATCH`, `SUBSCRIBE` et les commandes bloquantes exigent une connexion exclusive ; en multiplexage, elles s'entrelaceraient avec d'autres commandes. Au besoin, ouvrez une connexion dédiée avec `redis::Client::get_async_connection()`.

**Timeouts** : `query_timeout_secs: 0` dans la configuration signifie **désactivé** (et non « timeout après 0 seconde ») ; les 30 secondes ne s'appliquent que si le champ est omis. Au niveau de la bibliothèque, `run_with_timeout(kind, Some(Duration::ZERO), fut)` est l'inverse — cela **expire immédiatement** (tokio sonde d'abord le future interne, donc un future déjà prêt réussit quand même). Les deux « 0 » n'ont pas le même sens ; ne recopiez pas la valeur de configuration en appelant directement la fonction de bibliothèque.

**Le breaker est actif par défaut** — avec des seuils prudents (taux d'échec 0,5, fenêtre 30 s, sondes half-open 3, ouvert 10 s), il ne s'ouvre qu'en cas d'échecs persistants. **Il n'y a actuellement aucun interrupteur général** : `BreakerConfig` ne contient que ces quatre champs de seuil, sans `enabled` ; écrire `{"enabled": false}` ne produit qu'une erreur de désérialisation. Pour le désactiver vraiment, il faut pousser les seuils hors d'atteinte (p. ex. `failure_ratio: 1.1`).

### Memcached — MemcachedConfig

```yaml
memcached:
  # username: "memcache"    # 可选: 保留字段（当前为内存实现）
  # password: "secret"      # 可选: 保留字段
  {}
```

| Champ | Type | Description |
|------|------|------|
| `username` | `Option<String>` | Optionnel : champ réservé |
| `password` | `Option<String>` | Optionnel : champ réservé |

Il s'agit actuellement d'une implémentation en mémoire ; les champs d'authentification sont réservés pour une utilisation future.

### ClickHouse — ClickhouseConfig

```yaml
clickhouse:
  base_url: "http://host:8123"
  database: "default"
  # username: "default"   # facultatif
  # password: "secret"    # facultatif
  # query_timeout_secs: 30  # facultatif : timeout par appel, 0 = désactivé
  # breaker: {}             # facultatif : config du breaker, omis = valeurs par défaut prudentes (0,5 / 30 s / ouvert 10 s)
  # max_concurrency: 32     # facultatif : limite de concurrence (sémaphore de ce crate)
```

| Champ | Type | Valeur par défaut | Description |
|------|------|--------|------|
| `base_url` | `String` | — | Adresse de l'interface HTTP |
| `database` | `String` | `"default"` | Nom de la base de données |
| `username` | `Option<String>` | `None` | Optionnel : nom d'utilisateur HTTP Basic Auth |
| `password` | `Option<String>` | `None` | Optionnel : mot de passe HTTP Basic Auth |
| `query_timeout_secs` | `Option<u64>` | `30` | Timeout par appel, en secondes ; **`0` = désactivé** (comme Redis) |
| `breaker` | `Option<BreakerConfig>` | défauts prudents | Seuils et fenêtre du breaker ; pas d'interrupteur général `enabled` non plus |
| `max_concurrency` | `Option<usize>` | `32` | Limite de concurrence ; **sémaphore propre à ce crate**, pas un réglage reqwest (reqwest n'a que `pool_max_idle_per_host` — connexions inactives conservées, pas de plafond) |

**Deux couches de timeout** : le `reqwest::Client` construit par `from_config` (`ecat-tls`) apporte ses propres 5 s de timeout de connexion + 30 s au total ; `query_timeout_secs` est le budget **externe** — quand les deux sont actifs, **le premier qui expire gagne** ; si c'est l'interne, l'erreur est `RdbmsError::Database` et **n'est pas comptée** dans `ecat_outbound_timeouts_total` (le compteur des timeouts externes, feature `metrics`). `new` / `with_auth` utilisent un `reqwest::Client::new()` nu, sans timeout interne.

### QuestDB — QuestdbConfig

```yaml
questdb:
  base_url: "http://host:9000"
  # username: "admin"     # 可选
  # password: "quest"     # 可选
```

| Champ | Type | Description |
|------|------|------|
| `base_url` | `String` | Adresse de l'API HTTP |
| `username` | `Option<String>` | Optionnel : nom d'utilisateur HTTP Basic Auth |
| `password` | `Option<String>` | Optionnel : mot de passe HTTP Basic Auth |

### Elasticsearch — ElasticsearchConfig

```yaml
elasticsearch:
  base_url: "http://host:9200"
  # username: "elastic"   # 可选
  # password: "secret"    # 可选
```

| Champ | Type | Description |
|------|------|------|
| `base_url` | `String` | Adresse de l'API REST |
| `username` | `Option<String>` | Optionnel : nom d'utilisateur HTTP Basic Auth |
| `password` | `Option<String>` | Optionnel : mot de passe HTTP Basic Auth |

### OpenSearch — OpenSearchConfig

```yaml
opensearch:
  base_url: "http://host:9200"
  # username: "admin"     # 可选
  # password: "secret"    # 可选
```

| Champ | Type | Description |
|------|------|------|
| `base_url` | `String` | Adresse de l'API REST |
| `username` | `Option<String>` | Optionnel : nom d'utilisateur HTTP Basic Auth |
| `password` | `Option<String>` | Optionnel : mot de passe HTTP Basic Auth |

### InfluxDB — InfluxConfig

```yaml
influxdb:
  base_url: "http://host:8086"
  org: "myorg"
  bucket: "mybucket"
  token: "my-token"
```

| Champ | Type | Description |
|------|------|------|
| `base_url` | `String` | Adresse de l'API InfluxDB 2.x |
| `org` | `String` | Nom de l'organisation |
| `bucket` | `String` | Nom du bucket |
| `token` | `String` | Jeton d'authentification |

### Neo4j — Neo4jConfig

```yaml
neo4j:
  base_url: "http://host:7474"
  username: "neo4j"
  password: "secret"
```

| Champ | Type | Description |
|------|------|------|
| `base_url` | `String` | Adresse de l'API REST |
| `username` | `String` | Nom d'utilisateur |
| `password` | `String` | Mot de passe |

### NebulaGraph — NebulaGraphConfig

```yaml
nebulagraph:
  base_url: "http://host:19669"
  space: "my_space"
  # username: "root"      # 可选
  # password: "nebula"    # 可选
```

| Champ | Type | Description |
|------|------|------|
| `base_url` | `String` | Adresse de l'API |
| `space` | `String` | Nom de l'espace de graphe |
| `username` | `Option<String>` | Optionnel : nom d'utilisateur HTTP Basic Auth |
| `password` | `Option<String>` | Optionnel : mot de passe HTTP Basic Auth |

### ArangoDB — ArangoConfig

```yaml
arangodb:
  base_url: "http://host:8529"
  db: "mydb"
  username: "root"
  password: "secret"
```

| Champ | Type | Description |
|------|------|------|
| `base_url` | `String` | Adresse de l'API |
| `db` | `String` | Nom de la base de données |
| `username` | `String` | Nom d'utilisateur |
| `password` | `String` | Mot de passe |

### IoTDB — IotdbConfig

```yaml
iotdb:
  base_url: "http://host:18080"
  username: "root"
  password: "root"
```

| Champ | Type | Description |
|------|------|------|
| `base_url` | `String` | Adresse de l'API REST |
| `username` | `String` | Nom d'utilisateur |
| `password` | `String` | Mot de passe |

---

## Création programmatique

### Sans authentification

```rust
let es = ElasticsearchClient::new("http://localhost:9200");
let ch = ClickhouseClient::new("http://localhost:8123", "default");
```

### Avec authentification

```rust
let es = ElasticsearchClient::with_auth("http://es:9200", "elastic", "secret");
let ch = ClickhouseClient::with_auth("http://ch:8123", "default", "admin", "pass");
let qdb = QuestdbClient::with_auth("http://qdb:9000", "admin", "quest");
let ng = NebulaGraphClient::with_auth("http://ng:19669", "space1", "root", "nebula");
```

---

---

## Configuration TLS des certificats

Tous les backends de données prennent en charge, en règle générale, l'authentification client TLS optionnelle (champ `tls`), avec **deux exceptions** : `ecat-data-sqlx` ne prend pas en charge ce champ — le renseigner fait échouer le démarrage (son TLS passe par les paramètres d'URL) ; le champ de `ecat-data-memcached` est **silencieusement inopérant** — déclaré, mais jamais lu dans le crate.

### Exemple de configuration

```yaml
clickhouse:
  base_url: "https://ch.internal:8443"
  tls:
    ca_cert: "/etc/ecat/ca.pem"
    client_cert: "/etc/ecat/client.pem"
    client_key: "/etc/ecat/client-key.pem"
    # skip_verify: true  # 仅测试环境
```

### Génération automatique des certificats (ecat-tls)

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

### Génération manuelle (OpenSSL)

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

### Description des champs TLS

| Champ | Type | Description |
|------|------|------|
| `ca_cert` | `Option<String>` | Chemin du certificat CA PEM (validation du serveur) |
| `client_cert` | `Option<String>` | Chemin du certificat client PEM (mTLS) |
| `client_key` | `Option<String>` | Chemin de la clé privée client PEM (mTLS) |
| `skip_verify` | `Option<bool>` | Ignorer la validation des certificats (test uniquement) |

---

## Utilisation avancée

### Surcharge par variable d'environnement

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

### Intégration avec le framework ecat-config

```rust
use ecat_config::{Config, FileSource};

let mut app_config = Config::new();
app_config.load(&FileSource::new("databases.yaml")).await?;

let redis_cfg: RedisConfig = serde_json::from_value(
    app_config.get::<serde_json::Value>("redis").unwrap()
)?;
let cache = RedisCache::from_config(redis_cfg).await?;
```

### Configuration à la demande

Les bases de données non utilisées sont omises dans le YAML ; les structures Rust utilisent `Option` :

```rust
#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: Option<RedisConfig>,
    clickhouse: Option<ClickhouseConfig>,
}
```

---

## Documents associés

- [Rapport d'audit r5](audit-report-2026-08-01-r5.md)
- [Tutoriel d'authentification par certificat TLS](tls-certificate-tutorial.md)
- [Exemple de fichier de configuration](../../../config/databases.example.yaml)
