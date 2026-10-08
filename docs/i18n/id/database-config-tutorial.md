# Tutorial Konfigurasi Database

**Versi:** 2.4.2 · **Tanggal:** 2026-08-01

Ke-14 backend data e-cat semuanya mendukung pemuatan info koneksi melalui file konfigurasi, tanpa perlu hardcode di kode. `username` / `password` keduanya kolom opsional, jika dihilangkan maka autentikasi dilewati.

---

## Memulai Cepat

### 1. Membuat File Konfigurasi

Salin template contoh dan sesuaikan dengan lingkungan aktual:

```bash
cp config/databases.example.yaml databases.yaml
```

Edit `databases.yaml`, isi dengan info koneksi yang sebenarnya:

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

### 2. Menambahkan Dependensi

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
ecat-data-sqlx = { path = "../ecat-data-sqlx" }
ecat-data-redis = { path = "../ecat-data-redis" }
ecat-data-clickhouse = { path = "../ecat-data-clickhouse" }
```

### 3. Memuat dan Menggunakan

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
    // Muat konfigurasi YAML
    let yaml = std::fs::read_to_string("databases.yaml")?;
    let cfg: AppConfig = serde_yaml::from_str(&yaml)?;

    // Buat klien basis data — tanpa informasi koneksi yang di-hardcode
    let db = SqlxClient::from_config(cfg.sql).await?;
    let cache = RedisCache::from_config(cfg.redis).await?;
    let ch = ClickhouseClient::from_config(cfg.clickhouse);

    // Penggunaan
    let rows = db.query("SELECT id, name FROM users LIMIT 10").await?;
    cache.set("health", b"ok", std::time::Duration::from_secs(30)).await?;

    Ok(())
}
```

---

## Referensi Konfigurasi Lengkap

### Mendefinisikan Struct Konfigurasi Tingkat Atas

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

### Contoh YAML Lengkap

Lihat `config/databases.example.yaml`.

---

## Referensi Cepat Kolom Config per Backend

### RDBMS — SqlxConfig

#### Pool native (PostgreSQL / MySQL / SQLite)

Driver native dipilih otomatis dari scheme URL, tanpa konfigurasi tambahan:

| scheme | Driver |
|---|---|
| `postgres://` / `postgresql://` | PostgreSQL |
| `mysql://` / `mariadb://` | MySQL |
| `sqlite:` | SQLite |

- **Dukungan native untuk tipe waktu**: jalur lama lewat `AnyPool` dari sqlx tidak mendukung tipe waktu, sehingga harus `CAST` ke teks sendiri; sekarang kolom waktu bisa dibaca langsung.
- **Huruf besar/kecil maupun spasi di awal/akhir** scheme **keduanya ditoleransi** (`"POSTGRES://…"` dan `" postgres://…"` sama-sama bisa); scheme yang tidak dikenali akan error dan **menyebutkan** scheme mana itu.
- `mssql://` **tidak disediakan backend ini** (gunakan `ecat-data-mssql`); memberikannya ke sini akan ditolak secara eksplisit.

#### Contoh konfigurasi

```yaml
sql:
  url: "postgres://host:5432/dbname"
  # username: "app_user"    # opsional
  # password: "secret"      # opsional
```

| Kolom | Tipe | Nilai default | Keterangan |
|------|------|--------|------|
| `url` | `String` | — | String koneksi sqlx, mendukung SQLite/PG/MySQL/TiDB |
| `username` | `Option<String>` | `None` | Opsional: autentikasi tertanam di URL (berpasangan dengan password) |
| `password` | `Option<String>` | `None` | Opsional: autentikasi tertanam di URL (berpasangan dengan username) |
| `max_connections` | `u32` | `10` | Jumlah maksimum koneksi dalam pool |
| `min_connections` | `u32` | `0` | Jumlah koneksi minimum yang dijaga; dijepit ke ≤ `max_connections` |
| `acquire_timeout_secs` | `u64` | `30` | Batas waktu menunggu koneksi; **`0` = timeout seketika** (gagal bila tidak ada koneksi bebas), kebalikan dari `0` = nonaktif di `query_timeout_secs` |
| `idle_timeout_secs` | `u64` | `600` | Daur ulang koneksi menganggur |
| `max_lifetime_secs` | `u64` | `1800` | Masa hidup maksimum koneksi |
| `query_timeout_secs` | `u64` | `30` | Timeout per kueri; **0 = nonaktif** |
| `slow_query_ms` | `u64` | `1000` | Ambang peringatan kueri lambat (ms); tidak diisi = 1000, **0 = mati** (hanya feature `tracing`) |
| `test_before_acquire` | `bool` | `false` | Ping dulu sebelum koneksi diberikan |
| `session_init` | `string[]` | Sesuai dialek | Pernyataan inisialisasi sesi untuk setiap koneksi baru |

#### `session_init` (inisialisasi sesi)

Setiap kali koneksi baru terbentuk, pernyataan-pernyataan ini dijalankan berurutan:

| Dialek | Default `session_init` |
|---|---|
| PostgreSQL | `SET TIME ZONE 'UTC'`, `SET application_name = 'ecat'` |
| MySQL | `SET time_zone = '+00:00'` |
| SQLite | Tidak ada konsep sesi — default kosong |

Tujuannya agar **sisi basis data langsung mengembalikan UTC**, selaras dengan konvensi framework bahwa semua waktu disajikan seragam sebagai RFC3339 UTC.

- **Array kosong eksplisit `[]` berarti "sengaja dimatikan"** dan menimpa default dialek (konvensi penimpaan eksplisit yang sama dengan `query_timeout_secs: 0` yang berarti nonaktif).
- Jika salah satu pernyataan gagal → pembuatan koneksi gagal, **tanpa penurunan diam-diam**.

```yaml
sql:
  url: "mysql://host:3306/dbname"
  session_init:
    - "SET time_zone = '+00:00'"
    - "SET NAMES utf8mb4"
```

#### `warm_up()` — Pemanasan

Membuka `min_connections` koneksi secara aktif saat start lalu mengembalikannya, agar layanan langsung siap saat menyala:

```rust
let db = SqlxClient::from_config(cfg).await?;
db.warm_up().await?;   // panggil sekali saat start
```

Kenapa perlu: sqlx memelihara `min_connections` **secara asinkron lewat tugas latar belakang**, jadi kembalinya `connect()` tidak menjamin pool sudah penuh — gelombang permintaan pertama akan berlomba dengan tugas latar belakang itu.

#### Feature observabilitas (`metrics` / `health` / `tracing`)

Ketiga feature **nonaktif secara default** (agar axum — dependensi `ecat-metrics` / `ecat-health` — tidak masuk ke pohon dependensi inti); aktifkan sesuai kebutuhan. `ecat-data-mssql` menyediakan ketiganya juga.

```toml
ecat-data-sqlx = { path = "../ecat-data-sqlx", features = ["metrics", "health", "tracing"] }
```

```rust
use std::sync::Arc;
use ecat_data_sqlx::{RdbmsHealthCheck, SqlxClient, SqlxConfig, register_pool_metrics};
use ecat_health::HealthRegistry;

let db = SqlxClient::from_config(cfg).await?;

// metrics: setelah didaftarkan, endpoint /metrics bertambah empat metrik —
// ecat_rdbms_pool_connections (gauge, dengan state="idle"/"active"),
// ecat_rdbms_pool_timeouts_total, ecat_rdbms_query_timeout_total,
// ecat_rdbms_transactions_leaked_total (semuanya counter, berlabel backend)
register_pool_metrics("primary", db.pool());

// health: probe konektivitas SELECT 1, didaftarkan ke readyz /health
let registry = HealthRegistry::new()
    .with_check(RdbmsHealthCheck::new("sql", Arc::new(db)));
```

Feature `tracing` menulis warn ketika kueri melewati `slow_query_ms` (waktu tempuh + SQL yang dipotong 200 karakter pertama). Field ini hanya dibaca oleh feature tersebut — saat feature mati tetap diparsing, tetapi tidak berefek.

#### Waktu dan tanggal: seragam RFC3339 UTC

**Teks waktu/tanggal di sqlite ditulis ulang menjadi RFC3339 UTC**:

- teks berbentuk `2026-10-05 12:34:56` → `"2026-10-05T12:34:56Z"`
- teks berbentuk `2026-10-05` → `"2026-10-05T00:00:00Z"` (titik tengah malam UTC)

Alasan: sqlite tidak punya sistem tipe, sehingga teks berbentuk tanggal/waktu memang ambigu; framework memperlakukannya seragam sebagai waktu.

**Efek samping**: Kolom teks yang kebetulan berbentuk tanggal — nomor versi, kode bisnis — juga ikut ditulis ulang. Jika perilaku ini tidak diinginkan, `CAST` kolom itu secara eksplisit ke bentuk non-tanggal, atau pakai tipe lain.

Kolom `DATE` / `TIMESTAMP` asli di PG / MySQL juga disajikan sebagai string RFC3339 UTC, dan **tanggal murni pun tetap membawa `T00:00:00Z`** — karena `"2026-10-05"` bukan RFC3339 yang valid, dan framework memakai satu format seragam secara internal agar lapisan atas mudah mem-parsing-nya.

### Redis — RedisConfig

```yaml
redis:
  url: "redis://host:6379"
  # password: "auth_token"  # opsional
  # query_timeout_secs: 30     # opsional: timeout per perintah, 0 = nonaktif
  # breaker: {}                # opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
```

| Kolom | Tipe | Nilai default | Keterangan |
|------|------|--------|------|
| `url` | `String` | — | URL koneksi Redis |
| `password` | `Option<String>` | `None` | Opsional: kata sandi AUTH Redis |
| `query_timeout_secs` | `Option<u64>` | `30` | Timeout per perintah dalam detik; **`0` = nonaktif** |
| `breaker` | `Option<BreakerConfig>` | default konservatif | Ambang dan jendela breaker; kolom boleh dihilangkan, `breaker: {}` berarti semua default |

**Batas kemampuan**: client ini memakai `MultiplexedConnection` (satu koneksi TCP melayani semua konkurensi), **bukan connection pool** — untuk beban cache ini lebih baik daripada pool: koneksi dan round-trip lebih sedikit. Harganya: **rangkaian perintah stateful tidak bisa memakainya** — transaksi `MULTI`/`EXEC`, `WATCH`, `SUBSCRIBE`, dan perintah blocking butuh koneksi eksklusif; di bawah multiplexing perintah-perintah itu akan berselang-seling dengan perintah lain. Bila perlu, buka koneksi khusus dengan `redis::Client::get_async_connection()`.

**Timeout**: `query_timeout_secs: 0` di konfigurasi berarti **nonaktif** (bukan «timeout setelah 0 detik»); default 30 detik hanya berlaku bila kolom dihilangkan. Di tingkat library, `run_with_timeout(kind, Some(Duration::ZERO), fut)` justru sebaliknya — itu **langsung timeout** (tokio lebih dulu men-poll future dalam, jadi future yang sudah siap tetap berhasil). Kedua «0» itu berbeda arti; jangan meniru nilai konfigurasi saat memanggil fungsi library secara langsung.

**Breaker aktif secara default** — dengan ambang konservatif (rasio gagal 0.5, jendela 30 detik, probe half-open 3, terbuka 10 detik) ia hanya membuka saat gagal terus-menerus. **Saat ini tidak ada sakelar utama**: `BreakerConfig` hanya punya keempat kolom ambang itu, tanpa `enabled`; menulis `{"enabled": false}` hanya menghasilkan galat deserialisasi. Untuk benar-benar menonaktifkannya, jauhkan ambangnya dari jangkauan (mis. `failure_ratio: 1.1`).

### Memcached — MemcachedConfig

```yaml
memcached:
  # username: "memcache"    # opsional: bidang yang dicadangkan (saat ini implementasi in-memory)
  # password: "secret"      # opsional: bidang yang dicadangkan
  {}
```

| Kolom | Tipe | Keterangan |
|------|------|------|
| `username` | `Option<String>` | Opsional: kolom cadangan |
| `password` | `Option<String>` | Opsional: kolom cadangan |

Saat ini merupakan implementasi memori, kolom autentikasi dicadangkan.

### ClickHouse — ClickhouseConfig

```yaml
clickhouse:
  base_url: "http://host:8123"
  database: "default"
  # username: "default"   # opsional
  # password: "secret"    # opsional
  # query_timeout_secs: 30  # opsional: timeout per panggilan, 0 = nonaktif
  # breaker: {}             # opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
  # max_concurrency: 32     # opsional: batas konkurensi (semaphore crate ini)
```

| Kolom | Tipe | Nilai default | Keterangan |
|------|------|--------|------|
| `base_url` | `String` | — | Alamat antarmuka HTTP |
| `database` | `String` | `"default"` | Nama database |
| `username` | `Option<String>` | `None` | Opsional: nama pengguna HTTP Basic Auth |
| `password` | `Option<String>` | `None` | Opsional: kata sandi HTTP Basic Auth |
| `query_timeout_secs` | `Option<u64>` | `30` | Timeout per panggilan dalam detik; **`0` = nonaktif** (sama seperti Redis) |
| `breaker` | `Option<BreakerConfig>` | default konservatif | Ambang dan jendela breaker; juga tanpa sakelar utama `enabled` |
| `max_concurrency` | `Option<usize>` | `32` | Batas konkurensi; **semaphore milik crate ini sendiri**, bukan tombol reqwest (reqwest hanya punya `pool_max_idle_per_host` — koneksi menganggur yang disimpan, bukan batas atas) |

**Dua lapis timeout**: `reqwest::Client` yang dibuat `from_config` (`ecat-tls`) membawa timeout koneksi 5 detik + total 30 detik sendiri; `query_timeout_secs` adalah anggaran **luar** — bila keduanya aktif, **yang lebih dulu habis menang**; bila lapis dalam yang habis, galatnya `RdbmsError::Database` dan **tidak dihitung** di `ecat_outbound_timeouts_total` (counter untuk timeout luar, feature `metrics`). `new` / `with_auth` memakai `reqwest::Client::new()` telanjang, tanpa timeout dalam.

### QuestDB — QuestdbConfig

```yaml
questdb:
  base_url: "http://host:9000"
  # username: "admin"     # opsional
  # password: "quest"     # opsional
  # query_timeout_secs: 30   # Opsional: timeout per panggilan, 0 = nonaktif
  # breaker: {}              # Opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
  # max_concurrency: 32      # Opsional: batas konkurensi (semaphore crate ini)
```

| Kolom | Tipe | Keterangan |
|------|------|------|
| `base_url` | `String` | Alamat HTTP API |
| `username` | `Option<String>` | Opsional: nama pengguna HTTP Basic Auth |
| `password` | `Option<String>` | Opsional: kata sandi HTTP Basic Auth |
| `query_timeout_secs` | `Option<u64>` | Timeout per panggilan dalam detik; dihilangkan = `30`, **`0` = nonaktif** |
| `breaker` | `Option<BreakerConfig>` | Ambang dan jendela breaker; tanpa sakelar utama `enabled` |
| `max_concurrency` | `Option<usize>` | Batas konkurensi (default `32`); semaphore milik crate ini sendiri |

**Jenis galat**: QuestDB lewat `SqlExecutor` (keluarga RDBMS) — timeout adalah `RdbmsError::Timeout`,
penolakan breaker adalah `RdbmsError::Connection("circuit breaker is open")`; seluruh backend HTTP lain
seragam mengembalikan `ecat_errors::Error` (`code = DeadlineExceeded` / `Unavailable`, `reason` = nama backend).

### Elasticsearch — ElasticsearchConfig

```yaml
elasticsearch:
  base_url: "http://host:9200"
  # username: "elastic"   # opsional
  # password: "secret"    # opsional
  # query_timeout_secs: 30   # Opsional: timeout per panggilan, 0 = nonaktif
  # breaker: {}              # Opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
  # max_concurrency: 32      # Opsional: batas konkurensi (semaphore crate ini)
```

| Kolom | Tipe | Keterangan |
|------|------|------|
| `base_url` | `String` | Alamat REST API |
| `username` | `Option<String>` | Opsional: nama pengguna HTTP Basic Auth |
| `password` | `Option<String>` | Opsional: kata sandi HTTP Basic Auth |
| `query_timeout_secs` | `Option<u64>` | Timeout per panggilan dalam detik; dihilangkan = `30`, **`0` = nonaktif** |
| `breaker` | `Option<BreakerConfig>` | Ambang dan jendela breaker; tanpa sakelar utama `enabled` |
| `max_concurrency` | `Option<usize>` | Batas konkurensi (default `32`); semaphore milik crate ini sendiri |

### OpenSearch — OpenSearchConfig

```yaml
opensearch:
  base_url: "http://host:9200"
  # username: "admin"     # opsional
  # password: "secret"    # opsional
  # query_timeout_secs: 30   # Opsional: timeout per panggilan, 0 = nonaktif
  # breaker: {}              # Opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
  # max_concurrency: 32      # Opsional: batas konkurensi (semaphore crate ini)
```

| Kolom | Tipe | Keterangan |
|------|------|------|
| `base_url` | `String` | Alamat REST API |
| `username` | `Option<String>` | Opsional: nama pengguna HTTP Basic Auth |
| `password` | `Option<String>` | Opsional: kata sandi HTTP Basic Auth |
| `query_timeout_secs` | `Option<u64>` | Timeout per panggilan dalam detik; dihilangkan = `30`, **`0` = nonaktif** |
| `breaker` | `Option<BreakerConfig>` | Ambang dan jendela breaker; tanpa sakelar utama `enabled` |
| `max_concurrency` | `Option<usize>` | Batas konkurensi (default `32`); semaphore milik crate ini sendiri |

### InfluxDB — InfluxConfig

```yaml
influxdb:
  base_url: "http://host:8086"
  org: "myorg"
  bucket: "mybucket"
  token: "my-token"
  # query_timeout_secs: 30   # Opsional: timeout per panggilan, 0 = nonaktif
  # breaker: {}              # Opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
  # max_concurrency: 32      # Opsional: batas konkurensi (semaphore crate ini)
```

| Kolom | Tipe | Keterangan |
|------|------|------|
| `base_url` | `String` | Alamat API InfluxDB 2.x |
| `org` | `String` | Nama organisasi |
| `bucket` | `String` | Nama bucket |
| `token` | `String` | Token autentikasi |
| `query_timeout_secs` | `Option<u64>` | Timeout per panggilan dalam detik; dihilangkan = `30`, **`0` = nonaktif** |
| `breaker` | `Option<BreakerConfig>` | Ambang dan jendela breaker; tanpa sakelar utama `enabled` |
| `max_concurrency` | `Option<usize>` | Batas konkurensi (default `32`); semaphore milik crate ini sendiri |

### Neo4j — Neo4jConfig

```yaml
neo4j:
  base_url: "http://host:7474"
  username: "neo4j"
  password: "secret"
  # query_timeout_secs: 30   # Opsional: timeout per panggilan, 0 = nonaktif
  # breaker: {}              # Opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
  # max_concurrency: 32      # Opsional: batas konkurensi (semaphore crate ini)
```

| Kolom | Tipe | Keterangan |
|------|------|------|
| `base_url` | `String` | Alamat REST API |
| `username` | `String` | Nama pengguna |
| `password` | `String` | Kata sandi |
| `query_timeout_secs` | `Option<u64>` | Timeout per panggilan dalam detik; dihilangkan = `30`, **`0` = nonaktif** |
| `breaker` | `Option<BreakerConfig>` | Ambang dan jendela breaker; tanpa sakelar utama `enabled` |
| `max_concurrency` | `Option<usize>` | Batas konkurensi (default `32`); semaphore milik crate ini sendiri |

### NebulaGraph — NebulaGraphConfig

```yaml
nebulagraph:
  base_url: "http://host:19669"
  space: "my_space"
  # username: "root"      # opsional
  # password: "nebula"    # opsional
  # query_timeout_secs: 30   # Opsional: timeout per panggilan, 0 = nonaktif
  # breaker: {}              # Opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
  # max_concurrency: 32      # Opsional: batas konkurensi (semaphore crate ini)
```

| Kolom | Tipe | Keterangan |
|------|------|------|
| `base_url` | `String` | Alamat API |
| `space` | `String` | Nama graph space |
| `username` | `Option<String>` | Opsional: nama pengguna HTTP Basic Auth |
| `password` | `Option<String>` | Opsional: kata sandi HTTP Basic Auth |
| `query_timeout_secs` | `Option<u64>` | Timeout per panggilan dalam detik; dihilangkan = `30`, **`0` = nonaktif** |
| `breaker` | `Option<BreakerConfig>` | Ambang dan jendela breaker; tanpa sakelar utama `enabled` |
| `max_concurrency` | `Option<usize>` | Batas konkurensi (default `32`); semaphore milik crate ini sendiri |

### ArangoDB — ArangoConfig

```yaml
arangodb:
  base_url: "http://host:8529"
  db: "mydb"
  username: "root"
  password: "secret"
  # query_timeout_secs: 30   # Opsional: timeout per panggilan, 0 = nonaktif
  # breaker: {}              # Opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
  # max_concurrency: 32      # Opsional: batas konkurensi (semaphore crate ini)
```

| Kolom | Tipe | Keterangan |
|------|------|------|
| `base_url` | `String` | Alamat API |
| `db` | `String` | Nama database |
| `username` | `String` | Nama pengguna |
| `password` | `String` | Kata sandi |
| `query_timeout_secs` | `Option<u64>` | Timeout per panggilan dalam detik; dihilangkan = `30`, **`0` = nonaktif** |
| `breaker` | `Option<BreakerConfig>` | Ambang dan jendela breaker; tanpa sakelar utama `enabled` |
| `max_concurrency` | `Option<usize>` | Batas konkurensi (default `32`); semaphore milik crate ini sendiri |

### IoTDB — IotdbConfig

```yaml
iotdb:
  base_url: "http://host:18080"
  username: "root"
  password: "root"
  # query_timeout_secs: 30   # Opsional: timeout per panggilan, 0 = nonaktif
  # breaker: {}              # Opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
  # max_concurrency: 32      # Opsional: batas konkurensi (semaphore crate ini)
```

| Kolom | Tipe | Keterangan |
|------|------|------|
| `base_url` | `String` | Alamat REST API |
| `username` | `String` | Nama pengguna |
| `password` | `String` | Kata sandi |
| `query_timeout_secs` | `Option<u64>` | Timeout per panggilan dalam detik; dihilangkan = `30`, **`0` = nonaktif** |
| `breaker` | `Option<BreakerConfig>` | Ambang dan jendela breaker; tanpa sakelar utama `enabled` |
| `max_concurrency` | `Option<usize>` | Batas konkurensi (default `32`); semaphore milik crate ini sendiri |

### TDengine — TdengineConfig

```yaml
tdengine:
  base_url: "http://host:6041"
  username: "root"
  password: "taosdata"
  # database: "my_db"        # Opsional: bila kosong, database default di jalur REST yang dipakai
  # query_timeout_secs: 30   # Opsional: timeout per panggilan, 0 = nonaktif
  # breaker: {}              # Opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
  # max_concurrency: 32      # Opsional: batas konkurensi (semaphore crate ini)
```

| Kolom | Tipe | Keterangan |
|------|------|------|
| `base_url` | `String` | Alamat antarmuka REST (taosAdapter, port default 6041) |
| `username` | `String` | Nama pengguna |
| `password` | `String` | Kata sandi |
| `database` | `Option<String>` | Opsional: nama database default (disisipkan ke jalur REST) |
| `query_timeout_secs` | `Option<u64>` | Timeout per panggilan dalam detik; dihilangkan = `30`, **`0` = nonaktif** |
| `breaker` | `Option<BreakerConfig>` | Ambang dan jendela breaker; tanpa sakelar utama `enabled` |
| `max_concurrency` | `Option<usize>` | Batas konkurensi (default `32`); semaphore milik crate ini sendiri |

**Satu anggaran untuk seluruh panggilan**: `write()` memecah satu batch titik data menjadi beberapa permintaan HTTP, dan `query_timeout_secs` menutup **seluruh panggilan** (semua bagian), bukan satu anggaran per bagian.

### MongoDB — MongoConfig

```yaml
mongodb:
  url: "mongodb://host:27017"
  database: "app"
  # max_pool_size: 10        # Opsional: batas atas connection pool, dihilangkan = default driver (**10**)
  # min_pool_size: 0         # Opsional: batas bawah connection pool (koneksi yang dijaga di latar belakang)
  # query_timeout_secs: 30   # Opsional: timeout per perintah, 0 = nonaktif
  # breaker: {}              # Opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
```

| Kolom | Tipe | Keterangan |
|------|------|------|
| `url` | `String` | URI koneksi (autentikasi, replica set, dan opsi TLS semuanya ada di URI) |
| `database` | `String` | Nama database |
| `max_pool_size` | `Option<u32>` | Batas atas connection pool; dihilangkan = default driver **10** (diukur pada `mongodb` 3.8.0, bukan 100) |
| `min_pool_size` | `Option<u32>` | Batas bawah connection pool; dihilangkan = default driver `0` |
| `query_timeout_secs` | `Option<u64>` | Timeout per perintah dalam detik; dihilangkan = `30`, **`0` = nonaktif** |
| `breaker` | `Option<BreakerConfig>` | Ambang dan jendela breaker; tanpa sakelar utama `enabled` |

**Backpressure konkurensi lewat pool driver**: crate ini **tidak punya** `max_concurrency` (dan juga bukan HTTP) — driver membawa pool-nya sendiri; bila butuh konkurensi lebih tinggi, setel `max_pool_size` secara eksplisit.

### S3 / MinIO — S3Config

```yaml
s3:
  endpoint: "http://host:9000"
  region: "us-east-1"
  access_key: "minioadmin"
  secret_key: "minioadmin"
  # query_timeout_secs: 30   # Opsional: timeout per panggilan, 0 = nonaktif
  # breaker: {}              # Opsional: konfigurasi breaker, dihilangkan = default konservatif (0.5 / 30s / terbuka 10s)
  # max_concurrency: 32      # Opsional: batas konkurensi (semaphore crate ini)
```

| Kolom | Tipe | Keterangan |
|------|------|------|
| `endpoint` | `String` | Alamat layanan kompatibel S3 (MinIO / gateway self-hosted) |
| `region` | `String` | Region untuk penandatanganan; MinIO tidak peduli nilainya, `us-east-1` cukup |
| `access_key` | `String` | Access Key |
| `secret_key` | `String` | Secret Key |
| `query_timeout_secs` | `Option<u64>` | Timeout per panggilan dalam detik; dihilangkan = `30`, **`0` = nonaktif** |
| `breaker` | `Option<BreakerConfig>` | Ambang dan jendela breaker; tanpa sakelar utama `enabled` |
| `max_concurrency` | `Option<usize>` | Batas konkurensi (default `32`); semaphore milik crate ini sendiri |

**Satu anggaran untuk seluruh panggilan**: `list()` mengikuti continuation token dan mengirim beberapa GET dalam satu panggilan; timeout menutup **seluruh penelusuran halaman**.

> **Kesamaan resiliensi keluar** (semua backend HTTP di bagian ini): urutannya **izin → breaker → timeout**; galat timeout membawa `code = DeadlineExceeded`, penolakan breaker `code = Unavailable` dengan `message = "circuit breaker is open"`; dengan feature `metrics` keduanya dihitung di `ecat_outbound_timeouts_total{backend="<nama bagian konfigurasi>"}` — labelnya adalah **nama bagian konfigurasi** (`"arangodb"` / `"mongodb"` / …), bukan nama kategori trait.

---

## Pembuatan Programatik

### Tanpa Autentikasi

```rust
let es = ElasticsearchClient::new("http://localhost:9200");
let ch = ClickhouseClient::new("http://localhost:8123", "default");
```

### Dengan Autentikasi

```rust
let es = ElasticsearchClient::with_auth("http://es:9200", "elastic", "secret");
let ch = ClickhouseClient::with_auth("http://ch:8123", "default", "admin", "pass");
let qdb = QuestdbClient::with_auth("http://qdb:9000", "admin", "quest");
let ng = NebulaGraphClient::with_auth("http://ng:19669", "space1", "root", "nebula");
```

---

---

## Konfigurasi Sertifikat TLS

Semua backend data umumnya mendukung autentikasi klien TLS opsional (kolom `tls`), dengan **dua pengecualian**: `ecat-data-sqlx` tidak mendukung field ini — mengisinya membuat startup gagal (TLS-nya lewat parameter URL); field `ecat-data-memcached` **diam-diam tidak berefek** — dideklarasikan, tetapi tidak pernah dibaca di crate.

### Contoh Konfigurasi

```yaml
clickhouse:
  base_url: "https://ch.internal:8443"
  tls:
    ca_cert: "/etc/ecat/ca.pem"
    client_cert: "/etc/ecat/client.pem"
    client_key: "/etc/ecat/client-key.pem"
    # skip_verify: true  # hanya lingkungan pengujian
```

### Pembuatan Sertifikat Otomatis (ecat-tls)

```rust
use ecat_tls::{generate_ca, generate_server_cert, generate_client_cert};

// 1. Buat CA
let ca = generate_ca("MyOrg")?;
std::fs::write("ca.pem", &ca.cert_pem)?;
std::fs::write("ca-key.pem", &ca.key_pem)?;

// 2. Buat sertifikat server
let srv = generate_server_cert("db.example.com")?;
std::fs::write("server.pem", &srv.cert_pem)?;
std::fs::write("server-key.pem", &srv.key_pem)?;

// 3. Buat sertifikat klien (mTLS)
let client = generate_client_cert("myapp")?;
std::fs::write("client.pem", &client.cert_pem)?;
std::fs::write("client-key.pem", &client.key_pem)?;
```

### Pembuatan Manual (OpenSSL)

```bash
# CA
openssl req -x509 -newkey rsa:4096 -keyout ca-key.pem -out ca.pem -days 3650 -nodes

# Sertifikat server
openssl req -new -newkey rsa:4096 -keyout server-key.pem -out server.csr -nodes -subj "/CN=db.example.com"
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -out server.pem -days 365

# Sertifikat klien (mTLS)
openssl req -new -newkey rsa:4096 -keyout client-key.pem -out client.csr -nodes -subj "/CN=myapp"
openssl x509 -req -in client.csr -CA ca.pem -CAkey ca-key.pem -out client.pem -days 365
```

### Keterangan Kolom TLS

| Kolom | Tipe | Keterangan |
|------|------|------|
| `ca_cert` | `Option<String>` | Jalur PEM sertifikat CA (memverifikasi server) |
| `client_cert` | `Option<String>` | Jalur PEM sertifikat klien (mTLS) |
| `client_key` | `Option<String>` | Jalur PEM kunci privat klien (mTLS) |
| `skip_verify` | `Option<bool>` | Lewati verifikasi sertifikat (hanya pengujian) |

---

## Penggunaan Lanjutan

### Override Variabel Lingkungan

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

### Menggabungkan dengan Framework ecat-config

```rust
use ecat_config::{Config, FileSource};

let mut app_config = Config::new();
app_config.load(&FileSource::new("databases.yaml")).await?;

let redis_cfg: RedisConfig = serde_json::from_value(
    app_config.get::<serde_json::Value>("redis").unwrap()
)?;
let cache = RedisCache::from_config(redis_cfg).await?;
```

### Konfigurasi Sesuai Kebutuhan

Database yang tidak digunakan dihilangkan di YAML, struct Rust ditandai dengan `Option`:

```rust
#[derive(Deserialize)]
struct AppConfig {
    sql: SqlxConfig,
    redis: Option<RedisConfig>,
    clickhouse: Option<ClickhouseConfig>,
}
```

---

## Dokumen Terkait

- [Laporan Audit r5](audit-report-2026-08-01-r5.md)
- [Tutorial Sertifikat TLS](tls-certificate-tutorial.md)
- [File konfigurasi contoh](../../../config/databases.example.yaml)
