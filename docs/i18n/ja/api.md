<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Ecat API リファレンス

本ページは Ecat フレームワークのインターフェース（API）面をまとめたものです：ポート規約、組み込みエンドポイント、エラーフォーマット、拡張インターフェース。ビジネスルートは各サービスが自ら登録します。

## ポート規約

| プロトコル | 待ち受けアドレス | 説明 |
|------|----------|------|
| HTTP | `0.0.0.0:8000` | axum ルーティング、デフォルトのサンプルポート |
| gRPC | `0.0.0.0:9000` | tonic Server、デフォルトのサンプルポート |

## 組み込みエンドポイント

以下のエンドポイントはエコシステム crate が提供し、サービスとともにマウントされます：

| エンドポイント | 提供元 | 説明 |
|------|------|------|
| `/health` | ecat-health | 生存チェック（サービス名、バージョン、起動時間を返す） |
| `/ready` | ecat-health | 準備完了チェック（依存関係が準備完了後に 200 を返す） |
| `/metrics` | ecat-metrics | Prometheus メトリクス公開（`ecat_http_requests_total` / `ecat_http_request_duration_seconds`） |
| `/{service}/{method}` | ユーザールート | 例：`/helloworld/ecat` |

> メトリクスエンドポイントはパスに ID 等の高カーディナリティがある場合、`MetricsLayer::new().with_path_fn(...)` で正規化し、メトリクスのカーディナリティ爆発を防いでください。

## リクエスト処理フロー

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

## エラーフォーマット

`ecat-errors` は `ErrorCode` + `Error` を提供し、コンパイル時に HTTP ステータスコードへマッピングします：

```rust
use ecat_errors::{Error, ErrorCode};

Error::new(ErrorCode::InvalidArgument, "bad_request", "user id must be positive");
```

エラーレスポンスは middleware によって JSON（または Protobuf）にエンコードされ、code / reason / message を保持します。

## 拡張インターフェース

| 能力 | Crate | インターフェース |
|------|-------|------|
| GraphQL | ecat-graphql | `/graphql` エンドポイント；フィールドパラメータとネスト selection をサポート、エイリアス・fragment・複数トップレベルフィールドは未対応 |
| OpenAPI | ecat-openapi | ルートから OpenAPI spec を生成 |
| WebSocket | ecat-transport-ws | アップグレードされた WS トランスポート |
| API バージョンルーティング | ecat-versioning | `/v1/...` プレフィックスのバージョンルーティング |
| 認証 | ecat-auth | JWT / API Key ミドルウェア；JWT キーは ≥32 バイト必須、チェーンで `required_issuer`/`required_audience` を強制可能 |
| gRPC クライアント | ecat-transport-grpc | サービスディスカバリとロードバランシングを統合 |

## サービス間通信

- `HttpClient`（ecat-client）：サービスディスカバリとロードバランシングを統合、CircuitBreaker によるサーキットブレーカー保護
- `GrpcClient`（ecat-transport-grpc）：同上、gRPC プロトコル
- ミドルウェアは統一して `tower::ServiceBuilder` で組み合わせます（Recovery / Tracing / Logging / Timeout / RateLimit / Security / CircuitBreaker / Metrics / Retry / Validate / CORS）

## データバックエンドインターフェース

すべてのデータバックエンド（`ecat-data-*`）は統一 trait（`RdbmsClient` はトランザクション、`SqlExecutor` は実行とダイアレクト / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`）で抽象化されています；REST 系バックエンド（Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine / OpenSearch / Elasticsearch / S3）は `base_url` ベースで対応する HTTP インターフェースにアクセスします。接続設定は [データベース設定チュートリアル](database-config-tutorial.md) を参照してください。

## ORM（ecat-orm）

`ecat-orm` はエンティティ派生マクロ、型安全なクエリビルダー、CRUD、関連のプリロード、マイグ
レーションを提供します。データ操作はすべて `&impl SqlExecutor` を取るため、**クライアントでも
`Transaction` でも同じ API** が使えます：

```rust
User::find_by_id(&db, 1).await?;              // db: SqlxClient / MssqlClient
let tx = db.transaction().await?;
User::update(&tx, &user).await?;              // tx: Transaction
User::insert(&tx, &user).await?;              // トランザクション内でも挿入可：2 文はそのトランザクション内で実行される
tx.commit().await?;
```

SQL は方言レイヤーが生成します。SQLite / PostgreSQL / MySQL / TiDB（`ecat-data-sqlx`）と
SQL Server（`ecat-data-mssql`）で同じエンティティ定義を共有できます。

### エンティティ定義と `#[entity(...)]` 属性文法

`#[derive(Entity)]` は `Entity::META`（テーブル名 / 列 / 関連 / フラグ）、`from_row` / `to_values` /
`pk_value`、およびエンティティごとに 1 つの `XxxRelation` 列挙を生成します（変体名は関連フィールド名の
PascalCase）。

コンテナ属性：

| 記法 | 意味 |
|------|------|
| `#[entity(table = "users")]` | テーブル名。省略時は構造体名の snake_case（`User` → `user`、`UserProfile` → `user_profile`） |

列フィールド：

| 記法 | 意味 |
|------|------|
| `#[entity(column = "user_name")]` | 列名の上書き。既定はフィールド名 |
| `#[entity(pk)]` | 主キー |
| `#[entity(auto_increment)]` | 自動採番（pk を含意）。自動採番主キーは挿入列に入らず、データベースが生成します |
| `#[entity(created_at)]` / `#[entity(updated_at)]` | 自動タイムスタンプ：挿入時は両方を設定、更新時は `updated_at` のみ更新 |
| `#[entity(soft_delete)]` | ソフト削除列：読み取り経路が自動的にその列の `IS NULL` ゲートを付けます |
| `#[entity(version)]` | 楽観ロック列：更新は `version + 1` を書き、旧値を照合します。衝突は `OrmError::OptimisticLockConflict` |

関連フィールド（**コンテナ型はハード制約**。裸のエンティティ型はコンパイル時に拒否されます —
「見つからなかった」を表現できないため）：

| 記法 | フィールド型 | 意味 |
|------|----------|------|
| `#[entity(has_many = "Post", foreign_key = "user_id")]` | `Vec<Post>` | 1 対多：自テーブルの `local_key` の値を相手の `foreign_key` 列で照合 |
| `#[entity(has_one = "Profile", foreign_key = "user_id")]` | `Option<Profile>` | 1 対 1。単一値の関連は先頭行のみ採用 |
| `#[entity(belongs_to = "Tag", foreign_key = "tag_code")]` | `Option<Tag>` | 多対 1：自テーブルの `foreign_key` の値を**相手の主キー**で照合 |

`local_key` は省略可能：`has_many` / `has_one` は自テーブルの主キー、`belongs_to` は相手の主キーが既定です。
関連フィールドは**列にはなりません**。フィールド型から列型への対応は `value::ColumnValue` の impl にのみ
存在します（マクロは対応表を複製しません）。未対応の型はそのフィールドを指す `T: ColumnValue` 未充足
エラーになります。

### CRUD

| メソッド | 説明 |
|------|------|
| `Entity::insert(&db, &e) -> i64` | 挿入して新しい主キーを返す。MySQL の 2 段階（`INSERT` の後に `SELECT LAST_INSERT_ID()`、しかも**接続スコープ**）は `SqlExecutor::execute_then_query` が 1 つのトランザクションに包みます |
| `Entity::insert_many(&db, &[e]) -> u64` | 一括挿入。影響行数を返す（主キーは返さない：`LAST_INSERT_ID()` は先頭行、SQLite は末尾行で、バックエンド間で意味が信頼できない） |
| `Entity::save(&db, &e)` | 主キーが「未設定」（自動採番かつ値が 0）なら挿入、そうでなければ更新。`()` を返す |
| `Entity::update(&db, &e) -> u64` | 主キーによる行全体の更新。影響 0 行はエラー：`version` なしは `OrmError::NotFound`、ありは `OrmError::OptimisticLockConflict`（追加クエリなしでは両者を区別できない） |
| `Entity::update_many(&db, &[e]) -> u64` | 1 行ずつ更新し行数を合算。一括では**どの行が**衝突したかは分かりません（戻り値の行数が入力より少なければ誰かがバージョンゲートで止められています） |
| `Entity::upsert(&db, &e) -> u64` | 主キーによる upsert（方言ごとに `ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE`） |
| `Entity::find_by_id(&db, pk) -> Option<Self>` | 主キーで 1 行取得。存在しなければ `Ok(None)` |
| `Entity::find_all(&db) -> Vec<Self>` | フィルタなしの全件取得（**大きな表では全表スキャン**。ページングには `paginate` を使用） |
| `Entity::delete_by_id(&db, pk) -> u64` | `soft_delete` 宣言時は削除時刻を設定する `UPDATE`。行はテーブルに残り、再削除は `NotFound` で時刻も更新しません |
| `Entity::hard_delete_by_id(&db, pk) -> u64` | ソフト削除を迂回して実際に `DELETE` を発行 |

### クエリビルダー

`User::query()` から始めると `Query<User, Unfiltered>` が返り、フィルタを足すと
`Query<User, Filtered>`（型状態）になります — 「WHERE なしで削除」はコンパイルできません。

```rust
use ecat_orm::query::{Op, Order};

let users = User::query()
    .filter("name", Op::Like, "alice%")?        // Eq / Ne / Lt / Le / Gt / Ge / Like
    .filter("email", Op::NotNull, serde_json::json!(null))?
    .filter("id", Op::In, serde_json::json!([1, 2, 3]))?  // In / NotIn は配列値を取る
    .order_by("id", Order::Desc)?
    .limit(10)
    .offset(20)
    .fetch(&db)
    .await?;
```

- 列名は `EntityMeta.columns` のホワイトリストで検証されます（`OrmError::UnknownColumn`）。
  結合先の表の列はホワイトリストにないため `filter_raw("...")` を使いますが、これは
  **一切の検証を行わないので入力は信頼できるものに限ります**。
- `with_trashed()` はソフト削除ゲートを解除します（ソフト削除済みも含めて取得）。
- `join(JoinType::Left, "posts", "posts.user_id = users.id")` は `Inner` / `Left` に対応し、
  `filter_raw` で結合先の列により絞り込むためのものです。**列リストに表接頭辞が付かない**ため、
  結合先が主体と同じ列名を持つと実データベースは `ambiguous column name` を返します —
  主体と列名が重ならない表を結合してください（SQLite で実測）。
- `delete_where(&db)` / `hard_delete_where(&db)` は同じ条件で削除します（ソフト削除エンティティは `UPDATE`）。
- `fetch(&db)` が実行の入口で、`find_by_id` / `find_all` / `paginate` もその背後にある同じ SQL 生成経路を再利用します。

### チャンク分割とページング

- **一括書き込みは自動でチャンク分割**：`insert_many` / `update_many` は方言の 1 文あたりの
  パラメータ上限（SQL Server なら 2100 など）で分割し、行数を合算します。手動で分割する場合も同じ上限を：
  `ecat_orm::dialect::lookup(dialect).max_params_per_stmt()`。
- **ページング**：`paginate(&db, page, per_page)` は 2 クエリ（COUNT + ページ取得）を発行し、
  ページ番号は**1 始まり**です。COUNT は同じ WHERE / JOIN を再利用しつつ ORDER BY / LIMIT / OFFSET を外します。
  戻り値は `Page { items, total, page, per_page }` で、`total_pages()` / `has_next()` は `total` から算出します。
- 大きな表の深いページングには `paginate_without_count(&db, page, per_page)`（1 クエリ）：`total` は `None` で、
  `total_pages()` / `has_next()` も答えを出せません — 計算できないものは推測せず、そう伝えます。

### 関連のプリロード

```rust
let users = User::query()
    .with(&[UserRelation::Posts, UserRelation::Profile])
    .fetch(&db)
    .await?;
```

- 各関連は**1 本**の `IN` クエリを発行し（キーが 1 文あたりのパラメータ上限を超える場合は分割）、
  主体クエリ自体は JOIN しません — **N+1 を排除**：3 ユーザーとその posts を取るのは 4 本ではなく
  2 本の SELECT（主体 1 + `IN` 1）です。
- `with()` を宣言しない場合、関連フィールドは空（`Vec::new()` / `None`）で、前回ロードした古い値を持ち越しません。
  `set_relation` は（空結果でも）全主体に対して呼ばれ、これが残留を消すための仕組みです。
- 単一値の関連（`has_one` / `belongs_to`）は先頭行のみ採用します。

### マイグレーション

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Migrator, create_table};

Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .add("002_posts", create_table::<Post>())
    .status().await?;      // MigrationStatus { applied, pending, unknown }
    .run().await?;         // pending を適用し、1 件ずつバージョン表に記録。再実行は冪等
    .down(1).await?;       // 001 の逆 SQL を実行し、バージョン表からその行を削除
```

- マイグレーション名の数字接頭辞がそのままバージョンです（`"001_users"` → 1）。数字でない接頭辞は
  `OrmError::InvalidMigrationName` — 0 と勝手に解釈しません。
- `create_table::<E>()` / `drop_table::<E>()` は `EntityMeta` から DDL を生成し、**方言は `run()` 時に
  接続の `dialect()` から解決**されるため、マイグレーション一覧は接続文字列から切り離されます。
  独自 SQL（ALTER、データ移行）には `ecat_orm::migrate::MigrationBuilder::new(|d| ...)` を使います。
- 逆 SQL のないマイグレーションに `down` を呼ぶと `OrmError::MigrationIrreversible` — 「DROP してから
  CREATE し直す」はデータを失うため、呼び出し側の代わりに推測はしません。
- バージョン表は `Migrator` が自動で作成します（MSSQL には `CREATE TABLE IF NOT EXISTS` がないため、
  先に存在確認を行います）。
