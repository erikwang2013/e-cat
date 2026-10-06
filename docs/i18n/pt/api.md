<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Referência da API do Ecat

Esta página resume a superfície de interface (API) do framework Ecat: convenções de porta, endpoints embutidos, formato de erro e interfaces de extensão. As rotas de negócio são registradas por cada serviço.

## Convenções de porta

| Protocolo | Endereço de escuta | Descrição |
|------|----------|------|
| HTTP | `0.0.0.0:8000` | Rotas axum, porta padrão dos exemplos |
| gRPC | `0.0.0.0:9000` | Servidor tonic, porta padrão dos exemplos |

## Endpoints embutidos

Os seguintes endpoints são fornecidos pelos crates do ecossistema e montados junto com o serviço:

| Endpoint | Origem | Descrição |
|------|------|------|
| `/health` | ecat-health | Verificação de liveness (retorna nome do serviço, versão, tempo de inicialização) |
| `/ready` | ecat-health | Verificação de readiness (retorna 200 quando as dependências estão prontas) |
| `/metrics` | ecat-metrics | Exposição de métricas Prometheus (`ecat_http_requests_total` / `ecat_http_request_duration_seconds`) |
| `/{service}/{method}` | Rotas do usuário | Exemplo: `/helloworld/ecat` |

> Em cenários de alta cardinalidade (ex.: caminhos contendo IDs), use `MetricsLayer::new().with_path_fn(...)` para normalizar e evitar explosão de cardinalidade de métricas.

## Fluxo de processamento de requisições

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

## Formato de erro

`ecat-errors` fornece `ErrorCode` + `Error`, com mapeamento de status HTTP em tempo de compilação:

```rust
use ecat_errors::{Error, ErrorCode};

Error::new(ErrorCode::InvalidArgument, "bad_request", "user id must be positive");
```

A resposta de erro é codificada como JSON (ou Protobuf) pelo middleware, carregando code / reason / message.

## Interfaces de extensão

| Capacidade | Crate | Interface |
|------|-------|------|
| GraphQL | ecat-graphql | Endpoint `/graphql`; suporta argumentos de campo e selections aninhadas; não suporta aliases, fragments nem múltiplos campos de nível superior |
| OpenAPI | ecat-openapi | Gera spec OpenAPI a partir das rotas |
| WebSocket | ecat-transport-ws | Transporte WS atualizado (upgrade) |
| Roteamento de versão de API | ecat-versioning | Roteamento por prefixo de versão `/v1/...` |
| Autenticação | ecat-auth | Middlewares JWT / API Key; a chave JWT deve ter ≥32 bytes, com `required_issuer`/`required_audience` encadeáveis |
| Cliente gRPC | ecat-transport-grpc | Integra descoberta de serviço e balanceamento de carga |

## Comunicação entre serviços

- `HttpClient` (ecat-client): integra descoberta de serviço e balanceamento de carga, com proteção do CircuitBreaker
- `GrpcClient` (ecat-transport-grpc): o mesmo, via protocolo gRPC
- Middlewares combinados de forma unificada com `tower::ServiceBuilder` (Recovery / Tracing / Logging / Timeout / RateLimit / Security / CircuitBreaker / Metrics / Retry / Validate / CORS)

## Interfaces de backend de dados

Todos os backends de dados (`ecat-data-*`) são abstraídos por traits unificados (`RdbmsClient` para transações, `SqlExecutor` para execução e dialeto / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`); backends do tipo REST (Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine / OpenSearch / Elasticsearch / S3) acessam as interfaces HTTP correspondentes via `base_url`. Consulte o [Tutorial de configuração de banco de dados](database-config-tutorial.md) para a configuração de conexão.

## ORM (ecat-orm)

`ecat-orm` fornece uma macro derivada de entidade, um construtor de consultas tipado, CRUD, pré-carga de
relações e migrações. Toda operação de dados recebe `&impl SqlExecutor`, portanto **a mesma API funciona
com um cliente e com uma `Transaction`**:

```rust
User::find_by_id(&db, 1).await?;              // db: SqlxClient / MssqlClient
let tx = db.transaction().await?;
User::update(&tx, &user).await?;              // tx: Transaction
User::insert(&tx, &user).await?;              // também dentro de uma transação: as duas instruções rodam nela
tx.commit().await?;
```

O SQL é gerado pela camada de dialeto: SQLite / PostgreSQL / MySQL / TiDB (`ecat-data-sqlx`) e
SQL Server (`ecat-data-mssql`) compartilham a mesma definição de entidade.

### Definição de entidade e gramática do atributo `#[entity(...)]`

`#[derive(Entity)]` gera `Entity::META` (tabela / colunas / relações / flags), `from_row` / `to_values` /
`pk_value` e uma enumeração `XxxRelation` por entidade (os nomes das variantes são o PascalCase do nome do
campo de relação).

Atributo de contêiner:

| Sintaxe | Significado |
|------|------|
| `#[entity(table = "users")]` | Nome da tabela; se omitido, o snake_case do nome da estrutura (`User` → `user`, `UserProfile` → `user_profile`) |

Campos de coluna:

| Sintaxe | Significado |
|------|------|
| `#[entity(column = "user_name")]` | Nome de coluna alternativo; por padrão, o nome do campo |
| `#[entity(pk)]` | Chave primária |
| `#[entity(auto_increment)]` | Auto-incremento (implica pk); a chave auto-incrementada fica fora da lista de colunas de inserção e é gerada pelo banco |
| `#[entity(created_at)]` / `#[entity(updated_at)]` | Carimbos de tempo automáticos: ambos são preenchidos na inserção e só `updated_at` é atualizado na alteração |
| `#[entity(soft_delete)]` | Coluna de exclusão lógica: o caminho de leitura acrescenta automaticamente um filtro `IS NULL` sobre ela |
| `#[entity(version)]` | Coluna de bloqueio otimista: as atualizações escrevem `version + 1` e comparam com o valor antigo; um conflito devolve `OrmError::OptimisticLockConflict` |

Campos de relação (**o tipo do contêiner é uma restrição rígida**; um tipo de entidade puro é recusado em
compilação — ele não consegue expressar «não encontrado»):

| Sintaxe | Tipo do campo | Significado |
|------|----------|------|
| `#[entity(has_many = "Post", foreign_key = "user_id")]` | `Vec<Post>` | Um-para-muitos: o valor de `local_key` desta tabela é comparado com a coluna `foreign_key` do destino |
| `#[entity(has_one = "Profile", foreign_key = "user_id")]` | `Option<Profile>` | Um-para-um; uma relação de valor único pega apenas a primeira linha |
| `#[entity(belongs_to = "Tag", foreign_key = "tag_code")]` | `Option<Tag>` | Muitos-para-um: o valor de `foreign_key` desta tabela é comparado com a **chave primária do destino** |

`local_key` pode ser omitido: `has_many` / `has_one` usam por padrão a chave primária desta tabela,
`belongs_to` a do destino. Campos de relação **não são colunas**. A correspondência entre tipo de campo e
tipo de coluna existe apenas nas impls de `value::ColumnValue` (a macro não guarda uma segunda cópia); um
tipo não suportado produz um erro de `T: ColumnValue` não satisfeito apontando para esse campo.

### CRUD

| Método | Observação |
|------|------|
| `Entity::insert(&db, &e) -> i64` | Insere e devolve a nova chave primária. O caminho em duas etapas do MySQL (`INSERT` e depois `SELECT LAST_INSERT_ID()`, que é **ligado à conexão**) é envolvido numa única transação por `SqlExecutor::execute_then_query` |
| `Entity::insert_many(&db, &[e]) -> u64` | Inserção em massa, devolve linhas afetadas (sem chaves de volta: `LAST_INSERT_ID()` dá a primeira linha, o SQLite a última — pouco confiável entre backends) |
| `Entity::save(&db, &e)` | Insere se a chave primária estiver «não definida» (auto-incrementada e igual a 0), senão atualiza; devolve `()` |
| `Entity::update(&db, &e) -> u64` | Atualização completa pela chave primária. Zero linhas afetadas é erro: `OrmError::NotFound` sem `version`, `OrmError::OptimisticLockConflict` com ela (sem uma consulta extra não se distinguem) |
| `Entity::update_many(&db, &[e]) -> u64` | Atualiza linha a linha e soma as linhas; uma chamada em massa **não diz qual linha** conflitou (resultado menor que a entrada significa que alguém foi barrado pelo portão de versão) |
| `Entity::upsert(&db, &e) -> u64` | Upsert pela chave primária (`ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE` conforme o dialeto) |
| `Entity::find_by_id(&db, pk) -> Option<Self>` | Busca uma linha pela chave primária; devolve `Ok(None)` se não existir |
| `Entity::find_all(&db) -> Vec<Self>` | Todas as linhas sem filtro (**varredura completa em tabelas grandes**; para paginar use `paginate`) |
| `Entity::delete_by_id(&db, pk) -> u64` | Com `soft_delete` declarado envia um `UPDATE` que grava o instante da exclusão; a linha continua na tabela, e excluir de novo devolve `NotFound` sem atualizar o instante |
| `Entity::hard_delete_by_id(&db, pk) -> u64` | Ignora a exclusão lógica e envia mesmo `DELETE` |

### Construtor de consultas

Começa-se com `User::query()`, que devolve `Query<User, Unfiltered>`; acrescentar um filtro vira
`Query<User, Filtered>` (estado de tipo) — «excluir sem WHERE» não compila.

```rust
use ecat_orm::query::{Op, Order};

let users = User::query()
    .filter("name", Op::Like, "alice%")?        // Eq / Ne / Lt / Le / Gt / Ge / Like
    .filter("email", Op::NotNull, serde_json::json!(null))?
    .filter("id", Op::In, serde_json::json!([1, 2, 3]))?  // In / NotIn recebem um array
    .order_by("id", Order::Desc)?
    .limit(10)
    .offset(20)
    .fetch(&db)
    .await?;
```

- Os nomes de coluna são validados contra a lista branca `EntityMeta.columns` (`OrmError::UnknownColumn`);
  colunas de tabelas juntadas não estão nela — para elas use `filter_raw("...")`, sabendo que ele **não faz
  validação alguma: a entrada precisa ser confiável**.
- `with_trashed()` desliga o portão de exclusão lógica (as linhas excluídas logicamente também voltam).
- `join(JoinType::Left, "posts", "posts.user_id = users.id")` aceita `Inner` / `Left`, e serve para filtrar
  por colunas da tabela juntada dentro de `filter_raw`. A **lista de colunas não leva prefixo de tabela**,
  então se a tabela juntada compartilhar um nome de coluna com o sujeito, o banco real informa
  `ambiguous column name` — junte tabelas cujos nomes de coluna difiram do sujeito (verificado no SQLite).
- `delete_where(&db)` / `hard_delete_where(&db)` excluem com as mesmas condições (entidades com exclusão
  lógica passam por `UPDATE`).
- `fetch(&db)` é o ponto de entrada de execução; `find_by_id` / `find_all` / `paginate` reutilizam o mesmo
  caminho de geração de SQL por trás dele.

### Fatiamento e paginação

- **Escritas em massa são fatiadas automaticamente**: `insert_many` / `update_many` se dividem pelo limite
  de parâmetros por instrução do dialeto (por exemplo, 2100 no SQL Server) e somam as linhas. Para fatiar
  manualmente use o mesmo limite:
  `ecat_orm::dialect::lookup(dialect).max_params_per_stmt()`.
- **Paginação**: `paginate(&db, page, per_page)` emite 2 consultas (COUNT + busca da página) e as páginas
  **começam em 1**; o COUNT reutiliza o mesmo WHERE / JOIN mas remove ORDER BY / LIMIT / OFFSET. Devolve
  `Page { items, total, page, per_page }`, e `total_pages()` / `has_next()` saem de `total`.
- Para paginação profunda em tabelas grandes use `paginate_without_count(&db, page, per_page)` (1 consulta):
  `total` é `None`, e `total_pages()` / `has_next()` também não conseguem responder — o que não pode ser
  calculado é informado como tal, em vez de adivinhado.

### Pré-carga de relações

```rust
let users = User::query()
    .with(&[UserRelation::Posts, UserRelation::Profile])
    .fetch(&db)
    .await?;
```

- Cada relação emite **uma** consulta `IN` (fatiada quando as chaves passam do limite de parâmetros por
  instrução), e a consulta dos sujeitos não faz join — **sem N+1**: buscar 3 utilizadores e os seus posts
  são 2 SELECT (1 do sujeito + 1 `IN`), não 4.
- Sem `with()` os campos de relação ficam vazios (`Vec::new()` / `None`), nunca com valores obsoletos de um
  carregamento anterior; `set_relation` é chamado para cada sujeito (mesmo com resultado vazio), justamente
  para limpar resíduos.
- Relações de valor único (`has_one` / `belongs_to`) pegam apenas a primeira linha.

### Migrações

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Migrator, create_table};

Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .add("002_posts", create_table::<Post>())
    .status().await?;      // MigrationStatus { applied, pending, unknown }
    .run().await?;         // aplica as pendentes e regista cada uma na tabela de versões; reexecutar é idempotente
    .down(1).await?;       // executa o SQL inverso da 001 e remove essa linha da tabela de versões
```

- O prefixo numérico do nome da migração é a sua versão (`"001_users"` → 1); um prefixo não numérico gera
  `OrmError::InvalidMigrationName` — nunca é adivinhado como 0.
- `create_table::<E>()` / `drop_table::<E>()` geram o DDL a partir de `EntityMeta`, e **o dialeto é
  resolvido no `run()` a partir do `dialect()` da conexão**, o que desacopla a lista de migrações da string
  de conexão. Para SQL próprio (ALTER, preenchimento de dados) use
  `ecat_orm::migrate::MigrationBuilder::new(|d| ...)`.
- Chamar `down` numa migração sem SQL inverso gera `OrmError::MigrationIrreversible` — «DROP e recriar»
  perde dados, e isso não é adivinhado em nome de quem chama.
- A tabela de versões é criada automaticamente pelo `Migrator` (o MSSQL não tem
  `CREATE TABLE IF NOT EXISTS`, por isso a existência é verificada antes).
