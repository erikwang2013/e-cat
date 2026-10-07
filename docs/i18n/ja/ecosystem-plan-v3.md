# e-cat エコシステム計画 v3 — 最終評価

> **更新（2026-08-07, v2.3.3）**: 残ギャップ #1「transport への mTLS 接続」が完了 — `HttpServer::tls` / `GrpcServer::tls` が tokio-rustls / tonic rustls ベースで実際に有効（CA 検証とクライアント証明書強制に対応）；ギャップ #2（Redis レートリミット）、#3（GitLab CI）は v2.3.0 で完了済み。計画に挙げられたギャップはこれで全て実装済み。

> **更新（2026-10-07, v5.0.0）**: v4.0 計画はすべて完了しました —— `ecat-orm` / `ecat-orm-derive`、`ecat-data-mssql`、プール強化（`CircuitBreakerExecutor` / `RdbmsRouting`）、可観測性の 3 feature（`metrics` / `health` / `tracing`）はいずれも実装済みです。

**バージョン:** 2.4.2  
**日付:** 2026-08-01  
**crate 総数:** 55 · 全計画完了

---

## 現在のカバレッジ

| 領域 | 実装済み | カバレッジ |
|------|--------|--------|
| トランスポート層 | HTTP (axum), gRPC (tonic), WebSocket | 100% |
| エンコーディング | JSON, Protobuf | 100% |
| ミドルウェア | Recovery, Tracing, Logging, Timeout, RateLimit, Security, CircuitBreaker, Auth×3 | 100% |
| 設定 | env, file (JSON/YAML), Consul KV, 暗号化 (XOR) | 100% |
| レジストリ | memory, Consul, etcd | 100% |
| セキュリティ | 攻撃検知, JWT, API Key, OAuth2, TLS クライアント証明書, mTLS | 95% |
| 通信 | TLS クライアント証明書 — 全データバックエンド対応 | 95% |
| サービス通信 | HTTP Client, gRPC Client, Resolver, LoadBalancer | 95% |
| データ | RDBMS (sqlx), Redis, OpenSearch, Elasticsearch, ClickHouse, Memcached, Neo4j, NebulaGraph, ArangoDB, InfluxDB, IoTDB, QuestDB — すべて Config ファイル設定に対応 | 95% |
| メッセージ | MessageQueue trait, InMemory, Kafka, EventBus | 100% |
| 可観測性 | tracing, Prometheus, Health, 分散トレーシング | 100% |
| DevOps | CLI, Dockerfile, K8s, Helm, GitHub Actions, Bench, Testing | 95% |
| API ツール | OpenAPI, Versioning, GraphQL | 100% |

---

## 残りのギャップ

### やる価値がある（3 項目）

| # | ギャップ | 価値 | 作業量 |
|---|------|------|--------|
| 1 | **transport への mTLS 接続** | TlsConfig はあるが HttpServer/GrpcServer に未接続 | 小 |
| 2 | **Redis レートリミットバックエンド** | RateLimitLayer はメモリのみ、複数インスタンスで共有が必要 | 小 |
| 3 | **GitLab CI テンプレート** | GitHub Actions はある | 小 |

### やる必要がない（2 項目）

| # | ギャップ | 理由 |
|---|------|------|
| 4 | 設定の AES-GCM | 現状の XOR で十分 |
| 5 | サービスメッシュ/API ゲートウェイ | コミュニティに委ねる（Linkerd/Kong/K8s） |

---

## 判定

**e-cat は本番利用可能な成熟度に達しています。** 47 個の crate がマイクロサービスの全スタックをカバー：トランスポート → ミドルウェア → サービスディスカバリ → 設定 → セキュリティ → データ → メッセージ → 可観測性 → DevOps → API ツール。残りの 3 ギャップは小規模作業の最適化であり、構造的な欠落はありません。

## データバックエンドのカバレッジ（16 個）

| カテゴリ | データベース | Crate | ドライバ方式 |
|------|--------|-------|----------|
| RDBMS | SQLite/PostgreSQL/MySQL/TiDB | `ecat-data-sqlx` | sqlx（公式非同期ドライバ） |
| RDBMS | SQL Server | `ecat-data-mssql` | tiberius-ng + deadpool（TDS ドライバ + コネクションプール） |
| キャッシュ | Redis | `ecat-data-redis` | redis-rs（公式ドライバ） |
| キャッシュ | Memcached | `ecat-data-memcached` | ⚠️ メモリ実装（非本番用） |
| ドキュメント | MongoDB | `ecat-data-mongodb` | mongodb（公式ドライバ） |
| オブジェクトストレージ | S3 / MinIO | `ecat-data-s3` | HTTP/REST（reqwest+rustls、自前 SigV4） |
| OLAP | ClickHouse | `ecat-data-clickhouse` | HTTP/REST（reqwest） |
| 検索 | OpenSearch | `ecat-data-opensearch` | HTTP/REST（reqwest） |
| 検索 | Elasticsearch | `ecat-data-elasticsearch` | HTTP/REST（reqwest） |
| グラフ | Neo4j | `ecat-data-neo4j` | HTTP/REST（reqwest） |
| グラフ | NebulaGraph | `ecat-data-nebulagraph` | HTTP/REST（reqwest） |
| グラフ | ArangoDB | `ecat-data-arangodb` | HTTP/REST（reqwest） |
| 時系列 | InfluxDB | `ecat-data-influxdb` | HTTP/REST（reqwest） |
| 時系列 | Apache IoTDB | `ecat-data-iotdb` | HTTP/REST（reqwest） |
| 時系列 | QuestDB | `ecat-data-questdb` | HTTP/REST（reqwest） |
| 時系列 | TDengine | `ecat-data-tdengine` | HTTP/REST（reqwest） |

---

## v4.0 計画（2026-10-05）— 完全な ORM と SQL Server

> 状態：**完了**（v5.0.0）。ORM、SQL Server、プール強化、可観測性はすべて実装済みです（下表参照）。
> 完全な設計は [`docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md`](../../../docs/superpowers/specs/2026-10-05-orm-and-mssql-design.md) を参照。

2 つの構造的ギャップを埋めます：

1. **完全な ORM**：現在のデータ層は手書き SQL の RDBMS クライアントのみ（`Row` = 列名 + JSON 値）で、
   エンティティマッピング・関連・マイグレーションはありません。v4.0 で `ecat-orm`
   （エンティティマクロ / CRUD / クエリビルダー / 関連の一括読み込み / 結合クエリ / バルク /
   ページング / マイグレーション）+ `ecat-orm-derive` を追加します。統一 `SqlExecutor` trait 上に
   構築され、**すべての RDBMS バックエンドを自然にカバーします**。
2. **SQL Server バックエンド**：sqlx 本体に MSSQL ドライバーがありません（0.7 以前に削除、書き直しは
   未公開）。v4.0 で `ecat-data-mssql`（`tiberius-ng` 0.13 + `deadpool` 0.13）を追加しました——
   データバックエンドは 15 個から **16 個**になりました（`ecat-data-mssql`）。

関連する基盤変更：

| 変更 | 説明 | 状態 |
|---|---|---|
| `ecat-data-sqlx` が `AnyPool` をやめネイティブプールへ | 時間型の制限を修正（CAST 回避が不要に）、ドライバー導入時の panic 面を除去、statement cache を有効化 | ✅ 完了（バッチ 1、`PgPool`/`MySqlPool`/`SqlitePool` の 3 系統ネイティブプール） |
| `ecat-data` が `SqlExecutor` supertrait を分割 | トランザクション内で SQL を実行可能（現在の `Transaction` は commit/rollback のみ）。ORM と読み書き分離の基盤 | ✅ 完了（バッチ 1） |
| コネクションプール強化 | クエリタイムアウト、`warm_up()` ウォームアップ、スマート recycle、サーキットブレーカー（`ecat-circuit-breaker` を再利用）、読み書き分離 `RdbmsRouting` | ✅ 完了（バッチ 4 —— クエリタイムアウトと `warm_up()` はバッチ 1、`CircuitBreakerExecutor` と `RdbmsRouting` はバッチ 4） |
| 可観測性 | プールメトリクスを `ecat-metrics` へ、プールのヘルスチェックを `ecat-health` へ、スロークエリを `ecat-tracing` へ（すべて opt-in feature） | ✅ 完了（バッチ 4、`metrics` / `health` / `tracing` の 3 feature は既定で無効） |

**破壊的変更**：trait 分割 + `SqlxClient::from_pool` のシグネチャ + `AnyPool` の削除——3 点はいずれも
ブランチ `feat/orm-mssql` に反映済み（バッチ 1、未リリース）；リリース時に workspace バージョン
3.0.3 → **4.0.0**。
