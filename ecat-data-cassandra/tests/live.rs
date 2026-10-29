// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Optional live Cassandra test. Set `ECAT_CASSANDRA_NODES` to comma-separated
//! `host:port` entries (e.g. `127.0.0.1:9042`).

use ecat_data::SqlExecutor;
use ecat_data_cassandra::{CassandraClient, CassandraConfig};
use serde_json::json;

#[tokio::test]
async fn live_cql_query_and_parameter_binding() {
    let Ok(nodes) = std::env::var("ECAT_CASSANDRA_NODES") else {
        eprintln!("ECAT_CASSANDRA_NODES not set; skipping live Cassandra test");
        return;
    };
    let nodes: Vec<String> = nodes.split(',').map(str::to_owned).collect();
    let cfg: CassandraConfig = serde_json::from_value(json!({
        "nodes": nodes,
        "keyspace": "system",
    }))
    .expect("config");
    let client = CassandraClient::from_config(cfg).await.expect("connect");

    let rows = client
        .query("SELECT release_version FROM system.local")
        .await
        .expect("query");
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0]
            .get("release_version")
            .and_then(|v| v.as_str())
            .is_some(),
        "release_version should be a string: {:?}",
        rows[0].values()
    );

    let rows = client
        .query_with(
            "SELECT release_version FROM system.local WHERE key = ?",
            &[json!("local")],
        )
        .await
        .expect("query_with");
    assert_eq!(rows.len(), 1);
}
