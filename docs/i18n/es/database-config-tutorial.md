# Tutorial de configuración de base de datos

**Versión:** 2.4.2 · **Fecha:** 2026-08-01

Los 14 backends de datos de e-cat admiten cargar la información de conexión desde archivos de configuración, sin necesidad de codificarla en el código. `username` / `password` son campos opcionales; si se omiten, se omite la autenticación.

---

## Inicio rápido

### 1. Crear el archivo de configuración

Copia la plantilla de ejemplo y modifícala según tu entorno real:

```bash
cp config/databases.example.yaml databases.yaml
```

Edita `databases.yaml` y completa la información de conexión real:

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

### 2. Añadir dependencias

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
ecat-data-sqlx = { path = "../ecat-data-sqlx" }
ecat-data-redis = { path = "../ecat-data-redis" }
ecat-data-clickhouse = { path = "../ecat-data-clickhouse" }
```

### 3. Cargar y usar

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

## Referencia completa de configuración

### Definir la estructura de configuración de nivel superior

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

### Ejemplo YAML completo

Consulta `config/databases.example.yaml`.

---

## Referencia rápida de campos Config por backend

### RDBMS — SqlxConfig

#### Pools nativos (PostgreSQL / MySQL / SQLite)

El controlador nativo se elige automáticamente según el esquema de la URL, sin configuración adicional:

| scheme | Controlador |
|---|---|
| `postgres://` / `postgresql://` | PostgreSQL |
| `mysql://` / `mariadb://` | MySQL |
| `sqlite:` | SQLite |

- **Tipos de tiempo nativos**: la ruta anterior con `AnyPool` de sqlx no admitía tipos de tiempo, había que hacer `CAST` a texto uno mismo; ahora las columnas de tiempo se leen directamente.
- **Tanto las mayúsculas/minúsculas como los espacios alrededor** del esquema **se toleran** (`"POSTGRES://…"` y `" postgres://…"` funcionan); un esquema no reconocido produce un error que **lo nombra**.
- `mssql://` **no lo sirve este backend** (usa `ecat-data-mssql`); pasarlo aquí se rechaza de forma explícita.

#### Ejemplo de configuración

```yaml
sql:
  url: "postgres://host:5432/dbname"
  # username: "app_user"    # 可选
  # password: "secret"      # 可选
```

| Campo | Tipo | Valor por defecto | Descripción |
|------|------|--------|------|
| `url` | `String` | — | Cadena de conexión sqlx; admite SQLite/PG/MySQL/TiDB |
| `username` | `Option<String>` | `None` | Opcional: autenticación embebida en la URL (junto con password) |
| `password` | `Option<String>` | `None` | Opcional: autenticación embebida en la URL (junto con username) |
| `max_connections` | `u32` | `10` | Número máximo de conexiones del pool |
| `min_connections` | `u32` | `0` | Conexiones mínimas mantenidas; se ajusta a ≤ `max_connections` |
| `acquire_timeout_secs` | `u64` | `30` | Tiempo de espera para obtener una conexión |
| `idle_timeout_secs` | `u64` | `600` | Reciclado de conexiones inactivas |
| `max_lifetime_secs` | `u64` | `1800` | Vida máxima de una conexión |
| `query_timeout_secs` | `u64` | `30` | Timeout por consulta; **0 = deshabilitado** |
| `test_before_acquire` | `bool` | `false` | Hacer ping antes de entregar la conexión |
| `session_init` | `string[]` | Según dialecto | Sentencias de inicialización de sesión para cada conexión nueva |

#### `session_init` (inicialización de sesión)

Tras establecer cada conexión nueva, se ejecutan estas sentencias en orden:

| Dialecto | Predeterminado `session_init` |
|---|---|
| PostgreSQL | `SET TIME ZONE 'UTC'`, `SET application_name = 'ecat'` |
| MySQL | `SET time_zone = '+00:00'` |
| SQLite | Sin concepto de sesión — vacío por defecto |

El objetivo es que la **base de datos devuelva UTC directamente**, en línea con la convención del framework de presentar todas las horas como RFC3339 UTC.

- Un **array vacío explícito `[]` significa "desactivado a propósito"** y anula el valor predeterminado del dialecto (la misma convención de anulación explícita que `query_timeout_secs: 0` para deshabilitar).
- Si falla cualquier sentencia → falla la creación de la conexión, **sin degradación silenciosa**.

```yaml
sql:
  url: "mysql://host:3306/dbname"
  session_init:
    - "SET time_zone = '+00:00'"
    - "SET NAMES utf8mb4"
```

#### `warm_up()` — Precalentamiento

Crea activamente `min_connections` conexiones al arrancar y las devuelve, para que el servicio esté listo desde el primer momento:

```rust
let db = SqlxClient::from_config(cfg).await?;
db.warm_up().await?;   // llamar una vez al arrancar
```

Por qué hace falta: sqlx mantiene `min_connections` **de forma asíncrona en una tarea de fondo**, así que el retorno de `connect()` no garantiza que el pool esté lleno — la primera oleada de peticiones competiría con la tarea de fondo.

#### Hora y fecha: siempre RFC3339 UTC

**El texto de fecha/hora de sqlite se reescribe a RFC3339 UTC**:

- texto con la forma `2026-10-05 12:34:56` → `"2026-10-05T12:34:56Z"`
- texto con la forma `2026-10-05` → `"2026-10-05T00:00:00Z"` (el instante de medianoche UTC)

Motivo: sqlite no tiene sistema de tipos, y el texto con forma de fecha u hora es ambiguo por naturaleza; el framework lo trata de forma uniforme como hora.

**Efecto secundario**: Las columnas de texto que solo casualmente parecen una fecha — números de versión, códigos de negocio — también se reescriben. Si no quieres este comportamiento, haz un CAST explícito de la columna a una forma no-dato o usa otro tipo.

Las columnas reales `DATE` / `TIMESTAMP` de PG / MySQL también se presentan como cadenas RFC3339 UTC, y **una fecha pura igualmente lleva `T00:00:00Z`** — porque `"2026-10-05"` no es RFC3339 válido y el framework usa internamente un único formato para que las capas superiores puedan analizarlo.

### Redis — RedisConfig

```yaml
redis:
  url: "redis://host:6379"
  # password: "auth_token"  # 可选
```

| Campo | Tipo | Descripción |
|------|------|------|
| `url` | `String` | URL de conexión Redis |
| `password` | `Option<String>` | Opcional: contraseña de Redis AUTH |

### Memcached — MemcachedConfig

```yaml
memcached:
  # username: "memcache"    # 可选: 保留字段（当前为内存实现）
  # password: "secret"      # 可选: 保留字段
  {}
```

| Campo | Tipo | Descripción |
|------|------|------|
| `username` | `Option<String>` | Opcional: campo reservado |
| `password` | `Option<String>` | Opcional: campo reservado |

Actualmente es una implementación en memoria; los campos de autenticación quedan reservados.

### ClickHouse — ClickhouseConfig

```yaml
clickhouse:
  base_url: "http://host:8123"
  database: "default"
  # username: "default"   # 可选
  # password: "secret"    # 可选
```

| Campo | Tipo | Valor por defecto | Descripción |
|------|------|--------|------|
| `base_url` | `String` | — | Dirección de la interfaz HTTP |
| `database` | `String` | `"default"` | Nombre de la base de datos |
| `username` | `Option<String>` | `None` | Opcional: usuario de HTTP Basic Auth |
| `password` | `Option<String>` | `None` | Opcional: contraseña de HTTP Basic Auth |

### QuestDB — QuestdbConfig

```yaml
questdb:
  base_url: "http://host:9000"
  # username: "admin"     # 可选
  # password: "quest"     # 可选
```

| Campo | Tipo | Descripción |
|------|------|------|
| `base_url` | `String` | Dirección de la API HTTP |
| `username` | `Option<String>` | Opcional: usuario de HTTP Basic Auth |
| `password` | `Option<String>` | Opcional: contraseña de HTTP Basic Auth |

### Elasticsearch — ElasticsearchConfig

```yaml
elasticsearch:
  base_url: "http://host:9200"
  # username: "elastic"   # 可选
  # password: "secret"    # 可选
```

| Campo | Tipo | Descripción |
|------|------|------|
| `base_url` | `String` | Dirección de la API REST |
| `username` | `Option<String>` | Opcional: usuario de HTTP Basic Auth |
| `password` | `Option<String>` | Opcional: contraseña de HTTP Basic Auth |

### OpenSearch — OpenSearchConfig

```yaml
opensearch:
  base_url: "http://host:9200"
  # username: "admin"     # 可选
  # password: "secret"    # 可选
```

| Campo | Tipo | Descripción |
|------|------|------|
| `base_url` | `String` | Dirección de la API REST |
| `username` | `Option<String>` | Opcional: usuario de HTTP Basic Auth |
| `password` | `Option<String>` | Opcional: contraseña de HTTP Basic Auth |

### InfluxDB — InfluxConfig

```yaml
influxdb:
  base_url: "http://host:8086"
  org: "myorg"
  bucket: "mybucket"
  token: "my-token"
```

| Campo | Tipo | Descripción |
|------|------|------|
| `base_url` | `String` | Dirección de la API de InfluxDB 2.x |
| `org` | `String` | Nombre de la organización |
| `bucket` | `String` | Nombre del bucket |
| `token` | `String` | Token de autenticación |

### Neo4j — Neo4jConfig

```yaml
neo4j:
  base_url: "http://host:7474"
  username: "neo4j"
  password: "secret"
```

| Campo | Tipo | Descripción |
|------|------|------|
| `base_url` | `String` | Dirección de la API REST |
| `username` | `String` | Nombre de usuario |
| `password` | `String` | Contraseña |

### NebulaGraph — NebulaGraphConfig

```yaml
nebulagraph:
  base_url: "http://host:19669"
  space: "my_space"
  # username: "root"      # 可选
  # password: "nebula"    # 可选
```

| Campo | Tipo | Descripción |
|------|------|------|
| `base_url` | `String` | Dirección de la API |
| `space` | `String` | Nombre del espacio de grafo |
| `username` | `Option<String>` | Opcional: usuario de HTTP Basic Auth |
| `password` | `Option<String>` | Opcional: contraseña de HTTP Basic Auth |

### ArangoDB — ArangoConfig

```yaml
arangodb:
  base_url: "http://host:8529"
  db: "mydb"
  username: "root"
  password: "secret"
```

| Campo | Tipo | Descripción |
|------|------|------|
| `base_url` | `String` | Dirección de la API |
| `db` | `String` | Nombre de la base de datos |
| `username` | `String` | Nombre de usuario |
| `password` | `String` | Contraseña |

### IoTDB — IotdbConfig

```yaml
iotdb:
  base_url: "http://host:18080"
  username: "root"
  password: "root"
```

| Campo | Tipo | Descripción |
|------|------|------|
| `base_url` | `String` | Dirección de la API REST |
| `username` | `String` | Nombre de usuario |
| `password` | `String` | Contraseña |

---

## Creación programática

### Sin autenticación

```rust
let es = ElasticsearchClient::new("http://localhost:9200");
let ch = ClickhouseClient::new("http://localhost:8123", "default");
```

### Con autenticación

```rust
let es = ElasticsearchClient::with_auth("http://es:9200", "elastic", "secret");
let ch = ClickhouseClient::with_auth("http://ch:8123", "default", "admin", "pass");
let qdb = QuestdbClient::with_auth("http://qdb:9000", "admin", "quest");
let ng = NebulaGraphClient::with_auth("http://ng:19669", "space1", "root", "nebula");
```

---

---

## Configuración de certificados TLS

Todos los backends de datos admiten autenticación TLS opcional de cliente (campo `tls`).

### Ejemplo de configuración

```yaml
clickhouse:
  base_url: "https://ch.internal:8443"
  tls:
    ca_cert: "/etc/ecat/ca.pem"
    client_cert: "/etc/ecat/client.pem"
    client_key: "/etc/ecat/client-key.pem"
    # skip_verify: true  # 仅测试环境
```

### Generación automática de certificados (ecat-tls)

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

### Generación manual (OpenSSL)

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

### Descripción de campos TLS

| Campo | Tipo | Descripción |
|------|------|------|
| `ca_cert` | `Option<String>` | Ruta del certificado CA PEM (verifica el servidor) |
| `client_cert` | `Option<String>` | Ruta del certificado de cliente PEM (mTLS) |
| `client_key` | `Option<String>` | Ruta de la clave privada de cliente PEM (mTLS) |
| `skip_verify` | `Option<bool>` | Omite la verificación de certificados (solo pruebas) |

---

## Uso avanzado

### Sobrescritura con variables de entorno

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

### Integración con el framework ecat-config

```rust
use ecat_config::{Config, FileSource};

let mut app_config = Config::new();
app_config.load(&FileSource::new("databases.yaml")).await?;

let redis_cfg: RedisConfig = serde_json::from_value(
    app_config.get::<serde_json::Value>("redis").unwrap()
)?;
let cache = RedisCache::from_config(redis_cfg).await?;
```

### Configuración bajo demanda

Las bases de datos no utilizadas se omiten en el YAML; la estructura Rust los marca con `Option`:

```rust
#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: Option<RedisConfig>,
    clickhouse: Option<ClickhouseConfig>,
}
```

---

## Documentos relacionados

- [Informe de auditoría r5](audit-report-2026-08-01-r5.md)
- [Tutorial de autenticación con certificados TLS](tls-certificate-tutorial.md)
- [Archivo de ejemplo de configuración](../../../config/databases.example.yaml)
