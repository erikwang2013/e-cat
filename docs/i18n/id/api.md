<!-- Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz -->
# Referensi API Ecat

Halaman ini merangkum permukaan antarmuka (API) framework Ecat: konvensi port, endpoint bawaan, format error, dan antarmuka ekstensi. Routing bisnis didaftarkan oleh masing-masing layanan.

## Konvensi Port

| Protokol | Alamat listen | Keterangan |
|------|----------|------|
| HTTP | `0.0.0.0:8000` | Routing axum, port contoh default |
| gRPC | `0.0.0.0:9000` | tonic Server, port contoh default |

## Endpoint Bawaan

Endpoint berikut disediakan oleh crate ekosistem, dipasang bersama layanan:

| Endpoint | Sumber | Keterangan |
|------|------|------|
| `/health` | ecat-health | Pemeriksaan kelangsungan hidup (mengembalikan nama layanan, versi, waktu mulai) |
| `/ready` | ecat-health | Pemeriksaan kesiapan (mengembalikan 200 setelah dependensi siap) |
| `/metrics` | ecat-metrics | Ekspos metrik Prometheus (`ecat_http_requests_total` / `ecat_http_request_duration_seconds`) |
| `/{service}/{method}` | Routing pengguna | Contoh: `/helloworld/ecat` |

> Untuk skenario kardinalitas tinggi seperti path endpoint metrik yang berisi ID, gunakan `MetricsLayer::new().with_path_fn(...)` untuk normalisasi, hindari ledakan kardinalitas metrik.

## Alur Pemrosesan Permintaan

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

## Format Error

`ecat-errors` menyediakan `ErrorCode` + `Error`, memetakan status HTTP pada waktu kompilasi:

```rust
use ecat_errors::{Error, ErrorCode};

Error::new(ErrorCode::InvalidArgument, "bad_request", "user id must be positive");
```

Respons error dienkode oleh middleware menjadi JSON (atau Protobuf), membawa code / reason / message.

## Antarmuka Ekstensi

| Kemampuan | Crate | Antarmuka |
|------|-------|------|
| GraphQL | ecat-graphql | Endpoint `/graphql`; mendukung parameter kolom dan selection bersarang, tidak mendukung alias, fragment, dan beberapa kolom tingkat atas |
| OpenAPI | ecat-openapi | Membuat spec OpenAPI dari routing |
| WebSocket | ecat-transport-ws | Transport WS yang di-upgrade |
| Routing versi API | ecat-versioning | Routing versi dengan prefiks `/v1/...` |
| Autentikasi | ecat-auth | Middleware JWT / API Key; kunci JWT harus ≥32 byte, dapat dirantai `required_issuer`/`required_audience` |
| Klien gRPC | ecat-transport-grpc | Terintegrasi dengan service discovery dan load balancing |

## Komunikasi Antar-layanan

- `HttpClient` (ecat-client): terintegrasi dengan service discovery dan load balancing, perlindungan circuit breaker dengan CircuitBreaker
- `GrpcClient` (ecat-transport-grpc): sama seperti di atas, protokol gRPC
- Middleware dikombinasikan secara terpadu dengan `tower::ServiceBuilder` (Recovery / Tracing / Logging / Timeout / RateLimit / Security / CircuitBreaker / Metrics / Retry / Validate / CORS)

## Antarmuka Backend Data

Semua backend data (`ecat-data-*`) diabstraksikan melalui trait terpadu (`RdbmsClient` untuk transaksi, `SqlExecutor` untuk eksekusi dan dialek / `Cache` / `SearchClient` / `GraphClient` / `TsdbClient` / `DocumentClient` / `StorageClient`); backend bergaya REST (Neo4j / NebulaGraph / ArangoDB / InfluxDB / IoTDB / QuestDB / TDengine / OpenSearch / Elasticsearch / S3) mengakses antarmuka HTTP terkait berdasarkan `base_url`. Konfigurasi koneksi lihat [Tutorial Konfigurasi Database](database-config-tutorial.md).

## ORM (ecat-orm)

`ecat-orm` menyediakan makro derive entitas, pembangun kueri yang aman tipe, CRUD, pramuat relasi, dan
migrasi. Setiap operasi data menerima `&impl SqlExecutor`, sehingga **API yang sama bekerja dengan klien
maupun dengan `Transaction`**:

```rust
User::find_by_id(&db, 1).await?;              // db: SqlxClient / MssqlClient
let tx = db.transaction().await?;
User::update(&tx, &user).await?;              // tx: Transaction
User::insert(&tx, &user).await?;              // juga bekerja di dalam transaksi: kedua pernyataan dijalankan di sana
tx.commit().await?;
```

SQL dihasilkan oleh lapisan dialek: SQLite / PostgreSQL / MySQL / TiDB (`ecat-data-sqlx`) dan SQL Server
(`ecat-data-mssql`) berbagi satu definisi entitas.

### Definisi entitas dan tata bahasa atribut `#[entity(...)]`

`#[derive(Entity)]` menghasilkan `Entity::META` (tabel / kolom / relasi / flag), `from_row` / `to_values` /
`pk_value`, serta satu enum `XxxRelation` per entitas (nama varian adalah PascalCase dari nama bidang
relasi).

Atribut kontainer:

| Bentuk | Arti |
|------|------|
| `#[entity(table = "users")]` | Nama tabel; bila dihilangkan dipakai snake_case dari nama struct (`User` → `user`, `UserProfile` → `user_profile`) |

Bidang kolom:

| Bentuk | Arti |
|------|------|
| `#[entity(column = "user_name")]` | Mengganti nama kolom; bawaannya nama bidang |
| `#[entity(pk)]` | Kunci primer |
| `#[entity(auto_increment)]` | Auto-increment (menyiratkan pk); kunci auto-increment tidak masuk daftar kolom INSERT, melainkan dihasilkan basis data |
| `#[entity(created_at)]` / `#[entity(updated_at)]` | Stempel waktu otomatis: keduanya diisi saat insert, dan hanya `updated_at` yang diperbarui saat update |
| `#[entity(soft_delete)]` | Kolom hapus-lunak: jalur baca otomatis menambahkan syarat `IS NULL` padanya |
| `#[entity(version)]` | Kolom kunci optimistik: update menulis `version + 1` dan membandingkan dengan nilai lama, konflik mengembalikan `OrmError::OptimisticLockConflict` |

Bidang relasi (**tipe kontainer adalah batasan keras**; tipe entitas telanjang ditolak saat kompilasi
karena tidak dapat menyatakan "tidak ditemukan"):

| Bentuk | Tipe bidang | Arti |
|------|----------|------|
| `#[entity(has_many = "Post", foreign_key = "user_id")]` | `Vec<Post>` | Satu-ke-banyak: nilai `local_key` tabel ini dicocokkan dengan kolom `foreign_key` tabel tujuan |
| `#[entity(has_one = "Profile", foreign_key = "user_id")]` | `Option<Profile>` | Satu-ke-satu; relasi bernilai tunggal mengambil baris pertama saja |
| `#[entity(belongs_to = "Tag", foreign_key = "tag_code")]` | `Option<Tag>` | Banyak-ke-satu: nilai `foreign_key` tabel ini dicocokkan dengan **kunci primer tabel tujuan** |

`local_key` boleh dihilangkan: `has_many` / `has_one` bawaannya kunci primer tabel ini, `belongs_to`
bawaannya kunci primer tabel tujuan. Bidang relasi **bukan kolom**. Pemetaan tipe bidang ke tipe kolom
hanya ada di implementasi `value::ColumnValue` (makro tidak menyimpan salinan kedua tabel itu); tipe yang
tidak didukung menghasilkan galat `T: ColumnValue` yang tidak terpenuhi dan menunjuk ke bidang tersebut.

### CRUD

| Metode | Catatan |
|------|------|
| `Entity::insert(&db, &e) -> i64` | Menyisipkan dan mengembalikan kunci primer baru. Jalur dua langkah MySQL (`INSERT` lalu `SELECT LAST_INSERT_ID()`, yang **terikat koneksi**) dibungkus `SqlExecutor::execute_then_query` menjadi satu transaksi |
| `Entity::insert_many(&db, &[e]) -> u64` | Insert massal, mengembalikan jumlah baris terpengaruh (kunci tidak dikembalikan: `LAST_INSERT_ID()` memberi baris pertama, SQLite baris terakhir — tidak andal antar backend) |
| `Entity::save(&db, &e)` | Insert bila kunci primer "belum diisi" (auto-increment dan bernilai 0), selain itu update; mengembalikan `()` |
| `Entity::update(&db, &e) -> u64` | Update satu baris penuh berdasarkan kunci primer. Nol baris terpengaruh adalah galat: `OrmError::NotFound` tanpa `version`, `OrmError::OptimisticLockConflict` dengannya (tanpa kueri tambahan keduanya tak dapat dibedakan) |
| `Entity::update_many(&db, &[e]) -> u64` | Update baris per baris dan menjumlahkan barisnya; panggilan massal **tidak dapat menunjukkan baris mana** yang konflik (hasil lebih kecil dari masukan berarti ada yang dihentikan syarat versi) |
| `Entity::upsert(&db, &e) -> u64` | Upsert berdasarkan kunci primer (`ON CONFLICT` / `ON DUPLICATE KEY` / `MERGE` menurut dialek) |
| `Entity::find_by_id(&db, pk) -> Option<Self>` | Mengambil satu baris berdasarkan kunci primer; `Ok(None)` bila tidak ada |
| `Entity::find_all(&db) -> Vec<Self>` | Semua baris tanpa penyaringan (**pemindaian tabel penuh pada tabel besar**; untuk penomoran halaman gunakan `paginate`) |
| `Entity::delete_by_id(&db, pk) -> u64` | Bila `soft_delete` dideklarasikan, mengirim `UPDATE` yang mengisi waktu hapus; baris tetap di tabel, dan menghapus lagi mengembalikan `NotFound` tanpa memperbarui waktunya |
| `Entity::hard_delete_by_id(&db, pk) -> u64` | Melewati hapus-lunak dan benar-benar mengirim `DELETE` |

### Pembangun kueri

Dimulai dengan `User::query()` yang mengembalikan `Query<User, Unfiltered>`; menambahkan filter
mengubahnya menjadi `Query<User, Filtered>` (state tipe) — "delete tanpa WHERE" tidak dapat dikompilasi.

```rust
use ecat_orm::query::{Op, Order};

let users = User::query()
    .filter("name", Op::Like, "alice%")?        // Eq / Ne / Lt / Le / Gt / Ge / Like
    .filter("email", Op::NotNull, serde_json::json!(null))?
    .filter("id", Op::In, serde_json::json!([1, 2, 3]))?  // In / NotIn menerima nilai array
    .order_by("id", Order::Desc)?
    .limit(10)
    .offset(20)
    .fetch(&db)
    .await?;
```

- Nama kolom diperiksa terhadap daftar putih `EntityMeta.columns` (`OrmError::UnknownColumn`); kolom
  tabel yang di-join tidak ada di sana — gunakan `filter_raw("...")` untuknya, dan ingat bahwa ia
  **tidak melakukan validasi apa pun, jadi masukannya harus tepercaya**.
- `with_trashed()` mematikan syarat hapus-lunak (baris yang dihapus lunak ikut terambil).
- `join(JoinType::Left, "posts", "posts.user_id = users.id")` mendukung `Inner` / `Left`, dan
  dimaksudkan untuk menyaring lewat kolom tabel yang di-join di dalam `filter_raw`. **Daftar kolom tidak
  memakai prefiks tabel**, jadi bila tabel yang di-join berbagi nama kolom dengan tabel utama, basis data
  sungguhan melaporkan `ambiguous column name` — join-kan tabel yang nama kolomnya berbeda dari tabel
  utama (terverifikasi di SQLite).
- `delete_where(&db)` / `hard_delete_where(&db)` menghapus dengan syarat yang sama (entitas ber-hapus-lunak
  melalui `UPDATE`).
- `fetch(&db)` adalah titik eksekusinya; `find_by_id` / `find_all` / `paginate` memakai ulang jalur
  pembangkitan SQL yang sama di belakangnya.

### Pemotongan batch dan penomoran halaman

- **Tulisan massal dipotong otomatis**: `insert_many` / `update_many` dibagi menurut batas parameter per
  pernyataan milik dialek (misalnya 2100 di SQL Server) lalu menjumlahkan barisnya. Untuk memotong manual,
  pakai batas yang sama: `ecat_orm::dialect::lookup(dialect).max_params_per_stmt()`.
- **Penomoran halaman**: `paginate(&db, page, per_page)` mengirim 2 kueri (COUNT + pengambilan halaman)
  dan halaman **dimulai dari 1**; COUNT memakai WHERE / JOIN yang sama tetapi membuang ORDER BY / LIMIT /
  OFFSET. Mengembalikan `Page { items, total, page, per_page }`, dan `total_pages()` / `has_next()`
  dihitung dari `total`.
- Untuk menelusuri tabel besar secara dalam gunakan `paginate_without_count(&db, page, per_page)`
  (1 kueri): `total` bernilai `None`, dan `total_pages()` / `has_next()` juga tidak bisa menjawab — yang
  tidak dapat dihitung dilaporkan apa adanya, bukan ditebak.

### Pramuat relasi

```rust
let users = User::query()
    .with(&[UserRelation::Posts, UserRelation::Profile])
    .fetch(&db)
    .await?;
```

- Setiap relasi mengirim **satu** kueri `IN` (dipotong bila kuncinya melebihi batas parameter per
  pernyataan), dan kueri subjeknya sendiri tidak melakukan join — **tanpa N+1**: mengambil 3 pengguna
  beserta post-nya adalah 2 SELECT (1 subjek + 1 `IN`), bukan 4.
- Tanpa `with()` bidang relasi kosong (`Vec::new()` / `None`) dan tidak pernah menyimpan nilai lama dari
  pemuatan sebelumnya; `set_relation` dipanggil untuk setiap subjek (termasuk hasil kosong) justru untuk
  membersihkan sisa-sisa.
- Relasi bernilai tunggal (`has_one` / `belongs_to`) mengambil baris pertama saja.

### Migrasi

```rust
use ecat_orm::migrate::drop_table_sql;
use ecat_orm::{Migrator, create_table};

Migrator::new(&db)
    .add("001_users", create_table::<User>().with_reverse(|d| drop_table_sql(User::META, d)))
    .add("002_posts", create_table::<Post>())
    .status().await?;      // MigrationStatus { applied, pending, unknown }
    .run().await?;         // menerapkan yang pending dan mencatat masing-masing di tabel versi; menjalankan ulang bersifat idempoten
    .down(1).await?;       // menjalankan SQL balik migrasi 001 dan menghapus barisnya dari tabel versi
```

- Prefiks numerik pada nama migrasi adalah versinya (`"001_users"` → 1); prefiks non-numerik
  menghasilkan `OrmError::InvalidMigrationName` — tidak pernah ditafsirkan sebagai 0.
- `create_table::<E>()` / `drop_table::<E>()` menghasilkan DDL dari `EntityMeta`, dan **dialek ditentukan
  saat `run()` dari `dialect()` koneksi**, sehingga daftar migrasi lepas dari string koneksi. Untuk SQL
  yang ditulis manual (ALTER, pengisian data) gunakan
  `ecat_orm::migrate::MigrationBuilder::new(|d| ...)`.
- Memanggil `down` pada migrasi tanpa SQL balik menghasilkan `OrmError::MigrationIrreversible` —
  "DROP lalu CREATE ulang" menghilangkan data, dan itu tidak ditebak atas nama pemanggil.
- Tabel versi dibuat otomatis oleh `Migrator` (MSSQL tidak punya `CREATE TABLE IF NOT EXISTS`, jadi
  keberadaannya diperiksa lebih dulu).
