# ecat-data-cassandra

Apache Cassandra CQL client for e-cat, backed by the pure-Rust
[`scylla`](https://crates.io/crates/scylla) driver (rustls, no OpenSSL).

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.

## Async construction

Building a Cassandra session performs network I/O, so the constructors are
`async` — unlike the synchronous RDBMS backends (`ecat-data-sqlx`,
`ecat-data-mssql`, QuestDB):

```rust
use ecat_data_cassandra::CassandraClient;

let client = CassandraClient::connect(&["127.0.0.1:9042".to_owned()], "my_keyspace").await?;
// or
let client = CassandraClient::from_config(config).await?;
```

## Limitations

- CQL has no multi-statement transactions: `transaction()` returns an error.
  Lightweight transactions (`IF NOT EXISTS`, `IF ...`) work per statement.
- `execute` returns 0 for writes (CQL reports no affected-row count); it only
  returns a row count when the server actually returns rows.
- JSON parameters: `null` binds CQL null, booleans, integers bind as `bigint`,
  floats as `double`, strings as `text`, and arrays of `0..=255` as `blob`.
  Objects are rejected. CQL markers are typed by the statement, so use an
  explicit cast (e.g. `CAST(? AS int)`) for columns of other numeric widths.
- Result mapping: blob → byte array, timestamp → RFC 3339 UTC, date →
  `YYYY-MM-DD`, time → `HH:MM:SS[.fffffffff]`, decimal/varint → decimal
  strings, list/set/tuple/vector → arrays, map/UDT → objects. Unknown future
  types fall back to the driver's `Debug` representation.
