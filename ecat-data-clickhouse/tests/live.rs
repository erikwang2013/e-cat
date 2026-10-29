// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Optional live ClickHouse test. Set `ECAT_CLICKHOUSE_URL` plus optional
//! `ECAT_CLICKHOUSE_USER` / `ECAT_CLICKHOUSE_PASSWORD`.

use ecat_data::SqlExecutor;
use ecat_data_clickhouse::ClickhouseClient;

#[tokio::test]
async fn live_query_and_write() {
    let Ok(url) = std::env::var("ECAT_CLICKHOUSE_URL") else {
        eprintln!("ECAT_CLICKHOUSE_URL not set; skipping live ClickHouse test");
        return;
    };
    let user = std::env::var("ECAT_CLICKHOUSE_USER").unwrap_or_else(|_| "default".into());
    let password = std::env::var("ECAT_CLICKHOUSE_PASSWORD").unwrap_or_default();
    let client = ClickhouseClient::with_auth(url, "default", user, password);

    let rows = client
        .query("SELECT 1 AS one, 'x' AS s")
        .await
        .expect("query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("one"), Some(&serde_json::json!(1)));
    assert_eq!(rows[0].get("s"), Some(&serde_json::json!("x")));

    client
        .execute("DROP TABLE IF EXISTS ecat_live_ch")
        .await
        .expect("drop");
    client
        .execute("CREATE TABLE ecat_live_ch (id UInt32) ENGINE = Memory")
        .await
        .expect("create");
    client
        .execute("INSERT INTO ecat_live_ch VALUES (1), (2)")
        .await
        .expect("insert");
    let rows = client
        .query("SELECT count() AS n FROM ecat_live_ch")
        .await
        .expect("count");
    assert_eq!(rows[0].get("n"), Some(&serde_json::json!(2)));
    client
        .execute("DROP TABLE ecat_live_ch")
        .await
        .expect("cleanup");
}
