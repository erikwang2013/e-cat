# Plano de ecossistema do e-cat v3 — avaliação final

> **Atualização (2026-08-07, v2.3.3)**: a lacuna restante #1 "mTLS no transport" foi concluída — `HttpServer::tls` / `GrpcServer::tls` funcionam de verdade com base em tokio-rustls / tonic rustls (com suporte a validação de CA e certificado de cliente obrigatório); as lacunas #2 (rate limit Redis) e #3 (CI GitLab) já haviam sido concluídas com o v2.3.0. Todas as lacunas listadas no plano foram, portanto, implementadas.

> **Atualização (2026-10-07, v5.0.0)**: o plano v4.0 está totalmente entregue — `ecat-orm` / `ecat-orm-derive`, `ecat-data-mssql`, as melhorias de pool (`CircuitBreakerExecutor` / `RdbmsRouting`) e as três features de observabilidade (`metrics` / `health` / `tracing`) estão implementadas.

**Versão:** 2.4.2  
**Data:** 2026-08-01  
**Total de crates:** 55 · todo o planejamento concluído

---

## Cobertura atual

| Domínio | Implementado | Cobertura |
|------|--------|--------|
| Camada de transporte | HTTP (axum), gRPC (tonic), WebSocket | 100% |
| Encoding | JSON, Protobuf | 100% |
| Middleware | Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, Auth×3 | 100% |
| Configuração | env, file (JSON/YAML), Consul KV, criptografia (XOR) | 100% |
| Registry | memory, Consul, etcd | 100% |
| Segurança | Detecção de ataques, JWT, API Key, OAuth2, certificado de cliente TLS, mTLS | 95% |
| Comunicação | Certificado de cliente TLS — suportado por todos os backends de dados | 95% |
| Comunicação de serviços | HTTP Client, gRPC Client, Resolver, LoadBalancer | 95% |
| Dados | RDBMS (sqlx), Redis, OpenSearch, Elasticsearch, ClickHouse, Memcached, Neo4j, NebulaGraph, ArangoDB, InfluxDB, IoTDB, QuestDB — todos suportam configuração por arquivo Config | 95% |
| Mensagens | trait MessageQueue, InMemory, Kafka, EventBus | 100% |
| Observabilidade | tracing, Prometheus, Health, rastreamento distribuído | 100% |
| DevOps | CLI, Dockerfile, K8s, Helm, GitHub Actions, Bench, Testing | 95% |
| Ferramentas de API | OpenAPI, Versioning, GraphQL | 100% |

---

## Lacunas restantes

### Que valem a pena (3 itens)

| # | Lacuna | Valor | Esforço |
|---|------|------|--------|
| 1 | **mTLS no transport** | TlsConfig já existe, ainda não conectado ao HttpServer/GrpcServer | Pequeno |
| 2 | **Backend de rate limit Redis** | RateLimitLayer apenas em memória, multi-instância precisa de compartilhamento | Pequeno |
| 3 | **Template de CI GitLab** | GitHub Actions já existe | Pequeno |

### Desnecessárias (2 itens)

| # | Lacuna | Motivo |
|---|------|------|
| 4 | Config AES-GCM | O XOR atual é suficiente |
| 5 | Service mesh / API Gateway | Deixar para a comunidade (Linkerd/Kong/K8s) |

---

## Veredito

**O e-cat alcançou maturidade pronta para produção.** 47 crates cobrem toda a stack de microsserviços: transporte → middleware → descoberta de serviço → configuração → segurança → dados → mensagens → observabilidade → DevOps → ferramentas de API. As 3 lacunas restantes são otimizações de baixo esforço, sem lacunas estruturais.

## Cobertura de backends de dados (16)

| Categoria | Banco de dados | Crate | Forma de driver |
|------|--------|-------|----------|
| RDBMS | SQLite/PostgreSQL/MySQL/TiDB | `ecat-data-sqlx` | sqlx (driver assíncrono oficial) |
| RDBMS | SQL Server | `ecat-data-mssql` | tiberius-ng + deadpool (driver TDS + pool de conexões) |
| Cache | Redis | `ecat-data-redis` | redis-rs (driver oficial) |
| Cache | Memcached | `ecat-data-memcached` | ⚠️ Implementação em memória (não para produção) |
| Documentos | MongoDB | `ecat-data-mongodb` | mongodb (driver oficial) |
| Armazenamento de objetos | S3 / MinIO | `ecat-data-s3` | HTTP/REST (reqwest+rustls, SigV4 próprio) |
| OLAP | ClickHouse | `ecat-data-clickhouse` | HTTP/REST (reqwest) |
| Busca | OpenSearch | `ecat-data-opensearch` | HTTP/REST (reqwest) |
| Busca | Elasticsearch | `ecat-data-elasticsearch` | HTTP/REST (reqwest) |
| Grafo | Neo4j | `ecat-data-neo4j` | HTTP/REST (reqwest) |
| Grafo | NebulaGraph | `ecat-data-nebulagraph` | HTTP/REST (reqwest) |
| Grafo | ArangoDB | `ecat-data-arangodb` | HTTP/REST (reqwest) |
| Séries temporais | InfluxDB | `ecat-data-influxdb` | HTTP/REST (reqwest) |
| Séries temporais | Apache IoTDB | `ecat-data-iotdb` | HTTP/REST (reqwest) |
| Séries temporais | QuestDB | `ecat-data-questdb` | HTTP/REST (reqwest) |
| Séries temporais | TDengine | `ecat-data-tdengine` | HTTP/REST (reqwest) |

---

## Plano v4.0 (2026-10-05) — ORM completo e SQL Server

> Status: **concluído** (v5.0.0). O ORM, o SQL Server, as melhorias de pool e a observabilidade estão prontos — veja a tabela abaixo.
> Design completo: [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../../../docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md).

Restam duas lacunas estruturais a fechar:

1. **ORM completo**: a camada de dados hoje só oferece clientes RDBMS com SQL escrito à mão
   (`Row` = nomes de coluna + valores JSON) — sem mapeamento de entidades, associações ou migrações.
   O v4.0 adiciona `ecat-orm` (macros de entidade / CRUD / construtor de consultas / pré-carregamento
   de associações / joins / lote / paginação / migrações) + `ecat-orm-derive`. Construído sobre o
   trait unificado `SqlExecutor`, **cobre naturalmente todos os backends RDBMS**.
2. **Backend SQL Server**: o sqlx principal não tem driver MSSQL (removido antes da 0.7, reescrita
   não publicada); o v4.0 adiciona `ecat-data-mssql` (`tiberius-ng` 0.13 + `deadpool` 0.13) —
   backends de dados passaram de 15 para **16** (`ecat-data-mssql`).

Mudanças de fundação associadas:

| Mudança | Descrição | Status |
|---|---|---|
| `ecat-data-sqlx` abandona o `AnyPool` por pools nativos | Corrige limitações de tipos temporais (não é mais necessário contornar com CAST), remove a superfície de panic na instalação do driver, habilita o statement cache | ✅ Concluído (lote 1, pools nativos `PgPool`/`MySqlPool`/`SqlitePool`) |
| `ecat-data` extrai o supertrait `SqlExecutor` | SQL executável dentro de transações (hoje `Transaction` só faz commit/rollback); base para o ORM e a separação leitura/escrita | ✅ Concluído (lote 1) |
| Melhorias do pool de conexões | Timeout de consulta, aquecimento `warm_up()`, recycle inteligente, circuit breaker (reutiliza `ecat-circuit-breaker`), separação leitura/escrita `RdbmsRouting` | ✅ Concluído (lote 4 — timeout de consulta e `warm_up()` no lote 1; `CircuitBreakerExecutor` e `RdbmsRouting` no lote 4) |
| Observabilidade | Métricas do pool para `ecat-metrics`, sondas de saúde do pool para `ecat-health`, consultas lentas para `ecat-tracing` (todas com feature opt-in) | ✅ Concluído (lote 4; as features `metrics` / `health` / `tracing` vêm desativadas por padrão) |

**Mudanças incompatíveis**: divisão do trait + assinatura de `SqlxClient::from_pool` + remoção do
`AnyPool` — as três já aplicadas na branch `feat/orm-mssql` (lote 1, ainda não publicado);
no lançamento, versão do workspace 3.0.3 → **4.0.0**.
