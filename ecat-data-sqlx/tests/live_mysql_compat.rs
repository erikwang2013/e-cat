// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Optional live check for the `mysql_compat` connection path. Set
//! `ECAT_MYSQL_URL` (e.g. `mysql://root:ecat@127.0.0.1:23306/mysql`).
//!
//! This validates that the Doris/StarRocks connection options still work against
//! a real MySQL-protocol server; it is not a substitute for a Doris/StarRocks
//! cluster test.

use ecat_data::SqlExecutor;
use ecat_data_sqlx::{SqlxClient, SqlxConfig};
use serde_json::json;

#[tokio::test]
async fn live_mysql_compat_connects_and_queries() {
    let Ok(url) = std::env::var("ECAT_MYSQL_URL") else {
        eprintln!("ECAT_MYSQL_URL not set; skipping live MySQL compat test");
        return;
    };
    for compat in ["doris", "starrocks"] {
        let cfg: SqlxConfig = serde_json::from_value(json!({
            "url": url,
            "mysql_compat": compat,
        }))
        .expect("config");
        let client = SqlxClient::from_config(cfg).await.expect("connect");
        let rows = client.query("SELECT 1 AS one").await.expect("query");
        assert_eq!(rows[0].get("one"), Some(&json!(1)), "mysql_compat={compat}");
    }
}
