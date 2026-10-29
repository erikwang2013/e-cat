# ecat-data-sqlx

SQLx multi-database client (PostgreSQL, MySQL, SQLite) for e-cat.

Apache Doris and StarRocks connect through the MySQL protocol; set
`mysql_compat: "doris"` / `"starrocks"` in `SqlxConfig` to disable sqlx's default
`SET sql_mode=(SELECT ...)` handshake statement, which StarRocks rejects.

Part of the [e-cat](https://github.com/erikwang2013/e-cat) ecosystem.
