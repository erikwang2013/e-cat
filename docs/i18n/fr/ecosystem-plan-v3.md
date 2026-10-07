# Plan d'écosystème e-cat v3 — évaluation finale

> **Mise à jour (2026-08-07, v2.3.3)** : la lacune restante #1 « Intégration mTLS au transport » est terminée — `HttpServer::tls` / `GrpcServer::tls` fonctionnent réellement via tokio-rustls / tonic rustls (prise en charge de la validation CA et du certificat client obligatoire) ; les lacunes #2 (limitation Redis) et #3 (GitLab CI) avaient déjà été terminées avec v2.3.0. Toutes les lacunes du plan sont désormais réalisées.

> **Mise à jour (2026-10-07, v5.0.0)** : le plan v4.0 est entièrement livré — `ecat-orm` / `ecat-orm-derive`, `ecat-data-mssql`, les améliorations de pool (`CircuitBreakerExecutor` / `RdbmsRouting`) et les trois features d'observabilité (`metrics` / `health` / `tracing`) sont implémentés.

**Version :** 2.4.2  
**Date :** 2026-08-01  
**Nombre total de crates :** 56 · tout le plan est terminé

---

## Couverture actuelle

| Domaine | Implémenté | Taux de couverture |
|------|--------|--------|
| Couche transport | HTTP (axum), gRPC (tonic), WebSocket | 100 % |
| Encodage | JSON, Protobuf | 100 % |
| Middleware | Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, Auth×3 | 100 % |
| Configuration | env, file (JSON/YAML), Consul KV, chiffrement (XOR) | 100 % |
| Registre | memory, Consul, etcd | 100 % |
| Sécurité | Détection d'attaques, JWT, API Key, OAuth2, certificats client TLS, mTLS | 95 % |
| Communication | Certificats client TLS — pris en charge par tous les backends de données | 95 % |
| Communication de services | Client HTTP, client gRPC, Resolver, LoadBalancer | 95 % |
| Données | RDBMS (sqlx), Redis, OpenSearch, Elasticsearch, ClickHouse, Memcached, Neo4j, NebulaGraph, ArangoDB, InfluxDB, IoTDB, QuestDB — tous prennent en charge la configuration par fichier Config | 95 % |
| Messages | trait MessageQueue, InMemory, Kafka, EventBus | 100 % |
| Observabilité | tracing, Prometheus, Health, traçage distribué | 100 % |
| DevOps | CLI, Dockerfile, K8s, Helm, GitHub Actions, Bench, Testing | 95 % |
| Outils API | OpenAPI, Versioning, GraphQL | 100 % |

---

## Lacunes restantes

### Qui valent la peine (3 éléments)

| # | Lacune | Valeur | Charge de travail |
|---|------|------|--------|
| 1 | **Intégration mTLS au transport** | TlsConfig existe déjà, pas encore branché sur HttpServer/GrpcServer | Faible |
| 2 | **Backend de limitation Redis** | RateLimitLayer est en mémoire uniquement, un partage est nécessaire pour plusieurs instances | Faible |
| 3 | **Modèles GitLab CI** | GitHub Actions existe déjà | Faible |

### À ne pas faire (2 éléments)

| # | Lacune | Raison |
|---|------|------|
| 4 | Chiffrement AES-GCM de la configuration | Le XOR actuel suffit |
| 5 | Service mesh / passerelle API | Confié à la communauté (Linkerd/Kong/K8s) |

---

## Verdict

**e-cat a atteint une maturité prête pour la production.** 56 crates couvrent toute la pile des microservices : transport → middleware → découverte de services → configuration → sécurité → données → messages → observabilité → DevOps → outils API. Les 3 lacunes restantes sont des optimisations à faible charge de travail, sans déficit structurel.

## Couverture des backends de données (16)

| Catégorie | Base de données | Crate | Mode de pilotage |
|------|--------|-------|----------|
| RDBMS | SQLite/PostgreSQL/MySQL/TiDB | `ecat-data-sqlx` | sqlx (pilote asynchrone officiel) |
| RDBMS | SQL Server | `ecat-data-mssql` | tiberius-ng + deadpool (pilote TDS + pool de connexions) |
| Cache | Redis | `ecat-data-redis` | redis-rs (pilote officiel) |
| Cache | Memcached | `ecat-data-memcached` | ⚠️ Implémentation en mémoire (non destinée à la production) |
| Documents | MongoDB | `ecat-data-mongodb` | mongodb (pilote officiel) |
| Stockage d'objets | S3 / MinIO | `ecat-data-s3` | HTTP/REST (reqwest+rustls, SigV4 auto-implémenté) |
| OLAP | ClickHouse | `ecat-data-clickhouse` | HTTP/REST (reqwest) |
| Recherche | OpenSearch | `ecat-data-opensearch` | HTTP/REST (reqwest) |
| Recherche | Elasticsearch | `ecat-data-elasticsearch` | HTTP/REST (reqwest) |
| Graphe | Neo4j | `ecat-data-neo4j` | HTTP/REST (reqwest) |
| Graphe | NebulaGraph | `ecat-data-nebulagraph` | HTTP/REST (reqwest) |
| Graphe | ArangoDB | `ecat-data-arangodb` | HTTP/REST (reqwest) |
| Séries temporelles | InfluxDB | `ecat-data-influxdb` | HTTP/REST (reqwest) |
| Séries temporelles | Apache IoTDB | `ecat-data-iotdb` | HTTP/REST (reqwest) |
| Séries temporelles | QuestDB | `ecat-data-questdb` | HTTP/REST (reqwest) |
| Séries temporelles | TDengine | `ecat-data-tdengine` | HTTP/REST (reqwest) |

---

## Plan v4.0 (2026-10-05) — ORM complet et SQL Server

> Statut : **terminé** (v5.0.0). L'ORM, SQL Server, les améliorations de pool et l'observabilité sont en place — voir le tableau ci-dessous.
> Conception complète : [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../../../docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md).

Deux lacunes structurelles restent à combler :

1. **ORM complet** : la couche de données ne propose aujourd'hui que des clients RDBMS en SQL écrit à
   la main (`Row` = noms de colonnes + valeurs JSON) — sans mapping d'entités, associations ni
   migrations. v4.0 ajoute `ecat-orm` (macros d'entités / CRUD / constructeur de requêtes /
   préchargement des associations / jointures / traitement par lots / pagination / migrations)
   + `ecat-orm-derive`. Bâti sur le trait unifié `SqlExecutor`, il **couvre naturellement tous les
   backends RDBMS**.
2. **Backend SQL Server** : sqlx en amont n'a pas de pilote MSSQL (retiré avant 0.7, réécriture non
   publiée) ; v4.0 ajoute `ecat-data-mssql` (`tiberius-ng` 0.13 + `deadpool` 0.13) —
   les backends de données passent de 15 à **16** (`ecat-data-mssql`).

Modifications de fondation associées :

| Modification | Description | Statut |
|---|---|---|
| `ecat-data-sqlx` abandonne `AnyPool` pour des pools natifs | Corrige les limitations de types temporels (plus besoin de contournement par CAST), supprime la surface de panic à l'installation du pilote, active le statement cache | ✅ Terminé (lot 1, pools natifs `PgPool`/`MySqlPool`/`SqlitePool`) |
| `ecat-data` extrait le supertrait `SqlExecutor` | SQL exécutable dans les transactions (`Transaction` ne fait actuellement que commit/rollback) ; base pour l'ORM et la séparation lecture/écriture | ✅ Terminé (lot 1) |
| Améliorations du pool de connexions | Délai d'expiration des requêtes, préchauffage `warm_up()`, recycle intelligent, disjoncteur (réutilise `ecat-circuit-breaker`), séparation lecture/écriture `RdbmsRouting` | ✅ Terminé (lot 4 — délai d'expiration et `warm_up()` au lot 1 ; `CircuitBreakerExecutor` et `RdbmsRouting` au lot 4) |
| Observabilité | Métriques du pool vers `ecat-metrics`, sondes de santé du pool vers `ecat-health`, requêtes lentes vers `ecat-tracing` (tous en feature opt-in) | ✅ Terminé (lot 4 ; les features `metrics` / `health` / `tracing` sont désactivées par défaut) |

**Changements incompatibles** : division du trait + signature `SqlxClient::from_pool` + suppression
d'`AnyPool` — les trois sont publiés avec **v4.0.0**. Ce lot (améliorations du pool et observabilité)
sort en **v5.0.0**, la nouvelle variante `RdbmsError::NoAvailableReplica` étant incompatible.
