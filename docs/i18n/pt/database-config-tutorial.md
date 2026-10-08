# Tutorial de configuração de banco de dados

**Versão:** 2.4.2 · **Data:** 2026-08-01

Os 14 backends de dados do e-cat suportam carregar informações de conexão via arquivo de configuração, sem hardcoding no código. `username` / `password` são campos opcionais; omitidos, a autenticação é pulada.

---

## Início rápido

### 1. Criar o arquivo de configuração

Copie o modelo de exemplo e adapte ao seu ambiente:

```bash
cp config/databases.example.yaml databases.yaml
```

Edite `databases.yaml` com as informações de conexão reais:

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

### 2. Adicionar as dependências

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
ecat-data-sqlx = { path = "../ecat-data-sqlx" }
ecat-data-redis = { path = "../ecat-data-redis" }
ecat-data-clickhouse = { path = "../ecat-data-clickhouse" }
```

### 3. Carregar e usar

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
    // Carregar a configuração YAML
    let yaml = std::fs::read_to_string("databases.yaml")?;
    let cfg: AppConfig = serde_yaml::from_str(&yaml)?;

    // Criar os clientes de banco de dados — sem informações de conexão codificadas
    let db = SqlxClient::from_config(cfg.sql).await?;
    let cache = RedisCache::from_config(cfg.redis).await?;
    let ch = ClickhouseClient::from_config(cfg.clickhouse);

    // Uso
    let rows = db.query("SELECT id, name FROM users LIMIT 10").await?;
    cache.set("health", b"ok", std::time::Duration::from_secs(30)).await?;

    Ok(())
}
```

---

## Referência completa de configuração

### Definir a estrutura de configuração de topo

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

### Exemplo YAML completo

Veja `config/databases.example.yaml`.

---

## Consulta rápida dos campos Config por backend

### RDBMS — SqlxConfig

#### Pools nativos (PostgreSQL / MySQL / SQLite)

O driver nativo é escolhido automaticamente pelo esquema da URL, sem configuração extra:

| scheme | Driver |
|---|---|
| `postgres://` / `postgresql://` | PostgreSQL |
| `mysql://` / `mariadb://` | MySQL |
| `sqlite:` | SQLite |

- **Tipos de tempo nativos**: o caminho antigo com `AnyPool` do sqlx não suportava tipos de tempo, era preciso fazer `CAST` para texto por conta própria; agora as colunas de tempo são lidas diretamente.
- **Tanto maiúsculas/minúsculas quanto espaços em volta** do esquema **são tolerados** (`"POSTGRES://…"` e `" postgres://…"` funcionam); um esquema não reconhecido gera erro e **aponta qual** é.
- `mssql://` **não é atendido por este backend** (use `ecat-data-mssql`); passá-lo aqui é rejeitado explicitamente.

#### Exemplo de configuração

```yaml
sql:
  url: "postgres://host:5432/dbname"
  # username: "app_user"    # opcional
  # password: "secret"      # opcional
```

| Campo | Tipo | Valor padrão | Descrição |
|------|------|--------|------|
| `url` | `String` | — | String de conexão sqlx, suporta SQLite/PG/MySQL/TiDB |
| `username` | `Option<String>` | `None` | Opcional: autenticação embutida na URL (combinado com password) |
| `password` | `Option<String>` | `None` | Opcional: autenticação embutida na URL (combinado com username) |
| `max_connections` | `u32` | `10` | Número máximo de conexões no pool |
| `min_connections` | `u32` | `0` | Conexões mínimas mantidas; ajustado para ≤ `max_connections` |
| `acquire_timeout_secs` | `u64` | `30` | Tempo de espera por uma conexão; **`0` = timeout imediato** (falha se não houver conexão livre), o oposto de `0` = desativado em `query_timeout_secs` |
| `idle_timeout_secs` | `u64` | `600` | Reciclagem de conexões ociosas |
| `max_lifetime_secs` | `u64` | `1800` | Vida máxima de uma conexão |
| `query_timeout_secs` | `u64` | `30` | Timeout por consulta; **0 = desativado** |
| `slow_query_ms` | `u64` | `1000` | Limite de aviso de consulta lenta (ms); não configurado = 1000, **0 = desligado** (apenas feature `tracing`) |
| `test_before_acquire` | `bool` | `false` | Fazer ping antes de entregar a conexão |
| `session_init` | `string[]` | Conforme o dialeto | Instruções de inicialização de sessão para cada nova conexão |

#### `session_init` (inicialização de sessão)

Após cada nova conexão ser estabelecida, estas instruções são executadas em ordem:

| Dialeto | Padrão `session_init` |
|---|---|
| PostgreSQL | `SET TIME ZONE 'UTC'`, `SET application_name = 'ecat'` |
| MySQL | `SET time_zone = '+00:00'` |
| SQLite | Sem conceito de sessão — vazio por padrão |

O objetivo é fazer o **banco retornar UTC diretamente**, alinhado à convenção do framework de apresentar todos os horários como RFC3339 UTC.

- Um **array vazio explícito `[]` significa "desligado de propósito"** e sobrescreve o padrão do dialeto (a mesma convenção de sobrescrita explícita de `query_timeout_secs: 0` para desabilitar).
- Se qualquer instrução falhar → a criação da conexão falha, **sem degradação silenciosa**.

```yaml
sql:
  url: "mysql://host:3306/dbname"
  session_init:
    - "SET time_zone = '+00:00'"
    - "SET NAMES utf8mb4"
```

#### `warm_up()` — Pré-aquecimento

Abre ativamente `min_connections` conexões na inicialização e as devolve, deixando o serviço pronto assim que sobe:

```rust
let db = SqlxClient::from_config(cfg).await?;
db.warm_up().await?;   // chamar uma vez na inicialização
```

Por que é necessário: o sqlx mantém `min_connections` **de forma assíncrona numa tarefa de segundo plano**, então o retorno de `connect()` não garante que o pool esteja cheio — a primeira leva de requisições competiria com a tarefa de segundo plano.

#### Features de observabilidade (`metrics` / `health` / `tracing`)

As três features vêm **desativadas por padrão** (para não trazer o axum — dependência de `ecat-metrics` / `ecat-health` — para a árvore de dependências do núcleo); habilite conforme a necessidade. O `ecat-data-mssql` oferece as mesmas três.

```toml
ecat-data-sqlx = { path = "../ecat-data-sqlx", features = ["metrics", "health", "tracing"] }
```

```rust
use std::sync::Arc;
use ecat_data_sqlx::{RdbmsHealthCheck, SqlxClient, SqlxConfig, register_pool_metrics};
use ecat_health::HealthRegistry;

let db = SqlxClient::from_config(cfg).await?;

// metrics: após registrar, o endpoint /metrics ganha quatro métricas —
// ecat_rdbms_pool_connections (gauge, com state="idle"/"active"),
// ecat_rdbms_pool_timeouts_total, ecat_rdbms_query_timeout_total,
// ecat_rdbms_transactions_leaked_total (todas counter, com rótulo backend)
register_pool_metrics("primary", db.pool());

// health: sonda de conectividade SELECT 1, registrada no readyz do /health
let registry = HealthRegistry::new()
    .with_check(RdbmsHealthCheck::new("sql", Arc::new(db)));
```

A feature `tracing` emite um warn quando uma consulta passa de `slow_query_ms` (tempo decorrido + o SQL truncado nos primeiros 200 caracteres). Esse campo só é lido por essa feature — sem ela continua sendo parseado, mas não tem efeito.

#### Data e hora: sempre RFC3339 UTC

**Textos de data/hora do sqlite são reescritos para RFC3339 UTC**:

- texto no formato `2026-10-05 12:34:56` → `"2026-10-05T12:34:56Z"`
- texto no formato `2026-10-05` → `"2026-10-05T00:00:00Z"` (o instante da meia-noite UTC)

Motivo: o sqlite não tem sistema de tipos, e texto com forma de data ou hora é ambíguo por natureza; o framework o trata uniformemente como tempo.

**Efeito colateral**: Colunas de texto que só por acaso parecem uma data — números de versão, códigos de negócio — também são reescritas. Se não quiser esse comportamento, faça CAST explícito da coluna para uma forma não-data, ou use outro tipo.

Colunas reais `DATE` / `TIMESTAMP` no PG / MySQL também são apresentadas como strings RFC3339 UTC, e **uma data pura ainda carrega `T00:00:00Z`** — porque `"2026-10-05"` não é RFC3339 válido e o framework usa internamente um único formato, para facilitar o parsing nas camadas superiores.

### Redis — RedisConfig

```yaml
redis:
  url: "redis://host:6379"
  # password: "auth_token"  # opcional
  # query_timeout_secs: 30     # opcional: timeout por comando, 0 = desativado
  # breaker: {}                # opcional: config do breaker, omitido = padrões conservadores (0.5 / 30s / aberto 10s)
```

| Campo | Tipo | Valor padrão | Descrição |
|------|------|--------|------|
| `url` | `String` | — | URL de conexão Redis |
| `password` | `Option<String>` | `None` | Opcional: senha AUTH do Redis |
| `query_timeout_secs` | `Option<u64>` | `30` | Timeout por comando em segundos; **`0` = desativado** |
| `breaker` | `Option<BreakerConfig>` | padrões conservadores | Limiares e janela do breaker; campos podem ser omitidos, `breaker: {}` significa tudo no padrão |

**Limite de capacidade**: este client usa uma `MultiplexedConnection` (uma única conexão TCP atende toda a concorrência), **não um pool de conexões** — para carga de cache isso é melhor que um pool: menos conexões e menos idas e voltas. O preço é que **sequências de comandos com estado não podem usá-la**: transações `MULTI`/`EXEC`, `WATCH`, `SUBSCRIBE` e comandos bloqueantes precisam de uma conexão exclusiva — sob multiplexação elas se intercalariam com outros comandos. Quando precisar, abra uma conexão dedicada com `redis::Client::get_async_connection()`.

**Timeouts**: `query_timeout_secs: 0` na configuração significa **desativado** (não «timeout em 0 segundo»); os 30 segundos só valem quando o campo é omitido. No nível da biblioteca, `run_with_timeout(kind, Some(Duration::ZERO), fut)` é o oposto — isso **expira imediatamente** (o tokio faz poll do future interno primeiro, então um future já pronto ainda tem sucesso). Os dois «0» têm significados diferentes; não copie o valor da configuração ao chamar a função da biblioteca diretamente.

**O breaker vem ligado por padrão** — com limiares conservadores (taxa de falha 0.5, janela 30 s, sondas half-open 3, aberto 10 s) ele só abre sob falha contínua. **No momento não há chave geral**: `BreakerConfig` tem apenas esses quatro campos de limiar, sem `enabled`; escrever `{"enabled": false}` só gera erro de desserialização. Para desativá-lo de verdade, leve os limiares para fora de alcance (p. ex. `failure_ratio: 1.1`).

### Memcached — MemcachedConfig

```yaml
memcached:
  # username: "memcache"    # opcional: campo reservado (atualmente implementação em memória)
  # password: "secret"      # opcional: campo reservado
  {}
```

| Campo | Tipo | Descrição |
|------|------|------|
| `username` | `Option<String>` | Opcional: campo reservado |
| `password` | `Option<String>` | Opcional: campo reservado |

Atualmente é uma implementação em memória; os campos de autenticação ficam reservados.

### ClickHouse — ClickhouseConfig

```yaml
clickhouse:
  base_url: "http://host:8123"
  database: "default"
  # username: "default"   # opcional
  # password: "secret"    # opcional
  # query_timeout_secs: 30  # opcional: timeout por chamada, 0 = desativado
  # breaker: {}             # opcional: config do breaker, omitido = padrões conservadores (0.5 / 30s / aberto 10s)
  # max_concurrency: 32     # opcional: limite de concorrência (semáforo deste crate)
```

| Campo | Tipo | Valor padrão | Descrição |
|------|------|--------|------|
| `base_url` | `String` | — | Endereço da interface HTTP |
| `database` | `String` | `"default"` | Nome do banco de dados |
| `username` | `Option<String>` | `None` | Opcional: usuário HTTP Basic Auth |
| `password` | `Option<String>` | `None` | Opcional: senha HTTP Basic Auth |
| `query_timeout_secs` | `Option<u64>` | `30` | Timeout por chamada em segundos; **`0` = desativado** (igual ao Redis) |
| `breaker` | `Option<BreakerConfig>` | padrões conservadores | Limiares e janela do breaker; também sem chave geral `enabled` |
| `max_concurrency` | `Option<usize>` | `32` | Limite de concorrência; **semáforo deste próprio crate**, não um botão do reqwest (o reqwest só tem `pool_max_idle_per_host` — conexões ociosas mantidas, não um teto) |

**Duas camadas de timeout**: o `reqwest::Client` criado por `from_config` (`ecat-tls`) traz o próprio timeout de conexão de 5 s + 30 s totais; `query_timeout_secs` é o orçamento **externo** — com as duas camadas ativas, **vence a que estourar primeiro**; quando a interna estoura, o erro é `RdbmsError::Database` e **não entra** em `ecat_outbound_timeouts_total` (o contador dos timeouts externos, feature `metrics`). `new` / `with_auth` usam um `reqwest::Client::new()` puro, sem timeout interno.

### QuestDB — QuestdbConfig

```yaml
questdb:
  base_url: "http://host:9000"
  # username: "admin"     # opcional
  # password: "quest"     # opcional
```

| Campo | Tipo | Descrição |
|------|------|------|
| `base_url` | `String` | Endereço da API HTTP |
| `username` | `Option<String>` | Opcional: usuário HTTP Basic Auth |
| `password` | `Option<String>` | Opcional: senha HTTP Basic Auth |

### Elasticsearch — ElasticsearchConfig

```yaml
elasticsearch:
  base_url: "http://host:9200"
  # username: "elastic"   # opcional
  # password: "secret"    # opcional
```

| Campo | Tipo | Descrição |
|------|------|------|
| `base_url` | `String` | Endereço da API REST |
| `username` | `Option<String>` | Opcional: usuário HTTP Basic Auth |
| `password` | `Option<String>` | Opcional: senha HTTP Basic Auth |

### OpenSearch — OpenSearchConfig

```yaml
opensearch:
  base_url: "http://host:9200"
  # username: "admin"     # opcional
  # password: "secret"    # opcional
```

| Campo | Tipo | Descrição |
|------|------|------|
| `base_url` | `String` | Endereço da API REST |
| `username` | `Option<String>` | Opcional: usuário HTTP Basic Auth |
| `password` | `Option<String>` | Opcional: senha HTTP Basic Auth |

### InfluxDB — InfluxConfig

```yaml
influxdb:
  base_url: "http://host:8086"
  org: "myorg"
  bucket: "mybucket"
  token: "my-token"
```

| Campo | Tipo | Descrição |
|------|------|------|
| `base_url` | `String` | Endereço da API InfluxDB 2.x |
| `org` | `String` | Nome da organização |
| `bucket` | `String` | Nome do bucket |
| `token` | `String` | Token de autenticação |

### Neo4j — Neo4jConfig

```yaml
neo4j:
  base_url: "http://host:7474"
  username: "neo4j"
  password: "secret"
```

| Campo | Tipo | Descrição |
|------|------|------|
| `base_url` | `String` | Endereço da API REST |
| `username` | `String` | Nome de usuário |
| `password` | `String` | Senha |

### NebulaGraph — NebulaGraphConfig

```yaml
nebulagraph:
  base_url: "http://host:19669"
  space: "my_space"
  # username: "root"      # opcional
  # password: "nebula"    # opcional
```

| Campo | Tipo | Descrição |
|------|------|------|
| `base_url` | `String` | Endereço da API |
| `space` | `String` | Nome do espaço de grafo |
| `username` | `Option<String>` | Opcional: usuário HTTP Basic Auth |
| `password` | `Option<String>` | Opcional: senha HTTP Basic Auth |

### ArangoDB — ArangoConfig

```yaml
arangodb:
  base_url: "http://host:8529"
  db: "mydb"
  username: "root"
  password: "secret"
```

| Campo | Tipo | Descrição |
|------|------|------|
| `base_url` | `String` | Endereço da API |
| `db` | `String` | Nome do banco de dados |
| `username` | `String` | Nome de usuário |
| `password` | `String` | Senha |

### IoTDB — IotdbConfig

```yaml
iotdb:
  base_url: "http://host:18080"
  username: "root"
  password: "root"
```

| Campo | Tipo | Descrição |
|------|------|------|
| `base_url` | `String` | Endereço da API REST |
| `username` | `String` | Nome de usuário |
| `password` | `String` | Senha |

---

## Criação programática

### Sem autenticação

```rust
let es = ElasticsearchClient::new("http://localhost:9200");
let ch = ClickhouseClient::new("http://localhost:8123", "default");
```

### Com autenticação

```rust
let es = ElasticsearchClient::with_auth("http://es:9200", "elastic", "secret");
let ch = ClickhouseClient::with_auth("http://ch:8123", "default", "admin", "pass");
let qdb = QuestdbClient::with_auth("http://qdb:9000", "admin", "quest");
let ng = NebulaGraphClient::with_auth("http://ng:19669", "space1", "root", "nebula");
```

---

---

## Configuração de certificados TLS

Todos os backends de dados geralmente suportam autenticação TLS opcional do cliente (campo `tls`), com **duas exceções**: o `ecat-data-sqlx` não suporta o campo — configurá-lo faz a inicialização falhar (o TLS dele vai por parâmetros de URL); o campo do `ecat-data-memcached` é **silenciosamente inerte** — declarado, mas nunca lido no crate.

### Exemplo de configuração

```yaml
clickhouse:
  base_url: "https://ch.internal:8443"
  tls:
    ca_cert: "/etc/ecat/ca.pem"
    client_cert: "/etc/ecat/client.pem"
    client_key: "/etc/ecat/client-key.pem"
    # skip_verify: true  # apenas ambiente de teste
```

### Geração automática de certificados (ecat-tls)

```rust
use ecat_tls::{generate_ca, generate_server_cert, generate_client_cert};

// 1. Gerar a CA
let ca = generate_ca("MyOrg")?;
std::fs::write("ca.pem", &ca.cert_pem)?;
std::fs::write("ca-key.pem", &ca.key_pem)?;

// 2. Gerar o certificado do servidor
let srv = generate_server_cert("db.example.com")?;
std::fs::write("server.pem", &srv.cert_pem)?;
std::fs::write("server-key.pem", &srv.key_pem)?;

// 3. Gerar o certificado do cliente (mTLS)
let client = generate_client_cert("myapp")?;
std::fs::write("client.pem", &client.cert_pem)?;
std::fs::write("client-key.pem", &client.key_pem)?;
```

### Geração manual (OpenSSL)

```bash
# CA
openssl req -x509 -newkey rsa:4096 -keyout ca-key.pem -out ca.pem -days 3650 -nodes

# Certificado do servidor
openssl req -new -newkey rsa:4096 -keyout server-key.pem -out server.csr -nodes -subj "/CN=db.example.com"
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -out server.pem -days 365

# Certificado do cliente (mTLS)
openssl req -new -newkey rsa:4096 -keyout client-key.pem -out client.csr -nodes -subj "/CN=myapp"
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca-key.pem -out client.pem -days 365
```

### Descrição dos campos TLS

| Campo | Tipo | Descrição |
|------|------|------|
| `ca_cert` | `Option<String>` | Caminho do PEM do certificado CA (valida o servidor) |
| `client_cert` | `Option<String>` | Caminho do PEM do certificado de cliente (mTLS) |
| `client_key` | `Option<String>` | Caminho do PEM da chave privada do cliente (mTLS) |
| `skip_verify` | `Option<bool>` | Pular verificação de certificado (apenas teste) |

---

## Uso avançado

### Sobrescrita por variável de ambiente

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

### Combinando com o framework ecat-config

```rust
use ecat_config::{Config, FileSource};

let mut app_config = Config::new();
app_config.load(&FileSource::new("databases.yaml")).await?;

let redis_cfg: RedisConfig = serde_json::from_value(
    app_config.get::<serde_json::Value>("redis").unwrap()
)?;
let cache = RedisCache::from_config(redis_cfg).await?;
```

### Configuração sob demanda

Bancos não utilizados podem ser omitidos do YAML; marque os campos com `Option` na estrutura Rust:

```rust
#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: Option<RedisConfig>,
    clickhouse: Option<ClickhouseConfig>,
}
```

---

## Documentação relacionada

- [Relatório de auditoria r5](audit-report-2026-08-01-r5.md)
- [Tutorial de autenticação com certificados TLS](tls-certificate-tutorial.md)
- [Arquivo de configuração de exemplo](../../../config/databases.example.yaml)
