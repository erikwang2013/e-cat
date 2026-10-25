// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! ClickHouse 冷查询：SQL 形状/转义/顺序，以及 env 门控的 live roundtrip。
use ecat_data::Row;
use ecat_data::SqlExecutor;
use ecat_data_clickhouse::ClickhouseClient;
use time::macros::datetime;

use super::{ClickhouseColdQuery, build_range_sql};
use crate::ColdQuery;

#[test]
fn clickhouse_cold_query_builds_validated_sql() {
    let sql = build_range_sql(
        "orders",
        "created_at",
        "id",
        "2026-01-01 00:00:00",
        "2026-02-01 00:00:00",
        10,
        5,
    )
    .unwrap();
    assert_eq!(
        sql,
        "SELECT * FROM orders WHERE created_at >= '2026-01-01 00:00:00' \
         AND created_at < '2026-02-01 00:00:00' ORDER BY id ASC LIMIT 10 OFFSET 5"
    );
}

#[test]
fn clickhouse_cold_query_rejects_bad_ident() {
    assert!(build_range_sql("orders;drop", "ts", "id", "a", "b", 1, 0).is_err());
    assert!(build_range_sql("orders", "bad col", "id", "a", "b", 1, 0).is_err());
    assert!(build_range_sql("orders", "ts", "1id", "a", "b", 1, 0).is_err());

    let client = ClickhouseClient::new("http://localhost:8123", "default");
    assert!(
        ClickhouseColdQuery::new(client, "bad-table".into(), "ts".into(), "id".into()).is_err()
    );
}

#[test]
fn clickhouse_cold_query_escapes_time_values() {
    let sql = build_range_sql("orders", "ts", "id", "2026-01-01' OR 1=1 --", r"b\c", 1, 0).unwrap();
    assert!(
        sql.contains(r"2026-01-01\' OR 1=1 --"),
        "single quote must be escaped: {sql}"
    );
    assert!(sql.contains(r"'b\\c'"), "backslash must be escaped: {sql}");
}

#[test]
fn clickhouse_cold_query_orders_by_pk_before_limit() {
    let sql = build_range_sql("orders", "ts", "id", "a", "b", 7, 3).unwrap();
    let order = sql.find("ORDER BY id ASC").expect("ORDER BY");
    let limit = sql.find("LIMIT 7").expect("LIMIT");
    let offset = sql.find("OFFSET 3").expect("OFFSET");
    assert!(order < limit, "ORDER BY must precede LIMIT: {sql}");
    assert!(limit < offset, "LIMIT must precede OFFSET: {sql}");
}

fn row_id(row: &Row) -> i64 {
    row.get("id")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
        .expect("id column")
}

#[tokio::test]
async fn clickhouse_cold_query_roundtrip() {
    let Ok(url) = std::env::var("ECAT_TEST_CLICKHOUSE_URL") else {
        eprintln!("skip clickhouse_cold_query_roundtrip: ECAT_TEST_CLICKHOUSE_URL not set");
        return;
    };
    let table = format!(
        "ecat_test_cold_{}",
        time::OffsetDateTime::now_utc()
            .unix_timestamp_nanos()
            .unsigned_abs()
    );
    let setup = ClickhouseClient::new(&url, "default");
    setup
        .execute(&format!(
            "CREATE TABLE {table} (id UInt64, ts DateTime) ENGINE = Memory"
        ))
        .await
        .expect("create table");
    for id in [3u64, 1, 2] {
        setup
            .execute(&format!(
                "INSERT INTO {table} (id, ts) VALUES ({id}, '2026-01-15 00:00:00')"
            ))
            .await
            .expect("insert");
    }

    let query = ClickhouseColdQuery::new(
        ClickhouseClient::new(&url, "default"),
        table.clone(),
        "ts".into(),
        "id".into(),
    )
    .unwrap();
    let rows = query
        .query_range(
            &table,
            datetime!(2026-01-01 00:00:00 UTC),
            datetime!(2026-02-01 00:00:00 UTC),
            &[],
            2,
            1,
        )
        .await
        .expect("cold query");
    assert_eq!(rows.iter().map(row_id).collect::<Vec<_>>(), vec![2, 3]);

    let _ = setup.execute(&format!("DROP TABLE {table}")).await;
}
