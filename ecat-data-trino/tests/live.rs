// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Optional live Trino test. Set `ECAT_TRINO_URL` (e.g. `http://127.0.0.1:18080`).

use ecat_data::SqlExecutor;
use ecat_data_trino::TrinoClient;

#[tokio::test]
async fn live_select_one() {
    let Ok(url) = std::env::var("ECAT_TRINO_URL") else {
        eprintln!("ECAT_TRINO_URL not set; skipping live Trino test");
        return;
    };
    let client = TrinoClient::new(url);
    let rows = client.query("SELECT 1 AS one").await.expect("query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("one"), Some(&serde_json::json!(1)));
}
